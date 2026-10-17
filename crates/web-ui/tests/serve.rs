//! End-to-end web UI tests: a real axum server streaming a scripted agent.
//!
//! Covers: the embedded page at `/`, the WebSocket protocol (accepted →
//! tokens → done), tool call/result events, invalid-message handling, and
//! sequential turns. No network beyond localhost.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use shell_agent_core::{Agent, AgentConfig};
use shell_key_manager::KeyStore;
use shell_llm::{
    ChatMessage, ChatResponse, CompletionRequest, LlmProvider, ProviderError, StreamEvent, ToolCall,
};
use shell_memory::MemoryStore;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message as WsMessage;

/// Provider with scripted responses; streams content in two halves.
struct ScriptedProvider {
    responses: Mutex<VecDeque<ChatResponse>>,
}

impl ScriptedProvider {
    fn new(responses: Vec<ChatResponse>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
        }
    }

    fn next_response(&self) -> Result<ChatResponse, ProviderError> {
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| ProviderError::Other("script exhausted".to_string()))
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

    async fn chat(&self, _req: CompletionRequest) -> Result<ChatResponse, ProviderError> {
        self.next_response()
    }

    async fn chat_streaming(
        &self,
        _req: CompletionRequest,
        on_event: &mut (dyn FnMut(StreamEvent) + Send),
    ) -> Result<ChatResponse, ProviderError> {
        let response = self.next_response()?;
        if !response.content.is_empty() {
            let mid = response.content.len() / 2;
            on_event(StreamEvent {
                content: response.content[..mid].to_string(),
                ..Default::default()
            });
            on_event(StreamEvent {
                content: response.content[mid..].to_string(),
                ..Default::default()
            });
        }
        if !response.tool_calls.is_empty() {
            on_event(StreamEvent {
                tool_calls: response.tool_calls.clone(),
                done: true,
                ..Default::default()
            });
        }
        Ok(response)
    }
}

fn text_response(content: &str) -> ChatResponse {
    ChatResponse {
        content: content.to_string(),
        tool_calls: Vec::new(),
    }
}

async fn scripted_agent(dir: &Path, responses: Vec<ChatResponse>) -> Agent {
    let memory = MemoryStore::init(&dir.join("sessions.db").to_string_lossy())
        .await
        .unwrap();
    let mut keys = KeyStore::new();
    keys.add_key("scripted", "scripted-placeholder-key");
    let mut config = AgentConfig::new("scripted", "scripted-1");
    config.workspace = dir.to_path_buf();
    Agent::with_provider(
        config,
        memory,
        keys,
        Arc::new(ScriptedProvider::new(responses)),
    )
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Send one prompt and collect protocol messages through the terminal `done`
/// (or `error`) message.
async fn turn(ws: &mut Ws, prompt: &str) -> Vec<serde_json::Value> {
    let payload = serde_json::json!({"prompt": prompt}).to_string();
    ws.send(WsMessage::Text(payload)).await.unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        let mut events: Vec<serde_json::Value> = Vec::new();
        while let Some(frame) = ws.next().await {
            let WsMessage::Text(text) = frame.unwrap() else {
                continue;
            };
            let value: serde_json::Value = serde_json::from_str(text.as_str()).unwrap();
            let terminal = matches!(value["type"].as_str(), Some("done") | Some("error"));
            events.push(value);
            if terminal {
                break;
            }
        }
        events
    })
    .await
    .expect("turn did not finish in time")
}

fn token_text(events: &[serde_json::Value]) -> String {
    events
        .iter()
        .filter(|event| event["type"] == "token")
        .filter_map(|event| event["text"].as_str())
        .collect()
}

/// Start the server on a random localhost port; returns its address.
async fn start(agent: Agent) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        shell_web_ui::serve(listener, agent).await.unwrap();
    });
    addr
}

#[tokio::test]
async fn serves_the_page_and_streams_turns_over_websocket() {
    let dir = tempfile::tempdir().unwrap();
    let agent = scripted_agent(
        dir.path(),
        vec![
            text_response("first streamed answer"),
            text_response("second answer"),
        ],
    )
    .await;
    let addr = start(agent).await;

    // `GET /` returns the embedded chat page.
    {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.contains("<html"), "page body missing");
        assert!(response.contains("WebSocket"), "page script missing");
    }

    // Two prompts over one WebSocket, streamed as tokens then the final text.
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
        .await
        .unwrap();

    let first = turn(&mut ws, "say hi").await;
    assert_eq!(first[0]["type"], "accepted");
    assert_eq!(token_text(&first), "first streamed answer");
    let done = first.last().unwrap();
    assert_eq!(done["type"], "done");
    assert_eq!(done["content"], "first streamed answer");
    assert_eq!(done["tool_calls"], 0);

    let second = turn(&mut ws, "again").await;
    assert_eq!(token_text(&second), "second answer");
    assert_eq!(second.last().unwrap()["content"], "second answer");
}

#[tokio::test]
async fn tool_events_stream_and_tool_runs_in_the_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let agent = scripted_agent(
        dir.path(),
        vec![
            ChatResponse {
                content: "writing the file".to_string(),
                tool_calls: vec![ToolCall {
                    id: "call_web_1".to_string(),
                    tool_name: "write".to_string(),
                    args: serde_json::json!({"path": "web.txt", "content": "from web"}),
                    thought_signature: None,
                }],
            },
            text_response("wrote the file"),
        ],
    )
    .await;
    let addr = start(agent).await;
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
        .await
        .unwrap();

    let events = turn(&mut ws, "write web.txt").await;
    let tool_call = events
        .iter()
        .find(|event| event["type"] == "tool_call")
        .expect("tool_call event");
    assert_eq!(tool_call["name"], "write");
    let tool_result = events
        .iter()
        .find(|event| event["type"] == "tool_result")
        .expect("tool_result event");
    assert_eq!(tool_result["name"], "write");
    assert_eq!(tool_result["success"], true);
    assert_eq!(events.last().unwrap()["content"], "wrote the file");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("web.txt")).unwrap(),
        "from web"
    );
}

#[tokio::test]
async fn invalid_messages_get_an_error_reply_without_closing() {
    let dir = tempfile::tempdir().unwrap();
    let agent = scripted_agent(dir.path(), vec![text_response("fine")]).await;
    let addr = start(agent).await;
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
        .await
        .unwrap();

    ws.send(WsMessage::Text("not json".into())).await.unwrap();
    let frame = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("error reply in time")
        .unwrap()
        .unwrap();
    let WsMessage::Text(text) = frame else {
        panic!("expected a text frame");
    };
    let value: serde_json::Value = serde_json::from_str(text.as_str()).unwrap();
    assert_eq!(value["type"], "error");

    // The connection still works.
    let events = turn(&mut ws, "still alive").await;
    assert_eq!(events.last().unwrap()["content"], "fine");
}
