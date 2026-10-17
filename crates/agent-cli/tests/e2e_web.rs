//! Phase 6 e2e: `S.I.C.K web` serves the web UI from the real binary.
//!
//! The example extension provides the LLM provider (no API key), so a
//! WebSocket client can run a full tool-calling turn against the real
//! `S.I.C.K web` process.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message as WsMessage;

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

fn free_local_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

fn spawn_web_sick(
    config_path: &Path,
    port: u16,
    home: &Path,
    cwd: &Path,
    example_provider: bool,
) -> WebChild {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sick"));
    cmd.args([
        "--config",
        &config_path.to_string_lossy(),
        "web",
        "--port",
        &port.to_string(),
    ])
    .current_dir(cwd)
    .env("SICK_HOME", home)
    .env("NO_PROXY", "127.0.0.1,localhost")
    .env("no_proxy", "127.0.0.1,localhost")
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    if example_provider {
        cmd.env("EXAMPLE_EXTENSION_PROVIDER", "1");
    }
    WebChild(cmd.spawn().expect("spawn S.I.C.K web"))
}

/// Kills the spawned `S.I.C.K web` process when the test ends, panics included
/// (failing asserts must not leak a server holding the extension directory).
struct WebChild(Child);

impl Drop for WebChild {
    fn drop(&mut self) {
        self.0.kill().ok();
        self.0.wait().ok();
    }
}

/// Wait until the port accepts TCP connections.
async fn wait_for_port(port: u16, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "sick web never opened port {port}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[test]
fn sick_web_serves_the_page_and_runs_turns() {
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
    let root = std::env::temp_dir().join(format!("sick-web-{stamp}"));
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

    let port = free_local_port();
    let child = spawn_web_sick(&config_path, port, &home, &workspace, true);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        wait_for_port(port, Duration::from_secs(30)).await;

        // `GET /` returns the embedded page.
        {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .unwrap();
            stream
                .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).await.unwrap();
            assert!(response.starts_with("HTTP/1.1 200"), "{response}");
            assert!(response.contains("<html"), "page body missing");
        }

        // One prompt over WebSocket: the canned provider calls
        // `example_message_count` first, then answers.
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/ws"))
            .await
            .unwrap();
        let payload = serde_json::json!({"prompt": "hello web"}).to_string();
        ws.send(WsMessage::Text(payload)).await.unwrap();

        let events = tokio::time::timeout(Duration::from_secs(30), async {
            let mut events: Vec<serde_json::Value> = Vec::new();
            while let Some(frame) = ws.next().await {
                let WsMessage::Text(text) = frame.unwrap() else {
                    continue;
                };
                let value: serde_json::Value = serde_json::from_str(text.as_str()).unwrap();
                let terminal = matches!(value["type"].as_str(), Some("done") | Some("error"));
                events.push(value);
                if terminal {
                    break;
                }
            }
            events
        })
        .await
        .expect("turn finished in time");

        let done = events.last().unwrap();
        assert_eq!(done["type"], "done", "{done}");
        assert_eq!(done["content"], "extension provider answer");

        let tool_call = events
            .iter()
            .find(|event| event["type"] == "tool_call")
            .expect("tool_call event");
        assert_eq!(tool_call["name"], "example_message_count");
        let tool_result = events
            .iter()
            .find(|event| event["type"] == "tool_result")
            .expect("tool_result event");
        assert_eq!(tool_result["success"], true);
    });

    // Kill the server before removing the extension directory it has locked.
    drop(child);
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn sick_web_reports_bad_config_before_serving() {
    let Some(_library) = fixture_library() else {
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
    let root = std::env::temp_dir().join(format!("sick-web-bad-{stamp}"));
    let workspace = root.join("workspace");
    let home = root.join("home");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&home).unwrap();

    // A config without provider override and without keys: `sick web` must
    // fail with the missing-key message instead of hanging.
    let config_path = root.join("settings.toml");
    std::fs::write(
        &config_path,
        "[llm]\ndefault_provider = \"openai\"\ndefault_model = \"gpt-4.1\"\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_sick"))
        .args([
            "--config",
            &config_path.to_string_lossy(),
            "web",
            "--port",
            &free_local_port().to_string(),
        ])
        .current_dir(&workspace)
        .env("SICK_HOME", &home)
        .env("SICK_KEYRING_FILE", home.join("keyring.json"))
        .env_remove("OPENAI_API_KEY")
        .output()
        .expect("run S.I.C.K web");
    assert!(!output.status.success(), "expected failure without keys");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("key"), "stderr: {stderr}");

    std::fs::remove_dir_all(&root).ok();
}
