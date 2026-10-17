//! Example S.I.C.K extension (Phase 6) — also the test fixture for the loader.
//!
//! Contributes three tools:
//!
//! - `example_echo` — echoes its `text` argument,
//! - `example_message_count` — how many conversation messages the extension
//!   has seen via `on_message`,
//! - `example_context` — the workspace directory handed to `init`.
//!
//! When the environment variable `EXAMPLE_EXTENSION_PROVIDER=1` is set at
//! load time it also provides a canned LLM provider: the first request asks
//! for `example_message_count`, the next answers `"extension provider
//! answer"`. That lets the CLI run a full tool-calling turn without any API
//! keys.
//!
//! Build it with `cargo build -p shell-extension-example` and copy the
//! artifact (`example_extension.dll` / `libexample_extension.so` /
//! `libexample_extension.dylib`) into the extensions directory configured via
//! `[extensions] dir`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{Value, json};
use shell_extensions::{Extension, ExtensionContext, ExtensionResult, export_extension};
use shell_llm::{
    ChatMessage, ChatResponse, CompletionRequest, LlmProvider, ProviderError, ToolCall,
};
use shell_tools::{Tool, ToolContext, ToolResult};

/// The extension itself.
pub struct ExampleExtension {
    messages_seen: Arc<AtomicUsize>,
    workspace: Arc<Mutex<String>>,
    with_provider: bool,
}

impl ExampleExtension {
    /// Create the extension; the provider is only offered when
    /// `EXAMPLE_EXTENSION_PROVIDER=1` is set.
    pub fn new() -> Self {
        Self {
            messages_seen: Arc::new(AtomicUsize::new(0)),
            workspace: Arc::new(Mutex::new(String::new())),
            with_provider: std::env::var("EXAMPLE_EXTENSION_PROVIDER").as_deref() == Ok("1"),
        }
    }
}

impl Default for ExampleExtension {
    fn default() -> Self {
        Self::new()
    }
}

impl Extension for ExampleExtension {
    fn name(&self) -> &str {
        "example"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    fn init(&mut self, ctx: &ExtensionContext) -> ExtensionResult<()> {
        *self.workspace.lock().unwrap() = ctx.workspace.display().to_string();
        Ok(())
    }

    fn on_message(&self, _msg: &ChatMessage) -> ExtensionResult<()> {
        self.messages_seen.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn provide_tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![
            Arc::new(EchoTool),
            Arc::new(MessageCountTool {
                messages_seen: Arc::clone(&self.messages_seen),
            }),
            Arc::new(ContextTool {
                workspace: Arc::clone(&self.workspace),
            }),
        ]
    }

    fn provide_provider(&self) -> Option<Arc<dyn LlmProvider>> {
        self.with_provider
            .then(|| Arc::new(CannedProvider) as Arc<dyn LlmProvider>)
    }
}

/// Echoes `text` back with an `example echo:` prefix.
struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        "example_echo"
    }

    fn description(&self) -> &str {
        "Echoes text back through the example extension"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {"text": {"type": "string"}},
            "required": ["text"]
        })
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        let text = args.get("text").and_then(Value::as_str).unwrap_or_default();
        ToolResult::ok(format!("example echo: {text}"))
    }
}

/// Reports how many messages `on_message` has delivered.
struct MessageCountTool {
    messages_seen: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for MessageCountTool {
    fn name(&self) -> &str {
        "example_message_count"
    }

    fn description(&self) -> &str {
        "How many conversation messages the example extension has seen"
    }

    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }

    async fn execute(&self, _args: Value, _ctx: &ToolContext) -> ToolResult {
        ToolResult::ok(self.messages_seen.load(Ordering::SeqCst).to_string())
    }
}

/// Reports the workspace directory the extension was initialized with.
struct ContextTool {
    workspace: Arc<Mutex<String>>,
}

#[async_trait]
impl Tool for ContextTool {
    fn name(&self) -> &str {
        "example_context"
    }

    fn description(&self) -> &str {
        "The workspace directory this extension was initialized with"
    }

    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }

    async fn execute(&self, _args: Value, _ctx: &ToolContext) -> ToolResult {
        ToolResult::ok(format!(
            "example workspace: {}",
            self.workspace.lock().unwrap()
        ))
    }
}

/// Canned provider: asks for the extension's message-count tool on the first
/// request, then answers with a fixed sentence. Shows that extension tools
/// and providers work together without any API key.
struct CannedProvider;

#[async_trait]
impl LlmProvider for CannedProvider {
    fn id(&self) -> &str {
        "example-extension"
    }

    fn count_tokens(&self, _messages: &[ChatMessage]) -> usize {
        0
    }

    fn context_window(&self, _model_id: &str) -> usize {
        4096
    }

    async fn get_completion(
        &self,
        _req: CompletionRequest,
    ) -> Result<Box<dyn tokio::io::AsyncRead + Unpin + Send>, ProviderError> {
        Err(ProviderError::Other(
            "canned provider has no byte stream".to_string(),
        ))
    }

    async fn chat(&self, req: CompletionRequest) -> Result<ChatResponse, ProviderError> {
        let saw_tool_result = req.messages.iter().any(|message| message.role == "tool");
        let has_counter_tool = req
            .tools
            .iter()
            .any(|tool| tool.name == "example_message_count");
        if !saw_tool_result && has_counter_tool {
            return Ok(ChatResponse {
                content: "Checking how many messages I have seen.".to_string(),
                tool_calls: vec![ToolCall {
                    id: "ext_call_1".to_string(),
                    tool_name: "example_message_count".to_string(),
                    args: json!({}),
                    thought_signature: None,
                }],
            });
        }
        Ok(ChatResponse {
            content: "extension provider answer".to_string(),
            tool_calls: Vec::new(),
        })
    }
}

export_extension!(ExampleExtension::new());
