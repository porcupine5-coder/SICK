//! MCP server lifecycle: spawn, JSON-RPC handshake, tool discovery, calls.

use std::collections::{HashMap, HashSet};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use shell_config::McpServerConfig;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex as AsyncMutex, oneshot};

/// MCP protocol revision this client speaks.
pub const PROTOCOL_VERSION: &str = "2024-11-05";

/// Default per-request timeout (handshake and tool calls both use it).
pub const DEFAULT_TIMEOUT_MS: u64 = 60_000;

/// Longest tool name accepted by OpenAI-style function schemas.
const MAX_TOOL_NAME: usize = 64;

/// One tool offered by a connected MCP server.
#[derive(Debug, Clone)]
pub struct McpToolInfo {
    /// Name exposed to the model (`<server>-<tool>`, sanitized, deduped).
    pub exposed_name: String,
    /// Configured server name.
    pub server: String,
    /// Tool name as the server knows it.
    pub tool: String,
    /// Description shown to the model.
    pub description: String,
    /// JSON Schema for the tool arguments.
    pub schema: Value,
}

/// Connection state of one configured server.
#[derive(Debug, Clone)]
enum ServerState {
    /// Handshake done; tools available.
    Ready,
    /// Never connected, or died mid-handshake.
    Failed(String),
}

struct ManagedServer {
    name: String,
    state: ServerState,
    tools: Vec<McpToolInfo>,
    channel: Option<Arc<ServerChannel>>,
}

/// A snapshot of one server's state, for status displays.
#[derive(Debug, Clone)]
pub struct McpServerStatus {
    /// Configured server name.
    pub name: String,
    /// Whether the handshake succeeded.
    pub ready: bool,
    /// `"ready"` or a failure reason.
    pub state: String,
    /// Number of tools the server contributed.
    pub tool_count: usize,
}

/// Owns every configured MCP server and their live connections.
///
/// Construct with [`McpManager::connect_all`]; the manager can be shared as
/// `Arc<McpManager>` and registered into an agent via
/// [`crate::McpManager::tools`]. Dropping it kills every server process.
pub struct McpManager {
    servers: Vec<ManagedServer>,
}

impl McpManager {
    /// Start and hand-shake every configured server.
    ///
    /// Never fails wholesale: a server that cannot start is recorded as
    /// failed (see [`McpManager::statuses`]) and contributes no tools.
    pub async fn connect_all(servers: &[McpServerConfig]) -> Self {
        let mut used_names: HashSet<String> = HashSet::new();
        let mut managed = Vec::new();
        for config in servers {
            if config.name.trim().is_empty() || config.command.trim().is_empty() {
                managed.push(ManagedServer {
                    name: config.name.clone(),
                    state: ServerState::Failed(
                        "each [[mcp.servers]] entry needs a name and a command".into(),
                    ),
                    tools: Vec::new(),
                    channel: None,
                });
                continue;
            }
            managed.push(connect_one(config, &mut used_names).await);
        }
        Self { servers: managed }
    }

    /// Whether any servers are configured.
    pub fn is_empty(&self) -> bool {
        self.servers.is_empty()
    }

    /// Configured servers.
    pub fn len(&self) -> usize {
        self.servers.len()
    }

    /// Per-server connection state (config order).
    pub fn statuses(&self) -> Vec<McpServerStatus> {
        self.servers
            .iter()
            .map(|server| McpServerStatus {
                name: server.name.clone(),
                ready: matches!(server.state, ServerState::Ready),
                state: match &server.state {
                    ServerState::Ready => "ready".to_string(),
                    ServerState::Failed(reason) => format!("failed: {reason}"),
                },
                tool_count: server.tools.len(),
            })
            .collect()
    }

    /// Metadata for every discovered tool (all servers, config order).
    pub fn tool_infos(&self) -> Vec<McpToolInfo> {
        self.servers
            .iter()
            .flat_map(|server| server.tools.iter().cloned())
            .collect()
    }

    /// Number of tools discovered across all servers.
    pub fn tool_count(&self) -> usize {
        self.servers.iter().map(|server| server.tools.len()).sum()
    }

    /// Call `tool` on `server` with JSON `arguments`.
    ///
    /// Returns the MCP result object (callers check `isError`/`content`), or a
    /// human-readable error when the server is unknown, not running, or the
    /// request timed out.
    pub async fn call_tool(
        &self,
        server: &str,
        tool: &str,
        arguments: Value,
    ) -> Result<Value, String> {
        let managed = self
            .servers
            .iter()
            .find(|candidate| candidate.name == server)
            .ok_or_else(|| format!("unknown MCP server `{server}`"))?;
        let channel = managed.channel.as_ref().ok_or_else(|| {
            let reason = match &managed.state {
                ServerState::Failed(reason) => reason.clone(),
                ServerState::Ready => "connection lost".to_string(),
            };
            format!("MCP server `{server}` is not running ({reason})")
        })?;
        channel
            .request("tools/call", json!({"name": tool, "arguments": arguments}))
            .await
    }
}

/// Connect to one server: spawn, read tools, assign exposed names.
async fn connect_one(config: &McpServerConfig, used_names: &mut HashSet<String>) -> ManagedServer {
    let failure = |reason: String| ManagedServer {
        name: config.name.clone(),
        state: ServerState::Failed(reason),
        tools: Vec::new(),
        channel: None,
    };

    let timeout = Duration::from_millis(config.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS));
    let channel = match spawn_server(config, timeout).await {
        Ok(channel) => channel,
        Err(error) => {
            tracing::warn!(server = %config.name, "MCP server failed to start: {error}");
            return failure(error);
        }
    };

    // Handshake: initialize → initialized → tools/list.
    let init = match channel
        .request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "sick", "version": env!("CARGO_PKG_VERSION")},
            }),
        )
        .await
    {
        Ok(result) => result,
        Err(error) => {
            tracing::warn!(server = %config.name, "MCP initialize failed: {error}");
            return failure(format!("initialize failed: {error}"));
        }
    };
    let server_name = init
        .pointer("/serverInfo/name")
        .and_then(Value::as_str)
        .unwrap_or(&config.name)
        .to_string();
    let protocol = init
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_string();
    tracing::debug!(
        server = %config.name,
        remote_name = %server_name,
        protocol = %protocol,
        "MCP server initialized"
    );
    channel.notify("notifications/initialized", json!({})).await;

    let mut tools = match list_tools(&channel, &config.name).await {
        Ok(tools) => tools,
        Err(error) => {
            tracing::warn!(server = %config.name, "MCP tools/list failed: {error}");
            return failure(format!("tools/list failed: {error}"));
        }
    };
    for tool in &mut tools {
        tool.exposed_name = exposed_name(&config.name, &tool.tool, used_names);
    }
    let count = tools.len();
    tracing::info!(server = %config.name, tools = count, "MCP server ready");

    ManagedServer {
        name: config.name.clone(),
        state: ServerState::Ready,
        tools,
        channel: Some(channel),
    }
}

/// Spawn the server process and start its reader task.
async fn spawn_server(
    config: &McpServerConfig,
    timeout: Duration,
) -> Result<Arc<ServerChannel>, String> {
    let mut command = Command::new(config.command.trim());
    command
        .args(&config.args)
        .envs(&config.env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) if cfg!(windows) && error.kind() == std::io::ErrorKind::NotFound => {
            // Windows `.cmd`/`.bat` shims (npx, npm-installed servers) are not
            // executable directly; retry through the command interpreter.
            let mut wrapped = Command::new("cmd.exe");
            wrapped
                .arg("/C")
                .arg(config.command.trim())
                .args(&config.args)
                .envs(&config.env)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            wrapped.spawn().map_err(|wrapped_error| {
                format!(
                    "failed to start `{}`: {error} (cmd /C fallback: {wrapped_error})",
                    config.command
                )
            })?
        }
        Err(error) => {
            return Err(format!("failed to start `{}`: {error}", config.command));
        }
    };

    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| "server stdin was not piped".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "server stdout was not piped".to_string())?;
    if let Some(stderr) = child.stderr.take() {
        let name = config.name.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(server = %name, "mcp[stderr] {line}");
            }
        });
    }

    let channel = Arc::new(ServerChannel {
        stdin: AsyncMutex::new(stdin),
        pending: Mutex::new(HashMap::new()),
        next_id: AtomicU64::new(1),
        exited: AtomicBool::new(false),
        timeout,
        _child: Mutex::new(child),
    });

    // Reader task: dispatch responses to waiters, tolerate non-JSON noise
    // (some launchers print banners), fail pending requests on EOF.
    let reader_channel = Arc::clone(&channel);
    let reader_name = config.name.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(message) = serde_json::from_str::<Value>(line) else {
                tracing::debug!(server = %reader_name, "mcp: ignoring non-JSON line: {line}");
                continue;
            };
            let Some(id) = message.get("id").and_then(Value::as_u64) else {
                continue; // notification or server-initiated request
            };
            let waiter = reader_channel.pending.lock().unwrap().remove(&id);
            if let Some(waiter) = waiter {
                let outcome = match message.get("error") {
                    Some(error) => Err(format!(
                        "server returned error {}: {}",
                        error.get("code").and_then(Value::as_i64).unwrap_or(-1),
                        error
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown")
                    )),
                    None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                };
                let _ = waiter.send(outcome);
            }
        }
        reader_channel.exited.store(true, Ordering::SeqCst);
        let abandoned: Vec<_> = reader_channel
            .pending
            .lock()
            .unwrap()
            .drain()
            .map(|(_, waiter)| waiter)
            .collect();
        for waiter in abandoned {
            let _ = waiter.send(Err("MCP server exited".to_string()));
        }
    });

    Ok(channel)
}

/// `tools/list`, following `nextCursor` pagination.
async fn list_tools(channel: &ServerChannel, server: &str) -> Result<Vec<McpToolInfo>, String> {
    let mut tools = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let params = match &cursor {
            Some(cursor) => json!({"cursor": cursor}),
            None => json!({}),
        };
        let result = channel.request("tools/list", params).await?;
        let items = result
            .get("tools")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for item in items {
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            if name.is_empty() {
                continue;
            }
            let description = item
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let schema = item
                .get("inputSchema")
                .cloned()
                .filter(Value::is_object)
                .unwrap_or_else(|| json!({"type": "object"}));
            tools.push(McpToolInfo {
                exposed_name: String::new(),
                server: server.to_string(),
                tool: name,
                description,
                schema,
            });
        }
        cursor = result
            .get("nextCursor")
            .and_then(Value::as_str)
            .map(str::to_string);
        match cursor {
            Some(_) => continue,
            None => break,
        }
    }
    Ok(tools)
}

/// Sanitize + dedupe the model-facing tool name.
fn exposed_name(server: &str, tool: &str, used: &mut HashSet<String>) -> String {
    let sanitized: String = format!("{server}-{tool}")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let base: String = sanitized.chars().take(MAX_TOOL_NAME).collect();
    let mut candidate = base.clone();
    let mut suffix = 2;
    while !used.insert(candidate.clone()) {
        let tag = format!("-{suffix}");
        let keep = MAX_TOOL_NAME.saturating_sub(tag.len());
        candidate = format!("{}{tag}", base.chars().take(keep).collect::<String>());
        suffix += 1;
    }
    candidate
}

/// One live server connection: request/response plumbing over stdio.
struct ServerChannel {
    stdin: AsyncMutex<ChildStdin>,
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>,
    next_id: AtomicU64,
    exited: AtomicBool,
    timeout: Duration,
    // Keeps the process alive for the lifetime of the channel
    // (`kill_on_drop` reaps it when the manager is dropped).
    _child: Mutex<Child>,
}

impl ServerChannel {
    /// Send a request and wait for the matching response (or a timeout).
    async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        if self.exited.load(Ordering::SeqCst) {
            return Err("MCP server exited".to_string());
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, sender);

        let line = serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))
        .map_err(|error| format!("failed to serialize request: {error}"))?
            + "\n";

        let write_result = {
            let mut stdin = self.stdin.lock().await;
            match stdin.write_all(line.as_bytes()).await {
                Ok(()) => stdin.flush().await,
                Err(error) => Err(error),
            }
        };
        if let Err(error) = write_result {
            self.pending.lock().unwrap().remove(&id);
            return Err(format!("failed to write to MCP server: {error}"));
        }

        match tokio::time::timeout(self.timeout, receiver).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) => Err("MCP request was dropped".to_string()),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err(format!(
                    "MCP request `{method}` timed out after {}ms",
                    self.timeout.as_millis()
                ))
            }
        }
    }

    /// Send a notification (no id, no response).
    async fn notify(&self, method: &str, params: Value) {
        let Ok(line) = serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        })) else {
            return;
        };
        let mut stdin = self.stdin.lock().await;
        let _ = stdin.write_all((line + "\n").as_bytes()).await;
        let _ = stdin.flush().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposed_names_are_sanitized_and_deduped() {
        let mut used = HashSet::new();
        assert_eq!(
            exposed_name("my server", "read.file", &mut used),
            "my-server-read-file"
        );
        assert_eq!(
            exposed_name("my server", "read.file", &mut used),
            "my-server-read-file-2"
        );
        assert_eq!(
            exposed_name("my server", "read.file", &mut used),
            "my-server-read-file-3"
        );
    }

    #[test]
    fn exposed_names_are_capped_at_64_chars() {
        let mut used = HashSet::new();
        let long = "x".repeat(100);
        let name = exposed_name(&long, &long, &mut used);
        assert_eq!(name.len(), MAX_TOOL_NAME);
        // A colliding name must stay within the cap too.
        let second = exposed_name(&long, &long, &mut used);
        assert!(second.len() <= MAX_TOOL_NAME, "{second}");
        assert!(second.ends_with("-2"));
    }

    #[test]
    fn unconfigured_manager_is_empty() {
        let manager = McpManager {
            servers: Vec::new(),
        };
        assert!(manager.is_empty());
        assert_eq!(manager.tool_count(), 0);
        assert!(manager.statuses().is_empty());
        assert!(manager.tool_infos().is_empty());
    }
}
