//! Parser for OpenAI's `text/event-stream` (SSE) streaming responses.

use std::collections::BTreeMap;

use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt};

use crate::provider::{ChatResponse, ProviderError, StreamEvent, ToolCall};

/// Accumulates the fragments of a single streamed tool call.
#[derive(Debug, Default)]
struct ToolCallAccumulator {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

impl ToolCallAccumulator {
    fn finish(self, index: usize) -> ToolCall {
        let args = if self.arguments.trim().is_empty() {
            serde_json::json!({})
        } else {
            serde_json::from_str(&self.arguments)
                .unwrap_or_else(|_| Value::String(self.arguments.clone()))
        };
        ToolCall {
            id: self.id.unwrap_or_else(|| format!("call_{index}")),
            tool_name: self.name.unwrap_or_default(),
            args,
            thought_signature: None,
        }
    }
}

/// Reads SSE chunks from an async reader and yields [`StreamEvent`]s.
///
/// Handles the OpenAI streaming format: `data: {json}` lines terminated by a
/// blank line and a final `data: [DONE]` marker. Tool-call fragments are
/// accumulated per `index` and finalized when `finish_reason` or `[DONE]` is
/// seen.
pub struct SseStreamParser<R> {
    reader: R,
    tool_acc: BTreeMap<usize, ToolCallAccumulator>,
    seen_done: bool,
}

impl<R: AsyncBufRead + Unpin> SseStreamParser<R> {
    /// Wrap an async buffered reader.
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            tool_acc: BTreeMap::new(),
            seen_done: false,
        }
    }

    /// Read the next event from the stream.
    ///
    /// Returns `Ok(None)` once the stream is exhausted.
    pub async fn next_event(&mut self) -> Result<Option<StreamEvent>, ProviderError> {
        if self.seen_done {
            return Ok(None);
        }

        loop {
            let mut line = String::new();
            let n = self
                .reader
                .read_line(&mut line)
                .await
                .map_err(|e| ProviderError::Other(format!("stream read error: {e}")))?;

            if n == 0 {
                // EOF without [DONE]: finalize anything pending.
                self.seen_done = true;
                let tool_calls = self.finalize_tool_calls();
                if !tool_calls.is_empty() {
                    return Ok(Some(StreamEvent {
                        content: String::new(),
                        tool_calls,
                        done: true,
                    }));
                }
                return Ok(None);
            }

            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let Some(data) = trimmed.strip_prefix("data:") else {
                // Ignore `event:`, `id:`, comments (`:`), etc.
                continue;
            };
            let data = data.trim();

            if data == "[DONE]" {
                self.seen_done = true;
                let tool_calls = self.finalize_tool_calls();
                return Ok(Some(StreamEvent {
                    content: String::new(),
                    tool_calls,
                    done: true,
                }));
            }

            let value: Value = serde_json::from_str(data).map_err(|e| {
                ProviderError::Other(format!("invalid SSE JSON payload: {e} (line: {data})"))
            })?;

            if let Some(err) = value.get("error") {
                return Err(ProviderError::Other(format!("stream error: {err}")));
            }

            let mut content = String::new();
            let mut finished = false;

            if let Some(choices) = value.get("choices").and_then(Value::as_array) {
                for choice in choices {
                    if let Some(delta) = choice.get("delta") {
                        if let Some(text) = delta.get("content").and_then(Value::as_str) {
                            content.push_str(text);
                        }

                        if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array)
                        {
                            for fragment in tool_calls {
                                let index =
                                    fragment.get("index").and_then(Value::as_u64).unwrap_or(0)
                                        as usize;
                                let acc = self.tool_acc.entry(index).or_default();

                                if let Some(id) = fragment.get("id").and_then(Value::as_str)
                                    && !id.is_empty()
                                {
                                    acc.id = Some(id.to_string());
                                }
                                if let Some(func) = fragment.get("function") {
                                    if let Some(name) = func.get("name").and_then(Value::as_str)
                                        && !name.is_empty()
                                    {
                                        acc.name = Some(name.to_string());
                                    }
                                    if let Some(args) =
                                        func.get("arguments").and_then(Value::as_str)
                                    {
                                        acc.arguments.push_str(args);
                                    }
                                }
                            }
                        }

                        if choice
                            .get("finish_reason")
                            .map(|f| !f.is_null())
                            .unwrap_or(false)
                        {
                            finished = true;
                        }
                    }
                }
            }

            if finished {
                self.seen_done = true;
                let tool_calls = self.finalize_tool_calls();
                return Ok(Some(StreamEvent {
                    content,
                    tool_calls,
                    done: true,
                }));
            }

            if !content.is_empty() {
                return Ok(Some(StreamEvent {
                    content,
                    tool_calls: Vec::new(),
                    done: false,
                }));
            }

            // Chunk without user-visible content (e.g. role-only preamble,
            // usage-only trailer): keep reading.
        }
    }

    fn finalize_tool_calls(&mut self) -> Vec<ToolCall> {
        std::mem::take(&mut self.tool_acc)
            .into_iter()
            .map(|(index, acc)| acc.finish(index))
            .collect()
    }

    /// Forward every event to `on_event` while accumulating the full
    /// [`ChatResponse`] (used by streaming chat calls).
    pub async fn stream_with(
        mut self,
        on_event: &mut (dyn FnMut(StreamEvent) + Send),
    ) -> Result<ChatResponse, ProviderError> {
        let mut response = ChatResponse::default();
        while let Some(event) = self.next_event().await? {
            response.content.push_str(&event.content);
            response.tool_calls.extend(event.tool_calls.clone());
            let done = event.done;
            on_event(event);
            if done {
                break;
            }
        }
        Ok(response)
    }

    /// Drain the whole stream into a single [`ChatResponse`].
    pub async fn collect(self) -> Result<ChatResponse, ProviderError> {
        self.stream_with(&mut |_| {}).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::BufReader;

    const TEXT_STREAM: &str = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\" world\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    );

    const TOOL_STREAM: &str = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"tool_calls\":[{\"index\":0,\"id\":\"call_abc\",\"type\":\"function\",\"function\":{\"name\":\"write\",\"arguments\":\"\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"pa\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"th\\\":\\\"a.txt\\\"}\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n",
    );

    const MIXED_STREAM: &str = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Let me check.\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\\\"a\\\"}\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n",
    );

    #[tokio::test]
    async fn parses_text_stream() {
        let parser = SseStreamParser::new(BufReader::new(TEXT_STREAM.as_bytes()));
        let response = parser.collect().await.expect("stream parses");
        assert_eq!(response.content, "Hello world");
        assert!(response.tool_calls.is_empty());
    }

    #[tokio::test]
    async fn parses_tool_call_stream_with_fragmented_arguments() {
        let parser = SseStreamParser::new(BufReader::new(TOOL_STREAM.as_bytes()));
        let response = parser.collect().await.expect("stream parses");
        assert_eq!(response.content, "");
        assert_eq!(response.tool_calls.len(), 1);
        let call = &response.tool_calls[0];
        assert_eq!(call.id, "call_abc");
        assert_eq!(call.tool_name, "write");
        assert_eq!(call.args["path"], "a.txt");
    }

    #[tokio::test]
    async fn parses_content_then_tool_call() {
        let parser = SseStreamParser::new(BufReader::new(MIXED_STREAM.as_bytes()));
        let response = parser.collect().await.expect("stream parses");
        assert_eq!(response.content, "Let me check.");
        assert_eq!(response.tool_calls.len(), 1);
        assert_eq!(response.tool_calls[0].tool_name, "read");
    }

    #[tokio::test]
    async fn handles_stream_without_done_marker() {
        let raw = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"},\"finish_reason\":\"stop\"}]}\n\n";
        let parser = SseStreamParser::new(BufReader::new(raw.as_bytes()));
        let response = parser.collect().await.expect("stream parses");
        assert_eq!(response.content, "partial");
    }

    #[tokio::test]
    async fn stream_with_forwards_events_then_finalizes() {
        let mut events = Vec::new();
        let response = SseStreamParser::new(BufReader::new(TEXT_STREAM.as_bytes()))
            .stream_with(&mut |event| {
                events.push((event.content.clone(), event.tool_calls.len(), event.done));
            })
            .await
            .expect("stream parses");
        assert_eq!(
            events,
            vec![
                ("Hello".to_string(), 0, false),
                (" world".to_string(), 0, false),
                (String::new(), 0, true),
            ]
        );
        assert_eq!(response.content, "Hello world");
    }

    #[tokio::test]
    async fn surfaces_error_payloads() {
        let raw =
            "data: {\"error\":{\"message\":\"bad key\",\"type\":\"invalid_request_error\"}}\n\n";
        let parser = SseStreamParser::new(BufReader::new(raw.as_bytes()));
        let err = parser.collect().await.expect_err("error surfaces");
        assert!(err.to_string().contains("bad key"));
    }
}
