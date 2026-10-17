//! OpenRouter provider (OpenAI-compatible endpoint).
//!
//! OpenRouter speaks the OpenAI chat-completions protocol, so this is a thin
//! wrapper around [`OpenAiCompatProvider`](crate::openai_compat::OpenAiCompatProvider).

pub use crate::openai_compat::OpenAiCompatProvider as OpenRouterProvider;

/// Default OpenRouter API base URL.
pub const DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1";

/// Create an OpenRouter provider.
pub fn new() -> OpenRouterProvider {
    crate::openai_compat::OpenAiCompatProvider::with_name("openrouter", DEFAULT_BASE_URL)
}
