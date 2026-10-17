//! OpenAI chat-completions provider (streaming).

use async_trait::async_trait;
use futures_util::TryStreamExt;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde_json::{Value, json};
use tokio::io::AsyncRead;
use tokio_util::io::StreamReader;

use crate::provider::{
    ChatMessage, ChatResponse, CompletionRequest, LlmProvider, ProviderError, StreamEvent, ToolCall,
};
use crate::stream_parser::SseStreamParser;

/// Default OpenAI API base URL.
pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// Provider for the OpenAI `/chat/completions` API (also works for any
/// OpenAI-compatible endpoint given a custom base URL).
#[derive(Debug, Clone)]
pub struct OpenAiProvider {
    base_url: String,
}

impl OpenAiProvider {
    /// Create a provider targeting the official OpenAI API.
    pub fn new() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
        }
    }

    /// Create a provider targeting an OpenAI-compatible base URL
    /// (e.g. `http://localhost:8080/v1`).
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

/// Build the OpenAI chat-completions JSON payload for a request.
pub(crate) fn build_chat_payload(req: &CompletionRequest) -> Value {
    let mut payload = json!({
        "model": req.model,
        "messages": req.messages.iter().map(message_to_json).collect::<Vec<_>>(),
        "temperature": req.temperature,
        "stream": req.stream,
    });

    if !req.tools.is_empty() {
        payload["tools"] = json!(
            req.tools
                .iter()
                .map(|t| json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters,
                    }
                }))
                .collect::<Vec<_>>()
        );
    }

    if let Some(max_tokens) = req.max_tokens {
        payload["max_tokens"] = json!(max_tokens);
    }

    payload
}

impl Default for OpenAiProvider {
    fn default() -> Self {
        Self::new()
    }
}

fn message_to_json(m: &ChatMessage) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("role".to_string(), json!(m.role));

    if let Some(content) = &m.content {
        obj.insert("content".to_string(), json!(content));
    } else if m.tool_calls.is_some() {
        // Assistant turns that only requested tools need an explicit null.
        obj.insert("content".to_string(), Value::Null);
    }

    if let Some(tool_calls) = &m.tool_calls {
        obj.insert(
            "tool_calls".to_string(),
            json!(tool_calls.iter().map(tool_call_to_json).collect::<Vec<_>>()),
        );
    }

    if let Some(tool_call_id) = &m.tool_call_id {
        obj.insert("tool_call_id".to_string(), json!(tool_call_id));
    }

    Value::Object(obj)
}

fn tool_call_to_json(tc: &ToolCall) -> Value {
    json!({
        "id": tc.id,
        "type": "function",
        "function": {
            "name": tc.tool_name,
            "arguments": tc.args.to_string(),
        }
    })
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
            if lower.contains("context") || lower.contains("too long") {
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

/// POST a chat-completions payload to `{base_url}/chat/completions` and hand
/// back the response body as an async reader.
///
/// Shared by [`OpenAiProvider`] and the OpenAI-compatible provider.
pub(crate) async fn post_chat_completions(
    base_url: &str,
    req: &CompletionRequest,
) -> Result<Box<dyn AsyncRead + Unpin + Send>, ProviderError> {
    let client = reqwest::Client::new();

    let mut headers = HeaderMap::new();
    let auth = HeaderValue::from_str(&format!("Bearer {}", req.api_key))
        .map_err(|e| ProviderError::Other(format!("invalid api key header: {e}")))?;
    headers.insert(AUTHORIZATION, auth);
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

    let payload = build_chat_payload(req);

    let resp = client
        .post(format!("{base_url}/chat/completions"))
        .headers(headers)
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

    // Body is handed back as an async reader; for `stream = true` it is the
    // raw `text/event-stream` payload decoded by the SSE parser.
    let stream = resp.bytes_stream().map_err(std::io::Error::other);
    let reader = StreamReader::new(stream);
    Ok(Box::new(reader))
}

/// Context-window table shared by OpenAI and OpenAI-compatible providers.
pub(crate) fn default_context_window(model_id: &str) -> usize {
    match model_id {
        m if m.contains("gpt-4.1") => 32768,
        m if m.contains("gpt-4o") => 128000,
        m if m.contains("gpt-3.5") => 16385,
        _ => 8192,
    }
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    fn id(&self) -> &str {
        "openai"
    }

    fn context_window(&self, model_id: &str) -> usize {
        default_context_window(model_id)
    }

    fn count_tokens(&self, messages: &[ChatMessage]) -> usize {
        // Approximation: 1 token ~= 4 chars.
        let total_chars: usize = messages
            .iter()
            .filter_map(|m| m.content.as_deref())
            .map(|c| c.chars().count())
            .sum();
        total_chars / 4
    }

    async fn get_completion(
        &self,
        req: CompletionRequest,
    ) -> Result<Box<dyn AsyncRead + Unpin + Send>, ProviderError> {
        post_chat_completions(&self.base_url, &req).await
    }

    async fn chat(&self, req: CompletionRequest) -> Result<ChatResponse, ProviderError> {
        let reader = self.get_completion(req).await?;
        SseStreamParser::new(tokio::io::BufReader::new(reader))
            .collect()
            .await
    }

    async fn chat_streaming(
        &self,
        req: CompletionRequest,
        on_event: &mut (dyn FnMut(StreamEvent) + Send),
    ) -> Result<ChatResponse, ProviderError> {
        let reader = self.get_completion(req).await?;
        SseStreamParser::new(tokio::io::BufReader::new(reader))
            .stream_with(on_event)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::ToolDefinition;

    fn sample_request() -> CompletionRequest {
        CompletionRequest {
            model: "gpt-4.1".to_string(),
            messages: vec![
                ChatMessage::new("system", "You are helpful."),
                ChatMessage {
                    role: "assistant".to_string(),
                    content: None,
                    tool_call_id: None,
                    tool_calls: Some(vec![ToolCall {
                        id: "call_1".to_string(),
                        tool_name: "read".to_string(),
                        args: json!({ "path": "a.txt" }),
                        thought_signature: None,
                    }]),
                },
                ChatMessage {
                    role: "tool".to_string(),
                    content: Some("file contents".to_string()),
                    tool_call_id: Some("call_1".to_string()),
                    tool_calls: None,
                },
            ],
            api_key: "sk-test".to_string(),
            tools: vec![ToolDefinition {
                name: "read".to_string(),
                description: "Read a file".to_string(),
                parameters: json!({"type": "object"}),
            }],
            temperature: 0.2,
            max_tokens: Some(100),
            stream: true,
        }
    }

    #[test]
    fn context_window_matches_plan_table() {
        let p = OpenAiProvider::new();
        assert_eq!(p.context_window("gpt-4.1"), 32768);
        assert_eq!(p.context_window("gpt-4o-mini"), 128000);
        assert_eq!(p.context_window("gpt-3.5-turbo"), 16385);
        assert_eq!(p.context_window("unknown-model"), 8192);
    }

    #[test]
    fn count_tokens_approximates_chars_over_four() {
        let p = OpenAiProvider::new();
        let msgs = vec![ChatMessage::new("user", "abcdefgh")];
        assert_eq!(p.count_tokens(&msgs), 2);
    }

    #[test]
    fn payload_contains_tools_and_messages() {
        let p = OpenAiProvider::with_base_url("http://localhost:1234/v1/");
        assert_eq!(p.base_url(), "http://localhost:1234/v1");

        let payload = build_chat_payload(&sample_request());
        assert_eq!(payload["model"], "gpt-4.1");
        assert_eq!(payload["stream"], true);
        assert_eq!(payload["max_tokens"], 100);
        assert_eq!(payload["messages"][1]["content"], Value::Null);
        assert_eq!(
            payload["messages"][1]["tool_calls"][0]["function"]["name"],
            "read"
        );
        assert_eq!(payload["messages"][2]["role"], "tool");
        assert_eq!(payload["tools"][0]["function"]["name"], "read");
    }

    #[test]
    fn error_classification_covers_http_signals() {
        assert!(matches!(
            classify_error(401, "unauthorized"),
            ProviderError::AuthenticationFailed
        ));
        assert!(matches!(
            classify_error(429, "rate limit"),
            ProviderError::RateLimitExceeded
        ));
        assert!(matches!(
            classify_error(429, "insufficient quota"),
            ProviderError::TokenQuotaExceeded
        ));
        assert!(matches!(
            classify_error(400, "maximum context length exceeded"),
            ProviderError::ContextWindowExceeded
        ));
        assert!(matches!(
            classify_error(500, "boom"),
            ProviderError::ServiceUnavailable { status: 500, .. }
        ));
        assert!(matches!(
            classify_error(503, "service unavailable"),
            ProviderError::ServiceUnavailable { status: 503, .. }
        ));
        assert!(matches!(
            classify_error(404, "not found"),
            ProviderError::Other(_)
        ));
    }
}
