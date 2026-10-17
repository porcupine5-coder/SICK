//! Provider abstraction shared by all LLM backends.

use async_trait::async_trait;

/// A single message in the conversation history.
#[derive(Debug, Clone, Default)]
pub struct ChatMessage {
    /// One of `system`, `user`, `assistant`, `tool`.
    pub role: String,
    /// Text content of the message (absent for pure tool-call assistant turns).
    pub content: Option<String>,
    /// Set for `tool` messages: the id of the tool call being answered.
    pub tool_call_id: Option<String>,
    /// Set for `assistant` messages that requested tool calls.
    pub tool_calls: Option<Vec<ToolCall>>,
}

impl ChatMessage {
    /// Convenience constructor for a simple role/content message.
    pub fn new(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: Some(content.into()),
            ..Default::default()
        }
    }
}

/// A tool call requested by the model.
#[derive(Debug, Clone)]
pub struct ToolCall {
    /// Provider-assigned call id (echoed back on the `tool` message).
    pub id: String,
    /// Name of the tool to execute.
    pub tool_name: String,
    /// Parsed JSON arguments for the tool.
    pub args: serde_json::Value,
    /// Gemini-only: opaque `thoughtSignature` returned alongside a `functionCall`
    /// part. Must be echoed back verbatim when the conversation (including this
    /// function call) is sent to the API again, or Gemini rejects the request
    /// with `400 INVALID_ARGUMENT`.
    ///
    /// `None` for providers that do not use thought signatures.
    pub thought_signature: Option<String>,
}

/// A tool exposed to the model.
#[derive(Debug, Clone)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    /// JSON Schema for the tool parameters.
    pub parameters: serde_json::Value,
}

/// A chat completion request.
#[derive(Debug, Clone)]
pub struct CompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub api_key: String,
    pub tools: Vec<ToolDefinition>,
    pub temperature: f32,
    pub max_tokens: Option<usize>,
    pub stream: bool,
}

/// A chunk of a streamed response.
#[derive(Debug, Clone, Default)]
pub struct StreamEvent {
    /// Content delta carried by this chunk (empty for chunks that only carry
    /// tool-call fragments or a finish marker).
    pub content: String,
    /// Tool calls finalized by the end of this chunk (normally emitted on the
    /// chunk carrying `finish_reason`).
    pub tool_calls: Vec<ToolCall>,
    /// True when the stream reported a `finish_reason` or the `[DONE]` marker.
    pub done: bool,
}

/// A fully accumulated assistant response.
#[derive(Debug, Clone, Default)]
pub struct ChatResponse {
    /// Complete assistant text.
    pub content: String,
    /// All tool calls requested by the model.
    pub tool_calls: Vec<ToolCall>,
}

/// Errors surfaced by LLM providers.
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("Rate limit exceeded")]
    RateLimitExceeded,
    #[error("Context window exceeded")]
    ContextWindowExceeded,
    #[error("Authentication failed")]
    AuthenticationFailed,
    #[error("Token quota exceeded")]
    TokenQuotaExceeded,
    #[error("Service unavailable ({status}): {message}")]
    ServiceUnavailable { status: u16, message: String },
    #[error("Other: {0}")]
    Other(String),
}

/// Abstracts a specific LLM chat-completions API.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Provider identifier, e.g. `"openai"`.
    fn id(&self) -> &str;

    /// Send a chat completion request.
    ///
    /// When `req.stream` is true the returned reader yields the raw
    /// `text/event-stream` body, which can be decoded with
    /// [`crate::stream_parser::SseStreamParser`].
    async fn get_completion(
        &self,
        req: CompletionRequest,
    ) -> Result<Box<dyn tokio::io::AsyncRead + Unpin + Send>, ProviderError>;

    /// Rough token count for the given messages (chars / 4 approximation).
    fn count_tokens(&self, messages: &[ChatMessage]) -> usize;

    /// Context window size (in tokens) for the given model id.
    fn context_window(&self, model_id: &str) -> usize;

    /// Send a chat completion request and accumulate the full response.
    ///
    /// The default implementation assumes the provider speaks the OpenAI
    /// `text/event-stream` format (true for OpenAI and OpenAI-compatible
    /// endpoints). Providers with their own streaming format override this.
    async fn chat(&self, req: CompletionRequest) -> Result<ChatResponse, ProviderError> {
        let reader = self.get_completion(req).await?;
        crate::stream_parser::SseStreamParser::new(tokio::io::BufReader::new(reader))
            .collect()
            .await
    }

    /// Send a chat completion request, reporting streamed chunks through
    /// `on_event` while accumulating the full response.
    ///
    /// The default implementation falls back to [`Self::chat`] and replays the
    /// finished response as events (one content event plus a final event
    /// carrying the tool calls), so every provider works with streaming
    /// consumers. Providers whose wire format supports incremental chunks
    /// override this to emit tokens as they arrive.
    async fn chat_streaming(
        &self,
        req: CompletionRequest,
        on_event: &mut (dyn FnMut(StreamEvent) + Send),
    ) -> Result<ChatResponse, ProviderError> {
        let response = self.chat(req).await?;
        if !response.content.is_empty() {
            on_event(StreamEvent {
                content: response.content.clone(),
                ..Default::default()
            });
        }
        on_event(StreamEvent {
            content: String::new(),
            tool_calls: response.tool_calls.clone(),
            done: true,
        });
        Ok(response)
    }
}
