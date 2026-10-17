//! MCP wiring through the agent (Phase 6): config → connect → tool calls.
//!
//! Uses the stdio fixture from [`shell_mcp::test_support`]; the model itself
//! is scripted, so no network is involved.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use shell_agent_core::{Agent, AgentConfig};
use shell_key_manager::KeyStore;
use shell_llm::{
    ChatMessage, ChatResponse, CompletionRequest, LlmProvider, ProviderError, StreamEvent, ToolCall,
};
use shell_mcp::McpManager;
use shell_mcp::test_support::fixture_config;
use shell_memory::MemoryStore;
use shell_tools::ToolContext;

#[test]
fn mcp_fixture() {
    shell_mcp::test_support::serve_if_requested();
}

/// Provider with scripted responses; records the requests it receives.
struct ScriptedProvider {
    responses: Mutex<VecDeque<ChatResponse>>,
    recorded_tools: Mutex<Vec<Vec<String>>>,
    recorded_messages: Mutex<Vec<Vec<ChatMessage>>>,
}

impl ScriptedProvider {
    fn new(responses: Vec<ChatResponse>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            recorded_tools: Mutex::new(Vec::new()),
            recorded_messages: Mutex::new(Vec::new()),
        }
    }

    fn record(&self, req: &CompletionRequest) {
        self.recorded_tools
            .lock()
            .unwrap()
            .push(req.tools.iter().map(|tool| tool.name.clone()).collect());
        self.recorded_messages
            .lock()
            .unwrap()
            .push(req.messages.clone());
    }

    fn next_response(&self) -> Result<ChatResponse, ProviderError> {
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| ProviderError::Other("script exhausted".to_string()))
    }

    fn tools_offered(&self, index: usize) -> Vec<String> {
        self.recorded_tools.lock().unwrap()[index].clone()
    }

    fn messages(&self, index: usize) -> Vec<ChatMessage> {
        self.recorded_messages.lock().unwrap()[index].clone()
    }
}

#[async_trait]
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

    async fn chat(&self, req: CompletionRequest) -> Result<ChatResponse, ProviderError> {
        self.record(&req);
        self.next_response()
    }

    async fn chat_streaming(
        &self,
        req: CompletionRequest,
        on_event: &mut (dyn FnMut(StreamEvent) + Send),
    ) -> Result<ChatResponse, ProviderError> {
        self.record(&req);
        let response = self.next_response()?;
        on_event(StreamEvent {
            content: response.content.clone(),
            ..Default::default()
        });
        on_event(StreamEvent {
            content: String::new(),
            tool_calls: response.tool_calls.clone(),
            done: true,
        });
        Ok(response)
    }
}

async fn memory_at(dir: &std::path::Path) -> MemoryStore {
    MemoryStore::init(&dir.join("sessions.db").to_string_lossy())
        .await
        .expect("session db")
}

#[tokio::test]
async fn agent_new_connects_mcp_servers_from_config() {
    let dir = tempfile::tempdir().unwrap();
    let memory = memory_at(dir.path()).await;
    let mut keys = KeyStore::new();
    keys.add_key("openai", "placeholder-key-never-used");

    let mut config = AgentConfig::new("openai", "gpt-4.1");
    config.workspace = dir.path().to_path_buf();
    config.mcp = vec![fixture_config("fixture", None, None)];

    let agent = Agent::new(config, memory, keys).await.unwrap();

    let statuses = agent.mcp_status();
    assert_eq!(statuses.len(), 1);
    assert!(statuses[0].ready, "{}", statuses[0].state);
    assert_eq!(statuses[0].tool_count, 3);
    assert!(agent.tools().contains("fixture-echo"));

    // The registered MCP tool executes through the real fixture process.
    let echo = agent.tools().get("fixture-echo").expect("fixture-echo");
    let result = echo
        .execute(
            serde_json::json!({"text": "via agent"}),
            &ToolContext::new(dir.path()),
        )
        .await;
    assert!(result.success, "{}", result.output);
    assert_eq!(result.output, "echo: via agent");
}

#[tokio::test]
async fn agent_runs_mcp_tool_through_the_loop() {
    let dir = tempfile::tempdir().unwrap();
    let memory = memory_at(dir.path()).await;
    let mut keys = KeyStore::new();
    keys.add_key("scripted", "scripted-placeholder-key");

    let provider = Arc::new(ScriptedProvider::new(vec![
        ChatResponse {
            content: "Calling the MCP tool.".to_string(),
            tool_calls: vec![ToolCall {
                id: "call_1".to_string(),
                tool_name: "fixture-echo".to_string(),
                args: serde_json::json!({"text": "hello-agent"}),
                thought_signature: None,
            }],
        },
        ChatResponse {
            content: "done: echo: hello-agent".to_string(),
            tool_calls: Vec::new(),
        },
    ]));

    let mut config = AgentConfig::new("scripted", "scripted-1");
    config.workspace = dir.path().to_path_buf();
    let mut agent = Agent::with_provider(config, memory, keys, provider.clone());

    // `with_provider` is synchronous, so connect MCP explicitly and attach it
    // (this is exactly what `Agent::new` does internally).
    let manager = Arc::new(McpManager::connect_all(&[fixture_config("fixture", None, None)]).await);
    assert!(manager.statuses()[0].ready);
    agent.attach_mcp(manager);

    let run = agent
        .run("please echo hello-agent".to_string())
        .await
        .unwrap();
    assert_eq!(run.content, "done: echo: hello-agent");
    assert_eq!(run.tool_calls, 1);

    // The model saw the MCP tool, and the tool result was fed back.
    assert!(
        provider
            .tools_offered(0)
            .contains(&"fixture-echo".to_string()),
        "{:?}",
        provider.tools_offered(0)
    );
    let follow_up = provider.messages(1);
    assert!(
        follow_up.iter().any(|message| message
            .content
            .as_deref()
            .unwrap_or("")
            .contains("echo: hello-agent")),
        "tool result missing from follow-up request"
    );
}
