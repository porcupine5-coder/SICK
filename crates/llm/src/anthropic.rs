//! Anthropic Messages API provider (`/v1/messages`, streaming).
//!
//! Translates the shared [`ChatMessage`] history into Anthropic's request
//! format (top-level `system`, `tool_use` / `tool_result` content blocks) and
//! parses Anthropic's SSE stream (`content_block_delta` events).

use std::collections::BTreeMap;

use async_trait::async_trait;
use futures_util::TryStreamExt;
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, BufReader};

use crate::provider::{
    ChatMessage, ChatResponse, CompletionRequest, LlmProvider, ProviderError, StreamEvent, ToolCall,
};

/// Default Anthropic API base URL.
pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

/// `anthropic-version` header value required by the Messages API.
pub const API_VERSION: &str = "2023-06-01";

/// Assumed max output tokens when the request does not specify one.
const DEFAULT_MAX_TOKENS: usize = 4096;

/// Provider for the Anthropic Messages API.
#[derive(Debug, Clone)]
pub struct AnthropicProvider {
    base_url: String,
}

impl AnthropicProvider {
    /// Create a provider targeting the official Anthropic API.
    pub fn new() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
        }
    }

    /// Create a provider targeting a custom base URL (tests, proxies).
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
        }
    }

    /// The configured API base URL.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn messages_url(&self) -> String {
        format!("{}/v1/messages", self.base_url)
    }
}

impl Default for AnthropicProvider {
    fn default() -> Self {
        Self::new()
    }
}

/// Build the Anthropic `/v1/messages` payload from a generic request.
pub(crate) fn build_payload(req: &CompletionRequest) -> Value {
    let mut system = String::new();
    let mut messages: Vec<Value> = Vec::new();

    for message in &req.messages {
        match message.role.as_str() {
            "system" => {
                if let Some(content) = &message.content {
                    if !system.is_empty() {
                        system.push('\n');
                    }
                    system.push_str(content);
                }
            }
            "user" => {
                push_user_blocks(
                    &mut messages,
                    json!({ "type": "text", "text": message.content.clone().unwrap_or_default() }),
                );
            }
            "assistant" => {
                let mut blocks: Vec<Value> = Vec::new();
                if let Some(content) = &message.content
                    && !content.is_empty()
                {
                    blocks.push(json!({ "type": "text", "text": content }));
                }
                if let Some(calls) = &message.tool_calls {
                    for call in calls {
                        blocks.push(json!({
                            "type": "tool_use",
                            "id": call.id,
                            "name": call.tool_name,
                            "input": call.args,
                        }));
                    }
                }
                if !blocks.is_empty() {
                    messages.push(json!({ "role": "assistant", "content": blocks }));
                }
            }
            "tool" => {
                let block = json!({
                    "type": "tool_result",
                    "tool_use_id": message.tool_call_id.clone().unwrap_or_default(),
                    "content": message.content.clone().unwrap_or_default(),
                });
                push_user_blocks(&mut messages, block);
            }
            _ => {}
        }
    }

    let mut payload = json!({
        "model": req.model,
        "max_tokens": req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
        "messages": messages,
        // Anthropic requires temperature in [0, 1].
        "temperature": req.temperature.clamp(0.0, 1.0),
        "stream": req.stream,
    });

    if !system.is_empty() {
        payload["system"] = json!(system);
    }

    if !req.tools.is_empty() {
        payload["tools"] = json!(
            req.tools
                .iter()
                .map(|t| json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.parameters,
                }))
                .collect::<Vec<_>>()
        );
    }

    payload
}

/// Append a content block to the conversation, merging into the trailing user
/// message when possible (Anthropic requires `tool_result` blocks to be
/// carried by a user message, and multiple results should be batched).
fn push_user_blocks(messages: &mut Vec<Value>, block: Value) {
    if let Some(last) = messages.last_mut()
        && last["role"] == "user"
        && let Some(blocks) = last["content"].as_array_mut()
    {
        let all_tool_results = blocks.iter().all(|b| b["type"] == "tool_result");
        let is_tool_result = block["type"] == "tool_result";
        // Only merge when both messages are tool results (or a text
        // block follows directly), so user text stays its own turn.
        if all_tool_results && is_tool_result {
            blocks.push(block);
            return;
        }
    }
    messages.push(json!({ "role": "user", "content": [block] }));
}

fn classify_error(status: u16, body: &str) -> ProviderError {
    let lower = body.to_lowercase();
    match status {
        401 | 403 => ProviderError::AuthenticationFailed,
        429 => {
            if lower.contains("quota") || lower.contains("credit balance") {
                ProviderError::TokenQuotaExceeded
            } else {
                ProviderError::RateLimitExceeded
            }
        }
        400 => {
            if lower.contains("quota") || lower.contains("credit balance") {
                ProviderError::TokenQuotaExceeded
            } else if lower.contains("too many content")
                || lower.contains("context length")
                || lower.contains("context window")
                || lower.contains("prompt is too long")
                || lower.contains("too long")
            {
                ProviderError::ContextWindowExceeded
            } else {
                ProviderError::Other(format!("bad request: {body}"))
            }
        }
        500..=599 => ProviderError::ServiceUnavailable {
            status,
            message: body.to_string(),
        },
        _ => ProviderError::Other(format!("unexpected status {status}: {body}")),
    }
}

/// Map an SSE `error` event payload to a [`ProviderError`].
fn classify_stream_error(value: &Value) -> ProviderError {
    let error = value.get("error");
    let kind = error
        .and_then(|e| e.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let message = error
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("anthropic stream error");
    match kind {
        "rate_limit_error" => ProviderError::RateLimitExceeded,
        "authentication_error" | "permission_error" => ProviderError::AuthenticationFailed,
        "overloaded_error" => ProviderError::ServiceUnavailable {
            status: 529,
            message: message.to_string(),
        },
        _ => ProviderError::Other(format!("{kind}: {message}")),
    }
}

/// Parse Anthropic's SSE stream into a full [`ChatResponse`], forwarding every
/// text delta to `on_event` (plus one final event carrying the tool calls).
pub(crate) async fn parse_message_stream<R, F>(
    mut reader: R,
    mut on_event: F,
) -> Result<ChatResponse, ProviderError>
where
    R: AsyncBufRead + Unpin,
    F: FnMut(StreamEvent),
{
    let mut response = ChatResponse::default();
    // index → (id, name, partial_json)
    let mut tool_blocks: BTreeMap<usize, (String, String, String)> = BTreeMap::new();

    loop {
        let mut line = String::new();
        let n = reader
            .read_line(&mut line)
            .await
            .map_err(|e| ProviderError::Other(format!("stream read error: {e}")))?;
        if n == 0 {
            break;
        }

        let trimmed = line.trim();
        let Some(data) = trimmed.strip_prefix("data:") else {
            continue; // `event:` lines and comments are ignored.
        };
        let data = data.trim();
        if data.is_empty() {
            continue;
        }

        let value: Value = serde_json::from_str(data).map_err(|e| {
            ProviderError::Other(format!("invalid SSE JSON payload: {e} (line: {data})"))
        })?;

        match value.get("type").and_then(Value::as_str) {
            Some("content_block_start") => {
                let index = value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                if let Some(block) = value.get("content_block")
                    && block.get("type").and_then(Value::as_str) == Some("tool_use")
                {
                    let id = block
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let name = block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    tool_blocks.insert(index, (id, name, String::new()));
                }
            }
            Some("content_block_delta") => {
                let index = value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                if let Some(delta) = value.get("delta") {
                    match delta.get("type").and_then(Value::as_str) {
                        Some("text_delta") => {
                            if let Some(text) = delta.get("text").and_then(Value::as_str) {
                                response.content.push_str(text);
                                on_event(StreamEvent {
                                    content: text.to_string(),
                                    ..Default::default()
                                });
                            }
                        }
                        Some("input_json_delta") => {
                            if let Some(partial) = delta.get("partial_json").and_then(Value::as_str)
                                && let Some(entry) = tool_blocks.get_mut(&index)
                            {
                                entry.2.push_str(partial);
                            }
                        }
                        _ => {}
                    }
                }
            }
            Some("error") => return Err(classify_stream_error(&value)),
            Some("message_stop") => break,
            _ => {}
        }
    }

    for (index, (id, name, arguments)) in tool_blocks {
        let args = if arguments.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&arguments).unwrap_or_else(|_| Value::String(arguments.clone()))
        };
        response.tool_calls.push(ToolCall {
            id: if id.is_empty() {
                format!("toolu_{index}")
            } else {
                id
            },
            tool_name: name,
            args,
            thought_signature: None,
        });
    }

    on_event(StreamEvent {
        content: String::new(),
        tool_calls: response.tool_calls.clone(),
        done: true,
    });

    Ok(response)
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
    fn id(&self) -> &str {
        "anthropic"
    }

    fn context_window(&self, model_id: &str) -> usize {
        match model_id {
            m if m.contains("claude-3-7") || m.contains("claude-3.7") => 200_000,
            m if m.contains("claude-3-5") || m.contains("claude-3.5") => 200_000,
            m if m.contains("claude-3-opus") => 200_000,
            m if m.contains("claude") => 200_000,
            _ => 200_000,
        }
    }

    fn count_tokens(&self, messages: &[ChatMessage]) -> usize {
        let total: usize = messages
            .iter()
            .filter_map(|m| m.content.as_deref())
            .map(|c| c.chars().count())
            .sum();
        total / 4
    }

    async fn get_completion(
        &self,
        req: CompletionRequest,
    ) -> Result<Box<dyn AsyncRead + Unpin + Send>, ProviderError> {
        let payload = build_payload(&req);

        let resp = reqwest::Client::new()
            .post(self.messages_url())
            .header("x-api-key", &req.api_key)
            .header("anthropic-version", API_VERSION)
            .header("content-type", "application/json")
            .json(&payload)
            .send()
            .await
            .map_err(|e| ProviderError::ServiceUnavailable {
                status: 0,
                message: format!("network error: {e}"),
            })?;

        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(classify_error(status, &body));
        }

        let stream = resp.bytes_stream().map_err(std::io::Error::other);
        Ok(Box::new(tokio_util::io::StreamReader::new(stream)))
    }

    /// Streaming is always requested here; Anthropic's SSE stream carries
    /// text deltas and tool-use input fragments.
    async fn chat(&self, req: CompletionRequest) -> Result<ChatResponse, ProviderError> {
        self.chat_streaming(req, &mut |_| {}).await
    }

    async fn chat_streaming(
        &self,
        mut req: CompletionRequest,
        on_event: &mut (dyn FnMut(StreamEvent) + Send),
    ) -> Result<ChatResponse, ProviderError> {
        req.stream = true;
        let reader = self.get_completion(req).await?;
        parse_message_stream(BufReader::new(reader), &mut *on_event).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::ToolDefinition;

    fn sample_request() -> CompletionRequest {
        CompletionRequest {
            model: "claude-3-7-sonnet-20250219".to_string(),
            messages: vec![
                ChatMessage::new("system", "You are helpful."),
                ChatMessage::new("user", "read a.txt"),
                ChatMessage {
                    role: "assistant".to_string(),
                    content: Some("Reading.".to_string()),
                    tool_call_id: None,
                    tool_calls: Some(vec![ToolCall {
                        id: "toolu_1".to_string(),
                        tool_name: "read".to_string(),
                        args: json!({ "path": "a.txt" }),
                        thought_signature: None,
                    }]),
                },
                ChatMessage {
                    role: "tool".to_string(),
                    content: Some("hello".to_string()),
                    tool_call_id: Some("toolu_1".to_string()),
                    tool_calls: None,
                },
            ],
            api_key: "sk-ant-test".to_string(),
            tools: vec![ToolDefinition {
                name: "read".to_string(),
                description: "Read a file".to_string(),
                parameters: json!({"type": "object"}),
            }],
            temperature: 0.2,
            max_tokens: Some(1024),
            stream: true,
        }
    }

    #[test]
    fn payload_hoists_system_and_uses_input_schema() {
        let payload = build_payload(&sample_request());
        assert_eq!(payload["system"], "You are helpful.");
        assert_eq!(payload["max_tokens"], 1024);
        assert_eq!(payload["tools"][0]["input_schema"]["type"], "object");
        assert_eq!(payload["messages"][0]["role"], "user");
        // Assistant turn carries its text block first, then the tool_use.
        assert_eq!(payload["messages"][1]["content"][0]["type"], "text");
        assert_eq!(payload["messages"][1]["content"][1]["type"], "tool_use");
        assert_eq!(payload["messages"][1]["content"][1]["id"], "toolu_1");
        // Tool results are merged into a single user message.
        let result_msg = &payload["messages"][2];
        assert_eq!(result_msg["role"], "user");
        assert_eq!(result_msg["content"][0]["type"], "tool_result");
        assert_eq!(result_msg["content"][0]["tool_use_id"], "toolu_1");
    }

    #[test]
    fn groups_consecutive_tool_results_into_one_message() {
        let mut req = sample_request();
        req.messages.push(ChatMessage {
            role: "tool".to_string(),
            content: Some("second".to_string()),
            tool_call_id: Some("toolu_2".to_string()),
            tool_calls: None,
        });
        let payload = build_payload(&req);
        let last = payload["messages"].as_array().unwrap().last().unwrap();
        assert_eq!(last["role"], "user");
        assert_eq!(last["content"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn clamps_temperature_to_anthropic_range() {
        let mut req = sample_request();
        req.temperature = 1.8;
        let payload = build_payload(&req);
        assert_eq!(payload["temperature"], 1.0);
    }

    #[test]
    fn error_classification_covers_anthropic_signals() {
        assert!(matches!(
            classify_error(429, "rate limited"),
            ProviderError::RateLimitExceeded
        ));
        assert!(matches!(
            classify_error(429, "quota exceeded"),
            ProviderError::TokenQuotaExceeded
        ));
        assert!(matches!(
            classify_error(401, "invalid x-api-key"),
            ProviderError::AuthenticationFailed
        ));
        assert!(matches!(
            classify_error(400, "prompt is too long: 250000 tokens > 200000 maximum"),
            ProviderError::ContextWindowExceeded
        ));
        assert!(matches!(
            classify_error(529, "overloaded"),
            ProviderError::ServiceUnavailable { .. }
        ));
    }

    const TOOL_STREAM: &str = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\"}}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Let me check.\"}}\n\n",
        "event: content_block_stop\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_9\",\"name\":\"read\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"pa\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"th\\\":\\\"a.txt\\\"}\"}}\n\n",
        "event: content_block_stop\n",
        "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    #[tokio::test]
    async fn streams_text_deltas_incrementally() {
        let raw = concat!(
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Let me \"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"check.\"}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let mut tokens = Vec::new();
        let mut saw_done = false;
        let response = parse_message_stream(BufReader::new(raw.as_bytes()), |event| {
            saw_done |= event.done;
            if !event.content.is_empty() {
                tokens.push(event.content);
            }
        })
        .await
        .expect("stream parses");
        assert_eq!(tokens, vec!["Let me ".to_string(), "check.".to_string()]);
        assert!(saw_done, "a final done event is emitted");
        assert_eq!(response.content, "Let me check.");
    }

    #[tokio::test]
    async fn parses_text_and_tool_use_stream() {
        let response = parse_message_stream(BufReader::new(TOOL_STREAM.as_bytes()), &mut |_| {})
            .await
            .expect("stream parses");
        assert_eq!(response.content, "Let me check.");
        assert_eq!(response.tool_calls.len(), 1);
        assert_eq!(response.tool_calls[0].id, "toolu_9");
        assert_eq!(response.tool_calls[0].tool_name, "read");
        assert_eq!(response.tool_calls[0].args["path"], "a.txt");
    }

    #[tokio::test]
    async fn surfaces_stream_error_events() {
        let raw = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"overloaded\"}}\n\n";
        let err = parse_message_stream(BufReader::new(raw.as_bytes()), &mut |_| {})
            .await
            .expect_err("stream error surfaces");
        assert!(matches!(err, ProviderError::ServiceUnavailable { .. }));
    }
}
