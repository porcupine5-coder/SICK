//! Model Context Protocol (MCP) client support for SICK (Phase 6).
//!
//! Starts the MCP servers listed in `[mcp]` of the config file, speaks the
//! stdio JSON-RPC transport with each of them, and exposes their tools as
//! regular [`shell_tools::Tool`]s that the agent can call like any built-in.
//!
//! ```toml
//! [mcp]
//! servers = [
//!   { name = "filesystem", command = "npx", args = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"] },
//! ]
//! ```
//!
//! A server whose process fails to start (or whose handshake fails) is
//! reported in [`McpManager::statuses`] and simply contributes no tools; it
//! never blocks the rest of the agent. Server processes are killed when the
//! manager is dropped.
//!
//! The protocol subset implemented here: `initialize`,
//! `notifications/initialized`, `tools/list` (with cursor pagination), and
//! `tools/call`, over newline-delimited JSON-RPC 2.0 on stdin/stdout
//! (revision `2024-11-05`).

pub mod manager;
pub mod test_support;
pub mod tool;

pub use manager::{DEFAULT_TIMEOUT_MS, McpManager, McpServerStatus, PROTOCOL_VERSION};
pub use tool::{McpTool, content_to_text};
