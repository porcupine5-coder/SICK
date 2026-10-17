//! Coding subagents: the `task` tool spawns a child [`Agent`] with its own
//! session and context window.
//!
//! Design:
//!
//! - The parent [`Agent`] owns an [`SubagentHub`]; the [`TaskTool`] registered
//!   in its tool registry holds an `Arc` to the same hub.
//! - Before executing a tool batch the parent snapshots its config, provider,
//!   keys, and memory into the hub; the tool reads that snapshot at call time.
//! - Child agents are built with the base tool set only — no `task` tool — so
//!   recursion is structurally impossible.
//! - Child progress is forwarded into the parent's event stream as
//!   [`AgentEvent::SubagentStarted`], [`AgentEvent::SubagentToken`],
//!   [`AgentEvent::SubagentToolCall`], and [`AgentEvent::SubagentFinished`].

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{Value, json};
use shell_key_manager::KeyStore;
use shell_llm::LlmProvider;
use shell_memory::MemoryStore;
use shell_tools::{Tool, ToolContext, ToolResult};
use tokio::sync::mpsc;

use crate::agent::{Agent, AgentConfig, AgentEvent, AgentMode};

/// Everything a child agent needs, snapshotted from the parent.
#[derive(Clone)]
pub(crate) struct SubagentSpec {
    pub config: AgentConfig,
    pub provider: Arc<dyn LlmProvider>,
    pub key_manager: KeyStore,
    pub memory: MemoryStore,
}

/// Shared state between the parent agent and its `task` tool.
///
/// Both sides only ever run inside the agent actor task, so a `std::sync::Mutex`
/// held for a clone is sufficient (no lock is held across `await`).
pub(crate) struct SubagentHub {
    events: Mutex<Option<mpsc::UnboundedSender<AgentEvent>>>,
    spec: Mutex<Option<SubagentSpec>>,
}

impl SubagentHub {
    pub(crate) fn new() -> Self {
        Self {
            events: Mutex::new(None),
            spec: Mutex::new(None),
        }
    }

    /// Record the event stream of the current parent turn.
    pub(crate) fn set_events(&self, events: Option<mpsc::UnboundedSender<AgentEvent>>) {
        *self.events.lock().unwrap() = events;
    }

    /// Refresh the parent snapshot used to build child agents.
    pub(crate) fn set_spec(&self, spec: SubagentSpec) {
        *self.spec.lock().unwrap() = Some(spec);
    }

    fn events(&self) -> Option<mpsc::UnboundedSender<AgentEvent>> {
        self.events.lock().unwrap().clone()
    }

    fn spec(&self) -> Option<SubagentSpec> {
        self.spec.lock().unwrap().clone()
    }
}

/// The `task` tool handed to the model.
pub(crate) struct TaskTool {
    hub: Arc<SubagentHub>,
}

impl TaskTool {
    pub(crate) fn new(hub: Arc<SubagentHub>) -> Self {
        Self { hub }
    }
}

#[async_trait]
impl Tool for TaskTool {
    fn name(&self) -> &str {
        "task"
    }

    fn description(&self) -> &str {
        "Spawn a subagent with its own context window to work on an independent \
         subtask (e.g. a focused search or refactor). The subagent has the same \
         filesystem tools and workspace but cannot spawn further subagents. \
         Returns the subagent's final answer."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "description": {
                    "type": "string",
                    "description": "Short name for the subagent run (shown in the UI)."
                },
                "prompt": {
                    "type": "string",
                    "description": "Complete, standalone instructions for the subagent."
                }
            },
            "required": ["description", "prompt"]
        })
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        let description = args
            .get("description")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("subtask");
        let name: String = description.chars().take(40).collect();
        let Some(prompt) = args
            .get("prompt")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return ToolResult::err("missing required string argument `prompt`");
        };

        let Some(spec) = self.hub.spec() else {
            return ToolResult::err("subagents are unavailable in this context");
        };
        let sink = self.hub.events();
        if let Some(tx) = &sink {
            let _ = tx.send(AgentEvent::SubagentStarted { name: name.clone() });
        }

        // Children run with the default system prompt and standard mode, and
        // without the `task` tool (no recursive spawning).
        let mut config = spec.config.clone();
        config.system_prompt = String::new();
        let mut child = Agent::build(
            config,
            spec.memory.clone(),
            spec.key_manager.clone(),
            spec.provider.clone(),
            false,
        );
        child.set_mode(AgentMode::Normal);

        let outcome = match sink.clone() {
            Some(tx) => {
                let (child_tx, mut child_rx) = mpsc::unbounded_channel::<AgentEvent>();
                let forward_name = name.clone();
                let forward = tokio::spawn(async move {
                    while let Some(event) = child_rx.recv().await {
                        match event {
                            AgentEvent::Token(text) => {
                                let _ = tx.send(AgentEvent::SubagentToken {
                                    name: forward_name.clone(),
                                    text,
                                });
                            }
                            AgentEvent::ToolCall { name: tool, args } => {
                                let _ = tx.send(AgentEvent::SubagentToolCall {
                                    name: forward_name.clone(),
                                    tool,
                                    args,
                                });
                            }
                            _ => {}
                        }
                    }
                });
                let outcome = child
                    .run_turn(Some(prompt.to_string()), Some(child_tx))
                    .await;
                // Drop the child before awaiting the forwarder: the child's
                // hub holds a sender clone, and the loop only ends when every
                // sender is gone.
                drop(child);
                let _ = forward.await;
                outcome
            }
            None => child.run_turn(Some(prompt.to_string()), None).await,
        };

        match outcome {
            Ok(turn) => {
                if let Some(tx) = &sink {
                    let _ = tx.send(AgentEvent::SubagentFinished {
                        name: name.clone(),
                        success: true,
                    });
                }
                let summary = if turn.content.trim().is_empty() {
                    "(no summary)"
                } else {
                    turn.content.trim()
                };
                ToolResult::ok(format!(
                    "Subagent `{name}` finished ({} iteration(s), {} tool call(s)).\n\n\
                     Final answer:\n{summary}",
                    turn.iterations, turn.tool_calls
                ))
            }
            Err(error) => {
                if let Some(tx) = &sink {
                    let _ = tx.send(AgentEvent::SubagentFinished {
                        name: name.clone(),
                        success: false,
                    });
                }
                ToolResult::err(format!("subagent `{name}` failed: {error}"))
            }
        }
    }
}
