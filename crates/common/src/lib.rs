//! Shared types and error handling for the SICK workspace.

pub use anyhow;
pub use thiserror;

/// Errors returned across SICK crates.
#[derive(thiserror::Error, Debug)]
pub enum ShellError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Config error: {0}")]
    Config(String),
    #[error("LLM error: {0}")]
    Llm(String),
    #[error("Key manager error: {0}")]
    KeyManager(String),
    #[error("Memory error: {0}")]
    Memory(String),
    #[error("Tool error: {0}")]
    Tool(String),
    #[error("{0}")]
    Other(String),
}

/// Convenience alias used throughout the workspace.
pub type ShellResult<T> = Result<T, ShellError>;

/// Root directory for SICK state (keys, sessions, config).
///
/// Resolution order:
/// 1. `SICK_HOME` when set (non-empty),
/// 2. legacy `SHELL_AGENT_HOME` when set (non-empty),
/// 3. `~/.sick` when that directory already exists,
/// 4. legacy `~/.shell-agent` when that directory already exists,
/// 5. `~/.sick` (created on demand by callers).
pub fn sick_home() -> std::path::PathBuf {
    let base = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    pick_home(
        std::env::var("SICK_HOME").ok(),
        std::env::var("SHELL_AGENT_HOME").ok(),
        &base,
    )
}

/// Pure home resolution (split out so the precedence rules are unit-testable).
fn pick_home(
    sick_env: Option<String>,
    legacy_env: Option<String>,
    base: &std::path::Path,
) -> std::path::PathBuf {
    for value in [sick_env, legacy_env].into_iter().flatten() {
        let value = value.trim();
        if !value.is_empty() {
            return std::path::PathBuf::from(value);
        }
    }
    let current = base.join(".sick");
    if current.is_dir() {
        return current;
    }
    let legacy = base.join(".shell-agent");
    if legacy.is_dir() {
        return legacy;
    }
    current
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_error_message_is_formatted() {
        let err = ShellError::Config("missing api key".into());
        assert_eq!(err.to_string(), "Config error: missing api key");
    }

    #[test]
    fn io_errors_convert_automatically() {
        let err: ShellError = std::io::Error::new(std::io::ErrorKind::NotFound, "nope").into();
        assert!(matches!(err, ShellError::Io(_)));
    }

    #[test]
    fn home_env_vars_win_over_directories() {
        let base = std::path::Path::new("C:/base");
        assert_eq!(
            pick_home(Some("C:/sick".into()), Some("C:/legacy".into()), base),
            std::path::PathBuf::from("C:/sick"),
        );
        assert_eq!(
            pick_home(None, Some("C:/legacy".into()), base),
            std::path::PathBuf::from("C:/legacy"),
        );
        // Blank env values are ignored and fall through to directory detection.
        assert_eq!(
            pick_home(Some("  ".into()), None, base),
            std::path::PathBuf::from("C:/base/.sick"),
        );
    }

    #[test]
    fn home_falls_back_to_legacy_dir_until_sick_dir_exists() {
        let dir = tempfile::tempdir().expect("tempdir");
        let base = dir.path();
        // Neither dir exists: default to the new `.sick`.
        assert_eq!(pick_home(None, None, base), base.join(".sick"));
        // Only the legacy dir exists: keep using it (existing data).
        std::fs::create_dir_all(base.join(".shell-agent")).unwrap();
        assert_eq!(pick_home(None, None, base), base.join(".shell-agent"));
        // Once `.sick` exists it wins, even next to the legacy dir.
        std::fs::create_dir_all(base.join(".sick")).unwrap();
        assert_eq!(pick_home(None, None, base), base.join(".sick"));
    }
}
