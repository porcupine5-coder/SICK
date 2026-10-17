//! The agent loop: prompt → LLM → tool execution → repeat.
//!
//! Phase 2 semantics: multiple providers with transparent key rotation and a
//! cross-provider fallback chain. `send_with_rotation` walks the primary
//! provider and its fallbacks, selecting keys from the [`KeyStore`] and
//! updating key health from the observed provider errors.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use shell_common::{ShellError, ShellResult};
use shell_config::{McpServerConfig, SandboxConfig};
use shell_extensions::ExtensionHost;
use shell_key_manager::{KeyStatus, KeyStatusInfo, KeyStore, normalize_provider};
use shell_llm::{
    AnthropicProvider, ChatMessage, ChatResponse, CompletionRequest, GoogleProvider, LlmProvider,
    OpenAiCompatProvider, OpenAiProvider, ProviderError, StreamEvent, ToolCall, ToolDefinition,
    cloudflare_base_url,
};
use shell_mcp::{McpManager, McpServerStatus};
use shell_memory::{MemoryStore, Message as StoredMessage, ToolCallRecord};
use shell_tools::{ToolContext, ToolRegistry, ToolResult};
use tokio::sync::mpsc;

use crate::skills::{self, Skill, SkillTool};
use crate::subagent::{SubagentHub, SubagentSpec, TaskTool};

/// Safety valve: maximum number of LLM round-trips per run.
const MAX_ITERATIONS: usize = 30;

/// Safety valve: maximum number of send attempts per `send_with_rotation` call.
const MAX_ROTATION_ATTEMPTS: usize = 10;

/// Tools the model may still call in plan mode (read-only exploration).
pub const PLAN_MODE_TOOLS: &[&str] = &["read", "grep", "glob", "ls"];

/// Minimum cosine score for a memory to be injected into the prompt.
const RECALL_MIN_SCORE: f32 = 0.30;

/// Maximum memories injected into a single request.
const RECALL_MAX_ITEMS: usize = 3;

/// Extra system instructions inserted into every request while in plan mode.
const PLAN_MODE_INSTRUCTIONS: &str = "You are currently in PLAN MODE. Explore the codebase \
     with the read-only tools (read, grep, glob, ls) — write, edit, and shell execution are \
     disabled. Do not attempt to modify anything. When you have enough context, reply with a \
     concrete, step-by-step plan: name every file you would change and what you would do, then \
     stop. The user will approve, edit, or cancel the plan.";

/// Operating mode of the agent loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentMode {
    /// Full tool access.
    #[default]
    Normal,
    /// Read-only exploration; the model drafts a plan instead of acting.
    ///
    /// The tool registry filters to [`PLAN_MODE_TOOLS`] and the system prompt
    /// gains plan instructions. The approved plan then executes in
    /// [`AgentMode::Normal`] (no separate executing state is needed: the
    /// normal loop already re-plans when a step fails).
    Plan,
}

/// Configuration for a single agent instance.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// Provider identifier, e.g. `"openai"`.
    pub provider: String,
    /// Model identifier, e.g. `"gpt-4.1"`.
    pub model: String,
    /// Providers to try when the primary provider's keys are all unusable:
    /// `[(provider, model), ...]`.
    pub fallback_chain: Vec<(String, String)>,
    /// Sampling temperature.
    pub temperature: f32,
    /// Maximum output tokens per completion (0 = provider default).
    pub max_tokens: usize,
    /// Optional system prompt override (empty = built-in default).
    pub system_prompt: String,
    /// Working directory for tools.
    pub workspace: PathBuf,
    /// Optional base URL override for the primary provider (OpenAI-compatible).
    pub base_url: String,
    /// `[sandbox]` settings used for the `bash` tool (Phase 6).
    pub sandbox: SandboxConfig,
    /// `[[mcp.servers]]` entries connected at agent construction (Phase 6).
    pub mcp: Vec<McpServerConfig>,
}

impl AgentConfig {
    /// Minimal configuration for a given model.
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
            fallback_chain: Vec::new(),
            temperature: 0.2,
            max_tokens: 0,
            system_prompt: String::new(),
            workspace: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            base_url: String::new(),
            sandbox: SandboxConfig::default(),
            mcp: Vec::new(),
        }
    }
}

/// Summary returned by a completed agent run.
#[derive(Debug, Clone)]
pub struct AgentRun {
    /// Session id the run was persisted under.
    pub session_id: String,
    /// Final assistant text.
    pub content: String,
    /// Number of LLM round-trips performed.
    pub iterations: usize,
    /// Total number of tool calls executed.
    pub tool_calls: usize,
}

/// Result of a single interactive turn (Phase 3 TUI, `run_turn`).
#[derive(Debug, Clone, Default)]
pub struct AgentTurn {
    /// Final assistant text of the turn.
    pub content: String,
    /// Number of LLM round-trips performed during the turn.
    pub iterations: usize,
    /// Total number of tool calls executed during the turn.
    pub tool_calls: usize,
}

/// Streaming progress events emitted during [`Agent::run_turn`].
///
/// UIs (the TUI) subscribe to these to render responses while the agent
/// works; the complete response is still persisted in the session store.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// A chunk of assistant text arrived.
    Token(String),
    /// The model requested a tool call.
    ToolCall {
        /// Tool name as requested by the model.
        name: String,
        /// Parsed tool arguments.
        args: serde_json::Value,
    },
    /// A tool finished executing.
    ToolResult {
        /// Tool name that was executed.
        name: String,
        /// Whether the tool reported success.
        success: bool,
        /// Full tool output (UIs may truncate for display).
        output: String,
        /// Wall-clock execution time in milliseconds.
        duration_ms: u64,
    },
    /// A `task` subagent started working.
    SubagentStarted {
        /// Subagent name (from the tool's `description` argument).
        name: String,
    },
    /// A chunk of subagent text arrived.
    SubagentToken {
        /// Subagent name.
        name: String,
        /// Streamed text chunk.
        text: String,
    },
    /// The subagent requested a tool call.
    SubagentToolCall {
        /// Subagent name.
        name: String,
        /// Tool name the subagent requested.
        tool: String,
        /// Parsed tool arguments.
        args: serde_json::Value,
    },
    /// The subagent finished (success = produced a final answer).
    SubagentFinished {
        /// Subagent name.
        name: String,
        /// Whether the child turn completed without an error.
        success: bool,
    },
}

/// The agent: owns the provider, key store, memory, tools, and session state.
pub struct Agent {
    provider: Arc<dyn LlmProvider>,
    key_manager: KeyStore,
    memory: MemoryStore,
    tools: ToolRegistry,
    config: AgentConfig,
    session_id: String,
    messages: Vec<ChatMessage>,
    turn_idx: i64,
    mode: AgentMode,
    active_skills: Vec<Skill>,
    hub: std::sync::Arc<SubagentHub>,
    mcp: Option<Arc<McpManager>>,
    extensions: Option<Arc<ExtensionHost>>,
}

impl Agent {
    /// Create an agent from configuration plus a memory store and key store.
    ///
    /// Fails early when the configured primary provider is unknown or points
    /// at a missing base URL.
    pub async fn new(
        config: AgentConfig,
        memory: MemoryStore,
        key_manager: KeyStore,
    ) -> ShellResult<Self> {
        let provider = resolve_provider(&config, &key_manager, &config.provider)?;
        let mut agent = Self::build(config, memory, key_manager, provider, true);
        agent.connect_mcp().await;
        Ok(agent)
    }

    /// Start the `[[mcp.servers]]` from the config and register their tools.
    ///
    /// Failures are per-server and never fatal: a server that cannot start is
    /// recorded in [`Agent::mcp_status`] and contributes no tools. Public so
    /// callers that construct the agent with an explicit provider
    /// ([`Agent::with_provider`]) can connect the same servers.
    pub async fn connect_mcp(&mut self) {
        if self.config.mcp.is_empty() {
            return;
        }
        let manager = Arc::new(McpManager::connect_all(&self.config.mcp).await);
        self.attach_mcp(manager);
    }

    /// Attach an already-connected MCP manager: registers its tools (which
    /// then behave like built-ins) and keeps it alive alongside the agent.
    ///
    /// Used by [`Agent::new`] and by callers that connect MCP servers
    /// themselves (e.g. tests); see also [`shell_mcp::McpManager::connect_all`].
    pub fn attach_mcp(&mut self, manager: Arc<McpManager>) {
        for tool in manager.tools() {
            self.tools.register_arc(tool);
        }
        self.mcp = Some(manager);
    }

    /// The attached MCP manager, when any servers are connected.
    pub fn mcp(&self) -> Option<&Arc<McpManager>> {
        self.mcp.as_ref()
    }

    /// Per-server MCP connection state, for status displays.
    pub fn mcp_status(&self) -> Vec<McpServerStatus> {
        self.mcp
            .as_ref()
            .map(|manager| manager.statuses())
            .unwrap_or_default()
    }

    /// Attach a loaded extension host: registers its tools (like built-ins)
    /// and delivers every subsequent conversation message to its
    /// `on_message` hooks.
    pub fn attach_extension_host(&mut self, host: Arc<ExtensionHost>) {
        for tool in host.tools() {
            self.tools.register_arc(tool);
        }
        self.extensions = Some(host);
    }

    /// The attached extension host, when extensions were loaded.
    pub fn extensions(&self) -> Option<&Arc<ExtensionHost>> {
        self.extensions.as_ref()
    }

    /// The session id of the most recent run (empty before the first run).
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// The agent configuration (provider, model, fallbacks, workspace).
    pub fn config(&self) -> &AgentConfig {
        &self.config
    }

    /// The session store backing this agent.
    pub fn memory(&self) -> &MemoryStore {
        &self.memory
    }

    /// Number of messages currently in the working set.
    pub fn message_count(&self) -> usize {
        self.messages.len()
    }

    /// Create an agent with an explicit provider implementation.
    ///
    /// Bypasses [`resolve_provider`]; used by tests and custom wiring.
    pub fn with_provider(
        config: AgentConfig,
        memory: MemoryStore,
        key_manager: KeyStore,
        provider: Arc<dyn LlmProvider>,
    ) -> Self {
        Self::build(config, memory, key_manager, provider, true)
    }

    /// Build an agent, optionally with the `task` subagent tool. Child agents
    /// (subagents) pass `with_task = false`, making recursion impossible.
    pub(crate) fn build(
        config: AgentConfig,
        memory: MemoryStore,
        key_manager: KeyStore,
        provider: Arc<dyn LlmProvider>,
        with_task: bool,
    ) -> Self {
        let hub = std::sync::Arc::new(SubagentHub::new());
        let mut tools = ToolRegistry::with_builtins_sandboxed(config.sandbox.clone());
        tools.register(SkillTool::new(skills::skills_dir()));
        if with_task {
            tools.register(TaskTool::new(std::sync::Arc::clone(&hub)));
        }
        Self {
            provider,
            key_manager,
            memory,
            tools,
            config,
            session_id: String::new(),
            messages: Vec::new(),
            turn_idx: 0,
            mode: AgentMode::Normal,
            active_skills: Vec::new(),
            hub,
            mcp: None,
            extensions: None,
        }
    }

    /// The tool registry (for inspection/tests).
    pub fn tools(&self) -> &ToolRegistry {
        &self.tools
    }

    /// The primary LLM provider.
    pub fn provider(&self) -> &dyn LlmProvider {
        self.provider.as_ref()
    }

    /// The key store (health/rotation state).
    pub fn key_manager(&self) -> &KeyStore {
        &self.key_manager
    }

    /// Mutable access to the key store (status updates from the TUI, tests).
    pub fn key_manager_mut(&mut self) -> &mut KeyStore {
        &mut self.key_manager
    }

    /// Promote keys whose cooldown elapsed back to `Active` (health recovery
    /// check). Returns how many keys recovered.
    pub fn refresh_key_health(&mut self) -> usize {
        self.key_manager.refresh_key_health()
    }

    /// Snapshot of every key's health: provider, non-secret id, status, and
    /// cooldown timers. Used by `sick keys`, the TUI `/keys`, and tests.
    pub fn health_check_keys(&self) -> Vec<KeyStatusInfo> {
        self.key_manager.get_status_report()
    }

    /// Fetch the model ids each key-configured provider currently serves.
    ///
    /// Providers whose lookup fails keep an empty list and carry the reason
    /// in `error` (callers fall back to the static model catalog). Keys are
    /// peeked, not rotated: listing models must not consume rotation state.
    pub async fn fetch_available_models(&self) -> Vec<ProviderModels> {
        fetch_provider_models(self.model_lookup_targets()).await
    }

    /// Credentials for a live model lookup, one per key-configured provider,
    /// in canonical id order.
    pub fn model_lookup_targets(&self) -> Vec<ModelLookup> {
        let mut providers: Vec<String> = self
            .key_manager
            .get_status_report()
            .iter()
            .map(|info| normalize_provider(&info.provider))
            .collect();
        providers.sort();
        providers.dedup();

        providers
            .into_iter()
            .filter_map(|provider| {
                let secret = self
                    .key_manager
                    .keys_for(&provider)
                    .first()
                    .map(|key| key.value.to_string())?;
                Some(ModelLookup {
                    base_url: self.key_manager.provider_base_url(&provider),
                    provider,
                    secret,
                })
            })
            .collect()
    }

    /// The current operating mode.
    pub fn mode(&self) -> AgentMode {
        self.mode
    }

    /// Switch the operating mode for subsequent turns.
    pub fn set_mode(&mut self, mode: AgentMode) {
        self.mode = mode;
    }

    /// Skills currently injected into the system prompt.
    pub fn active_skills(&self) -> &[Skill] {
        &self.active_skills
    }

    /// Activate a skill for this session (replaces same-named skills).
    pub fn activate_skill(&mut self, skill: Skill) {
        match self
            .active_skills
            .iter_mut()
            .find(|active| active.name == skill.name)
        {
            Some(existing) => *existing = skill,
            None => self.active_skills.push(skill),
        }
    }

    /// Load `<SICK_HOME>/skills/<name>/SKILL.md` and activate it.
    pub fn activate_skill_by_name(&mut self, name: &str) -> ShellResult<Skill> {
        let skill = skills::load_skill(name)?;
        self.activate_skill(skill.clone());
        Ok(skill)
    }

    /// Deactivate a skill by name; returns whether it was active.
    pub fn deactivate_skill(&mut self, name: &str) -> bool {
        let before = self.active_skills.len();
        self.active_skills.retain(|skill| skill.name != name);
        self.active_skills.len() != before
    }

    /// Resolve a provider instance from its string id (primary or fallback).
    ///
    /// The agent's primary provider is returned as-is (it may be an injected
    /// implementation); other ids resolve through [`resolve_provider`].
    pub fn get_provider(&self, provider_id: &str) -> ShellResult<Arc<dyn LlmProvider>> {
        if normalize_provider(provider_id) == normalize_provider(&self.config.provider) {
            return Ok(Arc::clone(&self.provider));
        }
        resolve_provider(&self.config, &self.key_manager, provider_id)
    }

    /// Run one non-interactive prompt to completion.
    pub async fn run(&mut self, prompt: String) -> ShellResult<AgentRun> {
        let title: String = prompt.chars().take(80).collect();
        self.start_session(&title).await?;
        let turn = self.run_turn(Some(prompt), None).await?;

        Ok(AgentRun {
            session_id: self.session_id.clone(),
            content: turn.content,
            iterations: turn.iterations,
            tool_calls: turn.tool_calls,
        })
    }

    /// Create a new session (resetting the working set) and seed it with the
    /// system prompt. Returns the new session id.
    pub async fn start_session(&mut self, title: &str) -> ShellResult<String> {
        self.session_id = self
            .memory
            .create_session(
                title,
                &self.config.model,
                &self.config.provider,
                &self.config.workspace.to_string_lossy(),
            )
            .await
            .map_err(memory_error)?;
        self.messages.clear();
        self.turn_idx = 0;
        self.push_message(ChatMessage::new("system", self.system_prompt()))
            .await?;
        Ok(self.session_id.clone())
    }

    /// Load a stored session's history into the working set and continue it.
    ///
    /// Tool call ids are re-paired with their `tool` result messages (the
    /// store persists results positionally, not with their call ids).
    pub async fn resume_session(&mut self, session_id: &str) -> ShellResult<Vec<ChatMessage>> {
        let history = self
            .memory
            .load_session_history(session_id)
            .await
            .map_err(memory_error)?;
        if history.is_empty() {
            return Err(ShellError::Memory(format!(
                "session `{session_id}` has no stored messages"
            )));
        }

        let mut pending_ids: VecDeque<String> = VecDeque::new();
        let mut messages = Vec::with_capacity(history.len());
        for stored in &history {
            let mut message = stored_to_chat_message(stored);
            match message.role.as_str() {
                "assistant" => {
                    if let Some(calls) = &message.tool_calls {
                        pending_ids.extend(calls.iter().map(|call| call.id.clone()));
                    }
                }
                "tool" => {
                    message.tool_call_id = pending_ids.pop_front();
                }
                _ => {}
            }
            messages.push(message);
        }

        self.session_id = session_id.to_string();
        self.turn_idx = history.iter().map(|m| m.turn_idx).max().unwrap_or(0);
        self.messages = messages;
        Ok(self.messages.clone())
    }

    /// Fork the current session: the stored history is copied into a new
    /// session and all subsequent messages continue there.
    pub async fn fork_session(&mut self, title: &str) -> ShellResult<String> {
        if self.session_id.is_empty() {
            return Err(ShellError::Memory("no active session to fork".to_string()));
        }
        let new_id = self
            .memory
            .fork_session(&self.session_id, title)
            .await
            .map_err(memory_error)?;
        self.session_id = new_id.clone();
        Ok(new_id)
    }

    /// Re-execute a stored tool call: exact id, unique id prefix, or `last`.
    ///
    /// Runs the tool directly (no LLM round-trip) against the workspace and
    /// returns a human-readable transcript line plus the tool output.
    pub async fn replay_tool_call(&self, id_or_prefix: &str) -> ShellResult<String> {
        let record = if id_or_prefix == "last" {
            self.memory
                .last_tool_call(&self.session_id)
                .await
                .map_err(memory_error)?
        } else {
            self.memory
                .find_tool_call(id_or_prefix)
                .await
                .map_err(memory_error)?
        };
        let Some(record) = record else {
            return Err(ShellError::Memory(format!(
                "no tool call matches `{id_or_prefix}`"
            )));
        };
        let Some(tool) = self.tools.get(&record.tool_name) else {
            return Err(ShellError::Config(format!(
                "stored tool `{}` is no longer registered",
                record.tool_name
            )));
        };
        let args = match record.args_json.as_deref() {
            Some(raw) if !raw.trim().is_empty() => serde_json::from_str(raw).map_err(|error| {
                ShellError::Memory(format!(
                    "stored args for `{}` are invalid JSON: {error}",
                    record.id
                ))
            })?,
            _ => serde_json::json!({}),
        };
        let ctx = ToolContext::new(self.config.workspace.clone());
        let result = tool.execute(args, &ctx).await;
        let status = if result.success { "ok" } else { "failed" };
        Ok(format!(
            "replayed `{}` ({}) → {status}\n{}",
            record.tool_name, record.id, result.output
        ))
    }

    /// Switch the primary provider/model for subsequent turns.
    ///
    /// When the provider id changes, any primary `base_url` override is
    /// dropped (it belonged to the previous provider).
    pub fn set_model(&mut self, provider: &str, model: &str) -> ShellResult<()> {
        let provider = provider.trim();
        let model = model.trim();
        if provider.is_empty() || model.is_empty() {
            return Err(ShellError::Config(
                "provider and model must be non-empty".to_string(),
            ));
        }

        let mut config = self.config.clone();
        if normalize_provider(provider) != normalize_provider(&config.provider) {
            config.base_url.clear();
        }
        config.provider = provider.to_string();
        config.model = model.to_string();

        let resolved = resolve_provider(&config, &self.key_manager, provider)?;
        self.provider = resolved;
        self.config = config;
        Ok(())
    }

    /// Run one user turn: optional prompt, then LLM ↔ tool execution until the
    /// model produces an answer without tool calls.
    ///
    /// Starting an empty agent begins a fresh session (titled from the
    /// prompt). `events` receives streaming updates for UIs.
    pub async fn run_turn(
        &mut self,
        prompt: Option<String>,
        events: Option<mpsc::UnboundedSender<AgentEvent>>,
    ) -> ShellResult<AgentTurn> {
        // Subagents forward their progress into this turn's event stream.
        self.hub.set_events(events.clone());

        if self.session_id.is_empty() {
            let title = prompt
                .as_deref()
                .unwrap_or("interactive session")
                .trim()
                .chars()
                .take(80)
                .collect::<String>();
            self.start_session(&title).await?;
        }

        if let Some(prompt) = prompt {
            self.push_message(ChatMessage::new("user", prompt)).await?;
        }

        let mut total_tool_calls = 0usize;
        let mut iterations = 0usize;
        let mut tool_names: Vec<String> = Vec::new();
        let mut failed_tools: Vec<String> = Vec::new();

        let content = loop {
            iterations += 1;
            if iterations > MAX_ITERATIONS {
                return Err(ShellError::Llm(format!(
                    "agent exceeded the maximum of {MAX_ITERATIONS} iterations"
                )));
            }

            self.turn_idx += 1;
            let response = self.send_to_llm(&events).await?;

            let assistant_message = ChatMessage {
                role: "assistant".to_string(),
                content: if response.content.is_empty() {
                    None
                } else {
                    Some(response.content.clone())
                },
                tool_call_id: None,
                tool_calls: if response.tool_calls.is_empty() {
                    None
                } else {
                    Some(response.tool_calls.clone())
                },
            };
            self.push_message(assistant_message).await?;

            if response.tool_calls.is_empty() {
                break response.content;
            }

            if let Some(tx) = &events {
                for call in &response.tool_calls {
                    let _ = tx.send(AgentEvent::ToolCall {
                        name: call.tool_name.clone(),
                        args: call.args.clone(),
                    });
                }
            }

            total_tool_calls += response.tool_calls.len();
            for call in &response.tool_calls {
                if !tool_names.contains(&call.tool_name) {
                    tool_names.push(call.tool_name.clone());
                }
            }
            let tool_messages = self
                .execute_tools(response.tool_calls, events.as_ref(), &mut failed_tools)
                .await;
            for message in tool_messages {
                self.push_message(message).await?;
            }
        };

        let approx_prompt_tokens = self.provider.count_tokens(&self.messages) as i64;
        if let Err(e) = self
            .memory
            .update_session_usage(&self.session_id, approx_prompt_tokens, 0)
            .await
        {
            tracing::warn!(error = %e, "failed to update session usage");
        }

        // Milestone turns (tools ran) feed semantic memory. Best-effort: a
        // memory failure never fails the turn.
        if total_tool_calls > 0
            && let Err(error) = self
                .remember_turn(&content, &tool_names, &failed_tools)
                .await
        {
            tracing::warn!(error = %error, "failed to extract a memory from the turn");
        }

        Ok(AgentTurn {
            content,
            iterations,
            tool_calls: total_tool_calls,
        })
    }

    /// Build the effective system prompt.
    fn system_prompt(&self) -> String {
        if !self.config.system_prompt.trim().is_empty() {
            return self.config.system_prompt.clone();
        }
        default_system_prompt(&self.config.workspace)
    }

    /// Persist a message locally and in the session store.
    async fn push_message(&mut self, message: ChatMessage) -> ShellResult<()> {
        // User prompts also feed the cross-session prompt history.
        if message.role == "user"
            && let Some(content) = message.content.as_deref()
            && !self.session_id.is_empty()
            && let Err(error) = self
                .memory
                .save_prompt(Some(&self.session_id), content, &self.config.model)
                .await
        {
            tracing::warn!(error = %error, "failed to save prompt history");
        }

        self.memory
            .append_message(
                &self.session_id,
                &to_stored_message(&message, self.turn_idx),
            )
            .await
            .map_err(memory_error)?;
        if let Some(host) = &self.extensions {
            host.on_message(&message);
        }
        self.messages.push(message);
        Ok(())
    }

    /// One LLM round-trip with key rotation, provider fallback, and optional
    /// streaming updates.
    async fn send_to_llm(
        &mut self,
        events: &Option<mpsc::UnboundedSender<AgentEvent>>,
    ) -> ShellResult<ChatResponse> {
        let mut messages = self.messages.clone();

        // Plan-mode instructions and active skills ride along as an ephemeral
        // system message: inserted for the request only, never persisted, so
        // they vanish when the mode changes or the skill is deactivated.
        let addendum = self.system_addendum().await;
        if !addendum.is_empty() {
            let at = usize::from(
                messages
                    .first()
                    .map(|message| message.role == "system")
                    .unwrap_or(false),
            );
            messages.insert(at, ChatMessage::new("system", addendum));
        }

        match events {
            Some(tx) => {
                let mut on_event = |event: StreamEvent| {
                    if !event.content.is_empty() {
                        let _ = tx.send(AgentEvent::Token(event.content));
                    }
                };
                self.send_with_rotation_streaming(&messages, Some(&mut on_event))
                    .await
            }
            None => self.send_with_rotation(&messages).await,
        }
    }

    /// Mode-, skill-, and memory-specific system instructions for the request.
    async fn system_addendum(&self) -> String {
        let mut sections: Vec<String> = Vec::new();

        if self.mode == AgentMode::Plan {
            sections.push(PLAN_MODE_INSTRUCTIONS.to_string());
        }

        let available = skills::list_skills();
        if !available.is_empty() {
            let mut text =
                String::from("Available skills (load one with the `skill` tool when it fits):");
            for skill in &available {
                text.push_str(&format!(
                    "\n- {}: {}",
                    skill.name,
                    if skill.description.is_empty() {
                        "(no description)"
                    } else {
                        skill.description.as_str()
                    }
                ));
            }
            sections.push(text);
        }

        for skill in &self.active_skills {
            sections.push(format!(
                "## Active skill: {}\n{}",
                skill.name,
                skill.body.trim()
            ));
        }

        if let Some(recall) = self.recall_memories().await {
            sections.push(recall);
        }

        sections.join("\n\n")
    }

    /// Semantic-memory recall for the latest user message (best-effort).
    ///
    /// Memories written by the current session are filtered out so a turn
    /// never recalls itself. Storage errors skip injection silently.
    async fn recall_memories(&self) -> Option<String> {
        let query = self
            .messages
            .iter()
            .rev()
            .find(|message| message.role == "user")
            .and_then(|message| message.content.as_deref())?;
        if query.trim().is_empty() {
            return None;
        }

        let embedding = shell_memory::embed::embed(query);
        let matches = match self.memory.search_memories(&embedding, 5).await {
            Ok(matches) => matches,
            Err(error) => {
                tracing::warn!(error = %error, "memory recall failed");
                return None;
            }
        };

        let mut lines = Vec::new();
        for entry in matches {
            if entry.source_session.as_deref() == Some(self.session_id.as_str()) {
                continue;
            }
            if entry.score.unwrap_or(0.0) < RECALL_MIN_SCORE {
                continue;
            }
            lines.push(format!("- {}", entry.content.replace('\n', " ")));
            if lines.len() >= RECALL_MAX_ITEMS {
                break;
            }
        }
        if lines.is_empty() {
            return None;
        }

        Some(format!(
            "Relevant memories from earlier sessions (use only if helpful):\n{}",
            lines.join("\n")
        ))
    }

    /// Heuristic memory extraction after a milestone turn (tools ran).
    ///
    /// Instead of spending a provider round-trip on extraction (the phase
    /// plan's suggestion), the turn is distilled deterministically — task,
    /// outcome, tools — then embedded and stored (dedup by exact content).
    /// Documented as a deviation in `Accomplish/A/phase-4-A.md`.
    async fn remember_turn(
        &self,
        summary: &str,
        tools: &[String],
        failed_tools: &[String],
    ) -> ShellResult<()> {
        let task = self
            .messages
            .iter()
            .rev()
            .find(|message| message.role == "user")
            .and_then(|message| message.content.clone())
            .unwrap_or_default();

        let mut content = String::new();
        if !task.trim().is_empty() {
            content.push_str(&format!("Task: {}", truncate_chars(task.trim(), 160)));
        }
        if !summary.trim().is_empty() {
            if !content.is_empty() {
                content.push('\n');
            }
            content.push_str(&format!("Outcome: {}", truncate_chars(summary.trim(), 240)));
        }
        if !tools.is_empty() {
            if !content.is_empty() {
                content.push('\n');
            }
            content.push_str(&format!("Tools: {}", tools.join(", ")));
        }
        if !failed_tools.is_empty() {
            if !content.is_empty() {
                content.push('\n');
            }
            content.push_str(&format!(
                "Mistakes to avoid: {} failed; inspect the error and change approach \
                 instead of repeating the same call unchanged.",
                failed_tools.join(", ")
            ));
        }
        if content.trim().is_empty() {
            return Ok(());
        }

        let embedding = shell_memory::embed::to_bytes(&shell_memory::embed::embed(&content));
        self.memory
            .add_memory(&content, Some(&embedding), Some(&self.session_id))
            .await
            .map_err(memory_error)?;
        Ok(())
    }

    /// Send a completion request with transparent key rotation.
    ///
    /// Walks the primary provider and the configured fallback chain. On each
    /// attempt a healthy key is selected from the [`KeyStore`]; provider
    /// errors update that key's health so the next attempt moves on:
    ///
    /// | Error | Key status | Action |
    /// |---|---|---|
    /// | 429 rate limit | `RateLimited` (60s) | next key |
    /// | 401/403 | `Invalid` | next key |
    /// | context window | `Exhausted` | truncate history, next key |
    /// | quota | `Exhausted` | next key |
    /// | 5xx/network | `UnknownError` | next key (cooldown after 3 in a row) |
    pub async fn send_with_rotation(
        &mut self,
        messages: &[ChatMessage],
    ) -> ShellResult<ChatResponse> {
        self.send_with_rotation_streaming(messages, None).await
    }

    /// Like [`Self::send_with_rotation`], but forwards streamed chunks of the
    /// assistant response to `on_event` as they arrive.
    ///
    /// Chunks already forwarded from a failed attempt are not replayed when a
    /// rotation retry starts (`provider.chat_streaming` fails before emitting
    /// on HTTP-level errors; transient mid-stream failures are terminal).
    pub async fn send_with_rotation_streaming(
        &mut self,
        messages: &[ChatMessage],
        mut on_event: Option<&mut (dyn FnMut(StreamEvent) + Send)>,
    ) -> ShellResult<ChatResponse> {
        let mut attempt = 0usize;
        let mut last_error: Option<String> = None;

        let mut providers_to_try: Vec<(String, String)> = self.config.fallback_chain.clone();
        let primary = normalize_provider(&self.config.provider);
        if !providers_to_try
            .iter()
            .any(|(p, _)| normalize_provider(p) == primary)
        {
            providers_to_try.insert(0, (self.config.provider.clone(), self.config.model.clone()));
        }

        let tools = self.tool_definitions();
        let mut history = messages.to_vec();

        'outer: for (provider_id, model) in &providers_to_try {
            let provider = match self.get_provider(provider_id) {
                Ok(provider) => provider,
                Err(e) => {
                    tracing::warn!(provider = %provider_id, error = %e, "skipping unknown fallback provider");
                    last_error = Some(e.to_string());
                    continue;
                }
            };

            loop {
                if attempt >= MAX_ROTATION_ATTEMPTS {
                    break 'outer;
                }

                let Some((key_id, key_value)) = self
                    .key_manager
                    .next_key(provider_id)
                    .map(|key| (key.id.clone(), key.value.to_string()))
                else {
                    tracing::warn!(provider = %provider.id(), "no healthy keys; trying next provider");
                    last_error = Some(format!("no healthy keys for provider `{}`", provider.id()));
                    break;
                };
                attempt += 1;

                let request = CompletionRequest {
                    model: model.clone(),
                    messages: history.clone(),
                    api_key: key_value,
                    tools: tools.clone(),
                    temperature: self.config.temperature,
                    max_tokens: if self.config.max_tokens > 0 {
                        Some(self.config.max_tokens)
                    } else {
                        None
                    },
                    stream: true,
                };

                tracing::debug!(
                    provider = %provider.id(),
                    model = %model,
                    key = %key_id,
                    attempt,
                    "attempting completion"
                );

                let result = match on_event.as_mut() {
                    Some(cb) => provider.chat_streaming(request, &mut **cb).await,
                    None => provider.chat(request).await,
                };

                match result {
                    Ok(response) => {
                        self.key_manager.mark_key_status(
                            provider_id,
                            &key_id,
                            KeyStatus::Active,
                            None,
                        );
                        return Ok(response);
                    }
                    Err(ProviderError::RateLimitExceeded) => {
                        tracing::warn!(provider = %provider.id(), key = %key_id, "rate limited; rotating key");
                        self.key_manager.mark_key_status(
                            provider_id,
                            &key_id,
                            KeyStatus::RateLimited,
                            Some(Duration::from_secs(60)),
                        );
                        last_error = Some(format!("provider `{}` rate limited", provider.id()));
                        continue;
                    }
                    Err(ProviderError::ContextWindowExceeded) => {
                        tracing::warn!(
                            provider = %provider.id(),
                            key = %key_id,
                            "context window exceeded; truncating history"
                        );
                        self.key_manager.mark_key_status(
                            provider_id,
                            &key_id,
                            KeyStatus::Exhausted,
                            None,
                        );
                        let window = provider.context_window(model);
                        history = self.truncate_history(window);
                        last_error = Some(format!(
                            "provider `{}` context window exceeded",
                            provider.id()
                        ));
                        continue;
                    }
                    Err(ProviderError::AuthenticationFailed) => {
                        tracing::warn!(provider = %provider.id(), key = %key_id, "authentication failed; key marked invalid");
                        self.key_manager.mark_key_status(
                            provider_id,
                            &key_id,
                            KeyStatus::Invalid,
                            None,
                        );
                        last_error = Some(format!(
                            "provider `{}` rejected key {key_id}",
                            provider.id()
                        ));
                        continue;
                    }
                    Err(ProviderError::TokenQuotaExceeded) => {
                        tracing::warn!(provider = %provider.id(), key = %key_id, "token quota exhausted; rotating key");
                        self.key_manager.mark_key_status(
                            provider_id,
                            &key_id,
                            KeyStatus::Exhausted,
                            None,
                        );
                        last_error = Some(format!("provider `{}` quota exhausted", provider.id()));
                        continue;
                    }
                    Err(ProviderError::ServiceUnavailable { status, .. }) => {
                        tracing::warn!(provider = %provider.id(), key = %key_id, status, "service unavailable; rotating key");
                        self.key_manager.mark_key_status(
                            provider_id,
                            &key_id,
                            KeyStatus::UnknownError,
                            None,
                        );
                        last_error = Some(format!(
                            "provider `{}` unavailable (status {status})",
                            provider.id()
                        ));
                        continue;
                    }
                    Err(e) => return Err(provider_error(e)),
                }
            }
        }

        Err(ShellError::Llm(match last_error {
            Some(detail) => format!("all providers and keys exhausted: {detail}"),
            None => "all providers and keys exhausted".to_string(),
        }))
    }

    /// Build the tool definitions exposed to the model.
    ///
    /// In plan mode only the read-only allowlist ( [`PLAN_MODE_TOOLS`] ) is
    /// offered, so the model cannot even request a mutating tool.
    fn tool_definitions(&self) -> Vec<ToolDefinition> {
        self.tools
            .specs()
            .into_iter()
            .filter(|spec| {
                self.mode != AgentMode::Plan || PLAN_MODE_TOOLS.contains(&spec.name.as_str())
            })
            .map(|spec| ToolDefinition {
                name: spec.name,
                description: spec.description,
                parameters: spec.parameters,
            })
            .collect()
    }

    /// Drop the oldest messages so the history fits `context_window`, keeping
    /// the system prompt and cutting at a `user` boundary where possible.
    fn truncate_history(&mut self, context_window: usize) -> Vec<ChatMessage> {
        let truncated = truncate_messages(&self.messages, context_window);
        if truncated.len() < self.messages.len() {
            tracing::warn!(
                before = self.messages.len(),
                after = truncated.len(),
                "truncated conversation history after context-window error"
            );
        }
        self.messages = truncated;
        self.messages.clone()
    }

    /// Execute tool calls sequentially and produce `tool` messages.
    ///
    /// Refreshes the subagent snapshot first so `task` children see the
    /// current config/keys, and enforces the plan-mode allowlist even if the
    /// model still requests a mutating tool.
    async fn execute_tools(
        &self,
        tool_calls: Vec<ToolCall>,
        events: Option<&mpsc::UnboundedSender<AgentEvent>>,
        failed_tools: &mut Vec<String>,
    ) -> Vec<ChatMessage> {
        self.sync_subagent_spec();
        let ctx = ToolContext::new(self.config.workspace.clone());
        let mut messages = Vec::with_capacity(tool_calls.len());

        for call in tool_calls {
            let started = Instant::now();
            let result: ToolResult = match self.tools.get(&call.tool_name) {
                Some(_)
                    if self.mode == AgentMode::Plan
                        && !PLAN_MODE_TOOLS.contains(&call.tool_name.as_str()) =>
                {
                    ToolResult::err(format!(
                        "tool `{}` is disabled in plan mode (read-only tools: {})",
                        call.tool_name,
                        PLAN_MODE_TOOLS.join(", ")
                    ))
                }
                Some(tool) => tool.execute(call.args.clone(), &ctx).await,
                None => ToolResult::err(format!("unknown tool `{}`", call.tool_name)),
            };
            let duration_ms = started.elapsed().as_millis() as i64;

            tracing::info!(
                tool = %call.tool_name,
                success = result.success,
                duration_ms,
                "tool executed"
            );
            if !result.success && !failed_tools.contains(&call.tool_name) {
                failed_tools.push(call.tool_name.clone());
            }

            if let Some(tx) = events {
                let _ = tx.send(AgentEvent::ToolResult {
                    name: call.tool_name.clone(),
                    success: result.success,
                    output: result.output.clone(),
                    duration_ms: duration_ms as u64,
                });
            }

            let record = ToolCallRecord {
                id: call.id.clone(),
                session_id: self.session_id.clone(),
                turn_idx: Some(self.turn_idx),
                tool_name: call.tool_name.clone(),
                args_json: Some(call.args.to_string()),
                result: Some(result.output.clone()),
                duration_ms: Some(duration_ms),
                exit_code: result.exit_code,
            };
            if let Err(e) = self.memory.append_tool_call(&record).await {
                tracing::warn!(error = %e, "failed to persist tool call");
            }

            messages.push(ChatMessage {
                role: "tool".to_string(),
                content: Some(result.output),
                tool_call_id: Some(call.id),
                tool_calls: None,
            });
        }

        messages
    }

    /// Snapshot config/provider/keys/memory into the subagent hub so the
    /// `task` tool spawns children consistent with the current agent state.
    fn sync_subagent_spec(&self) {
        self.hub.set_spec(SubagentSpec {
            config: self.config.clone(),
            provider: Arc::clone(&self.provider),
            key_manager: self.key_manager.clone(),
            memory: self.memory.clone(),
        });
    }
}

/// Convert a stored row back into an in-memory chat message.
fn stored_to_chat_message(message: &StoredMessage) -> ChatMessage {
    let tool_calls = message
        .tool_calls_json
        .as_deref()
        .and_then(|raw| serde_json::from_str::<Vec<serde_json::Value>>(raw).ok())
        .map(|calls| {
            calls
                .iter()
                .filter_map(|call| {
                    Some(ToolCall {
                        id: call
                            .get("id")
                            .and_then(serde_json::Value::as_str)?
                            .to_string(),
                        tool_name: call
                            .get("name")
                            .and_then(serde_json::Value::as_str)?
                            .to_string(),
                        args: call
                            .get("args")
                            .cloned()
                            .unwrap_or_else(|| serde_json::json!({})),
                        thought_signature: call
                            .get("thought_signature")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string),
                    })
                })
                .collect::<Vec<_>>()
        })
        .filter(|calls| !calls.is_empty());

    ChatMessage {
        role: message.role.clone(),
        content: message.content.clone(),
        tool_call_id: None,
        tool_calls,
    }
}

/// Convert an in-memory chat message into a storable row.
fn to_stored_message(message: &ChatMessage, turn_idx: i64) -> StoredMessage {
    let tool_calls_json = message.tool_calls.as_ref().map(|calls| {
        serde_json::to_string(
            &calls
                .iter()
                .map(|call| {
                    serde_json::json!({
                        "id": call.id,
                        "name": call.tool_name,
                        "args": call.args,
                        "thought_signature": call.thought_signature,
                    })
                })
                .collect::<Vec<_>>(),
        )
        .unwrap_or_else(|_| "[]".to_string())
    });

    StoredMessage {
        role: message.role.clone(),
        content: message.content.clone(),
        tool_calls_json,
        tool_result: None,
        token_count: None,
        turn_idx,
    }
}

fn provider_error(error: ProviderError) -> ShellError {
    ShellError::Llm(error.to_string())
}

/// Build the provider instance for `provider_id`.
///
/// Resolution order for the base URL:
/// 1. per-provider `base_url` from the keys file (OpenAI-compatible),
/// 2. `AgentConfig::base_url` for the configured primary provider,
/// 3. built-in endpoint per provider.
pub fn resolve_provider(
    config: &AgentConfig,
    key_manager: &KeyStore,
    provider_id: &str,
) -> ShellResult<Arc<dyn LlmProvider>> {
    let id = normalize_provider(provider_id);

    let config_base =
        if id == normalize_provider(&config.provider) && !config.base_url.trim().is_empty() {
            Some(config.base_url.trim().trim_end_matches('/').to_string())
        } else {
            None
        };
    let base_url = config_base.or_else(|| key_manager.provider_base_url(&id));

    if let Some(base_url) = base_url {
        return Ok(Arc::new(OpenAiCompatProvider::with_name(id, base_url)));
    }

    let provider: Arc<dyn LlmProvider> = match id.as_str() {
        "openai" => Arc::new(OpenAiProvider::new()),
        "anthropic" => Arc::new(AnthropicProvider::new()),
        "google" => Arc::new(GoogleProvider::new()),
        "deepseek" => Arc::new(OpenAiCompatProvider::with_name(
            "deepseek",
            "https://api.deepseek.com/v1",
        )),
        "openrouter" => Arc::new(OpenAiCompatProvider::with_name(
            "openrouter",
            "https://openrouter.ai/api/v1",
        )),
        "qwen" => Arc::new(OpenAiCompatProvider::with_name(
            "qwen",
            "https://dashscope.aliyuncs.com/compatible-mode/v1",
        )),
        "aionlabs" => Arc::new(OpenAiCompatProvider::with_name(
            "aionlabs",
            "https://api.aionlabs.ai/v1",
        )),
        "cohere" => Arc::new(OpenAiCompatProvider::with_name(
            "cohere",
            "https://api.cohere.com/v2",
        )),
        "mistral" => Arc::new(OpenAiCompatProvider::with_name(
            "mistral",
            "https://api.mistral.ai/v1",
        )),
        "zai" => Arc::new(OpenAiCompatProvider::with_name(
            "zai",
            "https://open.bigmodel.cn/api/paas/v4",
        )),
        "groq" => Arc::new(OpenAiCompatProvider::with_name(
            "groq",
            "https://api.groq.com/openai/v1",
        )),
        "cloudflare" => {
            let account_id = std::env::var("CLOUDFLARE_ACCOUNT_ID").unwrap_or_default();
            let base_url = cloudflare_base_url(&account_id)
                .map_err(|error| ShellError::Config(error.to_string()))?;
            Arc::new(OpenAiCompatProvider::with_name("cloudflare", base_url))
        }
        "huggingface" => Arc::new(OpenAiCompatProvider::with_name(
            "huggingface",
            "https://router.huggingface.co/v1",
        )),
        "kilo" => Arc::new(OpenAiCompatProvider::with_name(
            "kilo",
            "https://api.kilo.ai/api/gateway",
        )),
        "llm7" => Arc::new(OpenAiCompatProvider::with_name(
            "llm7",
            "https://api.llm7.io/v1",
        )),
        "modelscope" => Arc::new(OpenAiCompatProvider::with_name(
            "modelscope",
            "https://api-inference.modelscope.cn/v1",
        )),
        "nvidia" => Arc::new(OpenAiCompatProvider::with_name(
            "nvidia",
            "https://integrate.api.nvidia.com/v1",
        )),
        "ollama" => Arc::new(OpenAiCompatProvider::with_name(
            "ollama",
            "https://ollama.com/v1",
        )),
        "ovhcloud" => Arc::new(OpenAiCompatProvider::with_name(
            "ovhcloud",
            "https://oai.endpoints.kepler.ai.cloud.ovh.net/v1",
        )),
        "siliconflow" => Arc::new(OpenAiCompatProvider::with_name(
            "siliconflow",
            "https://api.siliconflow.cn/v1",
        )),
        "cerebras" => Arc::new(OpenAiCompatProvider::with_name(
            "cerebras",
            "https://api.cerebras.ai/v1",
        )),
        "sambanova" => Arc::new(OpenAiCompatProvider::with_name(
            "sambanova",
            "https://api.sambanova.ai/v1",
        )),
        "together" => Arc::new(OpenAiCompatProvider::with_name(
            "together",
            "https://api.together.xyz/v1",
        )),
        "chutes" => Arc::new(OpenAiCompatProvider::with_name(
            "chutes",
            "https://api.chutes.ai/v1",
        )),
        "github" => Arc::new(OpenAiCompatProvider::with_name(
            "github",
            "https://models.inference.ai.azure.com",
        )),
        other => {
            return Err(ShellError::Llm(format!(
                "provider `{other}` is not supported. Supported providers: \
                 openai, anthropic, google/gemini, deepseek, openrouter, qwen, \
                 aionlabs, cohere, mistral, zai, groq, cloudflare, huggingface, \
                 kilo, llm7, modelscope, nvidia, ollama, ovhcloud, siliconflow, \
                 cerebras, sambanova, together, chutes, github. \
                 For any other OpenAI-compatible endpoint, set `base_url` for \
                 the provider in the keys file."
            )));
        }
    };

    Ok(provider)
}

/// Sensible default model per provider when none is configured.
pub fn default_model_for(provider: &str) -> &'static str {
    match normalize_provider(provider).as_str() {
        "anthropic" => "claude-3-7-sonnet-20250219",
        "google" => "gemini-3.8-flash",
        "deepseek" => "deepseek-chat",
        "openrouter" => "anthropic/claude-3.5-sonnet",
        "qwen" => "qwen-plus",
        "aionlabs" => "aion-labs/aion-2.0",
        "cohere" => "command-r-plus",
        "mistral" => "mistral-small-latest",
        "zai" => "glm-4.7-flash",
        "groq" => "llama-3.3-70b-versatile",
        "cloudflare" => "@cf/meta/llama-3.3-70b-instruct-fp8-fast",
        "huggingface" => "meta-llama/Meta-Llama-3.1-8B-Instruct",
        "kilo" => "kilo-auto/free",
        "llm7" => "gpt-oss:20b",
        "modelscope" => "Qwen/Qwen3.5-27B",
        "nvidia" => "nvidia/nemotron-3-super-120b-a12b",
        "ollama" => "deepseek-v4-flash",
        "ovhcloud" => "Qwen3.5-397B-A17B",
        "siliconflow" => "Qwen/Qwen3-8B",
        "cerebras" => "llama-3.3-70b",
        "sambanova" => "Meta-Llama-3.3-70B-Instruct",
        "together" => "meta-llama/Llama-3.3-70B-Instruct-Turbo",
        "chutes" => "deepseek-ai/DeepSeek-V3-0324",
        "github" => "gpt-4o",
        _ => "gpt-4.1",
    }
}

/// Result of listing the models one configured provider currently serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderModels {
    /// Canonical provider id (e.g. `google`).
    pub provider: String,
    /// Model ids the provider offers (empty when the lookup failed).
    pub models: Vec<String>,
    /// Why the lookup failed, when it did.
    pub error: Option<String>,
}

/// Credentials for one provider's live model lookup.
#[derive(Debug, Clone)]
pub struct ModelLookup {
    /// Canonical provider id (e.g. `google`).
    pub provider: String,
    /// API key value used for the lookup.
    pub secret: String,
    /// Custom base URL from the keys file, if any.
    pub base_url: Option<String>,
}

/// Fetch `targets`' model lists concurrently, one [`ProviderModels`] each.
///
/// Spawned tasks mean a slow or unreachable provider cannot hold up the
/// others; callers typically run this off their main loop.
pub async fn fetch_provider_models(targets: Vec<ModelLookup>) -> Vec<ProviderModels> {
    let mut set = tokio::task::JoinSet::new();
    for target in targets {
        set.spawn(async move {
            match shell_llm::fetch_models(
                &target.provider,
                &target.secret,
                target.base_url.as_deref(),
            )
            .await
            {
                Ok(models) => ProviderModels {
                    provider: target.provider,
                    models,
                    error: None,
                },
                Err(error) => ProviderModels {
                    provider: target.provider,
                    models: Vec::new(),
                    error: Some(error.to_string()),
                },
            }
        });
    }

    let mut results = Vec::new();
    while let Some(joined) = set.join_next().await {
        if let Ok(entry) = joined {
            results.push(entry);
        }
    }
    results.sort_by(|a, b| a.provider.cmp(&b.provider));
    results
}

/// Model to use when validating a pasted key for `provider_id`: the keys-file
/// `default_model` when one is configured, otherwise the built-in default.
pub fn validation_model(key_manager: &KeyStore, provider_id: &str) -> String {
    key_manager
        .provider_config(provider_id)
        .and_then(|config| config.default_model.clone())
        .unwrap_or_else(|| default_model_for(provider_id).to_string())
}

/// Outcome of validating a pasted API key against a provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyValidation {
    /// The provider accepted the key.
    Verified,
    /// The request failed in a way that does not prove the key invalid
    /// (network error, rate limit, 5xx): the key may still be good.
    Unverified(String),
}

/// Check whether `secret` is accepted by `provider` with one minimal request.
///
/// `Err` means the provider rejected the key itself (401/403); every other
/// failure is reported as [`KeyValidation::Unverified`] so the caller can
/// still choose to save the key.
pub async fn validate_api_key(
    provider: &Arc<dyn LlmProvider>,
    model: &str,
    secret: &str,
) -> ShellResult<KeyValidation> {
    let request = CompletionRequest {
        model: model.to_string(),
        messages: vec![ChatMessage::new("user", "ping")],
        api_key: secret.to_string(),
        tools: Vec::new(),
        temperature: 0.0,
        max_tokens: Some(1),
        stream: false,
    };
    match provider.chat(request).await {
        Ok(_) => Ok(KeyValidation::Verified),
        Err(ProviderError::AuthenticationFailed) => Err(ShellError::KeyManager(
            "the provider rejected this key (401/403)".to_string(),
        )),
        Err(error) => Ok(KeyValidation::Unverified(error.to_string())),
    }
}

/// Estimated token cost of a single message (chars / 4, tool args included).
fn message_cost(message: &ChatMessage) -> usize {
    let content = message
        .content
        .as_deref()
        .map(|c| c.chars().count())
        .unwrap_or(0);
    let calls: usize = message
        .tool_calls
        .as_ref()
        .map(|calls| {
            calls
                .iter()
                .map(|c| c.tool_name.chars().count() + c.args.to_string().chars().count())
                .sum()
        })
        .unwrap_or(0);
    (content + calls) / 4
}

/// Keep the system prompt plus the newest messages that fit ~90% of the
/// context window, cutting at a `user` message so the request stays valid for
/// strict providers (Anthropic requires a user-first conversation).
fn truncate_messages(messages: &[ChatMessage], context_window: usize) -> Vec<ChatMessage> {
    if messages.len() <= 2 || context_window == 0 {
        return messages.to_vec();
    }

    let budget = context_window / 10 * 9;
    let has_system = messages[0].role == "system";
    let start = usize::from(has_system);

    // Walk backward: everything from `cut` onward fits the budget.
    let mut acc = 0usize;
    let mut cut = start;
    for i in (start..messages.len()).rev() {
        let cost = message_cost(&messages[i]);
        if acc + cost > budget {
            cut = i + 1;
            break;
        }
        acc += cost;
    }

    // Prefer a cut at (or after) the first retained user message.
    if let Some(idx) = (cut..messages.len()).find(|&i| messages[i].role == "user") {
        cut = idx;
    } else if let Some(idx) = messages[..cut].iter().rposition(|m| m.role == "user") {
        // No user message in the fitting suffix: keep the most recent one so
        // the conversation remains valid (may exceed the budget slightly).
        cut = idx;
    } else if cut >= messages.len() {
        cut = messages.len() - 1;
    }

    if cut <= start {
        return messages.to_vec();
    }

    let mut kept = Vec::with_capacity(messages.len() - cut + start);
    if has_system {
        kept.push(messages[0].clone());
    }
    kept.extend_from_slice(&messages[cut..]);
    kept
}

/// Built-in system prompt used when `AgentConfig::system_prompt` is empty.
fn default_system_prompt(workspace: &std::path::Path) -> String {
    format!(
        "You are S.I.C.K, a CLI coding agent running on {os}. \
         You have tools to read, write, and edit files, list directories, search \
         file contents, find files by glob, and run shell commands. Use the tools \
         to accomplish the user's request instead of guessing. Working directory: {workspace}. \
         When the task is complete, reply with a short summary of what you did.",
        os = std::env::consts::OS,
        workspace = workspace.display(),
    )
}

fn memory_error(error: shell_memory::MemoryError) -> ShellError {
    ShellError::Memory(error.to_string())
}

/// Truncate `text` to `max` chars, appending an ellipsis when cut.
fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut truncated: String = text.chars().take(max).collect();
    truncated.push('…');
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;
    use shell_memory::Message as StoredMessage;

    /// Provider whose responses are scripted; content is streamed in halves.
    /// Every request is recorded so tests can assert which tools/messages the
    /// agent sent.
    struct ScriptedProvider {
        responses: std::sync::Mutex<VecDeque<ChatResponse>>,
        recorded_tools: std::sync::Mutex<Vec<Vec<String>>>,
        recorded_messages: std::sync::Mutex<Vec<Vec<ChatMessage>>>,
    }

    impl ScriptedProvider {
        fn new(responses: Vec<ChatResponse>) -> Self {
            Self {
                responses: std::sync::Mutex::new(responses.into()),
                recorded_tools: std::sync::Mutex::new(Vec::new()),
                recorded_messages: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn next_response(&self) -> Result<ChatResponse, ProviderError> {
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| ProviderError::Other("script exhausted".to_string()))
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

        /// Tool names offered to the model in request `index`.
        fn tools_offered(&self, index: usize) -> Vec<String> {
            self.recorded_tools.lock().unwrap()[index].clone()
        }

        /// System-message texts sent with request `index`.
        fn system_texts(&self, index: usize) -> Vec<String> {
            self.recorded_messages.lock().unwrap()[index]
                .iter()
                .filter(|message| message.role == "system")
                .filter_map(|message| message.content.clone())
                .collect()
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
            on_event(StreamEvent {
                content: String::new(),
                tool_calls: response.tool_calls.clone(),
                done: true,
            });
            Ok(response)
        }
    }

    async fn scripted_agent(
        responses: Vec<ChatResponse>,
        workspace: &std::path::Path,
        db: &std::path::Path,
    ) -> Agent {
        scripted_agent_with_provider(responses, workspace, db)
            .await
            .0
    }

    /// Like [`scripted_agent`], but also returns the provider handle so tests
    /// can inspect the requests the agent sent.
    async fn scripted_agent_with_provider(
        responses: Vec<ChatResponse>,
        workspace: &std::path::Path,
        db: &std::path::Path,
    ) -> (Agent, Arc<ScriptedProvider>) {
        let memory = MemoryStore::init(&db.to_string_lossy()).await.unwrap();
        let mut cfg = AgentConfig::new("scripted", "scripted-1");
        cfg.workspace = workspace.to_path_buf();
        let mut keys = KeyStore::new();
        // Rotation selects a key per attempt even though the scripted provider
        // ignores it; register a placeholder so attempts are made.
        keys.add_key("scripted", "scripted-placeholder-key");
        let provider = Arc::new(ScriptedProvider::new(responses));
        let agent = Agent::with_provider(cfg, memory, keys, provider.clone());
        (agent, provider)
    }

    fn text_response(content: &str) -> ChatResponse {
        ChatResponse {
            content: content.to_string(),
            tool_calls: Vec::new(),
        }
    }

    #[tokio::test]
    async fn run_turn_streams_tokens_tools_and_results() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("sessions.db");
        let first = ChatResponse {
            content: "Writing the file.".to_string(),
            tool_calls: vec![ToolCall {
                id: "call_1".to_string(),
                tool_name: "write".to_string(),
                args: serde_json::json!({"path": "hello.txt", "content": "hi"}),
                thought_signature: None,
            }],
        };
        let mut agent = scripted_agent(vec![first, text_response("Done.")], dir.path(), &db).await;

        let (tx, mut rx) = mpsc::unbounded_channel();
        let turn = agent
            .run_turn(Some("create hello.txt".to_string()), Some(tx))
            .await
            .unwrap();

        assert_eq!(turn.content, "Done.");
        assert_eq!(turn.iterations, 2);
        assert_eq!(turn.tool_calls, 1);
        assert!(dir.path().join("hello.txt").exists(), "tool ran");
        assert!(!agent.session_id().is_empty(), "session was started");

        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        let streamed: String = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::Token(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(streamed, "Writing the file.Done.");
        let token_events = events
            .iter()
            .filter(|e| matches!(e, AgentEvent::Token(_)))
            .count();
        assert!(token_events >= 4, "content arrives in multiple chunks");

        let call_pos = events
            .iter()
            .position(|e| matches!(e, AgentEvent::ToolCall { name, .. } if name == "write"))
            .expect("tool call event");
        let result_pos = events
            .iter()
            .position(|e| matches!(e, AgentEvent::ToolResult { name, success, .. } if name == "write" && *success))
            .expect("tool result event");
        let last_token_pos = events
            .iter()
            .rposition(|e| matches!(e, AgentEvent::Token(_)))
            .unwrap();
        assert!(
            call_pos < result_pos && result_pos < last_token_pos,
            "order: call → result → final answer tokens"
        );
    }

    #[tokio::test]
    async fn resume_session_restores_history_and_pairs_tool_ids() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("sessions.db");
        let first = ChatResponse {
            content: "Checking.".to_string(),
            tool_calls: vec![ToolCall {
                id: "call_42".to_string(),
                tool_name: "ls".to_string(),
                args: serde_json::json!({}),
                thought_signature: None,
            }],
        };
        let mut agent =
            scripted_agent(vec![first, text_response("All good.")], dir.path(), &db).await;
        let run = agent.run("list the workspace".to_string()).await.unwrap();

        // Fresh agent, same database, resume the stored session.
        let mut resumed = scripted_agent(vec![text_response("Continuing.")], dir.path(), &db).await;
        let history = resumed.resume_session(&run.session_id).await.unwrap();

        assert_eq!(history[0].role, "system");
        assert_eq!(history[1].role, "user");
        let tool_message = history
            .iter()
            .find(|m| m.role == "tool")
            .expect("tool result stored");
        assert_eq!(
            tool_message.tool_call_id.as_deref(),
            Some("call_42"),
            "tool call ids are re-paired on resume"
        );

        let turn = resumed
            .run_turn(Some("anything else?".to_string()), None)
            .await
            .unwrap();
        assert_eq!(turn.content, "Continuing.");
        assert_eq!(
            resumed.session_id(),
            run.session_id,
            "resumed turns continue the same session"
        );
    }

    #[tokio::test]
    async fn set_model_switches_provider_and_validates() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("sessions.db");
        let mut agent = scripted_agent(vec![], dir.path(), &db).await;
        assert_eq!(agent.provider().id(), "scripted");

        agent
            .set_model("anthropic", "claude-3-7-sonnet-20250219")
            .unwrap();
        assert_eq!(agent.config().provider, "anthropic");
        assert_eq!(agent.config().model, "claude-3-7-sonnet-20250219");
        assert_eq!(agent.provider().id(), "anthropic");

        assert!(agent.set_model("", "x").is_err(), "empty provider rejected");
        assert!(
            agent.set_model("acme", "acme-1").is_err(),
            "unknown provider rejected"
        );
        assert_eq!(
            agent.config().provider,
            "anthropic",
            "failed switch leaves config untouched"
        );
    }

    fn config(provider: &str, model: &str) -> AgentConfig {
        AgentConfig::new(provider, model)
    }

    #[test]
    fn stored_message_carries_tool_calls_as_json() {
        let message = ChatMessage {
            role: "assistant".to_string(),
            content: None,
            tool_call_id: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_1".to_string(),
                tool_name: "write".to_string(),
                args: serde_json::json!({"path": "hello.txt"}),
                thought_signature: None,
            }]),
        };
        let stored = to_stored_message(&message, 2);
        assert_eq!(stored.role, "assistant");
        assert_eq!(stored.turn_idx, 2);
        let json: serde_json::Value =
            serde_json::from_str(stored.tool_calls_json.as_deref().unwrap()).unwrap();
        assert_eq!(json[0]["name"], "write");
        assert_eq!(json[0]["args"]["path"], "hello.txt");
    }

    #[test]
    fn stored_tool_call_round_trips_thought_signature() {
        let message = ChatMessage {
            role: "assistant".to_string(),
            content: None,
            tool_call_id: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_9".to_string(),
                tool_name: "ls".to_string(),
                args: serde_json::json!({}),
                thought_signature: Some("sig-123".to_string()),
            }]),
        };
        let stored = to_stored_message(&message, 1);
        let rebuilt = stored_to_chat_message(&stored);
        let calls = rebuilt
            .tool_calls
            .expect("tool calls survive the round trip");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].thought_signature.as_deref(), Some("sig-123"));
    }

    #[test]
    fn default_system_prompt_mentions_tools_and_workspace() {
        let prompt = default_system_prompt(&PathBuf::from("C:/work"));
        assert!(prompt.contains("tools"));
        assert!(prompt.contains("C:/work"));
        assert!(prompt.contains(std::env::consts::OS));
    }

    #[test]
    fn stored_message_without_tools_has_no_json() {
        let message = ChatMessage::new("user", "hi");
        let stored: StoredMessage = to_stored_message(&message, 1);
        assert!(stored.tool_calls_json.is_none());
        assert_eq!(stored.content.as_deref(), Some("hi"));
    }

    #[test]
    fn resolves_native_providers_by_id() {
        let cfg = config("openai", "gpt-4.1");
        let keys = KeyStore::new();
        assert_eq!(
            resolve_provider(&cfg, &keys, "openai").unwrap().id(),
            "openai"
        );
        assert_eq!(
            resolve_provider(&cfg, &keys, "anthropic").unwrap().id(),
            "anthropic"
        );
        assert_eq!(
            resolve_provider(&cfg, &keys, "google").unwrap().id(),
            "google"
        );
        assert_eq!(
            resolve_provider(&cfg, &keys, "gemini").unwrap().id(),
            "google"
        );
        assert_eq!(
            resolve_provider(&cfg, &keys, "deepseek").unwrap().id(),
            "deepseek"
        );
        assert_eq!(
            resolve_provider(&cfg, &keys, "openrouter").unwrap().id(),
            "openrouter"
        );
        assert_eq!(resolve_provider(&cfg, &keys, "qwen").unwrap().id(), "qwen");
    }

    #[test]
    fn config_base_url_overrides_only_the_primary_provider() {
        let mut cfg = config("google", "gemini-2.0-flash");
        cfg.base_url = "http://127.0.0.1:9999/v1/".to_string();
        let keys = KeyStore::new();

        // Primary provider is served by the OpenAI-compatible endpoint.
        assert_eq!(
            resolve_provider(&cfg, &keys, "google").unwrap().id(),
            "google"
        );
        // Other providers keep their native endpoints.
        assert_eq!(
            resolve_provider(&cfg, &keys, "anthropic").unwrap().id(),
            "anthropic"
        );
        assert_eq!(
            resolve_provider(&cfg, &keys, "openai").unwrap().id(),
            "openai"
        );
    }

    #[test]
    fn keys_file_base_url_applies_to_any_provider() {
        let cfg = config("openai", "gpt-4.1");
        let mut keys = KeyStore::new();
        keys.set_provider_base_url("anthropic", "http://127.0.0.1:8081/v1");
        // Resolved through the compat provider, but the id is preserved.
        assert_eq!(
            resolve_provider(&cfg, &keys, "anthropic").unwrap().id(),
            "anthropic"
        );
    }

    #[test]
    fn config_base_url_makes_custom_provider_valid() {
        let mut cfg = config("my-local", "qwen2.5-7b");
        cfg.base_url = "http://localhost:8080/v1".to_string();
        let provider = resolve_provider(&cfg, &KeyStore::new(), "my-local").unwrap();
        assert_eq!(provider.id(), "my-local");
    }

    #[test]
    fn unsupported_provider_without_base_url_is_an_error() {
        let cfg = config("acme", "acme-1");
        let err = match resolve_provider(&cfg, &KeyStore::new(), "acme") {
            Ok(_) => panic!("unknown provider should fail"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("acme"));
        assert!(err.to_string().contains("base_url"));
    }

    fn msg(role: &str, len: usize) -> ChatMessage {
        ChatMessage::new(role, "x".repeat(len))
    }

    #[test]
    fn truncate_is_noop_for_short_histories() {
        let messages = vec![
            ChatMessage::new("system", "sys"),
            ChatMessage::new("user", "hi"),
        ];
        let kept = truncate_messages(&messages, 10);
        assert_eq!(kept.len(), 2);
    }

    #[test]
    fn truncate_keeps_system_and_latest_turns_at_user_boundary() {
        let mut messages = vec![msg("system", 40)];
        for _ in 0..5 {
            messages.push(msg("user", 40));
            messages.push(msg("assistant", 40));
        }
        // 10 non-system messages × 10 estimated tokens = 100 > 90% of 100.
        let kept = truncate_messages(&messages, 100);
        assert!(kept.len() < messages.len(), "something should be dropped");
        assert_eq!(kept[0].role, "system");
        assert_eq!(kept[1].role, "user", "cut lands on a user boundary");
        assert_eq!(kept.last().unwrap().role, "assistant");
        assert_eq!(kept.len(), 9);
    }

    #[test]
    fn truncate_keeps_recent_user_when_suffix_has_none() {
        // [system, user, assistant(tool_use), tool] — the fitting suffix after
        // the budget cut contains no user message, so the cut must fall back
        // to the most recent user instead of orphaning the tool result.
        let mut assistant = msg("assistant", 100);
        assistant.tool_calls = Some(vec![ToolCall {
            id: "call_1".to_string(),
            tool_name: "read".to_string(),
            args: serde_json::json!({"path": "a"}),
            thought_signature: None,
        }]);
        let messages = vec![
            msg("system", 40),
            msg("user", 40),
            assistant,
            msg("tool", 400),
        ];
        let kept = truncate_messages(&messages, 20);
        assert_eq!(kept.len(), messages.len(), "nothing safe to drop");
        assert_eq!(kept[1].role, "user");
    }

    #[test]
    fn truncate_pulls_cut_back_to_last_user_before_overflow() {
        // [system, u1, a1, u2, a2(tool_use), t2, a3] — dropping must keep u2+.
        let mut a2 = msg("assistant", 200);
        a2.tool_calls = Some(vec![ToolCall {
            id: "call_1".to_string(),
            tool_name: "read".to_string(),
            args: serde_json::json!({"path": "a"}),
            thought_signature: None,
        }]);
        let messages = vec![
            msg("system", 4),
            msg("user", 400),
            msg("assistant", 400),
            msg("user", 4),
            a2,
            msg("tool", 40),
            msg("assistant", 40),
        ];
        let kept = truncate_messages(&messages, 100);
        assert_eq!(kept[0].role, "system");
        assert_eq!(kept[1].role, "user");
        assert!(kept.len() < messages.len());
    }

    /// Provider that always fails, with a fixed error kind.
    struct FailingProvider {
        authentication: bool,
    }

    impl FailingProvider {
        fn error(&self) -> ProviderError {
            if self.authentication {
                ProviderError::AuthenticationFailed
            } else {
                ProviderError::Other("boom".to_string())
            }
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for FailingProvider {
        fn id(&self) -> &str {
            "failing"
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
            Err(self.error())
        }

        async fn chat(&self, _req: CompletionRequest) -> Result<ChatResponse, ProviderError> {
            Err(self.error())
        }
    }

    #[test]
    fn default_models_cover_known_providers() {
        assert_eq!(default_model_for("anthropic"), "claude-3-7-sonnet-20250219");
        assert_eq!(default_model_for("gemini"), default_model_for("google"));
        assert_eq!(default_model_for("mystery"), "gpt-4.1");
    }

    #[tokio::test]
    async fn validate_api_key_reports_verified_unverified_and_rejected() {
        // The provider answered: the key is verified.
        let ok: Arc<dyn LlmProvider> = Arc::new(ScriptedProvider::new(vec![text_response("pong")]));
        assert_eq!(
            validate_api_key(&ok, "scripted-1", "sk-test")
                .await
                .unwrap(),
            KeyValidation::Verified
        );

        // Transient failure: reported as unverified, not rejected.
        let unverified: Arc<dyn LlmProvider> = Arc::new(FailingProvider {
            authentication: false,
        });
        match validate_api_key(&unverified, "m", "sk-test").await.unwrap() {
            KeyValidation::Unverified(reason) => assert!(reason.contains("boom")),
            other => panic!("expected Unverified, got {other:?}"),
        }

        // 401/403: the key itself is rejected.
        let rejected: Arc<dyn LlmProvider> = Arc::new(FailingProvider {
            authentication: true,
        });
        let error = validate_api_key(&rejected, "m", "sk-test")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("rejected"));
    }

    #[tokio::test]
    async fn plan_mode_offers_read_only_tools_and_blocks_mutations() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("sessions.db");
        let write_call = ChatResponse {
            content: "Writing the file.".to_string(),
            tool_calls: vec![ToolCall {
                id: "call_1".to_string(),
                tool_name: "write".to_string(),
                args: serde_json::json!({"path": "plan.txt", "content": "nope"}),
                thought_signature: None,
            }],
        };
        let (mut agent, provider) = scripted_agent_with_provider(
            vec![write_call, text_response("Plan: step 1, step 2.")],
            dir.path(),
            &db,
        )
        .await;

        agent.set_mode(AgentMode::Plan);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let turn = agent
            .run_turn(Some("plan the work".to_string()), Some(tx))
            .await
            .unwrap();

        assert_eq!(turn.content, "Plan: step 1, step 2.");
        assert!(
            !dir.path().join("plan.txt").exists(),
            "write must not run in plan mode"
        );

        // Only the read-only allowlist is offered to the model.
        let offered = provider.tools_offered(0);
        for tool in &offered {
            assert!(
                PLAN_MODE_TOOLS.contains(&tool.as_str()),
                "unexpected tool offered in plan mode: {tool}"
            );
        }
        for expected in PLAN_MODE_TOOLS {
            assert!(offered.contains(&expected.to_string()));
        }
        assert!(
            provider
                .system_texts(0)
                .iter()
                .any(|t| t.contains("PLAN MODE")),
            "plan instructions missing from system prompt"
        );

        // Belt and braces: even a requested write is refused.
        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        let (success, output) = events
            .iter()
            .find_map(|event| match event {
                AgentEvent::ToolResult {
                    name,
                    success,
                    output,
                    ..
                } if name == "write" => Some((*success, output.clone())),
                _ => None,
            })
            .expect("write tool result event");
        assert!(!success);
        assert!(output.contains("plan mode"), "{output}");
    }

    #[tokio::test]
    async fn normal_mode_restores_full_tools_and_drops_plan_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("sessions.db");
        let (mut agent, provider) = scripted_agent_with_provider(
            vec![text_response("the plan"), text_response("executed")],
            dir.path(),
            &db,
        )
        .await;

        agent.set_mode(AgentMode::Plan);
        agent
            .run_turn(Some("plan it".to_string()), None)
            .await
            .unwrap();
        agent.set_mode(AgentMode::Normal);
        agent
            .run_turn(Some("do it".to_string()), None)
            .await
            .unwrap();

        assert!(
            provider
                .system_texts(0)
                .iter()
                .any(|t| t.contains("PLAN MODE")),
            "plan turn carries plan instructions"
        );
        assert!(
            !provider
                .system_texts(1)
                .iter()
                .any(|t| t.contains("PLAN MODE")),
            "normal turn must not carry plan instructions"
        );
        let offered = provider.tools_offered(1);
        assert!(offered.contains(&"write".to_string()));
        assert!(offered.contains(&"bash".to_string()));
        assert!(offered.contains(&"task".to_string()));
    }

    #[tokio::test]
    async fn active_skill_is_injected_and_deactivates() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("sessions.db");
        let (mut agent, provider) = scripted_agent_with_provider(
            vec![text_response("one"), text_response("two")],
            dir.path(),
            &db,
        )
        .await;

        agent.activate_skill(Skill {
            name: "reviewer".to_string(),
            description: "review helper".to_string(),
            allowed_tools: vec!["read".to_string()],
            body: "Always check error handling.".to_string(),
            path: PathBuf::new(),
        });

        agent
            .run_turn(Some("first".to_string()), None)
            .await
            .unwrap();
        assert!(
            provider
                .system_texts(0)
                .iter()
                .any(|t| t.contains("Always check error handling.")),
            "active skill body missing from system prompt"
        );

        assert!(agent.deactivate_skill("reviewer"));
        agent
            .run_turn(Some("second".to_string()), None)
            .await
            .unwrap();
        assert!(
            !provider
                .system_texts(1)
                .iter()
                .any(|t| t.contains("Always check error handling.")),
            "deactivated skill must not be injected"
        );
        assert!(!agent.deactivate_skill("reviewer"));
    }

    #[tokio::test]
    async fn task_tool_runs_child_agent_and_forwards_events() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("sessions.db");
        let task_call = ChatResponse {
            content: "Spawning a subagent.".to_string(),
            tool_calls: vec![ToolCall {
                id: "call_task".to_string(),
                tool_name: "task".to_string(),
                args: serde_json::json!({"description": "child run", "prompt": "say hi"}),
                thought_signature: None,
            }],
        };
        // Order matters: parent request, child request, parent request.
        let (mut agent, provider) = scripted_agent_with_provider(
            vec![
                task_call,
                text_response("child says hi"),
                text_response("Parent done."),
            ],
            dir.path(),
            &db,
        )
        .await;

        let (tx, mut rx) = mpsc::unbounded_channel();
        let turn = agent
            .run_turn(Some("delegate".to_string()), Some(tx))
            .await
            .unwrap();
        assert_eq!(turn.content, "Parent done.");

        // Parent offered the task tool; the child did not (no recursion).
        assert!(provider.tools_offered(0).contains(&"task".to_string()));
        assert!(
            !provider.tools_offered(1).contains(&"task".to_string()),
            "child agents must not receive the task tool"
        );

        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        assert!(events.iter().any(|event| {
            matches!(event, AgentEvent::SubagentStarted { name } if name == "child run")
        }));
        assert!(events.iter().any(|event| {
            matches!(event, AgentEvent::SubagentToken { text, .. } if text.contains("child"))
        }));
        assert!(events.iter().any(|event| {
            matches!(event, AgentEvent::SubagentFinished { name, success } if name == "child run" && *success)
        }));
        let task_output = events
            .iter()
            .find_map(|event| match event {
                AgentEvent::ToolResult { name, output, .. } if name == "task" => {
                    Some(output.clone())
                }
                _ => None,
            })
            .expect("task tool result event");
        assert!(task_output.contains("child says hi"), "{task_output}");

        // The child ran in its own session.
        let sessions = agent.memory().list_sessions().await.unwrap();
        assert!(sessions.len() >= 2, "parent + child sessions: {sessions:?}");
    }

    #[tokio::test]
    async fn task_tool_rejects_bad_arguments_and_missing_spec() {
        use crate::subagent::{SubagentHub, TaskTool};
        use shell_tools::Tool;

        let tool = TaskTool::new(std::sync::Arc::new(SubagentHub::new()));
        let ctx = ToolContext::new(std::env::temp_dir());

        let missing_prompt = tool
            .execute(serde_json::json!({"description": "x"}), &ctx)
            .await;
        assert!(!missing_prompt.success);
        assert!(missing_prompt.output.contains("prompt"));

        let no_spec = tool
            .execute(
                serde_json::json!({"description": "x", "prompt": "do it"}),
                &ctx,
            )
            .await;
        assert!(!no_spec.success);
        assert!(no_spec.output.contains("unavailable"));
    }

    #[tokio::test]
    async fn prompt_history_records_user_turns() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("sessions.db");
        let (mut agent, _provider) =
            scripted_agent_with_provider(vec![text_response("hello")], dir.path(), &db).await;

        agent
            .run_turn(Some("first prompt".to_string()), None)
            .await
            .unwrap();

        let history = agent.memory().get_prompt_history(10).await.unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].content, "first prompt");
        assert_eq!(
            history[0].session_id.as_deref(),
            Some(agent.session_id()),
            "prompt is linked to its session"
        );
        assert_eq!(history[0].model.as_deref(), Some("scripted-1"));
    }

    #[tokio::test]
    async fn tool_turns_store_a_memory_that_later_sessions_recall() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("sessions.db");

        // First session does real tool work → a memory is extracted.
        let write_call = ChatResponse {
            content: "Writing the file.".to_string(),
            tool_calls: vec![ToolCall {
                id: "call_1".to_string(),
                tool_name: "write".to_string(),
                args: serde_json::json!({"path": "ritual.txt", "content": "zabriskie"}),
                thought_signature: None,
            }],
        };
        let (mut first, _provider) = scripted_agent_with_provider(
            vec![write_call, text_response("Wrote the ritual file.")],
            dir.path(),
            &db,
        )
        .await;
        first
            .run_turn(
                Some("remember the secret ritual word zabriskie".to_string()),
                None,
            )
            .await
            .unwrap();

        let memories = first.memory().list_memories(10).await.unwrap();
        assert_eq!(memories.len(), 1, "one memory extracted: {memories:?}");
        let memory = &memories[0].content;
        assert!(memory.contains("zabriskie"), "{memory}");
        assert!(memory.contains("Tools: write"), "{memory}");
        assert_eq!(
            memories[0].source_session.as_deref(),
            Some(first.session_id())
        );

        // A second session (same database) recalls it for a similar question.
        let (mut second, provider) =
            scripted_agent_with_provider(vec![text_response("Recalled.")], dir.path(), &db).await;
        second
            .run_turn(
                Some("what was the secret ritual word zabriskie".to_string()),
                None,
            )
            .await
            .unwrap();

        let systems = provider.system_texts(0).join("\n");
        assert!(
            systems.contains("Relevant memories") && systems.contains("zabriskie"),
            "memory not injected: {systems}"
        );

        // The same session does not recall its own memories.
        let (mut third, provider_third) =
            scripted_agent_with_provider(vec![text_response("Continuing.")], dir.path(), &db).await;
        third.resume_session(first.session_id()).await.unwrap();
        third
            .run_turn(Some("zabriskie again?".to_string()), None)
            .await
            .unwrap();
        let systems = provider_third.system_texts(0).join("\n");
        assert!(
            !systems.contains("Relevant memories"),
            "own-session memories must not be recalled: {systems}"
        );
    }

    #[tokio::test]
    async fn fork_session_continues_in_a_copy() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("sessions.db");
        let (mut agent, _provider) = scripted_agent_with_provider(
            vec![text_response("one"), text_response("two")],
            dir.path(),
            &db,
        )
        .await;

        agent
            .run_turn(Some("first".to_string()), None)
            .await
            .unwrap();
        let original = agent.session_id().to_string();
        let count_before = agent.memory().message_count(&original).await.unwrap();
        assert_eq!(count_before, 3, "system + user + assistant");

        let forked = agent.fork_session("branch").await.unwrap();
        assert_ne!(forked, original);
        assert_eq!(agent.session_id(), forked);
        assert_eq!(
            agent
                .memory()
                .get_session(&forked)
                .await
                .unwrap()
                .unwrap()
                .title,
            "branch"
        );
        assert_eq!(
            agent.memory().message_count(&forked).await.unwrap(),
            count_before,
            "fork copies the history"
        );

        // New messages land in the fork only.
        agent
            .run_turn(Some("second".to_string()), None)
            .await
            .unwrap();
        assert_eq!(agent.memory().message_count(&original).await.unwrap(), 3);
        assert_eq!(agent.memory().message_count(&forked).await.unwrap(), 5);
    }

    #[tokio::test]
    async fn replay_tool_call_reexecutes_stored_calls() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("sessions.db");
        let write_call = ChatResponse {
            content: "Writing.".to_string(),
            tool_calls: vec![ToolCall {
                id: "call_replay_1".to_string(),
                tool_name: "write".to_string(),
                args: serde_json::json!({"path": "replayed.txt", "content": "original"}),
                thought_signature: None,
            }],
        };
        let (mut agent, _provider) = scripted_agent_with_provider(
            vec![write_call, text_response("Wrote it.")],
            dir.path(),
            &db,
        )
        .await;
        agent
            .run_turn(Some("write it".to_string()), None)
            .await
            .unwrap();

        let file = dir.path().join("replayed.txt");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "original");
        std::fs::write(&file, "tampered").unwrap();

        // Exact id and unique prefix both resolve; the tool really runs again.
        let output = agent.replay_tool_call("call_replay_1").await.unwrap();
        assert!(output.contains("replayed `write`"), "{output}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "original");

        std::fs::write(&file, "tampered again").unwrap();
        let output = agent.replay_tool_call("last").await.unwrap();
        assert!(output.contains("(call_replay_1)"), "{output}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "original");

        let error = agent.replay_tool_call("nope").await.unwrap_err();
        assert!(
            error.to_string().contains("no tool call matches"),
            "{error}"
        );
    }
}
