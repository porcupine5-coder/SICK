//! TUI application: terminal lifecycle, event loop, input handling, slash
//! commands, and the background agent actor.
//!
//! The [`Agent`] is moved into a background Tokio task (the "actor"). The UI
//! sends it `AgentRequest`s and receives fine-grained [`AgentEvent`]s while
//! a turn runs, so streamed tokens and tool results render as they arrive.

use std::collections::HashMap;
use std::io::Stdout;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Frame;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::style::Style;
use shell_agent_core::{
    Agent, AgentEvent, AgentMode, AgentTurn, KeyValidation, ProviderModels, default_model_for,
    fetch_provider_models, skills, validate_api_key, validation_model,
};
use shell_common::{ShellError, ShellResult};
use shell_key_manager::{
    KeyStatusInfo, add_key_secure, add_key_secure_with_base_url, is_known_provider,
    normalize_provider, remove_key_secure, set_provider_base_url_secure,
};
use shell_llm::{
    ChatMessage, LlmProvider, OpenAiCompatProvider, cloudflare_base_url, known_models,
};
use shell_memory::{MemoryEntry, PromptRecord, SessionMeta};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{mpsc, oneshot};
use tui_textarea::TextArea;

use crate::layout::render_app;
use crate::state::{
    ColorWheelState, InputMode, JsonImportState, KeySetup, ModelOption, ModelsPicker, SetupStage,
    SkillOption, SkillsPicker, SubagentRun, ThemePickerItem, ThemePickerMode, ThemesPicker,
    TuiState, UiMessage, command_help_text,
};
use crate::theme::Theme;

/// Terminal type used by the interactive loop.
pub type TuiTerminal = Terminal<CrosstermBackend<Stdout>>;

/// Maximum tool output shown in the chat pane (full output stays in storage).
const TOOL_OUTPUT_DISPLAY_LIMIT: usize = 4000;

/// Maximum characters streamed into the chat by one `!command` run.
const SHELL_OUTPUT_DISPLAY_LIMIT: usize = 50_000;

/// Wall-clock cap for a single passthrough command.
const SHELL_TIMEOUT: Duration = Duration::from_secs(600);

/// Reply for a completed turn: result plus the (possibly new) session id.
#[derive(Debug)]
struct TurnReply {
    turn: AgentTurn,
    session_id: String,
}

/// Reply for a completed `keys add`: user-facing message + fresh key report.
#[derive(Debug)]
struct KeyAddReply {
    message: String,
    report: Vec<KeyStatusInfo>,
}

/// Reply for removing a saved API key.
#[derive(Debug)]
struct KeyRemoveReply {
    message: String,
    report: Vec<KeyStatusInfo>,
}

/// Combined `/search` results: prompt-history hits + semantic memory hits.
#[derive(Debug)]
struct SearchResults {
    prompts: Vec<PromptRecord>,
    memories: Vec<MemoryEntry>,
}

/// Requests sent from the UI to the agent actor.
enum AgentRequest {
    Turn {
        prompt: String,
        /// Run the turn in plan mode (read-only tools, plan instructions).
        plan: bool,
        done: oneshot::Sender<ShellResult<TurnReply>>,
    },
    Sessions {
        done: oneshot::Sender<ShellResult<Vec<SessionMeta>>>,
    },
    Resume {
        session_id: String,
        done: oneshot::Sender<ShellResult<Vec<UiMessage>>>,
    },
    KeyStatus {
        done: oneshot::Sender<ShellResult<Vec<KeyStatusInfo>>>,
    },
    AddKey {
        provider: String,
        secret: String,
        cloudflare_account_id: Option<String>,
        done: oneshot::Sender<ShellResult<KeyAddReply>>,
    },
    RemoveKey {
        provider: String,
        key_id: String,
        done: oneshot::Sender<ShellResult<KeyRemoveReply>>,
    },
    ConfigureCloudflare {
        account_id: String,
        done: oneshot::Sender<ShellResult<String>>,
    },
    ActivateSkill {
        name: String,
        done: oneshot::Sender<ShellResult<String>>,
    },
    Fork {
        title: String,
        done: oneshot::Sender<ShellResult<String>>,
    },
    History {
        limit: usize,
        done: oneshot::Sender<ShellResult<Vec<PromptRecord>>>,
    },
    RecentHistory {
        done: oneshot::Sender<ShellResult<Vec<PromptRecord>>>,
    },
    Search {
        query: String,
        done: oneshot::Sender<ShellResult<SearchResults>>,
    },
    Replay {
        call_id: String,
        done: oneshot::Sender<ShellResult<String>>,
    },
    SetModel {
        provider: String,
        model: String,
        done: oneshot::Sender<ShellResult<()>>,
    },
    ListModels {
        done: oneshot::Sender<ShellResult<Vec<ProviderModels>>>,
    },
    ConfigInfo {
        done: oneshot::Sender<ShellResult<String>>,
    },
    McpInfo {
        done: oneshot::Sender<ShellResult<String>>,
    },
}

/// Internal UI events: command replies delivered back to the app.
#[derive(Debug)]
enum AppEvent {
    TurnDone(ShellResult<TurnReply>),
    SessionsDone(ShellResult<Vec<SessionMeta>>),
    ResumeDone {
        session_id: String,
        result: ShellResult<Vec<UiMessage>>,
    },
    KeysDone(ShellResult<Vec<KeyStatusInfo>>),
    KeyAdded {
        provider: String,
        result: ShellResult<KeyAddReply>,
    },
    KeyRemoved {
        provider: String,
        key_id: String,
        result: ShellResult<KeyRemoveReply>,
    },
    CloudflareConfigured(ShellResult<String>),
    ModelSet {
        provider: String,
        model: String,
        result: ShellResult<()>,
    },
    ModelsDone(ShellResult<Vec<ProviderModels>>),
    ConfigDone(ShellResult<String>),
    McpDone(ShellResult<String>),
    SkillActivated {
        name: String,
        result: ShellResult<String>,
    },
    Forked(ShellResult<String>),
    HistoryDone(ShellResult<Vec<PromptRecord>>),
    RecentHistoryLoaded(ShellResult<Vec<PromptRecord>>),
    SearchDone {
        query: String,
        result: ShellResult<SearchResults>,
    },
    ReplayDone(ShellResult<String>),
    /// A streamed chunk of `!command` output.
    ShellChunk(String),
    /// The passthrough command finished (`Ok(None)` = terminated).
    ShellFinished {
        result: Result<Option<i32>, String>,
    },
    AgentGone,
}

/// Background task owning the [`Agent`]; serializes all requests.
async fn agent_actor(
    mut agent: Agent,
    mut requests: mpsc::UnboundedReceiver<AgentRequest>,
    events: mpsc::UnboundedSender<AgentEvent>,
) {
    while let Some(request) = requests.recv().await {
        match request {
            AgentRequest::Turn { prompt, plan, done } => {
                agent.set_mode(if plan {
                    AgentMode::Plan
                } else {
                    AgentMode::Normal
                });
                let result = agent.run_turn(Some(prompt), Some(events.clone())).await;
                let result = result.map(|turn| TurnReply {
                    turn,
                    session_id: agent.session_id().to_string(),
                });
                let _ = done.send(result);
            }
            AgentRequest::Sessions { done } => {
                let result = agent
                    .memory()
                    .list_sessions()
                    .await
                    .map_err(|error| ShellError::Memory(error.to_string()));
                let _ = done.send(result);
            }
            AgentRequest::Resume { session_id, done } => {
                let result = match agent.resume_session(&session_id).await {
                    Ok(history) => Ok(history.iter().filter_map(chat_to_ui).collect()),
                    Err(error) => Err(error),
                };
                let _ = done.send(result);
            }
            AgentRequest::KeyStatus { done } => {
                let _ = done.send(Ok(agent.key_manager().get_status_report()));
            }
            AgentRequest::AddKey {
                provider,
                secret,
                cloudflare_account_id,
                done,
            } => {
                let result = add_key_flow(
                    &mut agent,
                    &provider,
                    &secret,
                    cloudflare_account_id.as_deref(),
                )
                .await;
                let _ = done.send(result);
            }
            AgentRequest::RemoveKey {
                provider,
                key_id,
                done,
            } => {
                let result = remove_key_flow(&mut agent, &provider, &key_id);
                let _ = done.send(result);
            }
            AgentRequest::ConfigureCloudflare { account_id, done } => {
                let result = cloudflare_base_url(&account_id)
                    .map_err(|error| ShellError::Config(error.to_string()))
                    .and_then(|base_url| {
                        let save = set_provider_base_url_secure("cloudflare", &base_url, None)
                            .map_err(|error| ShellError::KeyManager(error.to_string()))?;
                        agent
                            .key_manager_mut()
                            .set_provider_base_url("cloudflare", &base_url);
                        if normalize_provider(&agent.config().provider) == "cloudflare" {
                            let model = agent.config().model.clone();
                            agent.set_model("cloudflare", &model)?;
                        }
                        Ok(format!(
                            "Cloudflare account configured and saved encrypted in {} ({})",
                            save.path.display(),
                            save.source.describe()
                        ))
                    });
                let _ = done.send(result);
            }
            AgentRequest::ActivateSkill { name, done } => {
                let result = agent.activate_skill_by_name(&name).map(|skill| {
                    let description = if skill.description.trim().is_empty() {
                        String::new()
                    } else {
                        format!(" — {}", skill.description)
                    };
                    format!(
                        "✓ Skill `{}` activated{description}; its instructions are now injected \
                         into every subsequent turn ({}).",
                        skill.name,
                        skill.path.display()
                    )
                });
                let _ = done.send(result);
            }
            AgentRequest::Fork { title, done } => {
                let result = agent.fork_session(&title).await;
                let _ = done.send(result);
            }
            AgentRequest::History { limit, done } => {
                let result = agent
                    .memory()
                    .get_prompt_history(limit)
                    .await
                    .map_err(|error| ShellError::Memory(error.to_string()));
                let _ = done.send(result);
            }
            AgentRequest::RecentHistory { done } => {
                let result = agent
                    .memory()
                    .get_recent_prompt_history(200, 7)
                    .await
                    .map_err(|error| ShellError::Memory(error.to_string()));
                let _ = done.send(result);
            }
            AgentRequest::Search { query, done } => {
                let result = search_all(&agent, &query).await;
                let _ = done.send(result);
            }
            AgentRequest::Replay { call_id, done } => {
                let result = agent.replay_tool_call(&call_id).await;
                let _ = done.send(result);
            }
            AgentRequest::SetModel {
                provider,
                model,
                done,
            } => {
                let _ = done.send(agent.set_model(&provider, &model));
            }
            AgentRequest::ListModels { done } => {
                // Fetch off the actor loop so picker actions (Enter to
                // switch) are not blocked behind slow network calls.
                let targets = agent.model_lookup_targets();
                tokio::spawn(async move {
                    let _ = done.send(Ok(fetch_provider_models(targets).await));
                });
            }
            AgentRequest::ConfigInfo { done } => {
                let _ = done.send(Ok(config_text(&agent)));
            }
            AgentRequest::McpInfo { done } => {
                let _ = done.send(Ok(mcp_text(&agent)));
            }
        }
    }
}

/// Convert a resumed chat message into a displayable one (`system` is skipped).
fn chat_to_ui(message: &ChatMessage) -> Option<UiMessage> {
    match message.role.as_str() {
        "system" => None,
        "assistant" => {
            let content = message.content.clone().unwrap_or_default();
            match &message.tool_calls {
                Some(calls) if !calls.is_empty() => {
                    let names: Vec<&str> =
                        calls.iter().map(|call| call.tool_name.as_str()).collect();
                    let marker = format!("[tool calls: {}]", names.join(", "));
                    let content = if content.is_empty() {
                        marker
                    } else {
                        format!("{content}\n{marker}")
                    };
                    Some(UiMessage::new("assistant", content))
                }
                _ if content.is_empty() => None,
                _ => Some(UiMessage::new("assistant", content)),
            }
        }
        "tool" => Some(UiMessage::tool(
            "tool_result",
            "tool",
            message.content.clone().unwrap_or_default(),
        )),
        role => Some(UiMessage::new(
            role,
            message.content.clone().unwrap_or_default(),
        )),
    }
}

/// Human-readable configuration summary (`/config`).
fn config_text(agent: &Agent) -> String {
    let config = agent.config();
    let mut text = format!(
        "Configuration:\n  provider: {}\n  model: {}\n  temperature: {}\n  max_tokens: {}\n  workspace: {}",
        config.provider,
        config.model,
        config.temperature,
        if config.max_tokens == 0 {
            "provider default".to_string()
        } else {
            config.max_tokens.to_string()
        },
        config.workspace.display()
    );
    let fallbacks: Vec<String> = config
        .fallback_chain
        .iter()
        .map(|(provider, model)| format!("{provider}:{model}"))
        .collect();
    if !fallbacks.is_empty() {
        text.push_str(&format!("\n  fallback chain: {}", fallbacks.join(" → ")));
    }
    if !config.base_url.is_empty() {
        text.push_str(&format!("\n  base_url: {}", config.base_url));
    }
    if !agent.session_id().is_empty() {
        text.push_str(&format!("\n  session: {}", agent.session_id()));
    }
    text
}

/// Human-readable MCP server summary (`/mcp`).
fn mcp_text(agent: &Agent) -> String {
    let statuses = agent.mcp_status();
    if statuses.is_empty() {
        return "MCP: no servers configured for this session.\n  Add [[mcp.servers]] entries to the config file \
                (see docs/advanced.md) and restart the TUI."
            .to_string();
    }
    let mut text = String::from("MCP servers:");
    for status in statuses {
        if status.ready {
            text.push_str(&format!(
                "\n  {}: ready ({} tool(s))",
                status.name, status.tool_count
            ));
        } else {
            text.push_str(&format!("\n  {}: {}", status.name, status.state));
        }
        if let Some(manager) = agent.mcp() {
            let tools: Vec<String> = manager
                .tool_infos()
                .into_iter()
                .filter(|info| info.server == status.name)
                .map(|info| info.exposed_name)
                .collect();
            if !tools.is_empty() {
                text.push_str(&format!("\n      {}", tools.join(", ")));
            }
        }
    }
    text
}

/// Validate a pasted key, save it to the keys file, and register it with the
/// running agent so it is usable immediately.
async fn add_key_flow(
    agent: &mut Agent,
    provider_id: &str,
    secret: &str,
    cloudflare_account_id: Option<&str>,
) -> ShellResult<KeyAddReply> {
    let provider_id = normalize_provider(provider_id);
    let account_base_url = cloudflare_account_id
        .map(cloudflare_base_url)
        .transpose()
        .map_err(|error| ShellError::Config(error.to_string()))?;
    let model = if normalize_provider(&agent.config().provider) == provider_id {
        agent.config().model.clone()
    } else {
        validation_model(agent.key_manager(), &provider_id)
    };
    let validation = if let Some(base_url) = &account_base_url {
        let provider: Arc<dyn LlmProvider> =
            Arc::new(OpenAiCompatProvider::with_name(&provider_id, base_url));
        validate_api_key(&provider, &model, secret).await?
    } else {
        match agent.get_provider(&provider_id) {
            Ok(provider) => validate_api_key(&provider, &model, secret).await?,
            Err(error) if provider_id == "cloudflare" => {
                KeyValidation::Unverified(error.to_string())
            }
            Err(error) => return Err(error),
        }
    };

    // The encrypted store is the source of record: load it, add the key, and
    // save everything back (migrating plaintext legacy files when present).
    let report = if let Some(base_url) = &account_base_url {
        add_key_secure_with_base_url(&provider_id, secret, Some(base_url), None)
    } else {
        add_key_secure(&provider_id, secret, None)
    }
    .map_err(|error| ShellError::KeyManager(error.to_string()))?;
    agent.key_manager_mut().add_key(&provider_id, secret);
    if let Some(base_url) = account_base_url {
        agent
            .key_manager_mut()
            .set_provider_base_url(&provider_id, base_url);
        if normalize_provider(&agent.config().provider) == provider_id {
            let current_model = agent.config().model.clone();
            agent.set_model(&provider_id, &current_model)?;
        }
    }

    let location = format!(
        "{} ({})",
        report.save.path.display(),
        report.save.source.describe()
    );
    let mut message = match (validation, report.added) {
        (KeyValidation::Verified, true) => {
            format!("✓ {provider_id} key saved (encrypted) and verified — {location}")
        }
        (KeyValidation::Verified, false) => {
            format!("✓ {provider_id} key is already saved in {location}")
        }
        (KeyValidation::Unverified(reason), true) => format!(
            "✓ {provider_id} key saved (encrypted, could not verify: {reason}) — {location}"
        ),
        (KeyValidation::Unverified(reason), false) => format!(
            "✓ {provider_id} key is already saved (could not verify: {reason}) — {location}"
        ),
    };
    if provider_id == "cloudflare" && cloudflare_account_id.is_none() {
        message.push_str(
            "\nCloudflare needs an account ID (not the API token ID). Set \
             CLOUDFLARE_ACCOUNT_ID or add the key again to save the account ID.",
        );
    }
    for migrated in &report.save.migrated {
        message.push_str(&format!(
            "\n  plaintext key file migrated to {}",
            migrated.display()
        ));
    }
    Ok(KeyAddReply {
        message,
        report: agent.key_manager().get_status_report(),
    })
}

fn remove_key_flow(agent: &mut Agent, provider: &str, key_id: &str) -> ShellResult<KeyRemoveReply> {
    let provider = normalize_provider(provider);
    let report = remove_key_secure(&provider, key_id, None)
        .map_err(|error| ShellError::KeyManager(error.to_string()))?;
    agent.key_manager_mut().remove_key(&provider, key_id);

    Ok(KeyRemoveReply {
        message: format!(
            "Removed saved {provider} key {key_id}… ({} saved key(s) remain).",
            report.remaining_for_provider
        ),
        report: agent.key_manager().get_status_report(),
    })
}

/// `/search`: prompt-history substring hits + semantic memory matches.
async fn search_all(agent: &Agent, query: &str) -> ShellResult<SearchResults> {
    let memory_error = |error: shell_memory::MemoryError| ShellError::Memory(error.to_string());

    let prompts = agent
        .memory()
        .search_prompts(query, 10)
        .await
        .map_err(memory_error)?;

    let embedding = shell_memory::embed::embed(query);
    let mut memories = agent
        .memory()
        .search_memories(&embedding, 5)
        .await
        .map_err(memory_error)?;
    memories.retain(|entry| entry.score.unwrap_or(0.0) >= 0.30);
    if memories.is_empty() {
        memories = agent
            .memory()
            .search_memories_text(query, 5)
            .await
            .map_err(memory_error)?;
    }

    Ok(SearchResults { prompts, memories })
}

/// Collapse whitespace so multi-line content fits one history/search line.
fn one_line(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        flat.chars().take(max).collect::<String>() + "…"
    }
}

/// Run a passthrough command, forwarding stdout/stderr lines to `ui_tx` as
/// they arrive. Returns the exit code (`Ok(None)` = terminated, e.g. signal).
async fn run_shell_streaming(
    command: &str,
    workspace: &std::path::Path,
    ui_tx: mpsc::UnboundedSender<AppEvent>,
    use_kali: bool,
) -> Result<Option<i32>, String> {
    let mut command = if use_kali {
        shell_shell::kali_command(command, workspace)
    } else {
        shell_shell::command(command, workspace)
    };
    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to start command: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "missing stdout pipe".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "missing stderr pipe".to_string())?;

    let inner = async {
        let mut stdout_lines = BufReader::new(stdout).lines();
        let mut stderr_lines = BufReader::new(stderr).lines();
        let mut stdout_done = false;
        let mut stderr_done = false;
        let mut forwarded = 0usize;
        let mut truncated = false;

        while !(stdout_done && stderr_done) {
            tokio::select! {
                result = stdout_lines.next_line(), if !stdout_done => match result {
                    Ok(Some(line)) => forward_chunk(&ui_tx, &line, &mut forwarded, &mut truncated),
                    Ok(None) => stdout_done = true,
                    Err(error) => {
                        let _ = ui_tx.send(AppEvent::ShellChunk(format!("[stdout read error: {error}]\n")));
                        stdout_done = true;
                    }
                },
                result = stderr_lines.next_line(), if !stderr_done => match result {
                    Ok(Some(line)) => forward_chunk(&ui_tx, &line, &mut forwarded, &mut truncated),
                    Ok(None) => stderr_done = true,
                    Err(error) => {
                        let _ = ui_tx.send(AppEvent::ShellChunk(format!("[stderr read error: {error}]\n")));
                        stderr_done = true;
                    }
                },
            }
        }

        child
            .wait()
            .await
            .map(|status| status.code())
            .map_err(|error| error.to_string())
    };

    match tokio::time::timeout(SHELL_TIMEOUT, inner).await {
        Ok(result) => result,
        Err(_) => Err(format!(
            "command timed out after {}s",
            SHELL_TIMEOUT.as_secs()
        )),
    }
}

/// Forward one output line unless the display cap is reached.
fn forward_chunk(
    ui_tx: &mpsc::UnboundedSender<AppEvent>,
    line: &str,
    forwarded: &mut usize,
    truncated: &mut bool,
) {
    if *truncated {
        return;
    }
    let chunk = format!("{line}\n");
    *forwarded += chunk.chars().count();
    if *forwarded > SHELL_OUTPUT_DISPLAY_LIMIT {
        let _ = ui_tx.send(AppEvent::ShellChunk("\n… output truncated\n".to_string()));
        *truncated = true;
        return;
    }
    let _ = ui_tx.send(AppEvent::ShellChunk(chunk));
}

/// RAII guard: raw mode + alternate screen, restored on drop.
struct TerminalGuard {
    terminal: TuiTerminal,
    active: bool,
}

impl TerminalGuard {
    fn enter() -> ShellResult<Self> {
        enable_raw_mode()?;
        let mut stdout = std::io::stdout();
        execute!(
            stdout,
            EnterAlternateScreen,
            EnableBracketedPaste,
            EnableMouseCapture
        )?;
        let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        Ok(Self {
            terminal,
            active: true,
        })
    }

    fn leave(&mut self) {
        if self.active {
            let _ = disable_raw_mode();
            let _ = execute!(self.terminal.backend_mut(), DisableMouseCapture);
            let _ = execute!(self.terminal.backend_mut(), DisableBracketedPaste);
            let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
            let _ = self.terminal.show_cursor();
            self.active = false;
        }
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        self.leave();
    }
}

/// The interactive application.
pub struct App {
    /// Rendered state.
    pub state: TuiState,
    /// Multiline input editor.
    pub textarea: TextArea<'static>,
    requests: mpsc::UnboundedSender<AgentRequest>,
    events: mpsc::UnboundedReceiver<AgentEvent>,
    ui_tx: mpsc::UnboundedSender<AppEvent>,
    ui_events: mpsc::UnboundedReceiver<AppEvent>,
    /// Index of the assistant message currently receiving streamed tokens.
    streaming_msg: Option<usize>,
    /// Index of the `shell` message receiving passthrough output.
    shell_output: Option<usize>,
    /// Indices of per-subagent streaming messages.
    subagent_streams: HashMap<String, usize>,
    /// Workspace directory for shell passthrough commands.
    workspace: PathBuf,
}

impl App {
    /// Create the app and spawn the agent actor task.
    ///
    /// Must be called from within a Tokio runtime.
    pub fn new(agent: Agent) -> Self {
        let (requests_tx, requests_rx) = mpsc::unbounded_channel();
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let (ui_tx, ui_rx) = mpsc::unbounded_channel();

        let provider = agent.config().provider.clone();
        let model = agent.config().model.clone();
        let workspace = agent.config().workspace.clone();
        let mut state = TuiState::new(provider, model);
        let mut theme_load_error = None;
        match Theme::load_saved() {
            Ok((active, custom)) => {
                state.theme = active;
                state.themes.extend(custom);
            }
            Err(error) => theme_load_error = Some(error),
        }
        state.key_status = agent.key_manager().get_status_report();
        if state.key_status.is_empty() {
            state.add_message(
                "system",
                "No API keys are configured yet — pick a provider and paste a key to get \
                 started (Esc skips setup; /keys add re-opens it later).",
            );
            state.key_setup = Some(KeySetup::new());
        } else {
            state.add_message(
                "system",
                "Welcome to S.I.C.K. Type a message and press Enter. /help lists commands.",
            );
        }
        if let Some(error) = theme_load_error {
            state.add_message("error", format!("theme preferences unavailable: {error}"));
        }

        let mut app = Self {
            state,
            textarea: TextArea::default(),
            requests: requests_tx,
            events: events_rx,
            ui_tx,
            ui_events: ui_rx,
            streaming_msg: None,
            shell_output: None,
            subagent_streams: HashMap::new(),
            workspace,
        };
        app.apply_textarea_styles();
        app.dispatch(
            |done| AgentRequest::RecentHistory { done },
            AppEvent::RecentHistoryLoaded,
        );

        tokio::spawn(agent_actor(agent, requests_rx, events_tx));
        app
    }

    /// Immediately load a stored session (used by `sick --resume <id>`).
    pub fn resume_on_startup(&mut self, session_id: String) {
        let request_id = session_id.clone();
        self.dispatch(
            move |done| AgentRequest::Resume {
                session_id: request_id,
                done,
            },
            move |result| AppEvent::ResumeDone { session_id, result },
        );
    }

    /// Run the interactive loop on the real terminal (alternate screen).
    pub async fn run(&mut self) -> ShellResult<()> {
        let mut guard = TerminalGuard::enter()?;
        self.event_loop(&mut guard.terminal).await
    }

    async fn event_loop(&mut self, terminal: &mut TuiTerminal) -> ShellResult<()> {
        loop {
            self.poll_events();
            terminal.draw(|frame| self.render(frame))?;
            if self.state.should_quit {
                break;
            }
            if event::poll(Duration::from_millis(50))? {
                match event::read()? {
                    Event::Key(key)
                        if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
                    {
                        self.handle_key(key);
                    }
                    Event::Mouse(mouse) => self.handle_mouse(mouse),
                    Event::Paste(text) => self.handle_paste(text),
                    _ => {}
                }
            }
        }
        Ok(())
    }

    /// Draw one frame.
    pub fn render(&mut self, frame: &mut Frame) {
        render_app(frame, &mut self.state, &mut self.textarea);
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        match mouse.kind {
            MouseEventKind::ScrollUp => self.state.scroll_by(-3),
            MouseEventKind::ScrollDown => self.state.scroll_by(3),
            _ => {}
        }
    }

    /// Whether a turn is currently in flight.
    pub fn is_busy(&self) -> bool {
        self.state.busy
    }

    /// Current textarea contents (lines joined by `\n`).
    pub fn input_text(&self) -> String {
        self.textarea.lines().join("\n")
    }

    /// Drain pending streaming events and command replies into the state.
    pub fn poll_events(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            self.apply_agent_event(event);
        }
        while let Ok(event) = self.ui_events.try_recv() {
            self.apply_ui_event(event);
        }
    }

    fn apply_agent_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::Token(text) => {
                let index = match self.streaming_msg {
                    Some(index) if index < self.state.messages.len() => index,
                    _ => {
                        self.state
                            .messages
                            .push(UiMessage::new("assistant", String::new()));
                        let index = self.state.messages.len() - 1;
                        self.streaming_msg = Some(index);
                        index
                    }
                };
                self.state.messages[index].content.push_str(&text);
                self.state.stick_to_bottom = true;
            }
            AgentEvent::ToolCall { name, args } => {
                self.streaming_msg = None;
                self.state.messages.push(UiMessage::tool(
                    "tool_call",
                    name.clone(),
                    format!("{name} {args}"),
                ));
                self.state.stick_to_bottom = true;
            }
            AgentEvent::ToolResult {
                name,
                success,
                output,
                duration_ms,
            } => {
                let status = if success { "ok" } else { "failed" };
                let mut content = format!("[{status} in {duration_ms}ms]\n{output}");
                if content.chars().count() > TOOL_OUTPUT_DISPLAY_LIMIT {
                    content = content.chars().take(TOOL_OUTPUT_DISPLAY_LIMIT).collect();
                    content.push_str("\n… (truncated)");
                }
                self.state
                    .messages
                    .push(UiMessage::tool("tool_result", name, content));
                self.state.stick_to_bottom = true;
            }
            AgentEvent::SubagentStarted { name } => {
                self.state.subagents.push(SubagentRun {
                    name: name.clone(),
                    status: "running",
                });
                self.state.messages.push(UiMessage::new(
                    "subagent",
                    format!("▶ subagent `{name}` started"),
                ));
                self.state.stick_to_bottom = true;
            }
            AgentEvent::SubagentToken { name, text } => {
                let index = match self.subagent_streams.get(&name).copied() {
                    Some(index) if index < self.state.messages.len() => index,
                    _ => {
                        self.state
                            .messages
                            .push(UiMessage::new("subagent", String::new()));
                        let index = self.state.messages.len() - 1;
                        self.subagent_streams.insert(name, index);
                        index
                    }
                };
                self.state.messages[index].content.push_str(&text);
                self.state.stick_to_bottom = true;
            }
            AgentEvent::SubagentToolCall { name, tool, args } => {
                self.state.messages.push(UiMessage::new(
                    "subagent",
                    format!("[{name}] tool: {tool} {args}"),
                ));
                self.state.stick_to_bottom = true;
            }
            AgentEvent::SubagentFinished { name, success } => {
                if let Some(run) = self
                    .state
                    .subagents
                    .iter_mut()
                    .find(|run| run.name == name && run.status == "running")
                {
                    run.status = if success { "done" } else { "failed" };
                }
                self.subagent_streams.remove(&name);
                let marker = if success { "■" } else { "✗" };
                let verb = if success { "finished" } else { "failed" };
                self.state.messages.push(UiMessage::new(
                    "subagent",
                    format!("{marker} subagent `{name}` {verb}"),
                ));
                self.state.stick_to_bottom = true;
            }
        }
    }

    fn apply_ui_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::TurnDone(Ok(reply)) => {
                self.state.busy = false;
                self.state.busy_since = None;
                self.streaming_msg = None;
                self.state.session_id = Some(reply.session_id);
                if !reply.turn.content.trim().is_empty() {
                    let already_shown = self.state.messages.iter().rev().take(4).any(|message| {
                        message.role == "assistant" && message.content == reply.turn.content
                    });
                    if !already_shown {
                        self.state
                            .add_message("assistant", reply.turn.content.clone());
                    }
                }
                if std::mem::take(&mut self.state.plan_pending) {
                    if reply.turn.content.trim().is_empty() {
                        self.state.add_message(
                            "system",
                            "The plan turn produced no text — try /plan again.",
                        );
                    } else {
                        self.state.plan_review = Some(reply.turn.content);
                    }
                }
            }
            AppEvent::TurnDone(Err(error)) => {
                self.state.busy = false;
                self.state.busy_since = None;
                self.streaming_msg = None;
                self.state.plan_pending = false;
                self.state.add_message("error", format!("error: {error}"));
            }
            AppEvent::SessionsDone(Ok(sessions)) => {
                if sessions.is_empty() {
                    self.state.add_message("system", "No saved sessions.");
                } else {
                    let mut text = format!("Saved sessions ({}):", sessions.len());
                    for session in sessions.iter().take(50) {
                        text.push_str(&format!(
                            "\n  {} · {} · {} · {}",
                            session.id,
                            session.updated_at,
                            session.model.clone().unwrap_or_else(|| "-".to_string()),
                            session.title,
                        ));
                    }
                    self.state.add_message("system", text);
                }
            }
            AppEvent::SessionsDone(Err(error)) => {
                self.state
                    .add_message("error", format!("could not list sessions: {error}"));
            }
            AppEvent::ResumeDone {
                session_id,
                result: Ok(messages),
            } => {
                if messages.is_empty() {
                    self.state.add_message(
                        "error",
                        format!("Session {session_id} has no displayable messages."),
                    );
                } else {
                    self.state.messages.clear();
                    self.streaming_msg = None;
                    self.state.messages.extend(messages);
                    self.state.session_id = Some(session_id.clone());
                    self.state.add_message(
                        "system",
                        format!("Resumed session {session_id} — keep typing to continue it."),
                    );
                }
            }
            AppEvent::ResumeDone {
                session_id,
                result: Err(error),
            } => {
                self.state
                    .add_message("error", format!("could not resume {session_id}: {error}"));
            }
            AppEvent::KeysDone(Ok(report)) => {
                self.state.key_status = report.clone();
                if report.is_empty() {
                    self.state.add_message("system", "No API keys configured.");
                } else {
                    let mut text = format!("API keys ({}):", report.len());
                    for info in &report {
                        text.push_str(&format!(
                            "\n  {} · {} · [{}]",
                            info.provider, info.key_id, info.status
                        ));
                        if let Some(reset) = info.reset_in() {
                            text.push_str(&format!(" · cooldown {}s", reset.as_secs()));
                        }
                    }
                    text.push_str("\nRemove a saved key with /keysrm <provider> <key-id>.");
                    self.state.add_message("system", text);
                }
            }
            AppEvent::KeysDone(Err(error)) => {
                self.state
                    .add_message("error", format!("could not read key status: {error}"));
            }
            AppEvent::KeyAdded {
                result: Ok(reply), ..
            } => {
                self.state.key_status = reply.report;
                self.state.key_setup = None;
                self.state.add_message("system", reply.message);
            }
            AppEvent::KeyAdded {
                provider,
                result: Err(error),
            } => {
                if let Some(setup) = self.state.key_setup.as_mut() {
                    setup.stage = SetupStage::EnterKey;
                    setup.status = Some(format!(
                        "{error} — fix the key and press Enter again (Esc cancels)"
                    ));
                } else {
                    self.state
                        .add_message("error", format!("could not add {provider} key: {error}"));
                }
            }
            AppEvent::KeyRemoved {
                result: Ok(reply), ..
            } => {
                self.state.key_status = reply.report;
                self.state.add_message("system", reply.message);
            }
            AppEvent::KeyRemoved {
                provider,
                key_id,
                result: Err(error),
            } => self.state.add_message(
                "error",
                format!("could not remove saved {provider} key {key_id}: {error}"),
            ),
            AppEvent::CloudflareConfigured(Ok(message)) => {
                self.state.add_message("system", message)
            }
            AppEvent::CloudflareConfigured(Err(error)) => self.state.add_message(
                "error",
                format!("could not configure Cloudflare account: {error}"),
            ),
            AppEvent::ModelSet {
                provider,
                model,
                result: Ok(()),
            } => {
                self.state.current_provider = provider.clone();
                self.state.current_model = model.clone();
                self.state
                    .add_message("system", format!("Switched to {provider}:{model}"));
            }
            AppEvent::ModelSet {
                provider,
                model,
                result: Err(error),
            } => {
                self.state.add_message(
                    "error",
                    format!("could not switch to {provider}:{model}: {error}"),
                );
            }
            AppEvent::ModelsDone(Ok(report)) => self.merge_live_models(report),
            AppEvent::ModelsDone(Err(error)) => {
                if let Some(picker) = self.state.models_picker.as_mut() {
                    picker.loading = false;
                    picker.status = Some(format!("model lookup failed: {error}"));
                }
            }
            AppEvent::ConfigDone(Ok(text)) => {
                self.state.add_message("system", text);
            }
            AppEvent::ConfigDone(Err(error)) => {
                self.state
                    .add_message("error", format!("could not read config: {error}"));
            }
            AppEvent::McpDone(Ok(text)) => {
                self.state.add_message("system", text);
            }
            AppEvent::McpDone(Err(error)) => {
                self.state
                    .add_message("error", format!("could not read MCP status: {error}"));
            }
            AppEvent::SkillActivated {
                result: Ok(message),
                ..
            } => {
                self.state.add_message("system", message);
            }
            AppEvent::SkillActivated {
                name,
                result: Err(error),
            } => {
                self.state
                    .add_message("error", format!("could not load skill `{name}`: {error}"));
            }
            AppEvent::Forked(Ok(session_id)) => {
                self.state.session_id = Some(session_id.clone());
                self.state.add_message(
                    "system",
                    format!("✓ Forked — history copied to session {session_id}; continuing there."),
                );
            }
            AppEvent::Forked(Err(error)) => {
                self.state
                    .add_message("error", format!("fork failed: {error}"));
            }
            AppEvent::HistoryDone(Ok(history)) => {
                if history.is_empty() {
                    self.state.add_message("system", "No prompt history yet.");
                } else {
                    let mut text = format!("Recent prompts ({}):", history.len());
                    for prompt in &history {
                        text.push_str(&format!(
                            "\n  [{}] {}",
                            prompt.timestamp,
                            one_line(&prompt.content, 100)
                        ));
                    }
                    self.state.add_message("system", text);
                }
            }
            AppEvent::HistoryDone(Err(error)) => {
                self.state
                    .add_message("error", format!("could not read prompt history: {error}"));
            }
            AppEvent::RecentHistoryLoaded(Ok(history)) => {
                self.state
                    .extend_history(history.into_iter().rev().map(|prompt| prompt.content));
            }
            AppEvent::RecentHistoryLoaded(Err(error)) => {
                self.state.add_message(
                    "error",
                    format!("could not preload prompt history: {error}"),
                );
            }
            AppEvent::SearchDone {
                query,
                result: Ok(results),
            } => {
                let mut text = format!(
                    "Search `{query}` — {} prompt(s), {} memory(ies):",
                    results.prompts.len(),
                    results.memories.len()
                );
                for prompt in &results.prompts {
                    text.push_str(&format!(
                        "\n  prompt [{}] {}",
                        prompt.timestamp,
                        one_line(&prompt.content, 100)
                    ));
                }
                for memory in &results.memories {
                    let score = memory
                        .score
                        .map(|value| format!("{value:.2}"))
                        .unwrap_or_else(|| "-".to_string());
                    text.push_str(&format!(
                        "\n  memory ({score}) {}",
                        one_line(&memory.content, 120)
                    ));
                }
                if results.prompts.is_empty() && results.memories.is_empty() {
                    text.push_str("\n  (nothing found)");
                }
                self.state.add_message("system", text);
            }
            AppEvent::SearchDone {
                result: Err(error), ..
            } => {
                self.state
                    .add_message("error", format!("search failed: {error}"));
            }
            AppEvent::ReplayDone(Ok(output)) => {
                self.state
                    .messages
                    .push(UiMessage::tool("tool_result", "replay", output));
                self.state.stick_to_bottom = true;
            }
            AppEvent::ReplayDone(Err(error)) => {
                self.state
                    .add_message("error", format!("replay failed: {error}"));
            }
            AppEvent::ShellChunk(text) => {
                if let Some(index) = self.shell_output
                    && index < self.state.messages.len()
                {
                    self.state.messages[index].content.push_str(&text);
                    self.state.stick_to_bottom = true;
                }
            }
            AppEvent::ShellFinished { result } => {
                self.state.busy = false;
                self.state.busy_since = None;
                let suffix = match result {
                    Ok(Some(code)) => format!("[exit code: {code}]"),
                    Ok(None) => "[terminated]".to_string(),
                    Err(error) => format!("[error: {error}]"),
                };
                match self.shell_output.take() {
                    Some(index) if index < self.state.messages.len() => {
                        let message = &mut self.state.messages[index];
                        if !message.content.is_empty() && !message.content.ends_with('\n') {
                            message.content.push('\n');
                        }
                        message.content.push_str(&suffix);
                    }
                    _ => self.state.add_message("shell", suffix),
                }
                self.state.stick_to_bottom = true;
            }
            AppEvent::AgentGone => {
                self.state.busy = false;
                self.state.busy_since = None;
                self.state
                    .add_message("error", "agent task stopped unexpectedly");
            }
        }
    }

    /// Send a request to the actor and route its reply into `AppEvent`.
    fn dispatch<T, F>(
        &self,
        request: impl FnOnce(oneshot::Sender<ShellResult<T>>) -> AgentRequest,
        wrap: F,
    ) where
        T: Send + 'static,
        F: FnOnce(ShellResult<T>) -> AppEvent + Send + 'static,
    {
        let (done, reply) = oneshot::channel();
        if self.requests.send(request(done)).is_err() {
            let _ = self.ui_tx.send(AppEvent::AgentGone);
            return;
        }
        let ui_tx = self.ui_tx.clone();
        tokio::spawn(async move {
            let result = match reply.await {
                Ok(result) => result,
                Err(_) => Err(ShellError::Other("agent task stopped".to_string())),
            };
            let _ = ui_tx.send(wrap(result));
        });
    }

    fn request_key_removal(&mut self, provider: &str, key_id: &str) {
        let provider = provider.trim();
        let key_id = key_id.trim();
        if provider.is_empty() || key_id.is_empty() {
            self.state.add_message(
                "system",
                "Usage: /keysrm <provider> <key-id> — use /keys to see provider names and key ids. \
                 Environment-variable keys cannot be removed here; clear their environment variable.",
            );
            return;
        }
        let provider = normalize_provider(provider);
        let key_id = key_id.to_string();
        let req_provider = provider.clone();
        let req_key_id = key_id.clone();
        self.dispatch(
            move |done| AgentRequest::RemoveKey {
                provider: req_provider,
                key_id: req_key_id,
                done,
            },
            move |result| AppEvent::KeyRemoved {
                provider,
                key_id,
                result,
            },
        );
    }

    /// Handle one key event. Returns `state.should_quit`.
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        if key.kind == KeyEventKind::Release {
            return self.state.should_quit;
        }

        // Ctrl+C quits, except in command mode where it cancels the command
        // (phase-3 spec: "Ctrl+C / Esc → if in command mode, cancel; else quit").
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            if self.state.input_mode == InputMode::Command {
                self.clear_input();
            } else {
                self.state.should_quit = true;
            }
            return self.state.should_quit;
        }

        // The key-setup wizard captures all other input while it is open.
        if self.state.key_setup.is_some() {
            self.handle_setup_key(key);
            return self.state.should_quit;
        }

        // The plan review (a/e/c) is modal too.
        if self.state.plan_review.is_some() {
            self.handle_plan_review_key(key);
            return self.state.should_quit;
        }

        // The models picker is modal too.
        if self.state.models_picker.is_some() {
            self.handle_models_picker_key(key);
            return self.state.should_quit;
        }

        if self.state.themes_picker.is_some() {
            self.handle_themes_picker_key(key);
            return self.state.should_quit;
        }

        if self.state.skills_picker.is_some() {
            self.handle_skills_picker_key(key);
            return self.state.should_quit;
        }

        // Ctrl+R opens the history recall overlay.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('r') {
            if !self.state.history.is_empty() {
                self.state.show_history = true;
                self.state.history_selection = self.state.history.len() - 1;
            }
            return self.state.should_quit;
        }

        // History overlay keys take precedence while it is open.
        if self.state.show_history {
            match key.code {
                KeyCode::Up => {
                    self.state.history_selection = self.state.history_selection.saturating_sub(1);
                }
                KeyCode::Down => {
                    let max = self.state.history.len().saturating_sub(1);
                    self.state.history_selection = (self.state.history_selection + 1).min(max);
                }
                KeyCode::Enter => {
                    if let Some(selected) = self
                        .state
                        .history
                        .get(self.state.history_selection)
                        .cloned()
                    {
                        self.set_input(&selected);
                    }
                    self.state.show_history = false;
                }
                KeyCode::Esc => {
                    self.state.show_history = false;
                }
                _ => {}
            }
            return self.state.should_quit;
        }

        // Esc cancels command mode, otherwise quits.
        if key.code == KeyCode::Esc {
            if self.state.input_mode == InputMode::Command {
                self.clear_input();
            } else {
                self.state.should_quit = true;
            }
            return self.state.should_quit;
        }

        // In command mode, Up/Down move the command-palette highlight.
        if self.state.input_mode == InputMode::Command
            && matches!(key.code, KeyCode::Up | KeyCode::Down)
            && key.modifiers.is_empty()
            && !self.state.matching_commands().is_empty()
        {
            self.state
                .move_command_selection(if key.code == KeyCode::Up { -1 } else { 1 });
            return self.state.should_quit;
        }

        // Keyboard scrolling shortcuts for chat:
        // Shift+Up / Ctrl+Up / Alt+Up: scroll chat up
        // Shift+Down / Ctrl+Down / Alt+Down: scroll chat down
        if (key.modifiers.contains(KeyModifiers::SHIFT)
            || key.modifiers.contains(KeyModifiers::CONTROL)
            || key.modifiers.contains(KeyModifiers::ALT))
            && key.code == KeyCode::Up
        {
            self.state.scroll_by(-3);
            return self.state.should_quit;
        }
        if (key.modifiers.contains(KeyModifiers::SHIFT)
            || key.modifiers.contains(KeyModifiers::CONTROL)
            || key.modifiers.contains(KeyModifiers::ALT))
            && key.code == KeyCode::Down
        {
            self.state.scroll_by(3);
            return self.state.should_quit;
        }

        // Scrolling with PageUp, PageDown, Home, End.
        if key.code == KeyCode::Home && !key.modifiers.contains(KeyModifiers::CONTROL) {
            if self.input_text().trim().is_empty() {
                self.state.scroll_to_top();
                return self.state.should_quit;
            }
        }
        match key.code {
            KeyCode::PageUp => {
                self.state.scroll_by(-10);
                return self.state.should_quit;
            }
            KeyCode::PageDown => {
                self.state.scroll_by(10);
                return self.state.should_quit;
            }
            KeyCode::End => {
                if self.input_text().trim().is_empty() {
                    self.state.scroll_to_bottom();
                    return self.state.should_quit;
                }
            }
            _ => {}
        }

        // Ctrl+Enter inserts a newline (multiline input).
        if key.code == KeyCode::Enter && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.textarea.insert_newline();
            self.sync_command_state();
            return self.state.should_quit;
        }

        // Enter submits (ignored while a turn is running).
        if key.code == KeyCode::Enter && key.modifiers.is_empty() {
            if self.state.busy {
                return self.state.should_quit;
            }
            if self.state.input_mode == InputMode::Command {
                let text = self.input_text();
                let text = text.trim().to_string();
                if !text.is_empty() {
                    // Enter runs the highlighted palette row when the filter
                    // matches commands, else the text exactly as typed.
                    let command = self
                        .state
                        .selected_command()
                        .map(str::to_string)
                        .unwrap_or(text);
                    self.execute_slash_command(&command);
                }
                self.clear_input();
            } else {
                self.submit_input();
            }
            return self.state.should_quit;
        }

        // Up/Down recall history when the input is empty (or already browsing);
        // otherwise they move the cursor inside the multiline editor.
        if matches!(key.code, KeyCode::Up | KeyCode::Down) && key.modifiers.is_empty() {
            let current = self.input_text();
            let browsing = self
                .state
                .history_index
                .map(|index| {
                    self.state
                        .history
                        .get(index)
                        .map(|entry| entry == &current)
                        .unwrap_or(false)
                })
                .unwrap_or(false);
            if current.trim().is_empty() || browsing {
                let recalled = if key.code == KeyCode::Up {
                    self.state.history_prev()
                } else {
                    self.state.history_next()
                };
                if let Some(text) = recalled {
                    self.set_input(&text);
                    return self.state.should_quit;
                }
            }
        }

        // Everything else is textarea editing.
        let _ = self.textarea.input(key);
        self.sync_command_state();
        self.state.should_quit
    }

    /// Route a key event to the key-setup wizard (open ⇒ all input lands here).
    fn handle_setup_key(&mut self, key: KeyEvent) {
        let stage = match self.state.key_setup.as_ref() {
            Some(setup) => setup.stage,
            None => return,
        };

        match stage {
            SetupStage::ChooseProvider => match key.code {
                KeyCode::Up => {
                    if let Some(setup) = self.state.key_setup.as_mut() {
                        setup.move_selection(-1);
                    }
                }
                KeyCode::Down => {
                    if let Some(setup) = self.state.key_setup.as_mut() {
                        setup.move_selection(1);
                    }
                }
                KeyCode::Enter => {
                    if let Some(setup) = self.state.key_setup.as_mut() {
                        setup.stage = SetupStage::EnterKey;
                        setup.status = None;
                    }
                }
                KeyCode::Esc => self.state.key_setup = None,
                _ => {}
            },
            SetupStage::EnterKey => match key.code {
                KeyCode::Enter => {
                    let (provider, secret) = match self.state.key_setup.as_ref() {
                        Some(setup) => (
                            setup.provider().to_string(),
                            setup.key_input.trim().to_string(),
                        ),
                        None => return,
                    };
                    if secret.is_empty() {
                        if let Some(setup) = self.state.key_setup.as_mut() {
                            setup.status = Some("paste a key first".to_string());
                        }
                    } else {
                        if normalize_provider(&provider) == "cloudflare" {
                            if let Some(setup) = self.state.key_setup.as_mut() {
                                setup.stage = SetupStage::EnterAccountId;
                                setup.status = None;
                            }
                        } else {
                            self.dispatch_key_add(provider, secret, None);
                        }
                    }
                }
                KeyCode::Esc => {
                    let custom = self
                        .state
                        .key_setup
                        .as_ref()
                        .map(|setup| setup.custom_provider.is_some())
                        .unwrap_or(false);
                    if custom {
                        // Opened via `/keys add <provider>`: cancel outright.
                        self.state.key_setup = None;
                    } else if let Some(setup) = self.state.key_setup.as_mut() {
                        setup.stage = SetupStage::ChooseProvider;
                        setup.key_input.clear();
                        setup.status = None;
                    }
                }
                KeyCode::Backspace => {
                    if let Some(setup) = self.state.key_setup.as_mut() {
                        setup.key_input.pop();
                    }
                }
                KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if let Some(setup) = self.state.key_setup.as_mut() {
                        setup.key_input.push(character);
                    }
                }
                _ => {}
            },
            SetupStage::EnterAccountId => match key.code {
                KeyCode::Enter => {
                    let account_id = self
                        .state
                        .key_setup
                        .as_ref()
                        .map(|setup| setup.account_id_input.trim().to_string())
                        .unwrap_or_default();
                    match cloudflare_base_url(&account_id) {
                        Ok(_) => {
                            let (provider, secret) = match self.state.key_setup.as_ref() {
                                Some(setup) => (
                                    setup.provider().to_string(),
                                    setup.key_input.trim().to_string(),
                                ),
                                None => return,
                            };
                            if let Some(setup) = self.state.key_setup.as_mut() {
                                setup.stage = SetupStage::Validating;
                                setup.status = None;
                            }
                            self.dispatch_key_add(provider, secret, Some(account_id));
                        }
                        Err(error) => {
                            if let Some(setup) = self.state.key_setup.as_mut() {
                                setup.status = Some(error.to_string());
                            }
                        }
                    }
                }
                KeyCode::Esc => {
                    if let Some(setup) = self.state.key_setup.as_mut() {
                        setup.stage = SetupStage::EnterKey;
                        setup.account_id_input.clear();
                        setup.status = None;
                    }
                }
                KeyCode::Backspace => {
                    if let Some(setup) = self.state.key_setup.as_mut() {
                        setup.account_id_input.pop();
                    }
                }
                KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if let Some(setup) = self.state.key_setup.as_mut() {
                        setup.account_id_input.push(character);
                    }
                }
                _ => {}
            },
            // Waiting for the actor's reply; ignore input.
            SetupStage::Validating => {}
        }
    }

    fn dispatch_key_add(
        &mut self,
        provider: String,
        secret: String,
        cloudflare_account_id: Option<String>,
    ) {
        if let Some(setup) = self.state.key_setup.as_mut() {
            setup.stage = SetupStage::Validating;
            setup.status = None;
        }
        let event_provider = provider.clone();
        self.dispatch(
            move |done| AgentRequest::AddKey {
                provider,
                secret,
                cloudflare_account_id,
                done,
            },
            move |result| AppEvent::KeyAdded {
                provider: event_provider,
                result,
            },
        );
    }

    /// Route a key event to the plan-review overlay (a/e/c while a plan waits).
    fn handle_plan_review_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('a') | KeyCode::Char('A') => self.approve_plan(),
            KeyCode::Char('e') | KeyCode::Char('E') => self.edit_plan(),
            KeyCode::Char('c') | KeyCode::Char('C') | KeyCode::Esc => self.cancel_plan(),
            _ => {}
        }
    }

    /// `a)pprove`: close the overlay and run the plan as a normal turn.
    fn approve_plan(&mut self) {
        self.state.plan_review = None;
        self.state.plan_feedback = false;
        self.state
            .add_message("system", "Plan approved — executing.");
        self.send_turn(
            "The plan above was approved. Execute it now, step by step.".to_string(),
            false,
        );
    }

    /// `e)dit`: the next submitted input becomes feedback for a re-plan.
    fn edit_plan(&mut self) {
        self.state.plan_review = None;
        self.state.plan_feedback = true;
        self.state.add_message(
            "system",
            "Type your feedback for the plan and press Enter — the agent will revise it.",
        );
    }

    /// `c)ancel` / Esc: drop the pending plan.
    fn cancel_plan(&mut self) {
        self.state.plan_review = None;
        self.state.plan_feedback = false;
        self.state
            .add_message("system", "Plan discarded. Send a new task when ready.");
    }

    /// Send a turn to the actor (plan mode or normal), marking busy state.
    fn send_turn(&mut self, text: String, plan: bool) {
        if self.state.busy {
            return;
        }
        self.state.busy = true;
        self.state.busy_since = Some(std::time::Instant::now());
        self.streaming_msg = None;
        self.state.plan_pending = plan;
        self.dispatch(
            move |done| AgentRequest::Turn {
                prompt: text,
                plan,
                done,
            },
            AppEvent::TurnDone,
        );
    }

    /// Handle a bracketed paste: into the key wizard, else the input editor.
    pub fn handle_paste(&mut self, text: String) {
        if let Some(setup) = self.state.key_setup.as_mut() {
            if setup.stage == SetupStage::EnterKey {
                setup.key_input.push_str(text.trim());
            } else if setup.stage == SetupStage::EnterAccountId {
                setup.account_id_input.push_str(text.trim());
            }
            return;
        }
        if let Some(picker) = self.state.themes_picker.as_mut() {
            if let ThemePickerMode::JsonImport(import) = &mut picker.mode {
                import.paste(&text);
                return;
            }
        }
        self.textarea.insert_str(&text);
        self.sync_command_state();
    }

    /// Submit the current input: `!command` passthrough, shell mode, plan
    /// feedback, or a normal user turn (no-op while busy or empty).
    pub fn submit_input(&mut self) {
        if self.state.busy {
            return;
        }
        let text = self.input_text().trim().to_string();
        if text.is_empty() {
            return;
        }
        self.state.push_history(&text);
        self.clear_input();

        // `!command`: run directly, no LLM involvement (Step 7).
        if let Some(rest) = text.strip_prefix('!') {
            let command = rest.trim();
            if command.is_empty() {
                self.state
                    .add_message("system", "Usage: !<command> — e.g. !cargo build");
            } else {
                self.state.add_message("shell", format!("$ {command}"));
                if !self.try_handle_cd(command) {
                    self.exec_passthrough(command.to_string(), false);
                }
            }
            return;
        }
        if self.state.shell_mode {
            self.state.add_message("shell", format!("$ {text}"));
            if !self.try_handle_cd(&text) {
                self.exec_passthrough(text, self.state.shell_use_kali);
            }
            return;
        }

        self.state.add_message("user", &text);
        let plan = std::mem::take(&mut self.state.plan_feedback);
        self.send_turn(text, plan);
    }

    /// Attempt to handle a `cd`, `pwd`, or drive-switch command in-process so
    /// `self.workspace` (the current working directory) persists across
    /// subsequent commands. Returns `true` if handled.
    fn try_handle_cd(&mut self, command: &str) -> bool {
        let trimmed = command.trim();
        let lower = trimmed.to_ascii_lowercase();

        // 1. Check for standalone cd / pwd
        if lower == "cd" || lower == "pwd" {
            self.state
                .add_message("shell", format!("{}", self.workspace.display()));
            return true;
        }

        // 2. Check for Windows drive switch like "C:" or "D:"
        if trimmed.len() == 2
            && trimmed.ends_with(':')
            && trimmed
                .chars()
                .next()
                .map(|c| c.is_ascii_alphabetic())
                .unwrap_or(false)
        {
            let drive = format!("{}\\", trimmed.to_ascii_uppercase());
            let path = PathBuf::from(&drive);
            if path.is_dir() {
                self.workspace = path;
                self.state
                    .add_message("shell", format!("{}", self.workspace.display()));
            } else {
                self.state.add_message(
                    "error",
                    format!("The drive {} does not exist or is not accessible.", drive),
                );
            }
            return true;
        }

        // 3. Check for cd <path>, chdir <path>, or Set-Location <path>
        let raw_target = if lower.starts_with("cd ") {
            &trimmed[3..]
        } else if lower.starts_with("chdir ") {
            &trimmed[6..]
        } else if lower.starts_with("set-location ") {
            &trimmed[13..]
        } else {
            return false;
        };

        let mut target = raw_target.trim();
        // Strip /d flag if present (cmd.exe flag to change drive and directory)
        if target.to_ascii_lowercase().starts_with("/d ") {
            target = target[3..].trim();
        }
        // Strip wrapping quotes
        if (target.starts_with('"') && target.ends_with('"') && target.len() >= 2)
            || (target.starts_with('\'') && target.ends_with('\'') && target.len() >= 2)
        {
            target = &target[1..target.len() - 1];
        }
        let target = target.trim();

        if target.is_empty() {
            self.state
                .add_message("shell", format!("{}", self.workspace.display()));
            return true;
        }

        // Expand home directory `~`
        let path = if target == "~" || target.starts_with("~/") || target.starts_with("~\\") {
            let home = std::env::var_os("USERPROFILE")
                .or_else(|| std::env::var_os("HOME"))
                .map(PathBuf::from)
                .unwrap_or_else(|| self.workspace.clone());
            if target == "~" {
                home
            } else {
                home.join(&target[2..])
            }
        } else {
            let p = Path::new(target);
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                self.workspace.join(p)
            }
        };

        let normalized = normalize_path(&path);
        if normalized.is_dir() {
            self.workspace = normalized;
            self.state
                .add_message("shell", format!("{}", self.workspace.display()));
        } else {
            self.state.add_message(
                "error",
                format!("The system cannot find the path specified: {target}"),
            );
        }
        true
    }

    /// Run `command` through the platform shell, streaming output into a
    /// `shell` chat message (no agent involved).
    fn exec_passthrough(&mut self, command: String, use_kali: bool) {
        let workspace = self.workspace.clone();
        let output_index = self.state.messages.len();
        self.state
            .messages
            .push(UiMessage::new("shell", String::new()));
        self.shell_output = Some(output_index);
        self.state.busy = true;
        self.state.stick_to_bottom = true;

        let ui_tx = self.ui_tx.clone();
        tokio::spawn(async move {
            let result = run_shell_streaming(&command, &workspace, ui_tx.clone(), use_kali).await;
            let _ = ui_tx.send(AppEvent::ShellFinished { result });
        });
    }

    /// Execute a slash command typed into the input line.
    pub fn execute_slash_command(&mut self, command: &str) {
        let command = command.trim().trim_start_matches('/');
        let mut parts = command.splitn(2, ' ');
        let name = parts.next().unwrap_or("").trim().to_ascii_lowercase();
        let arg = parts.next().unwrap_or("").trim().to_string();

        match name.as_str() {
            "help" => self.state.add_message("system", command_help_text()),
            "quit" | "exit" => self.state.should_quit = true,
            "plan" => {
                if arg.is_empty() {
                    self.state.add_message(
                        "system",
                        "Usage: /plan <task> — the agent drafts a plan first (read-only tools), \
                         then you choose a)pprove · e)dit · c)ancel.",
                    );
                } else {
                    let display = format!("/plan {arg}");
                    self.state.push_history(&display);
                    self.state.add_message("user", &display);
                    self.state.plan_feedback = false;
                    self.send_turn(arg.clone(), true);
                }
            }
            "skill" | "skills" => {
                if name.as_str() == "skills" && arg.is_empty() {
                    self.open_skills_picker();
                } else {
                    self.execute_skill_command(&arg);
                }
            }
            "agents" => {
                if self.state.subagents.is_empty() {
                    self.state.add_message(
                        "system",
                        "No subagent runs yet — the agent spawns them with the task tool when a \
                         job is independent enough to run in its own context window.",
                    );
                } else {
                    let mut text = format!("Subagent runs ({}):", self.state.subagents.len());
                    for run in &self.state.subagents {
                        text.push_str(&format!("\n  {} · [{}]", run.name, run.status));
                    }
                    self.state.add_message("system", text);
                }
            }
            "agent" => {
                if arg.is_empty() {
                    self.state.add_message(
                        "system",
                        "Usage: /agent <task> — explicitly delegate this focused task to a subagent.",
                    );
                } else {
                    self.state.push_history(&format!("/agent {arg}"));
                    self.state
                        .add_message("user", format!("Delegate task: {arg}"));
                    self.send_turn(
                        format!(
                            "Delegate this focused task to a subagent using the `task` tool. \
                             Give it complete, standalone instructions, then summarize its \
                             findings or result: {arg}"
                        ),
                        false,
                    );
                }
            }
            "shell" => {
                self.state.shell_mode = !self.state.shell_mode;
                self.state.shell_use_kali = false;
                let message = if self.state.shell_mode {
                    "Shell passthrough ON — commands run directly in your terminal (PowerShell/cmd on Windows; \
                     cd navigates directories). /shell turns it off; `!command` works in any mode; slash commands still work."
                } else {
                    "Shell passthrough OFF — back to chat mode."
                };
                self.state.add_message("system", message);
                self.apply_textarea_styles();
            }
            "kali" => {
                if cfg!(windows) {
                    self.state.shell_mode = !self.state.shell_mode;
                    self.state.shell_use_kali = self.state.shell_mode;
                    let message = if self.state.shell_mode {
                        "Kali shell ON — commands run in the Kali WSL distribution `kali-linux` \
                         (override with SICK_KALI_WSL_DISTRO); install it with `wsl --install -d \
                         kali-linux` if needed. /kali or /shell turns it off; `!command` uses the \
                         host shell."
                    } else {
                        "Shell passthrough OFF — back to chat mode."
                    };
                    self.state.add_message("system", message);
                    self.apply_textarea_styles();
                } else {
                    self.state
                        .add_message("system", "Kali WSL is only applicable on Windows.");
                }
            }
            "clear" => {
                self.state.messages.clear();
                self.streaming_msg = None;
                self.state.add_message("system", "Chat cleared.");
            }
            "theme" | "themes" => {
                let mut parts = arg.split_whitespace();
                match parts.next().unwrap_or("").to_ascii_lowercase().as_str() {
                    "" => {
                        self.open_themes_picker();
                    }
                    "list" => {
                        let names = self
                            .state
                            .themes
                            .iter()
                            .map(|theme| {
                                if theme.name() == self.state.theme.name() {
                                    format!("* {}", theme.name())
                                } else {
                                    format!("  {}", theme.name())
                                }
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        self.state.add_message(
                                "system",
                                format!(
                                    "Themes (* active):\n{names}\n\nCreate one with /theme create <name> \
                                     <background> <primary> <accent> <secondary> <success> <warning> \
                                     <error> <metadata> (eight #RRGGBB colors)."
                                ),
                            );
                    }
                    "create" => {
                        let values = parts.collect::<Vec<_>>();
                        if values.len() != 9 {
                            self.state.add_message(
                                "system",
                                "Usage: /theme create <name> <background> <primary> <accent> \
                                     <secondary> <success> <warning> <error> <metadata> (use eight \
                                     #RRGGBB colors).",
                            );
                            return;
                        }
                        let palette = Theme::palette(
                            values[1], values[2], values[3], values[4], values[5], values[6],
                            values[7], values[8],
                        );
                        match Theme::custom(values[0], palette) {
                            Ok(theme) => {
                                let original_themes = self.state.themes.clone();
                                self.state
                                    .themes
                                    .retain(|saved| saved.name() != theme.name());
                                self.state.themes.push(theme.clone());
                                if !self.activate_theme(theme) {
                                    self.state.themes = original_themes;
                                }
                            }
                            Err(error) => self
                                .state
                                .add_message("error", format!("could not create theme: {error}")),
                        }
                    }
                    theme_name => {
                        let requested =
                            format!("{theme_name} {}", parts.collect::<Vec<_>>().join(" "));
                        let requested_name = requested.trim().replace(' ', "-");
                        if let Some(theme) = self
                            .state
                            .themes
                            .iter()
                            .find(|theme| theme.name().eq_ignore_ascii_case(&requested_name))
                            .cloned()
                        {
                            self.activate_theme(theme);
                        } else {
                            self.state.add_message(
                                "error",
                                format!(
                                    "Unknown theme `{requested_name}`. Use /theme to browse themes."
                                ),
                            );
                        }
                    }
                }
            }
            "model" | "models" => {
                if name.as_str() == "models" && arg.is_empty() {
                    self.open_models_picker();
                } else if arg.is_empty() {
                    self.state.add_message(
                        "system",
                        format!(
                            "Current model: {} (provider: {}). Usage: /models <provider> <model> (or /model [provider:]model)",
                            self.state.current_model, self.state.current_provider
                        ),
                    );
                } else {
                    let (provider, model) =
                        parse_model_selector(&arg, &self.state.current_provider);
                    if model.is_empty() {
                        self.state.add_message(
                            "error",
                            "Usage: /models <provider> <model> or /model [provider:]model",
                        );
                    } else {
                        let request_provider = provider.clone();
                        let request_model = model.clone();
                        self.dispatch(
                            move |done| AgentRequest::SetModel {
                                provider: request_provider,
                                model: request_model,
                                done,
                            },
                            move |result| AppEvent::ModelSet {
                                provider,
                                model,
                                result,
                            },
                        );
                    }
                }
            }
            "keys" => {
                let mut arg_parts = arg.split_whitespace();
                match arg_parts.next() {
                    Some("add") => {
                        let setup = match arg_parts.next() {
                            Some(provider) => KeySetup::for_provider(provider),
                            None => KeySetup::new(),
                        };
                        self.state.key_setup = Some(setup);
                    }
                    Some("remove") => {
                        let provider = arg_parts.next().unwrap_or("");
                        let key_id = arg_parts.next().unwrap_or("");
                        self.request_key_removal(provider, key_id);
                    }
                    _ => {
                        self.dispatch(|done| AgentRequest::KeyStatus { done }, AppEvent::KeysDone);
                    }
                }
            }
            "keysrm" | "keyssm" => {
                let mut arg_parts = arg.split_whitespace();
                let provider = arg_parts.next().unwrap_or("");
                let key_id = arg_parts.next().unwrap_or("");
                self.request_key_removal(provider, key_id);
            }
            "cloudflare" => {
                let account_id = arg.trim().to_string();
                match cloudflare_base_url(&account_id) {
                    Ok(_) => self.dispatch(
                        move |done| AgentRequest::ConfigureCloudflare { account_id, done },
                        AppEvent::CloudflareConfigured,
                    ),
                    Err(error) => self.state.add_message(
                        "system",
                        format!(
                            "Usage: /cloudflare <32-character-account-id> — this is the Cloudflare \
                             account ID, not the API token ID. {error}"
                        ),
                    ),
                }
            }
            "keysadd" => {
                self.state.key_setup = Some(KeySetup::new());
            }
            "sessions" => {
                self.dispatch(
                    |done| AgentRequest::Sessions { done },
                    AppEvent::SessionsDone,
                );
            }
            "resume" => {
                if arg.is_empty() {
                    self.state
                        .add_message("system", "Usage: /resume <session-id> (see /sessions).");
                } else {
                    let session_id = arg.clone();
                    self.dispatch(
                        move |done| AgentRequest::Resume {
                            session_id: session_id.clone(),
                            done,
                        },
                        move |result| AppEvent::ResumeDone {
                            session_id: arg,
                            result,
                        },
                    );
                }
            }
            "fork" => {
                let title = if arg.trim().is_empty() {
                    "fork".to_string()
                } else {
                    arg.trim().to_string()
                };
                self.dispatch(
                    move |done| AgentRequest::Fork { title, done },
                    AppEvent::Forked,
                );
            }
            "history" => {
                let limit = arg.trim().parse::<usize>().unwrap_or(20);
                self.dispatch(
                    move |done| AgentRequest::History { limit, done },
                    AppEvent::HistoryDone,
                );
            }
            "search" => {
                if arg.trim().is_empty() {
                    self.state.add_message(
                        "system",
                        "Usage: /search <query> — searches prompt history and memories.",
                    );
                } else {
                    let query = arg.trim().to_string();
                    let request_query = query.clone();
                    self.dispatch(
                        move |done| AgentRequest::Search {
                            query: request_query,
                            done,
                        },
                        move |result| AppEvent::SearchDone { query, result },
                    );
                }
            }
            "replay" => {
                if arg.trim().is_empty() {
                    self.state.add_message(
                        "system",
                        "Usage: /replay <tool-call-id | last> — re-runs a stored tool call.",
                    );
                } else {
                    let call_id = arg.trim().to_string();
                    self.dispatch(
                        move |done| AgentRequest::Replay {
                            call_id: call_id.clone(),
                            done,
                        },
                        AppEvent::ReplayDone,
                    );
                }
            }
            "config" => {
                self.dispatch(
                    |done| AgentRequest::ConfigInfo { done },
                    AppEvent::ConfigDone,
                );
            }
            "mcp" => {
                self.dispatch(|done| AgentRequest::McpInfo { done }, AppEvent::McpDone);
            }
            other => {
                self.state
                    .add_message("error", format!("Unknown command: /{other} — try /help"));
            }
        }
    }

    /// `/models`: open the interactive model picker for providers whose keys
    /// are configured, then refresh it with each provider's live model list.
    fn open_models_picker(&mut self) {
        let mut providers: Vec<String> = self
            .state
            .key_status
            .iter()
            .map(|key| normalize_provider(&key.provider))
            .collect();
        providers.sort();
        providers.dedup();

        if providers.is_empty() {
            self.state.add_message(
                "system",
                "No API keys configured yet — run /keysadd to add one.",
            );
            return;
        }

        // Static catalog first so the picker is usable immediately; the live
        // lookup replaces each provider's rows when it lands.
        let mut options = Vec::new();
        for provider in &providers {
            for model in known_models(provider) {
                options.push(ModelOption {
                    provider: provider.clone(),
                    model: (*model).to_string(),
                });
            }
        }

        let mut picker = ModelsPicker::new(options);
        picker.loading = true;
        let current_provider = normalize_provider(&self.state.current_provider);
        let current_model = self.state.current_model.clone();
        picker.highlight_model(&current_provider, &current_model);
        self.state.models_picker = Some(picker);

        self.dispatch(
            |done| AgentRequest::ListModels { done },
            AppEvent::ModelsDone,
        );
    }

    /// Route a key event to the models picker (open ⇒ all input lands here).
    fn handle_models_picker_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up => self.state.move_models_selection(-1),
            KeyCode::Down => self.state.move_models_selection(1),
            KeyCode::PageUp => self.state.move_models_selection(-10),
            KeyCode::PageDown => self.state.move_models_selection(10),
            KeyCode::Backspace => {
                if let Some(picker) = self.state.models_picker.as_mut() {
                    picker.filter.pop();
                    picker.selection = 0;
                }
            }
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(picker) = self.state.models_picker.as_mut() {
                    picker.filter.push(character);
                    picker.selection = 0;
                }
            }
            KeyCode::Enter => {
                let Some(option) = self.state.selected_model() else {
                    return;
                };
                self.state.models_picker = None;
                let request_provider = option.provider.clone();
                let request_model = option.model.clone();
                self.dispatch(
                    move |done| AgentRequest::SetModel {
                        provider: request_provider,
                        model: request_model,
                        done,
                    },
                    move |result| AppEvent::ModelSet {
                        provider: option.provider,
                        model: option.model,
                        result,
                    },
                );
            }

            KeyCode::Esc => self.state.models_picker = None,
            _ => {}
        }
    }

    fn open_themes_picker(&mut self) {
        self.state.themes_picker = Some(ThemesPicker::new(
            self.state.themes.clone(),
            self.state.theme.clone(),
        ));
    }

    fn handle_themes_picker_key(&mut self, key: KeyEvent) {
        let Some(picker) = self.state.themes_picker.as_mut() else {
            return;
        };

        match &mut picker.mode {
            ThemePickerMode::ColorWheel(wheel) => {
                match key.code {
                    KeyCode::Esc => {
                        picker.mode = ThemePickerMode::Browse;
                    }
                    KeyCode::Tab | KeyCode::Down
                        if wheel.active_field != 0 || key.code == KeyCode::Tab =>
                    {
                        wheel.active_field = (wheel.active_field + 1) % 10;
                        wheel.sync_hsl_from_active_color();
                    }
                    KeyCode::BackTab | KeyCode::Up
                        if wheel.active_field != 0 || key.code == KeyCode::BackTab =>
                    {
                        wheel.active_field = if wheel.active_field == 0 {
                            9
                        } else {
                            wheel.active_field - 1
                        };
                        wheel.sync_hsl_from_active_color();
                    }
                    // Field 0: Theme Name input
                    KeyCode::Char(c)
                        if wheel.active_field == 0
                            && !key.modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                            wheel.name.push(c.to_ascii_lowercase());
                        }
                    }
                    KeyCode::Backspace if wheel.active_field == 0 => {
                        wheel.name.pop();
                    }
                    KeyCode::Enter if wheel.active_field == 0 => {
                        wheel.active_field = 1;
                        wheel.sync_hsl_from_active_color();
                    }
                    // Field 1..=8: Color roles navigation & tuning
                    KeyCode::Left | KeyCode::Char('h')
                        if wheel.active_field >= 1 && wheel.active_field <= 8 =>
                    {
                        wheel.hue = (wheel.hue - 10.0 + 360.0) % 360.0;
                        wheel.apply_hsl_to_active_color();
                    }
                    KeyCode::Right | KeyCode::Char('l')
                        if wheel.active_field >= 1 && wheel.active_field <= 8 =>
                    {
                        wheel.hue = (wheel.hue + 10.0) % 360.0;
                        wheel.apply_hsl_to_active_color();
                    }
                    KeyCode::Char('[') | KeyCode::Char('-')
                        if wheel.active_field >= 1 && wheel.active_field <= 8 =>
                    {
                        wheel.lightness = (wheel.lightness - 0.05).clamp(0.05, 0.95);
                        wheel.apply_hsl_to_active_color();
                    }
                    KeyCode::Char(']') | KeyCode::Char('+') | KeyCode::Char('=')
                        if wheel.active_field >= 1 && wheel.active_field <= 8 =>
                    {
                        wheel.lightness = (wheel.lightness + 0.05).clamp(0.05, 0.95);
                        wheel.apply_hsl_to_active_color();
                    }
                    KeyCode::Char('{') if wheel.active_field >= 1 && wheel.active_field <= 8 => {
                        wheel.saturation = (wheel.saturation - 0.05).clamp(0.0, 1.0);
                        wheel.apply_hsl_to_active_color();
                    }
                    KeyCode::Char('}') if wheel.active_field >= 1 && wheel.active_field <= 8 => {
                        wheel.saturation = (wheel.saturation + 0.05).clamp(0.0, 1.0);
                        wheel.apply_hsl_to_active_color();
                    }
                    KeyCode::Char(digit @ '1'..='8') => {
                        wheel.active_field = (digit as usize) - ('0' as usize);
                        wheel.sync_hsl_from_active_color();
                    }
                    // Save action
                    KeyCode::Enter if wheel.active_field == 9 => {
                        let pal_res = Theme::palette(
                            &wheel.colors[0],
                            &wheel.colors[1],
                            &wheel.colors[2],
                            &wheel.colors[3],
                            &wheel.colors[4],
                            &wheel.colors[5],
                            &wheel.colors[6],
                            &wheel.colors[7],
                        );
                        match Theme::custom(&wheel.name, pal_res) {
                            Ok(theme) => {
                                self.state.themes.retain(|t| t.name() != theme.name());
                                self.state.themes.push(theme.clone());
                                self.state.themes_picker = None;
                                self.activate_theme(theme);
                            }
                            Err(err) => {
                                wheel.message = Some((err, true));
                            }
                        }
                    }
                    KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        let pal_res = Theme::palette(
                            &wheel.colors[0],
                            &wheel.colors[1],
                            &wheel.colors[2],
                            &wheel.colors[3],
                            &wheel.colors[4],
                            &wheel.colors[5],
                            &wheel.colors[6],
                            &wheel.colors[7],
                        );
                        match Theme::custom(&wheel.name, pal_res) {
                            Ok(theme) => {
                                self.state.themes.retain(|t| t.name() != theme.name());
                                self.state.themes.push(theme.clone());
                                self.state.themes_picker = None;
                                self.activate_theme(theme);
                            }
                            Err(err) => {
                                wheel.message = Some((err, true));
                            }
                        }
                    }
                    _ => {}
                }
            }
            ThemePickerMode::JsonImport(import) => match key.code {
                KeyCode::Esc => {
                    picker.mode = ThemePickerMode::Browse;
                }
                KeyCode::Backspace => {
                    import.buffer.pop();
                    import.update_status();
                }
                KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if let Ok((_name, theme)) = &import.status {
                        let theme = theme.clone();
                        self.state.themes.retain(|t| t.name() != theme.name());
                        self.state.themes.push(theme.clone());
                        self.state.themes_picker = None;
                        self.activate_theme(theme);
                    }
                }
                KeyCode::Enter => {
                    if let Ok((_name, theme)) = &import.status {
                        let theme = theme.clone();
                        self.state.themes.retain(|t| t.name() != theme.name());
                        self.state.themes.push(theme.clone());
                        self.state.themes_picker = None;
                        self.activate_theme(theme);
                    } else {
                        import.buffer.push('\n');
                        import.update_status();
                    }
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    import.buffer.push(c);
                    import.update_status();
                }
                _ => {}
            },
            ThemePickerMode::Browse => match key.code {
                KeyCode::Up => {
                    self.state.move_themes_selection(-1);
                    if let Some(selected) = self.state.selected_theme() {
                        self.state.theme = selected;
                    }
                }
                KeyCode::Down => {
                    self.state.move_themes_selection(1);
                    if let Some(selected) = self.state.selected_theme() {
                        self.state.theme = selected;
                    }
                }
                KeyCode::PageUp => {
                    self.state.move_themes_selection(-10);
                    if let Some(selected) = self.state.selected_theme() {
                        self.state.theme = selected;
                    }
                }
                KeyCode::PageDown => {
                    self.state.move_themes_selection(10);
                    if let Some(selected) = self.state.selected_theme() {
                        self.state.theme = selected;
                    }
                }
                KeyCode::Backspace => {
                    if let Some(picker) = self.state.themes_picker.as_mut() {
                        picker.filter.pop();
                        picker.selection = 0;
                    }
                    if let Some(selected) = self.state.selected_theme() {
                        self.state.theme = selected;
                    }
                }
                KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if let Some(picker) = self.state.themes_picker.as_mut() {
                        picker.filter.push(character);
                        picker.selection = 0;
                    }
                    if let Some(selected) = self.state.selected_theme() {
                        self.state.theme = selected;
                    }
                }
                KeyCode::Enter => {
                    let item = self.state.selected_theme_item();
                    match item {
                        Some(ThemePickerItem::CreateCustomWheel) => {
                            let styles = self.state.theme.styles();
                            if let Some(picker) = self.state.themes_picker.as_mut() {
                                picker.mode = ThemePickerMode::ColorWheel(
                                    ColorWheelState::from_styles(&styles),
                                );
                            }
                        }
                        Some(ThemePickerItem::ImportCustomJson) => {
                            if let Some(picker) = self.state.themes_picker.as_mut() {
                                picker.mode = ThemePickerMode::JsonImport(JsonImportState::new());
                            }
                        }
                        Some(ThemePickerItem::CustomTheme(theme))
                        | Some(ThemePickerItem::BuiltinTheme(theme)) => {
                            self.state.themes_picker = None;
                            self.activate_theme(theme);
                        }
                        None => {}
                    }
                }
                KeyCode::Esc => {
                    if let Some(picker) = self.state.themes_picker.take() {
                        self.state.theme = picker.initial_theme;
                        self.apply_textarea_styles();
                    }
                }
                _ => {}
            },
        }
    }

    fn open_skills_picker(&mut self) {
        let options = skills::list_skills()
            .into_iter()
            .map(|skill| SkillOption {
                name: skill.name,
                description: skill.description,
                path: skill.path,
            })
            .collect::<Vec<_>>();
        self.state.skills_picker = Some(SkillsPicker::new(options));
    }

    fn handle_skills_picker_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up => self.state.move_skills_selection(-1),
            KeyCode::Down => self.state.move_skills_selection(1),
            KeyCode::PageUp => self.state.move_skills_selection(-10),
            KeyCode::PageDown => self.state.move_skills_selection(10),
            KeyCode::Enter => {
                if let Some(skill) = self.state.selected_skill() {
                    self.state.skills_picker = None;
                    let skill_name = skill.name;
                    let error_label = skill_name.clone();
                    self.dispatch(
                        move |done| AgentRequest::ActivateSkill {
                            name: skill_name,
                            done,
                        },
                        move |result| AppEvent::SkillActivated {
                            name: error_label,
                            result,
                        },
                    );
                }
            }
            KeyCode::Esc => self.state.skills_picker = None,
            KeyCode::Backspace => {
                if let Some(picker) = self.state.skills_picker.as_mut() {
                    picker.filter.pop();
                    picker.selection = 0;
                }
            }
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(picker) = self.state.skills_picker.as_mut() {
                    picker.filter.push(character);
                    picker.selection = 0;
                }
            }
            _ => {}
        }
    }

    /// Merge the live model lists into the open picker. Providers whose
    /// lookup failed keep their static catalog rows; the failures are
    /// surfaced in the picker's status line.
    fn merge_live_models(&mut self, report: Vec<ProviderModels>) {
        let current_provider = normalize_provider(&self.state.current_provider);
        let current_model = self.state.current_model.clone();
        let Some(picker) = self.state.models_picker.as_mut() else {
            return; // picker closed while the lookup was in flight
        };
        picker.loading = false;

        let previous = picker.options.get(picker.selection).cloned();
        let mut providers: Vec<String> = picker
            .options
            .iter()
            .map(|option| option.provider.clone())
            .collect();
        providers.sort();
        providers.dedup();

        let mut rebuilt: Vec<ModelOption> = Vec::new();
        for provider in providers {
            match report
                .iter()
                .find(|entry| entry.provider == provider && !entry.models.is_empty())
            {
                Some(entry) => {
                    for model in &entry.models {
                        rebuilt.push(ModelOption {
                            provider: provider.clone(),
                            model: model.clone(),
                        });
                    }
                }
                None => rebuilt.extend(
                    picker
                        .options
                        .iter()
                        .filter(|option| option.provider == provider)
                        .cloned(),
                ),
            }
        }
        picker.options = rebuilt;

        let reselect = previous
            .and_then(|prev| picker.options.iter().position(|option| *option == prev))
            .or_else(|| {
                picker.options.iter().position(|option| {
                    option.provider == current_provider && option.model == current_model
                })
            });
        picker.selection = reselect.unwrap_or(0);

        let failures: Vec<String> = report
            .iter()
            .filter(|entry| entry.models.is_empty())
            .filter_map(|entry| {
                entry
                    .error
                    .as_ref()
                    .map(|error| format!("{} ({error})", entry.provider))
            })
            .collect();
        if !failures.is_empty() {
            picker.status = Some(format!(
                "live lists unavailable for {}",
                failures.join("; ")
            ));
        }
    }

    /// `/skill` handling: list, scaffold, or activate a skill.
    fn execute_skill_command(&mut self, arg: &str) {
        let mut parts = arg.split_whitespace();
        match parts.next() {
            None | Some("list") => {
                let skill_list = skills::list_skills();
                if skill_list.is_empty() {
                    self.state.add_message(
                        "system",
                        format!(
                            "No skills found in {}. Create one with /skill new <name>.",
                            skills::skills_dir().display()
                        ),
                    );
                } else {
                    let mut text = format!("Skills ({}):", skill_list.len());
                    for skill in &skill_list {
                        let description = if skill.description.trim().is_empty() {
                            "(no description)"
                        } else {
                            skill.description.as_str()
                        };
                        text.push_str(&format!(
                            "\n  {} — {} ({})",
                            skill.name,
                            description,
                            skill.path.display()
                        ));
                    }
                    text.push_str("\nLoad one with /skill <name>.");
                    self.state.add_message("system", text);
                }
            }
            Some("new") => match parts.next() {
                None => self
                    .state
                    .add_message("system", "Usage: /skill new <name>"),
                Some(name) => match skills::scaffold_skill(name) {
                    Ok(path) => self.state.add_message(
                        "system",
                        format!(
                            "✓ Created skill template {}. Edit it, then /skill {name} to activate it.",
                            path.display()
                        ),
                    ),
                    Err(error) => self
                        .state
                        .add_message("error", format!("could not scaffold skill: {error}")),
                },
            },
            Some(name) => {
                let skill_name = name.to_string();
                let error_label = arg.to_string();
                self.dispatch(
                    move |done| AgentRequest::ActivateSkill {
                        name: skill_name.clone(),
                        done,
                    },
                    move |result| AppEvent::SkillActivated {
                        name: error_label,
                        result,
                    },
                );
            }
        }
    }

    fn set_input(&mut self, text: &str) {
        self.textarea.select_all();
        self.textarea.cut();
        self.textarea.insert_str(text);
        self.sync_command_state();
    }

    fn clear_input(&mut self) {
        self.textarea.select_all();
        self.textarea.cut();
        self.sync_command_state();
    }

    fn sync_command_state(&mut self) {
        let text = self.input_text();
        self.state.command_selection = 0;
        if text.starts_with('/') && !text.contains('\n') {
            self.state.input_mode = InputMode::Command;
            self.state.command_palette = true;
            self.state.command_text = text.trim_start_matches('/').to_string();
        } else {
            self.state.input_mode = InputMode::Normal;
            self.state.command_palette = false;
            self.state.command_text.clear();
        }
    }

    fn apply_textarea_styles(&mut self) {
        let styles = self.state.theme.styles();
        self.textarea.set_cursor_line_style(Style::default());
        self.textarea.set_style(styles.base());
        self.textarea
            .set_cursor_style(Style::default().bg(styles.assistant_color).fg(styles.bg));
        self.textarea
            .set_placeholder_text(if self.state.shell_mode {
                if self.state.shell_use_kali {
                    "Type a Kali Linux command to run…"
                } else {
                    "Type a command to run (PowerShell / cmd / terminal)…"
                }
            } else {
                "Type a message…"
            });
    }

    fn activate_theme(&mut self, theme: Theme) -> bool {
        let previous_theme = self.state.theme.clone();
        let previous_themes = self.state.themes.clone();
        self.state.theme = theme;
        let custom = self
            .state
            .themes
            .iter()
            .filter(|theme| Theme::parse(theme.name()).is_none())
            .cloned()
            .collect::<Vec<_>>();
        if let Err(error) = Theme::save(&custom, &self.state.theme) {
            self.state.theme = previous_theme;
            self.state.themes = previous_themes;
            self.state
                .add_message("error", format!("could not save theme: {error}"));
            return false;
        }
        self.apply_textarea_styles();
        self.state
            .add_message("system", format!("Theme: {}", self.state.theme.name()));
        true
    }
}

/// Normalize a path by resolving `.` and `..` components cleanly without
/// Windows `\\?\` prefix side effects.
fn normalize_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c.as_os_str()),
        }
    }
    out
}

/// Parse user input for `/model` or `/models`.
/// Supports:
/// - `<provider> <model>` (e.g. `google gemini-2.5-pro` or `cerebras custom-model`)
/// - `<provider>:<model>` (e.g. `google:gemini-2.5-pro`)
/// - `<provider>/<model>` (e.g. `cerebras/llama-3.3-70b` when provider is recognized)
/// - `<provider>` alone (switches to provider's default model)
/// - `<model>` (e.g. `gemini-2.5-pro` or custom model, retaining current provider)
pub fn parse_model_selector(arg: &str, current_provider: &str) -> (String, String) {
    let trimmed = arg.trim();
    if trimmed.is_empty() {
        return (normalize_provider(current_provider), String::new());
    }
    if let Some((prov, mdl)) = trimmed.split_once(' ') {
        let prov = prov.trim();
        let mdl = mdl.trim();
        if !prov.is_empty() && !mdl.is_empty() {
            return (normalize_provider(prov), mdl.to_string());
        }
    }
    if let Some((prov, mdl)) = trimmed.split_once(':') {
        let prov = prov.trim();
        let mdl = mdl.trim();
        if !prov.is_empty() && !mdl.is_empty() {
            return (normalize_provider(prov), mdl.to_string());
        }
    }
    if let Some((prov, mdl)) = trimmed.split_once('/') {
        let prov = prov.trim();
        let mdl = mdl.trim();
        if !prov.is_empty() && !mdl.is_empty() && is_known_provider(prov) {
            return (normalize_provider(prov), mdl.to_string());
        }
    }
    if is_known_provider(trimmed) {
        return (
            normalize_provider(trimmed),
            default_model_for(trimmed).to_string(),
        );
    }
    (normalize_provider(current_provider), trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_model_selector_space_separated() {
        let (prov, mdl) = parse_model_selector("google gemini-3.8-flash", "openai");
        assert_eq!(prov, "google");
        assert_eq!(mdl, "gemini-3.8-flash");

        let (prov, mdl) = parse_model_selector("aion-labs aion-labs/aion-2.0", "openai");
        assert_eq!(prov, "aionlabs");
        assert_eq!(mdl, "aion-labs/aion-2.0");

        let (prov, mdl) = parse_model_selector("openrouter meta-llama/llama-3-8b", "openai");
        assert_eq!(prov, "openrouter");
        assert_eq!(mdl, "meta-llama/llama-3-8b");

        let (prov, mdl) = parse_model_selector("cerebras custom-model-name", "openai");
        assert_eq!(prov, "cerebras");
        assert_eq!(mdl, "custom-model-name");
    }

    #[test]
    fn parse_model_selector_colon_separated() {
        let (prov, mdl) = parse_model_selector("mistral:mistral-small-latest", "openai");
        assert_eq!(prov, "mistral");
        assert_eq!(mdl, "mistral-small-latest");

        let (prov, mdl) = parse_model_selector("gemini:gemini-2.5-pro", "openai");
        assert_eq!(prov, "google");
        assert_eq!(mdl, "gemini-2.5-pro");
    }

    #[test]
    fn parse_model_selector_slash_separated() {
        let (prov, mdl) = parse_model_selector("cerebras/llama-3.3-70b", "openai");
        assert_eq!(prov, "cerebras");
        assert_eq!(mdl, "llama-3.3-70b");

        let (prov, mdl) =
            parse_model_selector("together/meta-llama/Llama-3.3-70B-Instruct-Turbo", "openai");
        assert_eq!(prov, "together");
        assert_eq!(mdl, "meta-llama/Llama-3.3-70B-Instruct-Turbo");
    }

    #[test]
    fn parse_model_selector_provider_only() {
        let (prov, mdl) = parse_model_selector("cerebras", "openai");
        assert_eq!(prov, "cerebras");
        assert_eq!(mdl, "llama-3.3-70b");
    }

    #[test]
    fn parse_model_selector_model_only() {
        let (prov, mdl) = parse_model_selector("gpt-4.1", "openai");
        assert_eq!(prov, "openai");
        assert_eq!(mdl, "gpt-4.1");

        let (prov, mdl) = parse_model_selector("custom-unlisted-model", "cerebras");
        assert_eq!(prov, "cerebras");
        assert_eq!(mdl, "custom-unlisted-model");
    }
}
