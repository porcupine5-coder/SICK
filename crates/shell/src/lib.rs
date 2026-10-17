//! Platform shell execution helpers for SICK.
//!
//! One place that decides which shell runs a command line (`cmd.exe` on
//! Windows, `sh` on Unix, overridable via `SICK_SHELL` / legacy
//! `SHELL_AGENT_SHELL`) and builds Tokio commands for it. Used by:
//!
//! - the `bash` tool (captured output, optionally sandboxed — see
//!   [`sandbox`]),
//! - the TUI shell passthrough (`!command`, `/shell` mode, streaming output),
//! - the CLI (`sick -c "command"`, inherited stdio).

pub mod sandbox;

use std::path::Path;
use std::process::Stdio;

use tokio::process::Command;

/// The platform shell binary plus its "run command" flag.
///
/// `SICK_SHELL` wins, then the legacy `SHELL_AGENT_SHELL`; both are expected
/// to accept `-c` on Unix and `/C` on Windows.
pub fn shell_command() -> (String, &'static str) {
    for var in ["SICK_SHELL", "SHELL_AGENT_SHELL"] {
        if let Ok(shell) = std::env::var(var) {
            let shell = shell.trim();
            if !shell.is_empty() {
                return (shell.to_string(), if cfg!(windows) { "/C" } else { "-c" });
            }
        }
    }
    if cfg!(windows) {
        ("cmd.exe".to_string(), "/C")
    } else {
        ("sh".to_string(), "-c")
    }
}

/// A command that runs `command_line` in `working_dir` with piped stdout and
/// stderr, no stdin, and `kill_on_drop` enabled.
pub fn command(command_line: &str, working_dir: &Path) -> Command {
    let (shell, flag) = shell_command();
    let mut cmd = Command::new(shell);
    cmd.arg(flag).arg(command_line);
    piped_command(cmd, working_dir)
}

/// A command that runs inside Kali Linux on WSL (or the platform shell on
/// non-Windows systems), preserving the workspace as its working directory.
pub fn kali_command(command_line: &str, working_dir: &Path) -> Command {
    #[cfg(windows)]
    {
        let distro = std::env::var("SICK_KALI_WSL_DISTRO")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "kali-linux".to_string());
        let script = kali_script(command_line, working_dir);
        let mut cmd = Command::new("wsl.exe");
        cmd.args(["--distribution", &distro, "--exec", "bash", "-lc", &script]);
        piped_command(cmd, working_dir)
    }
    #[cfg(not(windows))]
    {
        command(command_line, working_dir)
    }
}

#[cfg(windows)]
fn posix_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(windows)]
fn kali_script(command_line: &str, working_dir: &Path) -> String {
    format!(
        "cd -- \"$(wslpath -a -- {})\" && {command_line}",
        posix_quote(&working_dir.to_string_lossy())
    )
}

fn piped_command(mut cmd: Command, working_dir: &Path) -> Command {
    cmd.current_dir(working_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    cmd
}

/// Like [`command`], but stdio is inherited: output streams straight to the
/// terminal (used by the non-interactive `sick -c` path).
pub fn inherit_command(command_line: &str, working_dir: &Path) -> Command {
    let (shell, flag) = shell_command();
    let mut cmd = Command::new(shell);
    cmd.arg(flag)
        .arg(command_line)
        .current_dir(working_dir)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_command_returns_a_shell_and_flag() {
        let (shell, flag) = shell_command();
        assert!(!shell.trim().is_empty());
        assert!(flag == "-c" || flag == "/C");
    }

    #[cfg(windows)]
    #[test]
    fn posix_quote_escapes_single_quotes() {
        assert_eq!(posix_quote(r"C:\Users\O'Brien"), r"'C:\Users\O'\''Brien'");
    }

    #[cfg(windows)]
    #[test]
    fn kali_script_changes_to_workspace_before_running_command() {
        let workspace = Path::new(r"C:\Users\O'Brien\Shell Workspace");
        assert_eq!(
            kali_script("which nmap", workspace),
            r#"cd -- "$(wslpath -a -- 'C:\Users\O'\''Brien\Shell Workspace')" && which nmap"#
        );
    }

    #[tokio::test]
    async fn command_runs_and_captures_output() {
        let dir = std::env::temp_dir();
        let output = command("echo sick-shell-ok", &dir)
            .output()
            .await
            .expect("run echo");
        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("sick-shell-ok"), "stdout was: {stdout}");
    }
}
