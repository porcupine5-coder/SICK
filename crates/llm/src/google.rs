//! Google Gemini provider (native `generateContent` API, streaming via
//! `streamGenerateContent?alt=sse`).
//!
//! Translates the shared [`ChatMessage`] history into Gemini's `contents`
//! format (`functionCall` / `functionResponse` parts) and parses the SSE
//! stream of `GenerateContentResponse` objects.

use std::collections::HashMap;

use async_trait::async_trait;
use futures_util::TryStreamExt;
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, BufReader};

use crate::provider::{
    ChatMessage, ChatResponse, CompletionRequest, LlmProvider, ProviderError, StreamEvent, ToolCall,
};

/// Default Gemini API base URL.
pub const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

/// Provider for the Google Gemini `generateContent` API.
#[derive(Debug, Clone)]
pub struct GoogleProvider {
    base_url: String,
}

impl GoogleProvider {
    /// Create a provider targeting the official Gemini API.
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
}

impl Default for GoogleProvider {
    fn default() -> Self {
        Self::new()
    }
}

/// Build the Gemini `generateContent` payload from a generic request.
pub(crate) fn build_payload(req: &CompletionRequest) -> Value {
    // Gemini's functionResponse requires the function *name*, but the shared
    // tool message only carries the call id: look it up from the assistant
    // turns that requested the call.
    let call_names: HashMap<&str, &str> = req
        .messages
        .iter()
        .filter_map(|m| m.tool_calls.as_ref())
        .flatten()
        .map(|c| (c.id.as_str(), c.tool_name.as_str()))
        .collect();

    let mut contents: Vec<Value> = Vec::new();
    let mut system = String::new();

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
                push_parts(
                    &mut contents,
                    "user",
                    &[json!({ "text": message.content.clone().unwrap_or_default() })],
                );
            }
            "assistant" => {
                let mut parts: Vec<Value> = Vec::new();
                if let Some(content) = &message.content
                    && !content.is_empty()
                {
                    parts.push(json!({ "text": content }));
                }
                if let Some(calls) = &message.tool_calls {
                    for call in calls {
                        let mut part = json!({
                            "functionCall": {
                                "name": call.tool_name,
                                "args": call.args,
                            }
                        });
                        // Gemini 3 rejects replayed function calls whose
                        // `thoughtSignature` is missing, so echo it verbatim.
                        if let Some(signature) = &call.thought_signature {
                            part["thoughtSignature"] = json!(signature);
                        }
                        parts.push(part);
                    }
                }
                if !parts.is_empty() {
                    push_parts(&mut contents, "model", &parts);
                }
            }
            "tool" => {
                let name = message
                    .tool_call_id
                    .as_deref()
                    .and_then(|id| call_names.get(id).copied())
                    .unwrap_or("unknown");
                push_parts(
                    &mut contents,
                    "user",
                    &[json!({
                        "functionResponse": {
                            "name": name,
                            "response": {
                                "result": message.content.clone().unwrap_or_default(),
                            }
                        }
                    })],
                );
            }
            _ => {}
        }
    }

    let mut payload = json!({
        "contents": contents,
        "generationConfig": {
            "temperature": req.temperature,
        },
    });

    if let Some(max_tokens) = req.max_tokens {
        payload["generationConfig"]["maxOutputTokens"] = json!(max_tokens);
    }

    if !system.is_empty() {
        payload["systemInstruction"] = json!({ "parts": [{ "text": system }] });
    }

    if !req.tools.is_empty() {
        // The REST API expects `tools` to be an *array* of tool objects, each
        // carrying `functionDeclarations`.
        payload["tools"] = json!([{
            "functionDeclarations": req.tools
                .iter()
                .map(|t| json!({
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                }))
                .collect::<Vec<_>>()
        }]);
    }

    payload
}

/// Append parts to `contents`, merging into the trailing message when it has
/// the same role (Gemini is happiest with alternating roles).
fn push_parts(contents: &mut Vec<Value>, role: &str, parts: &[Value]) {
    if let Some(last) = contents.last_mut()
        && last["role"] == role
        && let Some(existing) = last["parts"].as_array_mut()
    {
        existing.extend_from_slice(parts);
        return;
    }
    contents.push(json!({ "role": role, "parts": parts }));
}

fn classify_error(status: u16, body: &str) -> ProviderError {
    let lower = body.to_lowercase();
    match status {
        401 | 403 => ProviderError::AuthenticationFailed,
        429 => {
            if lower.contains("quota") {
                ProviderError::TokenQuotaExceeded
            } else {
                ProviderError::RateLimitExceeded
            }
        }
        400 => {
            if lower.contains("token count") && lower.contains("exceed")
                || lower.contains("context")
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

/// Parse the `alt=sse` stream of `GenerateContentResponse` chunks, forwarding
/// every text part to `on_event` (plus one final event carrying the tool
/// calls).
pub(crate) async fn parse_generate_content_stream<R, F>(
    mut reader: R,
    mut on_event: F,
) -> Result<ChatResponse, ProviderError>
where
    R: AsyncBufRead + Unpin,
    F: FnMut(StreamEvent),
{
    let mut response = ChatResponse::default();
    let mut call_index = 0usize;

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
            continue;
        };
        let data = data.trim();
        if data.is_empty() {
            continue;
        }

        let value: Value = serde_json::from_str(data).map_err(|e| {
            ProviderError::Other(format!("invalid SSE JSON payload: {e} (line: {data})"))
        })?;

        if let Some(error) = value.get("error") {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("gemini stream error");
            let status = error.get("code").and_then(Value::as_u64).unwrap_or(0) as u16;
            return Err(classify_error(status, message));
        }

        let Some(candidates) = value.get("candidates").and_then(Value::as_array) else {
            continue;
        };

        for candidate in candidates {
            let Some(parts) = candidate
                .get("content")
                .and_then(|c| c.get("parts"))
                .and_then(Value::as_array)
            else {
                continue;
            };

            for part in parts {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    response.content.push_str(text);
                    on_event(StreamEvent {
                        content: text.to_string(),
                        ..Default::default()
                    });
                }
                if let Some(call) = part.get("functionCall") {
                    let tool_name = call
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let args = call.get("args").cloned().unwrap_or_else(|| json!({}));
                    // Gemini 3 requires the opaque `thoughtSignature` that
                    // accompanied this function call to be sent back verbatim
                    // on the next request (see `build_payload`).
                    let thought_signature = part
                        .get("thoughtSignature")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    response.tool_calls.push(ToolCall {
                        id: format!("call_{call_index}"),
                        tool_name,
                        args,
                        thought_signature,
                    });
                    call_index += 1;
                }
            }
        }
    }

    on_event(StreamEvent {
        content: String::new(),
        tool_calls: response.tool_calls.clone(),
        done: true,
    });

    Ok(response)
}

#[async_trait]
impl LlmProvider for GoogleProvider {
    fn id(&self) -> &str {
        "google"
    }

    fn context_window(&self, model_id: &str) -> usize {
        match model_id {
            m if m.contains("gemini-2.0") || m.contains("gemini-1.5") => 1_048_576,
            m if m.contains("gemini") => 1_048_576,
            _ => 1_048_576,
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

        let method = if req.stream {
            "streamGenerateContent"
        } else {
            "generateContent"
        };
        let url = format!("{}/models/{}:{}", self.base_url, req.model, method);

        let mut query: Vec<(&str, &str)> = vec![("key", req.api_key.as_str())];
        if req.stream {
            query.push(("alt", "sse"));
        }

        let resp = reqwest::Client::new()
            .post(url)
            .query(&query)
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

    /// Streaming is always requested here; Gemini's `alt=sse` stream yields
    /// one `GenerateContentResponse` JSON block per `data:` line.
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
        parse_generate_content_stream(BufReader::new(reader), &mut *on_event).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::ToolDefinition;

    fn sample_request() -> CompletionRequest {
        CompletionRequest {
            model: "gemini-2.0-flash".to_string(),
            messages: vec![
                ChatMessage::new("system", "You are helpful."),
                ChatMessage::new("user", "read a.txt"),
                ChatMessage {
                    role: "assistant".to_string(),
                    content: None,
                    tool_call_id: None,
                    tool_calls: Some(vec![ToolCall {
                        id: "call_0".to_string(),
                        tool_name: "read".to_string(),
                        args: json!({ "path": "a.txt" }),
                        thought_signature: None,
                    }]),
                },
                ChatMessage {
                    role: "tool".to_string(),
                    content: Some("hello".to_string()),
                    tool_call_id: Some("call_0".to_string()),
                    tool_calls: None,
                },
            ],
            api_key: "AIza-test".to_string(),
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
    fn payload_uses_contents_and_function_declarations() {
        let payload = build_payload(&sample_request());
        assert_eq!(
            payload["systemInstruction"]["parts"][0]["text"],
            "You are helpful."
        );
        assert_eq!(payload["generationConfig"]["maxOutputTokens"], 1024);
        assert_eq!(
            payload["tools"][0]["functionDeclarations"][0]["name"],
            "read"
        );
        assert_eq!(payload["contents"][0]["role"], "user");
        assert_eq!(payload["contents"][1]["role"], "model");
        assert_eq!(
            payload["contents"][1]["parts"][0]["functionCall"]["name"],
            "read"
        );
        // Function response carries the *name*, resolved from the call id.
        let response_part = &payload["contents"][2]["parts"][0]["functionResponse"];
        assert_eq!(response_part["name"], "read");
        assert_eq!(response_part["response"]["result"], "hello");
    }

    #[test]
    fn error_classification_covers_gemini_signals() {
        assert!(matches!(
            classify_error(429, "RESOURCE_EXHAUSTED"),
            ProviderError::RateLimitExceeded
        ));
        assert!(matches!(
            classify_error(429, "Quota exceeded for quota metric"),
            ProviderError::TokenQuotaExceeded
        ));
        assert!(matches!(
            classify_error(403, "permission denied"),
            ProviderError::AuthenticationFailed
        ));
        assert!(matches!(
            classify_error(400, "The input token count exceeds the maximum"),
            ProviderError::ContextWindowExceeded
        ));
        assert!(matches!(
            classify_error(503, "The model is overloaded"),
            ProviderError::ServiceUnavailable { .. }
        ));
    }

    const TEXT_STREAM: &str = concat!(
        "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Hello\"}],\"role\":\"model\"}}]}\n\n",
        "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\" world\"}],\"role\":\"model\"},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"totalTokenCount\":7}}\n\n",
    );

    const TOOL_STREAM: &str = "data: {\"candidates\":[{\"content\":{\"parts\":[{\"functionCall\":{\"name\":\"read\",\"args\":{\"path\":\"a.txt\"}}}],\"role\":\"model\"},\"finishReason\":\"STOP\"}]}\n\n";

    #[tokio::test]
    async fn parses_text_stream() {
        let response =
            parse_generate_content_stream(BufReader::new(TEXT_STREAM.as_bytes()), &mut |_| {})
                .await
                .expect("stream parses");
        assert_eq!(response.content, "Hello world");
        assert!(response.tool_calls.is_empty());
    }

    #[tokio::test]
    async fn parses_function_call_stream() {
        let response =
            parse_generate_content_stream(BufReader::new(TOOL_STREAM.as_bytes()), &mut |_| {})
                .await
                .expect("stream parses");
        assert_eq!(response.tool_calls.len(), 1);
        assert_eq!(response.tool_calls[0].id, "call_0");
        assert_eq!(response.tool_calls[0].tool_name, "read");
        assert_eq!(response.tool_calls[0].args["path"], "a.txt");
    }

    const SIGNED_TOOL_STREAM: &str = "data: {\"candidates\":[{\"content\":{\"parts\":[{\"functionCall\":{\"name\":\"ls\",\"args\":{\"path\":\".\"}},\"thoughtSignature\":\"c2lnbmF0dXJl\"}],\"role\":\"model\"},\"finishReason\":\"STOP\"}]}\n\n";

    #[tokio::test]
    async fn parses_thought_signature_from_function_call_stream() {
        let response = parse_generate_content_stream(
            BufReader::new(SIGNED_TOOL_STREAM.as_bytes()),
            &mut |_| {},
        )
        .await
        .expect("stream parses");
        assert_eq!(response.tool_calls.len(), 1);
        assert_eq!(
            response.tool_calls[0].thought_signature.as_deref(),
            Some("c2lnbmF0dXJl")
        );
    }

    #[tokio::test]
    async fn payload_echoes_thought_signature_verbatim() {
        let response = parse_generate_content_stream(
            BufReader::new(SIGNED_TOOL_STREAM.as_bytes()),
            &mut |_| {},
        )
        .await
        .expect("stream parses");

        let mut request = sample_request();
        request.messages = vec![
            ChatMessage::new("user", "list the workspace"),
            ChatMessage {
                role: "assistant".to_string(),
                content: None,
                tool_call_id: None,
                tool_calls: Some(response.tool_calls.clone()),
            },
            ChatMessage {
                role: "tool".to_string(),
                content: Some(".".to_string()),
                tool_call_id: Some("call_0".to_string()),
                tool_calls: None,
            },
        ];
        let payload = build_payload(&request);
        // The exact signature must be replayed on the functionCall part...
        assert_eq!(
            payload["contents"][1]["parts"][0]["thoughtSignature"],
            "c2lnbmF0dXJl"
        );
        assert_eq!(
            payload["contents"][1]["parts"][0]["functionCall"]["name"],
            "ls"
        );
        // ...while the tool response still pairs back by name.
        assert_eq!(
            payload["contents"][2]["parts"][0]["functionResponse"]["name"],
            "ls"
        );

        // Calls without a signature must not gain the field.
        let plain = build_payload(&sample_request());
        assert!(
            plain["contents"][1]["parts"][0]
                .get("thoughtSignature")
                .is_none()
        );
    }

    #[tokio::test]
    async fn streams_text_parts_incrementally() {
        let mut tokens = Vec::new();
        let response =
            parse_generate_content_stream(BufReader::new(TEXT_STREAM.as_bytes()), |event| {
                if !event.content.is_empty() {
                    tokens.push(event.content);
                }
            })
            .await
            .expect("stream parses");
        assert_eq!(tokens, vec!["Hello".to_string(), " world".to_string()]);
        assert_eq!(response.content, "Hello world");
    }

    #[tokio::test]
    async fn final_event_carries_tool_calls() {
        let mut done_tool_calls = 0usize;
        let mut saw_done = false;
        let _ = parse_generate_content_stream(BufReader::new(TOOL_STREAM.as_bytes()), |event| {
            if event.done {
                saw_done = true;
                done_tool_calls = event.tool_calls.len();
            }
        })
        .await
        .expect("stream parses");
        assert!(saw_done, "a final done event is emitted");
        assert_eq!(done_tool_calls, 1);
    }
}
