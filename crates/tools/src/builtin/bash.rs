//! `bash` tool: run a shell command and capture its output.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use shell_config::SandboxConfig;

use crate::tool::{Tool, ToolContext, ToolResult, optional_usize, required_str};

/// Default command timeout.
const DEFAULT_TIMEOUT_MS: u64 = 30_000;

/// Maximum characters of combined output returned to the model.
const MAX_OUTPUT_CHARS: usize = 50_000;

/// Runs commands through the platform shell (`cmd.exe` on Windows, `sh` on
/// Unix). Override the shell binary with `SICK_SHELL` (legacy:
/// `SHELL_AGENT_SHELL`).
///
/// When `[sandbox]` is enabled in the config, commands run inside the
/// configured sandbox (bubblewrap / firejail / docker container); see
/// [`shell_shell::sandbox`].
pub struct BashTool {
    /// Sandbox settings from `[sandbox]` (disabled by default).
    pub sandbox: SandboxConfig,
}

impl BashTool {
    /// A bash tool running commands with the given sandbox settings.
    pub fn new(sandbox: SandboxConfig) -> Self {
        Self { sandbox }
    }

    fn truncate(mut text: String) -> String {
        if text.chars().count() > MAX_OUTPUT_CHARS {
            text = text.chars().take(MAX_OUTPUT_CHARS).collect::<String>();
            text.push_str("\n... [output truncated]");
        }
        text
    }
}

impl Default for BashTool {
    fn default() -> Self {
        Self::new(SandboxConfig::default())
    }
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Run a shell command in the working directory and return its stdout, \
         stderr, and exit code."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to run."
                },
                "timeout_ms": {
                    "type": "integer",
                    "description": "Timeout in milliseconds (default 30000)."
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let command = match required_str(&args, "command") {
            Ok(c) => c,
            Err(e) => return ToolResult::err(e),
        };
        let timeout_ms =
            optional_usize(&args, "timeout_ms").unwrap_or(DEFAULT_TIMEOUT_MS as usize) as u64;

        let (shell, _) = shell_shell::shell_command();
        let mut cmd = match shell_shell::sandbox::sandboxed_command(
            &self.sandbox,
            &command,
            &ctx.working_dir,
        ) {
            Ok(cmd) => cmd,
            Err(error) => {
                return ToolResult::err(format!("sandbox refused to run the command: {error}"));
            }
        };

        let child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => {
                return ToolResult::err(format!("failed to spawn shell `{shell}`: {e}"));
            }
        };

        let output =
            match tokio::time::timeout(Duration::from_millis(timeout_ms), child.wait_with_output())
                .await
            {
                Ok(Ok(output)) => output,
                Ok(Err(e)) => return ToolResult::err(format!("command failed to run: {e}")),
                Err(_) => {
                    return ToolResult::err(format!(
                        "command timed out after {timeout_ms}ms: {command}"
                    ));
                }
            };

        let exit_code = output.status.code();
        let mut text = String::new();
        text.push_str(&String::from_utf8_lossy(&output.stdout));
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.trim().is_empty() {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str("[stderr]\n");
            text.push_str(&stderr);
        }

        if text.trim().is_empty() {
            text.push_str("(no output)");
        }
        if let Some(code) = exit_code {
            text.push_str(&format!("\n[exit code: {code}]"));
        }

        let success = output.status.success();
        let mut result = if success {
            ToolResult::ok(text)
        } else {
            ToolResult::err(text)
        };
        result.exit_code = exit_code;
        result.output = BashTool::truncate(result.output);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::ToolContext;
    use serde_json::json;

    fn ctx() -> (tempfile::TempDir, ToolContext) {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(dir.path());
        (dir, ctx)
    }

    fn bash() -> BashTool {
        BashTool::default()
    }

    #[tokio::test]
    async fn runs_command_and_captures_output() {
        let (_dir, ctx) = ctx();
        let result = bash()
            .execute(json!({"command": "echo hello-from-bash"}), &ctx)
            .await;
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("hello-from-bash"));
        assert_eq!(result.exit_code, Some(0));
    }

    #[tokio::test]
    async fn runs_in_working_directory() {
        let (dir, ctx) = ctx();
        let result = bash().execute(json!({"command": "cd"}), &ctx).await;
        assert!(result.success, "{}", result.output);
        // Windows `cd` prints the cwd; Unix `cd` is a no-op, so just check it ran.
        if cfg!(windows) {
            let expected = dir.path().to_string_lossy().to_lowercase();
            let printed = result.output.to_lowercase();
            assert!(printed.contains(expected.trim_end()), "{}", result.output);
        }
    }

    #[tokio::test]
    async fn nonzero_exit_is_reported() {
        let (_dir, ctx) = ctx();
        let result = bash().execute(json!({"command": "exit 3"}), &ctx).await;
        assert!(!result.success);
        assert_eq!(result.exit_code, Some(3));
        assert!(result.output.contains("exit code: 3"));
    }

    #[tokio::test]
    async fn timeout_kills_command() {
        let (_dir, ctx) = ctx();
        let command = if cfg!(windows) {
            "ping -n 10 127.0.0.1 > nul"
        } else {
            "sleep 10"
        };
        let result = bash()
            .execute(json!({"command": command, "timeout_ms": 300}), &ctx)
            .await;
        assert!(!result.success);
        assert!(result.output.contains("timed out"));
    }

    #[tokio::test]
    async fn enabled_sandbox_with_unsupported_mode_refuses_to_run() {
        let (_dir, ctx) = ctx();
        let sandboxed = BashTool::new(SandboxConfig {
            enabled: true,
            mode: "definitely-not-a-backend".to_string(),
            ..Default::default()
        });
        let result = sandboxed
            .execute(json!({"command": "echo should-not-run"}), &ctx)
            .await;
        assert!(!result.success, "{}", result.output);
        assert!(
            result.output.contains("sandbox refused"),
            "{}",
            result.output
        );
        assert!(
            !result.output.contains("should-not-run"),
            "{}",
            result.output
        );
    }
}
