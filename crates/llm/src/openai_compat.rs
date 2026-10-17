//! Generic OpenAI-compatible provider.
//!
//! Speaks the same `/chat/completions` protocol as
//! [`OpenAiProvider`](crate::openai::OpenAiProvider) but is
//! constructed with an arbitrary base URL (local llama.cpp, vLLM, LiteLLM,
//! OpenRouter, DeepSeek, Qwen DashScope, ...). 5xx responses are classified as
//! retryable [`ProviderError::ServiceUnavailable`] so the key-rotation layer
//! can fall back to another key or provider.

use async_trait::async_trait;
use tokio::io::AsyncRead;

use crate::openai::post_chat_completions;
use crate::provider::{
    ChatMessage, ChatResponse, CompletionRequest, LlmProvider, ProviderError, StreamEvent,
};
use crate::stream_parser::SseStreamParser;

/// Provider for any OpenAI-compatible chat-completions endpoint.
#[derive(Debug, Clone)]
pub struct OpenAiCompatProvider {
    name: String,
    base_url: String,
}

impl OpenAiCompatProvider {
    /// Create a provider for `base_url` with a generic id.
    pub fn new(base_url: impl Into<String>) -> Self {
        Self::with_name("openai-compatible", base_url)
    }

    /// Create a provider with an explicit id (e.g. `"deepseek"`).
    pub fn with_name(name: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
        }
    }

    /// The configured API base URL.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}

impl Default for OpenAiCompatProvider {
    fn default() -> Self {
        Self::new("http://localhost:8080/v1")
    }
}

#[async_trait]
impl LlmProvider for OpenAiCompatProvider {
    fn id(&self) -> &str {
        &self.name
    }

    fn context_window(&self, model_id: &str) -> usize {
        crate::openai::default_context_window(model_id)
    }

    fn count_tokens(&self, messages: &[ChatMessage]) -> usize {
        crate::openai::OpenAiProvider::new().count_tokens(messages)
    }

    async fn get_completion(
        &self,
        req: CompletionRequest,
    ) -> Result<Box<dyn AsyncRead + Unpin + Send>, ProviderError> {
        // Shared with `OpenAiProvider`; the classifier maps 5xx to
        // `ServiceUnavailable` so this is retryable (per the phase spec).
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

    #[test]
    fn trims_trailing_slashes_and_keeps_name() {
        let p = OpenAiCompatProvider::with_name("deepseek", "https://api.deepseek.com/v1/");
        assert_eq!(p.id(), "deepseek");
        assert_eq!(p.base_url(), "https://api.deepseek.com/v1");

        let generic = OpenAiCompatProvider::new("http://localhost:1234/v1");
        assert_eq!(generic.id(), "openai-compatible");
    }

    #[test]
    fn shares_openai_context_window_table() {
        let p = OpenAiCompatProvider::new("http://localhost:8080/v1");
        assert_eq!(p.context_window("gpt-4o-mini"), 128000);
    }
}
