//! Phase 6 e2e: an extension library can provide the LLM provider, letting
//! `sick` run with no API key at all.
//!
//! Uses the `shell-extension-example` library with
//! `EXAMPLE_EXTENSION_PROVIDER=1` (its canned provider answers every request
//! with `"extension provider answer"`).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Locate the example-extension library, building it on demand.
fn fixture_library() -> Option<PathBuf> {
    let target = workspace_root().join("target");
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let suffix = if cfg!(windows) {
        "dll"
    } else if cfg!(target_os = "macos") {
        "dylib"
    } else {
        "so"
    };
    let candidates: Vec<PathBuf> = ["example_extension", "libexample_extension"]
        .into_iter()
        .flat_map(|stem| {
            [
                target.join(profile).join(format!("{stem}.{suffix}")),
                target
                    .join(profile)
                    .join("deps")
                    .join(format!("{stem}.{suffix}")),
            ]
        })
        .collect();
    if let Some(found) = candidates.iter().find(|candidate| candidate.is_file()) {
        return Some(found.clone());
    }
    let status = std::process::Command::new(env!("CARGO"))
        .args(["build", "-p", "shell-extension-example"])
        .current_dir(workspace_root())
        .status();
    if !matches!(status, Ok(status) if status.success()) {
        return None;
    }
    candidates.into_iter().find(|candidate| candidate.is_file())
}

struct CliOutput {
    success: bool,
    stdout: String,
    stderr: String,
}

fn run_cli(args: &[&str], envs: &[(&str, &str)], cwd: &Path, timeout: Duration) -> CliOutput {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sick"));
    cmd.args(args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in envs {
        cmd.env(key, value);
    }

    let mut child = cmd.spawn().expect("spawn sick");
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait().expect("poll sick") {
            Some(status) => {
                let output = child.wait_with_output().expect("collect output");
                return CliOutput {
                    success: status.success(),
                    stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                };
            }
            None => {
                if Instant::now() > deadline {
                    let _ = child.kill();
                    panic!("sick timed out after {timeout:?}");
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

#[test]
fn extension_provider_runs_without_api_keys() {
    let Some(library) = fixture_library() else {
        eprintln!("skipping: example extension library was not built");
        return;
    };

    let stamp = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let root = std::env::temp_dir().join(format!("sick-ext-{stamp}"));
    let workspace = root.join("workspace");
    let home = root.join("home");
    let extensions = root.join("extensions");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&extensions).unwrap();
    std::fs::copy(&library, extensions.join(library.file_name().unwrap())).unwrap();

    let config_path = root.join("settings.toml");
    std::fs::write(
        &config_path,
        format!(
            "[llm]\n\
             default_provider = \"openai\"\n\
             default_model = \"test-model\"\n\n\
             [extensions]\n\
             dir = '{}'\n",
            extensions.display()
        ),
    )
    .unwrap();

    let output = run_cli(
        &[
            "--config",
            &config_path.to_string_lossy(),
            "hello from the extension test",
        ],
        &[
            ("SICK_HOME", &home.to_string_lossy()),
            ("EXAMPLE_EXTENSION_PROVIDER", "1"),
            ("NO_PROXY", "127.0.0.1,localhost"),
        ],
        &workspace,
        Duration::from_secs(60),
    );

    assert!(
        output.success,
        "sick failed.\nstdout: {}\nstderr: {}",
        output.stdout, output.stderr
    );
    assert!(
        output.stdout.contains("extension provider answer"),
        "canned provider answer missing from stdout: {}",
        output.stdout
    );
    // No key files were needed, but the session still persisted.
    let db_path = home.join("sessions.db");
    assert!(db_path.is_file(), "sessions.db missing");

    // The canned provider called `example_message_count`, which reads the
    // counter fed by the extension's `on_message` hook: the tool result must
    // be a positive number, proving the whole extension loop ran.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let store = shell_memory::MemoryStore::init(&db_path.to_string_lossy())
            .await
            .expect("open session store");
        let sessions = store.list_sessions().await.expect("list sessions");
        let history = store
            .load_session_history(&sessions[0].id)
            .await
            .expect("load history");
        let tool_message = history
            .iter()
            .find(|message| message.role == "tool")
            .expect("tool message from extension tool");
        let count: usize = tool_message
            .content
            .as_deref()
            .unwrap_or_default()
            .trim()
            .parse()
            .expect("tool result is a count");
        assert!(count >= 1, "on_message hook never fired (count {count})");
    });

    std::fs::remove_dir_all(&root).ok();
}
