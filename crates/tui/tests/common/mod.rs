//! Shared helpers for TUI integration tests.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use shell_agent_core::{Agent, AgentConfig};
use shell_key_manager::KeyStore;
use shell_llm::{
    ChatMessage, ChatResponse, CompletionRequest, LlmProvider, ProviderError, StreamEvent,
};
use shell_memory::MemoryStore;
use shell_tui::App;

/// Provider with scripted responses; content streams in word-sized chunks.
pub struct ScriptedProvider {
    responses: Mutex<Vec<ChatResponse>>,
}

impl ScriptedProvider {
    /// Create a provider that answers with the given responses, in order.
    pub fn new(responses: Vec<ChatResponse>) -> Self {
        Self {
            responses: Mutex::new(responses),
        }
    }

    fn next(&self) -> Result<ChatResponse, ProviderError> {
        let mut guard = self.responses.lock().unwrap();
        if guard.is_empty() {
            Err(ProviderError::Other("script exhausted".to_string()))
        } else {
            Ok(guard.remove(0))
        }
    }
}

#[async_trait::async_trait]
impl LlmProvider for ScriptedProvider {
    fn id(&self) -> &str {
        "scripted"
    }

    fn count_tokens(&self, _messages: &[ChatMessage]) -> usize {
        0
    }

    fn context_window(&self, _model_id: &str) -> usize {
        8192
    }

    async fn get_completion(
        &self,
        _req: CompletionRequest,
    ) -> Result<Box<dyn tokio::io::AsyncRead + Unpin + Send>, ProviderError> {
        Err(ProviderError::Other(
            "scripted provider has no byte stream".to_string(),
        ))
    }

    async fn chat(&self, _req: CompletionRequest) -> Result<ChatResponse, ProviderError> {
        self.next()
    }

    async fn chat_streaming(
        &self,
        _req: CompletionRequest,
        on_event: &mut (dyn FnMut(StreamEvent) + Send),
    ) -> Result<ChatResponse, ProviderError> {
        let response = self.next()?;
        for chunk in response.content.split_inclusive(' ').map(str::to_string) {
            on_event(StreamEvent {
                content: chunk,
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

/// Point `SICK_HOME` at a per-process temp dir once, so key writes in tests
/// never touch the real user home, and back the keyring with a file inside
/// that temp dir (`SICK_KEYRING_FILE`) so tests never touch the real OS
/// keychain (Windows Credential Manager / Secret Service).
pub fn isolate_sick_home() {
    static INIT: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    INIT.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("sick-tui-e2e-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp SICK_HOME");
        // SAFETY: `set_var` runs exactly once (OnceLock), before any test in
        // this binary reads `SICK_HOME`; every helper that can trigger a key
        // file write funnels through this function first.
        unsafe {
            std::env::set_var("SICK_HOME", &dir);
            std::env::set_var("SICK_KEYRING_FILE", dir.join("keyring"));
        }
    });
}

/// Build an app whose agent answers with the given scripted responses and keys.
pub async fn scripted_app_with_keys(
    workspace: &std::path::Path,
    responses: Vec<ChatResponse>,
    keys: &[(&str, &str)],
) -> App {
    isolate_sick_home();
    let memory = MemoryStore::init(&workspace.join("sessions.db").to_string_lossy())
        .await
        .unwrap();
    let mut config = AgentConfig::new("scripted", "scripted-model");
    config.workspace = workspace.to_path_buf();
    let mut store = KeyStore::new();
    for (provider, secret) in keys {
        store.add_key(provider, secret);
    }
    let agent = Agent::with_provider(
        config,
        memory,
        store,
        Arc::new(ScriptedProvider::new(responses)),
    );
    App::new(agent)
}

/// Build an app whose agent answers with the given scripted responses.
pub async fn scripted_app(workspace: &std::path::Path, responses: Vec<ChatResponse>) -> App {
    scripted_app_with_keys(workspace, responses, &[("scripted", "test-key-123")]).await
}

/// A plain key press.
pub fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// Type `text` one character at a time through `App::handle_key`.
pub fn type_text(app: &mut App, text: &str) {
    for character in text.chars() {
        app.handle_key(key(KeyCode::Char(character)));
    }
}

/// Pump the app until `predicate` holds (or a 10s timeout expires).
pub async fn pump_until(app: &mut App, predicate: impl Fn(&App) -> bool) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        app.poll_events();
        if predicate(app) {
            return true;
        }
        if tokio::time::Instant::now() > deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Render the app on a `TestBackend` and flatten the cells into a string.
pub fn render_to_string(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| app.render(frame)).unwrap();
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}
