//! Configuration loading for S.I.C.K.
//!
//! Loads TOML configuration files (see `config/example-config.toml`) into typed
//! [`ShellConfig`] values. Besides `[llm]`, the file can configure MCP servers
//! (`[mcp]`), sandboxed tool execution (`[sandbox]`), dynamic-library plugins
//! (`[extensions]`), and the local web UI (`[web]`) — all Phase 6.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use shell_common::{ShellError, ShellResult};

/// Top-level S.I.C.K configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ShellConfig {
    /// `[llm]` section.
    #[serde(default)]
    pub llm: LlmConfig,
    /// `[mcp]` section: Model Context Protocol servers (Phase 6).
    #[serde(default)]
    pub mcp: McpConfig,
    /// `[sandbox]` section: sandboxed `bash` tool execution (Phase 6).
    #[serde(default)]
    pub sandbox: SandboxConfig,
    /// `[extensions]` section: dynamic-library plugins (Phase 6).
    #[serde(default)]
    pub extensions: ExtensionsConfig,
    /// `[web]` section: the local web UI (Phase 6).
    #[serde(default)]
    pub web: WebConfig,
}

/// `[mcp]` section: MCP servers started alongside the agent.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct McpConfig {
    /// Servers launched over stdio; their tools join the tool registry.
    #[serde(default)]
    pub servers: Vec<McpServerConfig>,
}

/// One `[[mcp.servers]]` entry (inline `{ ... }` tables also work).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Short unique name; exposed tool names are prefixed with it.
    pub name: String,
    /// Executable to launch (resolved through `PATH`; `.cmd` shims work on
    /// Windows via an automatic `cmd /C` fallback).
    pub command: String,
    /// Command-line arguments.
    #[serde(default)]
    pub args: Vec<String>,
    /// Extra environment variables for the server process.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Per-request timeout in milliseconds (default 60000).
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

/// `[sandbox]` section: sandboxed execution for the `bash` tool.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SandboxConfig {
    /// Turn sandboxing on (default `false`: commands run directly).
    #[serde(default)]
    pub enabled: bool,
    /// Backend: `"bwrap"`/`"firejail"` (Linux), `"container"` (docker, any OS).
    /// Empty = pick the platform default at run time.
    #[serde(default)]
    pub mode: String,
    /// Host directories the sandboxed command may read and write. Everything
    /// else on the system stays read-only (bwrap) or absent (firejail).
    #[serde(default)]
    pub allowed_dirs: Vec<PathBuf>,
    /// Disable network access inside the sandbox.
    #[serde(default)]
    pub deny_network: bool,
    /// Container image used by `mode = "container"`.
    #[serde(default)]
    pub image: String,
}

/// `[extensions]` section: dynamic-library plugins (Phase 6).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExtensionsConfig {
    /// Directory scanned for extension libraries (`.dll`/`.so`/`.dylib`).
    /// Empty = `<SICK_HOME>/extensions`. A leading `~` expands to the home
    /// directory.
    #[serde(default)]
    pub dir: String,
}

/// `[web]` section: the local web UI (Phase 6).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebConfig {
    /// Serve the web UI alongside the interactive TUI (default `false`).
    /// `sick web` starts it explicitly regardless of this flag.
    #[serde(default)]
    pub enabled: bool,
    /// Listen port (`sick web --port` overrides it).
    #[serde(default = "default_web_port")]
    pub port: u16,
}

/// Default port for the web UI (matches the Phase 6 spec).
pub fn default_web_port() -> u16 {
    8080
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            port: default_web_port(),
        }
    }
}

/// LLM defaults (`[llm]` section).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    /// Provider identifier, e.g. `"openai"`.
    pub default_provider: String,
    /// Model identifier, e.g. `"gpt-4.1"`.
    pub default_model: String,
    /// Sampling temperature.
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    /// Maximum number of output tokens.
    #[serde(default = "default_max_tokens")]
    pub max_tokens: usize,
    /// Optional API base URL override (e.g. an OpenAI-compatible endpoint).
    #[serde(default)]
    pub base_url: Option<String>,
    /// Providers to try (in order) when the default provider's keys fail.
    #[serde(default)]
    pub fallback_chain: Vec<FallbackEntry>,
}

/// One entry of `[llm].fallback_chain`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FallbackEntry {
    /// Provider identifier, e.g. `"anthropic"`.
    pub provider: String,
    /// Model to use with this provider.
    pub model: String,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            default_provider: "openai".to_string(),
            default_model: "gpt-4.1".to_string(),
            temperature: default_temperature(),
            max_tokens: default_max_tokens(),
            base_url: None,
            fallback_chain: Vec::new(),
        }
    }
}

fn default_temperature() -> f32 {
    0.2
}

fn default_max_tokens() -> usize {
    64_000
}

impl ShellConfig {
    /// Load configuration from a TOML file.
    pub fn load(path: &Path) -> ShellResult<Self> {
        let raw = std::fs::read_to_string(path)?;
        Self::parse(&raw)
    }

    /// Parse configuration from a TOML string.
    pub fn parse(raw: &str) -> ShellResult<Self> {
        toml::from_str(raw).map_err(|err| ShellError::Config(err.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_example_config() {
        // The example file is user-editable documentation; only assert that it
        // parses and that defaults/required values hold.
        let cfg = ShellConfig::parse(include_str!("../../../config/example-config.toml"))
            .expect("example config parses");
        assert!(!cfg.llm.default_provider.trim().is_empty());
        assert!(!cfg.llm.default_model.trim().is_empty());
        assert!((cfg.llm.temperature - 0.2).abs() < f32::EPSILON);
        assert_eq!(cfg.llm.max_tokens, 64_000);
    }

    #[test]
    fn defaults_apply_when_optional_fields_missing() {
        let cfg = ShellConfig::parse(
            r#"
            [llm]
            default_provider = "anthropic"
            default_model = "claude-3-7-sonnet-20250219"
            "#,
        )
        .expect("config parses");
        assert_eq!(cfg.llm.temperature, 0.2);
        assert_eq!(cfg.llm.max_tokens, 64_000);
        assert_eq!(cfg.llm.base_url, None);
        assert!(cfg.llm.fallback_chain.is_empty());
    }

    #[test]
    fn parses_fallback_chain_entries() {
        let cfg = ShellConfig::parse(
            r#"
            [llm]
            default_provider = "openai"
            default_model = "gpt-4.1"
            fallback_chain = [
              { provider = "anthropic", model = "claude-3-7-sonnet-20250219" },
              { provider = "google", model = "gemini-2.0-flash" },
            ]
            "#,
        )
        .expect("config parses");
        assert_eq!(cfg.llm.fallback_chain.len(), 2);
        assert_eq!(cfg.llm.fallback_chain[0].provider, "anthropic");
        assert_eq!(cfg.llm.fallback_chain[1].model, "gemini-2.0-flash");
    }

    #[test]
    fn defaults_apply_when_mcp_and_sandbox_are_missing() {
        let cfg = ShellConfig::parse(
            r#"
            [llm]
            default_provider = "openai"
            default_model = "gpt-4.1"
            "#,
        )
        .expect("config parses");
        assert!(cfg.mcp.servers.is_empty());
        assert!(!cfg.sandbox.enabled);
        assert!(cfg.sandbox.allowed_dirs.is_empty());
        assert!(!cfg.sandbox.deny_network);
    }

    #[test]
    fn parses_mcp_servers() {
        let cfg = ShellConfig::parse(
            r#"
            [llm]
            default_provider = "openai"
            default_model = "gpt-4.1"

            [mcp]
            servers = [
              { name = "filesystem", command = "npx", args = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"] },
              { name = "search", command = "brave-search-mcp", env = { BRAVE_API_KEY = "x" }, timeout_ms = 5000 },
            ]
            "#,
        )
        .expect("config parses");
        assert_eq!(cfg.mcp.servers.len(), 2);
        let filesystem = &cfg.mcp.servers[0];
        assert_eq!(filesystem.name, "filesystem");
        assert_eq!(filesystem.command, "npx");
        assert_eq!(filesystem.args.len(), 3);
        assert_eq!(filesystem.timeout_ms, None);
        let search = &cfg.mcp.servers[1];
        assert_eq!(
            search.env.get("BRAVE_API_KEY").map(String::as_str),
            Some("x")
        );
        assert_eq!(search.timeout_ms, Some(5000));
    }

    #[test]
    fn parses_sandbox_section() {
        let cfg = ShellConfig::parse(
            r#"
            [llm]
            default_provider = "openai"
            default_model = "gpt-4.1"

            [sandbox]
            enabled = true
            mode = "bwrap"
            allowed_dirs = ["/home/user/projects", "/tmp/work"]
            deny_network = true
            "#,
        )
        .expect("config parses");
        assert!(cfg.sandbox.enabled);
        assert_eq!(cfg.sandbox.mode, "bwrap");
        assert_eq!(cfg.sandbox.allowed_dirs.len(), 2);
        assert!(cfg.sandbox.deny_network);
        assert!(cfg.sandbox.image.is_empty());
    }

    #[test]
    fn parses_extensions_section() {
        let cfg = ShellConfig::parse(
            r#"
            [llm]
            default_provider = "openai"
            default_model = "gpt-4.1"

            [extensions]
            dir = "~/.sick/extensions"
            "#,
        )
        .expect("config parses");
        assert_eq!(cfg.extensions.dir, "~/.sick/extensions");

        // Missing section defaults to empty (resolved to <SICK_HOME>/extensions).
        let cfg = ShellConfig::parse(
            r#"
            [llm]
            default_provider = "openai"
            default_model = "gpt-4.1"
            "#,
        )
        .expect("config parses");
        assert!(cfg.extensions.dir.is_empty());
    }

    #[test]
    fn parses_web_section() {
        let cfg = ShellConfig::parse(
            r#"
            [llm]
            default_provider = "openai"
            default_model = "gpt-4.1"

            [web]
            enabled = true
            port = 9000
            "#,
        )
        .expect("config parses");
        assert!(cfg.web.enabled);
        assert_eq!(cfg.web.port, 9000);

        // Defaults: disabled, port 8080.
        let cfg = ShellConfig::parse(
            r#"
            [llm]
            default_provider = "openai"
            default_model = "gpt-4.1"
            "#,
        )
        .expect("config parses");
        assert!(!cfg.web.enabled);
        assert_eq!(cfg.web.port, 8080);

        // A partial section keeps the default port.
        let cfg = ShellConfig::parse(
            r#"
            [llm]
            default_provider = "openai"
            default_model = "gpt-4.1"

            [web]
            enabled = true
            "#,
        )
        .expect("config parses");
        assert_eq!(cfg.web.port, 8080);
    }
}
