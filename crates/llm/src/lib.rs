//! LLM provider abstractions (OpenAI, Anthropic, Gemini, DeepSeek, OpenRouter).
//!
//! Concrete providers: [`OpenAiProvider`] (native OpenAI), [`AnthropicProvider`]
//! (native Messages API), [`GoogleProvider`] (native Gemini API), and
//! [`OpenAiCompatProvider`] for any OpenAI-compatible endpoint (DeepSeek,
//! OpenRouter, Qwen DashScope, local llama.cpp, ...).

pub mod anthropic;
pub mod catalog;
pub mod google;
pub mod models_api;
pub mod openai;
pub mod openai_compat;
pub mod openrouter;
pub mod provider;
pub mod stream_parser;

pub use anthropic::AnthropicProvider;
pub use catalog::known_models;
pub use google::GoogleProvider;
pub use models_api::{cloudflare_base_url, fetch_models};
pub use openai::{DEFAULT_BASE_URL, OpenAiProvider};
pub use openai_compat::OpenAiCompatProvider;
pub use provider::{
    ChatMessage, ChatResponse, CompletionRequest, LlmProvider, ProviderError, StreamEvent,
    ToolCall, ToolDefinition,
};
pub use stream_parser::SseStreamParser;
