//! Binary-level end-to-end test for key rotation (Phase 2).
//!
//! Runs the real `sick` binary against a scripted HTTP server:
//! 1. The first request (key 1) is answered with HTTP 429.
//! 2. The agent must rotate to key 2 in the same process and retry.
//! 3. The second request (key 2) is answered with a streaming success.
//!
//! Proves the user-visible behavior:
//! `OPENAI_API_KEY_1=... OPENAI_API_KEY_2=... sick "<prompt>"` continues
//! transparently after a rate limit.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// SSE response carrying the final assistant text (no tool calls).
fn final_text_response() -> String {
    concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Rotated to the second key.\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    )
    .to_string()
}

/// Serve one 429 followed by one 200 SSE response; returns the Authorization
/// header seen on each request.
fn spawn_scripted_server() -> (u16, std::thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
    let port = listener.local_addr().unwrap().port();

    let handle = std::thread::spawn(move || {
        let mut seen = Vec::new();

        // Request 1 → 429 rate limit.
        if let Ok((mut stream, _)) = listener.accept() {
            let headers = read_http_request(&mut stream);
            seen.push(extract_authorization(&headers));
            let body =
                r#"{"error":{"message":"rate limit exceeded","type":"rate_limit_exceeded"}}"#;
            let response = format!(
                "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }

        // Request 2 → 200 streaming success.
        if let Ok((mut stream, _)) = listener.accept() {
            let headers = read_http_request(&mut stream);
            seen.push(extract_authorization(&headers));
            let body = final_text_response();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }

        seen
    });

    (port, handle)
}

/// Read request headers + body; returns the raw header text.
fn read_http_request(stream: &mut std::net::TcpStream) -> String {
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];

    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        match stream.read(&mut tmp) {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
        }
    }

    let headers = String::from_utf8_lossy(&buf).to_string();
    let content_length = headers
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
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

    headers
}

fn extract_authorization(headers: &str) -> String {
    headers
        .lines()
        .find(|line| line.to_ascii_lowercase().starts_with("authorization:"))
        .map(|line| line.trim().to_string())
        .unwrap_or_default()
}

struct CliOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn run_cli(args: &[&str], envs: &[(&str, &str)], cwd: &Path, timeout: Duration) -> CliOutput {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sick"));
    cmd.args(args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Isolate from host keys so key order is deterministic: key 1 first.
    for var in [
        "OPENAI_API_KEY",
        "SICK_OPENAI_API_KEY",
        "SHELL_AGENT_OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "SICK_ANTHROPIC_API_KEY",
        "SHELL_AGENT_ANTHROPIC_API_KEY",
        "GOOGLE_API_KEY",
        "GEMINI_API_KEY",
        "SICK_GOOGLE_API_KEY",
        "SHELL_AGENT_GOOGLE_API_KEY",
        "DEEPSEEK_API_KEY",
        "SICK_DEEPSEEK_API_KEY",
        "SHELL_AGENT_DEEPSEEK_API_KEY",
        "OPENROUTER_API_KEY",
        "SICK_OPENROUTER_API_KEY",
        "SHELL_AGENT_OPENROUTER_API_KEY",
        "QWEN_API_KEY",
        "SICK_QWEN_API_KEY",
        "SHELL_AGENT_QWEN_API_KEY",
        "SICK_HOME",
        "SHELL_AGENT_HOME",
    ] {
        cmd.env_remove(var);
    }
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

#[test]
fn cli_rotates_to_second_key_after_rate_limit() {
    let (port, server) = spawn_scripted_server();

    let root = std::env::temp_dir().join(format!(
        "sick-rotation-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
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
             base_url = \"http://127.0.0.1:{port}/v1\"\n"
        ),
    )
    .unwrap();

    let output = run_cli(
        &["--config", &config_path.to_string_lossy(), "say hi"],
        &[
            ("SICK_HOME", &home.to_string_lossy()),
            ("OPENAI_API_KEY_1", "sk-mock-key-1"),
            ("OPENAI_API_KEY_2", "sk-mock-key-2"),
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
        stdout.contains("Rotated to the second key."),
        "final text missing from stdout: {stdout}"
    );

    let seen = server.join().expect("mock server thread");
    assert_eq!(seen.len(), 2, "expected exactly two HTTP requests");
    assert!(
        seen[0].contains("Bearer sk-mock-key-1"),
        "first request should use key 1: {}",
        seen[0]
    );
    assert!(
        seen[1].contains("Bearer sk-mock-key-2"),
        "second request should use the rotated key 2: {}",
        seen[1]
    );

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn keys_flag_lists_configured_keys() {
    let root = std::env::temp_dir().join(format!(
        "sick-keys-flag-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();

    let output = run_cli(
        &["--keys"],
        &[
            ("SICK_HOME", &root.to_string_lossy()),
            ("OPENAI_API_KEY_1", "sk-aaaabbbbcccc"),
            ("ANTHROPIC_API_KEY", "sk-ant-dddd"),
            ("NO_PROXY", "127.0.0.1,localhost"),
            ("no_proxy", "127.0.0.1,localhost"),
        ],
        &root,
        Duration::from_secs(30),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "stdout: {stdout}");
    assert!(stdout.contains("Provider: openai"), "stdout: {stdout}");
    assert!(stdout.contains("Provider: anthropic"), "stdout: {stdout}");
    assert!(stdout.contains("sk-aaaa"), "key id shown: {stdout}");
    assert!(stdout.contains("[Active]"), "status shown: {stdout}");

    std::fs::remove_dir_all(&root).ok();
}
