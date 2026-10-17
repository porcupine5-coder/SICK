//! Web UI for SICK (Phase 6): a localhost chat page over WebSocket.
//!
//! Serves one agent per server process (the same model as the TUI: a single
//! conversation). The page at `/` lets the user type prompts; `/ws` speaks a
//! small JSON protocol:
//!
//! Client → server:
//!
//! ```json
//! { "prompt": "list the files here" }
//! ```
//!
//! Server → client (streamed, in order):
//!
//! - `{ "type": "accepted" }` — prompt queued (also for prompts queued behind a running turn),
//! - `{ "type": "token", "text": "..." }` — assistant text chunk,
//! - `{ "type": "tool_call", "name": "...", "args": { ... } }`,
//! - `{ "type": "tool_result", "name": "...", "success": true, "output": "...", "duration_ms": 3 }`,
//! - `{ "type": "subagent_started" | "subagent_token" | "subagent_tool_call" | "subagent_finished", ... }`,
//! - `{ "type": "done", "content": "...", "iterations": 2, "tool_calls": 1 }`, or
//! - `{ "type": "error", "message": "..." }`.
//!
//! Prompts from every connected client share the single agent and run one
//! turn at a time; all clients see the same event stream. The server binds
//! `127.0.0.1` only (start it with `sick web`).

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Html;
use axum::response::Response;
use axum::routing::get;
use serde::Deserialize;
use serde_json::json;
use shell_agent_core::{Agent, AgentEvent};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc, oneshot};

/// The chat page served at `/`.
const INDEX_HTML: &str = include_str!("index.html");

/// Event broadcast buffer; slow clients may lag and skip events.
const EVENT_BUFFER: usize = 1024;

/// Shared server state: prompt queue into the agent actor plus the event fan-out.
#[derive(Clone)]
struct WebState {
    prompts: mpsc::UnboundedSender<String>,
    events: broadcast::Sender<Arc<str>>,
}

/// Client → server message.
#[derive(Deserialize)]
struct ClientMessage {
    prompt: String,
}

/// Serve the web UI on `listener` until the process exits or the server
/// errors. The agent is moved into a dedicated actor task; every connection
/// talks to that one agent.
pub async fn serve(listener: TcpListener, agent: Agent) -> std::io::Result<()> {
    let (prompts_tx, prompts_rx) = mpsc::unbounded_channel::<String>();
    let (events_tx, _events_rx) = broadcast::channel::<Arc<str>>(EVENT_BUFFER);

    tokio::spawn(agent_actor(agent, prompts_rx, events_tx.clone()));

    let state = WebState {
        prompts: prompts_tx,
        events: events_tx,
    };
    let app = Router::new()
        .route("/", get(index))
        .route("/ws", get(ws_handler))
        .with_state(state);

    axum::serve(listener, app).await
}

/// `GET /`: the embedded chat page.
async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

/// `GET /ws`: upgrade to the chat WebSocket.
async fn ws_handler(ws: WebSocketUpgrade, State(state): State<WebState>) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

/// One client connection: forwards prompts to the actor and streams events.
async fn handle_socket(mut socket: WebSocket, state: WebState) {
    let mut events = state.events.subscribe();
    loop {
        tokio::select! {
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<ClientMessage>(text.as_str()) {
                            Ok(message) if !message.prompt.trim().is_empty() => {
                                if state.prompts.send(message.prompt).is_err() {
                                    break; // agent actor gone
                                }
                                let _ = socket
                                    .send(Message::Text(r#"{"type":"accepted"}"#.into()))
                                    .await;
                            }
                            Ok(_) => {
                                let reply = json!({"type": "error", "message": "empty prompt"});
                                let _ = socket.send(Message::Text(reply.to_string().into())).await;
                            }
                            Err(error) => {
                                let reply = json!({
                                    "type": "error",
                                    "message": format!("invalid message (expected {{\"prompt\": \"...\"}}): {error}"),
                                });
                                let _ = socket.send(Message::Text(reply.to_string().into())).await;
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {} // ping/pong/binary: ignore
                    Some(Err(_)) => break,
                }
            }
            event = events.recv() => {
                match event {
                    Ok(text) => {
                        if socket.send(Message::Text(text.to_string().into())).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::debug!(skipped, "web client lagged; events skipped");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
}

/// Owns the agent; runs one turn per queued prompt and broadcasts progress.
async fn agent_actor(
    mut agent: Agent,
    mut prompts: mpsc::UnboundedReceiver<String>,
    events: broadcast::Sender<Arc<str>>,
) {
    while let Some(prompt) = prompts.recv().await {
        let (event_tx, mut event_rx) = mpsc::unbounded_channel::<AgentEvent>();
        let (cancel_tx, mut cancel_rx) = oneshot::channel::<()>();

        // Forward this turn's events to every connected client. The agent's
        // subagent hub keeps a clone of the sender alive between turns, so the
        // forwarder is stopped with an explicit cancel (after draining).
        let broadcast = events.clone();
        let forward = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut cancel_rx => {
                        while let Ok(event) = event_rx.try_recv() {
                            let _ = broadcast.send(event_to_json(&event).into());
                        }
                        break;
                    }
                    event = event_rx.recv() => match event {
                        Some(event) => {
                            let _ = broadcast.send(event_to_json(&event).into());
                        }
                        None => break,
                    },
                }
            }
        });

        let result = agent.run_turn(Some(prompt), Some(event_tx)).await;
        let _ = cancel_tx.send(());
        let _ = forward.await;

        let done = match result {
            Ok(turn) => json!({
                "type": "done",
                "content": turn.content,
                "iterations": turn.iterations,
                "tool_calls": turn.tool_calls,
            }),
            Err(error) => json!({"type": "error", "message": error.to_string()}),
        };
        let _ = events.send(done.to_string().into());
    }
}

/// Serialize one [`AgentEvent`] as a JSON protocol message.
fn event_to_json(event: &AgentEvent) -> String {
    let value = match event {
        AgentEvent::Token(text) => json!({"type": "token", "text": text}),
        AgentEvent::ToolCall { name, args } => {
            json!({"type": "tool_call", "name": name, "args": args})
        }
        AgentEvent::ToolResult {
            name,
            success,
            output,
            duration_ms,
        } => json!({
            "type": "tool_result",
            "name": name,
            "success": success,
            "output": output,
            "duration_ms": duration_ms,
        }),
        AgentEvent::SubagentStarted { name } => {
            json!({"type": "subagent_started", "name": name})
        }
        AgentEvent::SubagentToken { name, text } => {
            json!({"type": "subagent_token", "name": name, "text": text})
        }
        AgentEvent::SubagentToolCall { name, tool, args } => {
            json!({"type": "subagent_tool_call", "name": name, "tool": tool, "args": args})
        }
        AgentEvent::SubagentFinished { name, success } => {
            json!({"type": "subagent_finished", "name": name, "success": success})
        }
    };
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_serialize_to_the_documented_protocol() {
        let token = event_to_json(&AgentEvent::Token("hi".to_string()));
        assert_eq!(token, r#"{"text":"hi","type":"token"}"#);

        let result = event_to_json(&AgentEvent::ToolResult {
            name: "bash".to_string(),
            success: false,
            output: "boom".to_string(),
            duration_ms: 12,
        });
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["type"], "tool_result");
        assert_eq!(parsed["name"], "bash");
        assert_eq!(parsed["success"], false);
        assert_eq!(parsed["duration_ms"], 12);

        let finished = event_to_json(&AgentEvent::SubagentFinished {
            name: "child".to_string(),
            success: true,
        });
        let parsed: serde_json::Value = serde_json::from_str(&finished).unwrap();
        assert_eq!(parsed["type"], "subagent_finished");
    }

    #[test]
    fn index_page_is_embedded() {
        assert!(INDEX_HTML.contains("<html"));
        assert!(INDEX_HTML.contains("WebSocket"));
    }
}
