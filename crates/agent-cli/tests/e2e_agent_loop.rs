//! Full agent-loop end-to-end test against a local mock OpenAI server.
//!
//! Proves, without any real API key:
//! 1. `sick "<prompt>"` runs non-interactively.
//! 2. The LLM response (streamed SSE) triggers the `write` tool.
//! 3. The tool creates `hello.txt` in the working directory.
//! 4. The session is persisted to SQLite and can be loaded back.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// SSE response that asks the agent to call `write`.
fn tool_call_response() -> String {
    concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"tool_calls\":[{\"index\":0,\"id\":\"call_e2e\",\"type\":\"function\",\"function\":{\"name\":\"write\",\"arguments\":\"\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"path\\\": \\\"hello.txt\\\", \\\"content\\\": \\\"Hello, World!\\\"}\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n",
    )
    .to_string()
}

/// SSE response with the final assistant text.
fn final_text_response() -> String {
    concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Done: created hello.txt.\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    )
    .to_string()
}

/// Serve `responses`, one per HTTP request, from a background thread.
fn spawn_mock_server(responses: Vec<String>) -> (u16, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
    let port = listener.local_addr().unwrap().port();

    let handle = std::thread::spawn(move || {
        for body in responses {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            read_http_request(&mut stream);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });

    (port, handle)
}

/// Read request headers + body so the client finishes sending before we reply.
fn read_http_request(stream: &mut std::net::TcpStream) {
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];

    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        match stream.read(&mut tmp) {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
        }
    }

    let headers = String::from_utf8_lossy(&buf);
    let content_length = headers
        .lines()
        .find_map(|line| {
            let lower = line.to_ascii_lowercase();
            lower
                .strip_prefix("content-length:")
                .and_then(|v| v.trim().parse::<usize>().ok())
        })
        .unwrap_or(0);

    let header_end = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|p| p + 4)
        .unwrap_or(buf.len());
    let mut remaining = content_length.saturating_sub(buf.len() - header_end);

    while remaining > 0 {
        match stream.read(&mut tmp) {
            Ok(0) | Err(_) => break,
            Ok(n) => remaining = remaining.saturating_sub(n),
        }
    }
}

struct CliOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// Run the CLI with a watchdog so a hung agent fails the test instead of
/// blocking forever.
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
                let mut stdout = Vec::new();
                let mut stderr = Vec::new();
                if let Some(mut out) = child.stdout.take() {
                    out.read_to_end(&mut stdout).ok();
                }
                if let Some(mut err) = child.stderr.take() {
                    err.read_to_end(&mut stderr).ok();
                }
                return CliOutput {
                    status,
                    stdout,
                    stderr,
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

/// Shared goal-test driver, parameterized by provider and key env var.
///
/// `provider` is also asserted back out of the persisted session row, so the
/// google run proves the `GEMINI_API_KEY` → `provider = "google"` plumbing.
fn run_goal_test(provider: &str, key_env: &str, key_value: &str) {
    // 1. Mock server: first request → tool call, second → final text.
    let (port, server) = spawn_mock_server(vec![tool_call_response(), final_text_response()]);

    // 2. Isolated workspace + state dir.
    let stamp = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let root = std::env::temp_dir().join(format!("sick-loop-{provider}-{stamp}"));
    let workspace = root.join("workspace");
    let home = root.join("home");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&home).unwrap();

    // 3. Config pointing the provider at the mock server.
    let config_path = root.join("settings.toml");
    std::fs::write(
        &config_path,
        format!(
            "[llm]\n\
             default_provider = \"{provider}\"\n\
             default_model = \"test-model\"\n\
             temperature = 0.0\n\
             max_tokens = 256\n\
             base_url = \"http://127.0.0.1:{port}/v1\"\n"
        ),
    )
    .unwrap();

    // 4. Run the goal-test prompt non-interactively.
    let output = run_cli(
        &[
            "--config",
            &config_path.to_string_lossy(),
            "create a hello world file in the current directory",
        ],
        &[
            ("SICK_HOME", &home.to_string_lossy()),
            (key_env, key_value),
            ("NO_PROXY", "127.0.0.1,localhost"),
            ("no_proxy", "127.0.0.1,localhost"),
        ],
        &workspace,
        Duration::from_secs(60),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "sick failed.\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("Done: created hello.txt."),
        "final text missing from stdout: {stdout}"
    );

    // 5. The write tool must have created the file.
    let hello = workspace.join("hello.txt");
    assert!(hello.is_file(), "hello.txt was not created");
    assert_eq!(std::fs::read_to_string(&hello).unwrap(), "Hello, World!");

    // 6. The session must be persisted and loadable.
    let db_path = home.join("sessions.db");
    assert!(db_path.is_file(), "sessions.db was not created");

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let store = shell_memory::MemoryStore::init(&db_path.to_string_lossy())
            .await
            .expect("open session store");

        let sessions = store.list_sessions().await.expect("list sessions");
        assert_eq!(sessions.len(), 1, "expected exactly one stored session");
        let session = &sessions[0];
        assert!(session.title.contains("create a hello world file"));
        assert_eq!(session.model.as_deref(), Some("test-model"));
        assert_eq!(session.provider.as_deref(), Some(provider));

        let history = store
            .load_session_history(&session.id)
            .await
            .expect("load history");
        let roles: Vec<&str> = history.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(
            roles,
            vec!["system", "user", "assistant", "tool", "assistant"]
        );

        let assistant = &history[2];
        let calls = assistant
            .tool_calls_json
            .as_deref()
            .expect("assistant message records tool calls");
        assert!(calls.contains("\"write\""), "tool calls json: {calls}");

        let tool_message = &history[3];
        assert!(
            tool_message
                .content
                .as_deref()
                .unwrap_or_default()
                .contains("hello.txt")
        );

        assert_eq!(
            history[4].content.as_deref(),
            Some("Done: created hello.txt.")
        );

        let tool_calls = store
            .tool_call_count(&session.id)
            .await
            .expect("count tool calls");
        assert_eq!(tool_calls, 1);
    });

    server.join().expect("mock server thread");
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn agent_loop_openai_provider_writes_file_and_persists_session() {
    run_goal_test("openai", "OPENAI_API_KEY", "sk-mock-key");
}

#[test]
fn agent_loop_google_provider_uses_gemini_key_and_persists_session() {
    run_goal_test("google", "GEMINI_API_KEY", "gm-mock-key");
}

#[test]
fn mcp_fixture() {
    shell_mcp::test_support::serve_if_requested();
}

/// `[[mcp.servers]]` TOML (inline table) pointing at this test executable in
/// fixture mode, for `[mcp]` config sections.
fn mcp_toml_block(name: &str) -> String {
    let config = shell_mcp::test_support::fixture_config(name, None, Some(20_000));
    let args = config
        .args
        .iter()
        .map(|arg| format!("'{arg}'"))
        .collect::<Vec<_>>()
        .join(", ");
    let envs = config
        .env
        .iter()
        .map(|(key, value)| format!("{key} = '{value}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "\n[mcp]\nservers = [{{ name = '{name}', command = '{}', args = [{args}], env = {{ {envs} }}, timeout_ms = 20000 }}]\n",
        config.command
    )
}

/// SSE response that asks the agent to call an arbitrary tool.
fn tool_call_response_for(tool: &str, args_json: &str) -> String {
    let first = serde_json::json!({
        "choices": [{"index": 0, "delta": {"role": "assistant", "tool_calls": [
            {"index": 0, "id": "call_mcp", "type": "function",
             "function": {"name": tool, "arguments": ""}}
        ]}, "finish_reason": null}]
    });
    let second = serde_json::json!({
        "choices": [{"index": 0, "delta": {"tool_calls": [
            {"index": 0, "function": {"arguments": args_json}}
        ]}, "finish_reason": null}]
    });
    let third = serde_json::json!({
        "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]
    });
    format!("data: {first}\n\ndata: {second}\n\ndata: {third}\n\ndata: [DONE]\n\n")
}

/// Phase 6: a `[mcp]` server listed in the config joins the tool registry, and
/// a model tool call reaches the real server process end-to-end.
#[test]
fn agent_loop_calls_mcp_tool_from_config() {
    let (port, server) = spawn_mock_server(vec![
        tool_call_response_for("fixture-echo", r#"{"text": "hi-from-mcp"}"#),
        final_text_response(),
    ]);

    let stamp = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let root = std::env::temp_dir().join(format!("sick-mcp-{stamp}"));
    let workspace = root.join("workspace");
    let home = root.join("home");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&home).unwrap();

    let config_path = root.join("settings.toml");
    std::fs::write(
        &config_path,
        format!(
            "[llm]\n\
             default_provider = \"openai\"\n\
             default_model = \"test-model\"\n\
             temperature = 0.0\n\
             max_tokens = 256\n\
             base_url = \"http://127.0.0.1:{port}/v1\"\n\
             {}",
            mcp_toml_block("fixture")
        ),
    )
    .unwrap();

    let output = run_cli(
        &[
            "--config",
            &config_path.to_string_lossy(),
            "use the mcp echo tool",
        ],
        &[
            ("SICK_HOME", &home.to_string_lossy()),
            ("OPENAI_API_KEY", "sk-mock-key"),
            ("NO_PROXY", "127.0.0.1,localhost"),
            ("no_proxy", "127.0.0.1,localhost"),
        ],
        &workspace,
        Duration::from_secs(60),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "sick failed.\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("Done: created hello.txt."),
        "final text missing from stdout: {stdout}"
    );

    // The session must record the MCP tool result.
    let db_path = home.join("sessions.db");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let store = shell_memory::MemoryStore::init(&db_path.to_string_lossy())
            .await
            .expect("open session store");
        let sessions = store.list_sessions().await.expect("list sessions");
        assert_eq!(sessions.len(), 1, "expected exactly one stored session");
        let history = store
            .load_session_history(&sessions[0].id)
            .await
            .expect("load history");
        let tool_message = history
            .iter()
            .find(|message| message.role == "tool")
            .expect("tool message");
        assert!(
            tool_message
                .content
                .as_deref()
                .unwrap_or_default()
                .contains("echo: hi-from-mcp"),
            "tool content: {:?}",
            tool_message.content
        );
    });

    server.join().expect("mock server thread");
    std::fs::remove_dir_all(&root).ok();
}
