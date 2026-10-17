//! Sandboxed command execution (Phase 6).
//!
//! The `bash` tool can run every command inside an OS-level sandbox:
//!
//! - **Linux**: `bwrap` (bubblewrap) with a read-only system and read-write
//!   binds for the workspace plus `allowed_dirs`, or `firejail` with
//!   `--whitelist` entries.
//! - **Any OS with docker**: `mode = "container"` runs the command in a
//!   throwaway container with only the workspace and `allowed_dirs` mounted.
//!
//! The workspace directory is always mounted read-write (that is the tool's
//! own working area); `allowed_dirs` grants additional read-write mounts.
//! `deny_network = true` disables networking inside the sandbox.
//!
//! Fail-closed by design: when `[sandbox].enabled = true` and no usable
//! backend exists on this platform, [`sandboxed_command`] returns an error and
//! the command does *not* run unsandboxed.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use shell_common::{ShellError, ShellResult};
use shell_config::SandboxConfig;
use tokio::process::Command;

/// Accepted `[sandbox].mode` values.
pub const MODES: &[&str] = &["bwrap", "firejail", "container"];

/// Check a sandbox configuration without running anything.
///
/// Returns `Ok(())` when sandboxing is disabled or usable on this platform,
/// and a human-readable reason otherwise (used for startup warnings and
/// `SandboxConfig` validation).
pub fn validate(cfg: &SandboxConfig) -> ShellResult<()> {
    if !cfg.enabled {
        return Ok(());
    }
    let mode = resolved_mode(cfg)?;
    match mode.as_str() {
        "bwrap" | "firejail" => {
            if !binary_on_path(&mode) {
                return Err(ShellError::Config(format!(
                    "sandbox mode `{mode}` is selected but `{mode}` was not found on PATH \
                     (install bubblewrap/firejail, use mode = \"container\", or set \
                     [sandbox] enabled = false)"
                )));
            }
        }
        "container" => {
            if cfg.image.trim().is_empty() {
                return Err(ShellError::Config(
                    "sandbox mode `container` needs [sandbox] image = \"<docker image>\"".into(),
                ));
            }
            if !binary_on_path("docker") {
                return Err(ShellError::Config(
                    "sandbox mode `container` needs docker on PATH".into(),
                ));
            }
        }
        other => unreachable!("resolved_mode validates the mode (got `{other}`)"),
    }
    Ok(())
}

/// Build the command for `command_line` under the given sandbox settings.
///
/// When the sandbox is disabled this is exactly
/// [`crate::command`] (direct platform shell). When it is enabled the
/// returned command runs through the selected backend, or returns an error
/// explaining what is missing (the command then never runs).
pub fn sandboxed_command(
    cfg: &SandboxConfig,
    command_line: &str,
    working_dir: &Path,
) -> ShellResult<Command> {
    if !cfg.enabled {
        return Ok(crate::command(command_line, working_dir));
    }
    validate(cfg)?;

    let mode = resolved_mode(cfg)?;
    let mounts = mount_dirs(cfg, working_dir);
    let mut cmd = match mode.as_str() {
        "bwrap" => {
            let args = bwrap_argv(command_line, working_dir, &mounts, cfg.deny_network);
            command_with("bwrap", &args)
        }
        "firejail" => {
            let args = firejail_argv(command_line, working_dir, &mounts, cfg.deny_network);
            command_with("firejail", &args)
        }
        "container" => {
            let args = docker_argv(
                cfg.image.trim(),
                command_line,
                working_dir,
                &mounts,
                cfg.deny_network,
            );
            command_with("docker", &args)
        }
        other => unreachable!("resolved_mode validates the mode (got `{other}`)"),
    };
    cmd.current_dir(working_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    Ok(cmd)
}

/// Resolve the effective backend for this platform, validating `mode`.
fn resolved_mode(cfg: &SandboxConfig) -> ShellResult<String> {
    let requested = cfg.mode.trim();
    if !requested.is_empty() {
        if !MODES.contains(&requested) {
            return Err(ShellError::Config(format!(
                "unknown [sandbox] mode `{requested}` (expected one of: {})",
                MODES.join(", ")
            )));
        }
        if (requested == "bwrap" || requested == "firejail") && !cfg!(target_os = "linux") {
            return Err(ShellError::Config(format!(
                "sandbox mode `{requested}` is Linux-only; use mode = \"container\" on this \
                 platform (or disable the sandbox)"
            )));
        }
        return Ok(requested.to_string());
    }

    if cfg!(target_os = "linux") {
        // Prefer bwrap (no setuid), then firejail.
        for candidate in ["bwrap", "firejail"] {
            if binary_on_path(candidate) {
                return Ok(candidate.to_string());
            }
        }
    }
    Ok("container".to_string())
}

/// The read-write mounts: the workspace plus every configured allowed dir
/// (workspace first, duplicates removed).
fn mount_dirs(cfg: &SandboxConfig, working_dir: &Path) -> Vec<PathBuf> {
    let mut mounts = vec![working_dir.to_path_buf()];
    for dir in &cfg.allowed_dirs {
        if !mounts.iter().any(|existing| existing == dir) {
            mounts.push(dir.clone());
        }
    }
    mounts
}

/// bubblewrap argv: read-only `/`, then the read-write binds, then the shell.
fn bwrap_argv(
    command_line: &str,
    working_dir: &Path,
    mounts: &[PathBuf],
    deny_network: bool,
) -> Vec<String> {
    let (shell, flag) = crate::shell_command();
    let mut args = vec![
        "--die-with-parent".to_string(),
        "--ro-bind".to_string(),
        "/".to_string(),
        "/".to_string(),
        "--dev".to_string(),
        "/dev".to_string(),
        "--proc".to_string(),
        "/proc".to_string(),
        "--tmpfs".to_string(),
        "/tmp".to_string(),
    ];
    if deny_network {
        args.push("--unshare-net".to_string());
    }
    for dir in mounts {
        args.push("--bind".to_string());
        args.push(dir.to_string_lossy().into_owned());
        args.push(dir.to_string_lossy().into_owned());
    }
    args.push("--chdir".to_string());
    args.push(working_dir.to_string_lossy().into_owned());
    args.push("--".to_string());
    args.push(shell);
    args.push(flag.to_string());
    args.push(command_line.to_string());
    args
}

/// firejail argv: quiet profile with `--whitelist` for each mount.
fn firejail_argv(
    command_line: &str,
    _working_dir: &Path,
    mounts: &[PathBuf],
    deny_network: bool,
) -> Vec<String> {
    let (shell, flag) = crate::shell_command();
    let mut args = vec![
        "--quiet".to_string(),
        "--nosound".to_string(),
        "--novideo".to_string(),
    ];
    if deny_network {
        args.push("--net=none".to_string());
    }
    for dir in mounts {
        args.push(format!("--whitelist={}", dir.to_string_lossy()));
    }
    args.push(shell);
    args.push(flag.to_string());
    args.push(command_line.to_string());
    args
}

/// docker argv: throwaway container, mounts bound at their host paths.
fn docker_argv(
    image: &str,
    command_line: &str,
    working_dir: &Path,
    mounts: &[PathBuf],
    deny_network: bool,
) -> Vec<String> {
    let mut args = vec![
        "run".to_string(),
        "--rm".to_string(),
        "-i".to_string(),
        "-w".to_string(),
        working_dir.to_string_lossy().into_owned(),
    ];
    if deny_network {
        args.push("--network=none".to_string());
    }
    for dir in mounts {
        let dir = dir.to_string_lossy().into_owned();
        args.push("-v".to_string());
        args.push(format!("{dir}:{dir}"));
    }
    args.push(image.to_string());
    args.push("sh".to_string());
    args.push("-lc".to_string());
    args.push(command_line.to_string());
    args
}

/// A piped-output command for `program` with `args` (stdio set by callers).
fn command_with(program: &str, args: &[String]) -> Command {
    let mut cmd = Command::new(program);
    cmd.args(args);
    cmd
}

/// Whether `binary` (or `binary.exe` on Windows) is on `PATH`.
fn binary_on_path(binary: &str) -> bool {
    let path = match std::env::var_os("PATH") {
        Some(path) => path,
        None => return false,
    };
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(binary);
        if candidate.is_file() {
            return true;
        }
        #[cfg(windows)]
        if dir.join(format!("{binary}.exe")).is_file() {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(enabled: bool, mode: &str) -> SandboxConfig {
        SandboxConfig {
            enabled,
            mode: mode.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn disabled_sandbox_returns_a_plain_shell_command() {
        let cmd = sandboxed_command(&cfg(false, ""), "echo hi", Path::new(".")).unwrap();
        let program = cmd.as_std().get_program().to_string_lossy().into_owned();
        let (shell, _) = crate::shell_command();
        assert_eq!(program, shell);
    }

    #[test]
    fn bwrap_argv_has_readonly_root_binds_and_shell_tail() {
        let work = PathBuf::from("/home/user/projects/repo");
        let mounts = vec![work.clone(), PathBuf::from("/data")];
        let args = bwrap_argv("echo hi", &work, &mounts, true);

        assert_eq!(args[0], "--die-with-parent");
        // Read-only system, then read-write binds for workspace + allowed dirs.
        let ro = args.windows(3).position(|w| w == ["--ro-bind", "/", "/"]);
        assert!(ro.is_some(), "missing --ro-bind /: {args:?}");
        let bind = args.windows(3).position(|w| w[0] == "--bind");
        let bind = bind.expect("missing --bind");
        assert!(bind > ro.unwrap(), "binds must come after the ro-bind");
        assert!(args.contains(&"--unshare-net".to_string()));
        assert!(args.contains(&"--chdir".to_string()));
        assert!(args.contains(&"/home/user/projects/repo".to_string()));
        assert!(args.contains(&"/data".to_string()));
        // Shell + flag + command tail.
        let (shell, flag) = crate::shell_command();
        let tail = &args[args.len() - 3..];
        assert_eq!(tail, [shell, flag.to_string(), "echo hi".to_string()]);
    }

    #[test]
    fn firejail_argv_whitelists_mounts_and_disables_network() {
        let work = PathBuf::from("/work");
        let mounts = vec![work.clone(), PathBuf::from("/data")];
        let args = firejail_argv("ls", &work, &mounts, true);
        assert!(args.contains(&"--net=none".to_string()));
        assert!(args.contains(&"--whitelist=/work".to_string()));
        assert!(args.contains(&"--whitelist=/data".to_string()));
        let (shell, flag) = crate::shell_command();
        let tail = &args[args.len() - 3..];
        assert_eq!(tail, [shell, flag.to_string(), "ls".to_string()]);
    }

    #[test]
    fn docker_argv_mounts_host_paths_and_sets_workdir() {
        let work = PathBuf::from("/work/repo");
        let mounts = vec![work.clone(), PathBuf::from("/data")];
        let args = docker_argv("alpine:3", "echo hi", &work, &mounts, true);
        assert_eq!(args[0], "run");
        assert!(args.contains(&"--rm".to_string()));
        let w = args.iter().position(|a| a == "-w").expect("-w missing");
        assert_eq!(args[w + 1], "/work/repo");
        assert!(args.contains(&"--network=none".to_string()));
        assert!(args.contains(&"/data:/data".to_string()));
        assert!(args.contains(&"/work/repo:/work/repo".to_string()));
        // Image, then POSIX shell command tail.
        let image = args.iter().position(|a| a == "alpine:3").expect("image");
        assert_eq!(&args[image + 1..], ["sh", "-lc", "echo hi"]);
    }

    #[test]
    fn unknown_mode_is_rejected() {
        let err = sandboxed_command(&cfg(true, "chroot"), "ls", Path::new(".")).unwrap_err();
        assert!(err.to_string().contains("unknown [sandbox] mode"));
    }

    #[test]
    fn linux_only_modes_are_rejected_elsewhere() {
        if !cfg!(target_os = "linux") {
            let err = sandboxed_command(&cfg(true, "bwrap"), "ls", Path::new(".")).unwrap_err();
            assert!(err.to_string().contains("Linux-only"));
        }
    }

    #[test]
    fn container_mode_requires_an_image() {
        let err = sandboxed_command(&cfg(true, "container"), "ls", Path::new(".")).unwrap_err();
        assert!(err.to_string().contains("image"), "{err}");
    }

    #[test]
    fn validate_is_ok_when_disabled() {
        assert!(validate(&cfg(false, "bwrap")).is_ok());
        assert!(validate(&SandboxConfig::default()).is_ok());
    }

    #[test]
    fn validate_reports_missing_binaries() {
        if cfg!(target_os = "linux") {
            // On Linux the default resolves to bwrap/firejail/container; any
            // missing tool must produce an error mentioning it.
            if let Err(err) = validate(&cfg(true, "")) {
                let text = err.to_string();
                assert!(
                    text.contains("bwrap") || text.contains("firejail") || text.contains("docker"),
                    "{text}"
                );
            }
        }
    }
}
