//! In-process MCP fixture server for tests.
//!
//! Both ends of the MCP stdio transport are plain processes, so integration
//! tests spawn *the test executable itself* in fixture mode instead of
//! shipping a helper binary:
//!
//! ```ignore
//! // Any `tests/*.rs` file that uses MCP fixtures must declare:
//! #[test]
//! fn mcp_fixture() {
//!     shell_mcp::test_support::serve_if_requested();
//! }
//! ```
//!
//! Then build the config with [`fixture_config`] (it points at
//! `std::env::current_exe()` and passes `--exact mcp_fixture --nocapture`).
//!
//! The fixture advertises three tools: `echo`, `fail` (always returns
//! `isError`), and `sleepy` (sleeps for `seconds`). Set `mode` in
//! [`fixture_config`] to test failure paths:
//!
//! - `"fail"`: exit immediately without speaking the protocol,
//! - `"bad-json"`: print a non-JSON banner line before speaking it,
//! - `"exit-after-init"`: answer `initialize`, then exit,
//! - `"slow"`: delay the `tools/list` response by 4 seconds.
//!
//! The harness prints `running 1 test` / `test mcp_fixture ... ` lines on
//! stdout before the fixture loop starts; the loop first writes an empty line
//! so those partial lines can never merge with a JSON-RPC message, and the
//! client skips any line that is not JSON.

use std::collections::BTreeMap;

use serde_json::{Value, json};
use shell_config::McpServerConfig;

/// Set to `1` in the environment to turn the test process into the server.
pub const FIXTURE_ENV: &str = "SICK_MCP_FIXTURE";

/// Selects a behavior mode (see module docs).
pub const FIXTURE_MODE_ENV: &str = "SICK_MCP_FIXTURE_MODE";

/// Name of the test that runs the fixture inside each test binary.
pub const FIXTURE_TEST_NAME: &str = "mcp_fixture";

/// Run the fixture server when spawned in fixture mode; otherwise return.
///
/// Call this from a top-level `#[test] fn mcp_fixture()` in every
/// `tests/*.rs` file that uses [`fixture_config`].
pub fn serve_if_requested() {
    if std::env::var(FIXTURE_ENV).as_deref() != Ok("1") {
        return;
    }
    serve(&std::env::var(FIXTURE_MODE_ENV).unwrap_or_default());
}

/// A `[[mcp.servers]]` config that starts the fixture server from the current
/// test executable. Must be called from a test binary that defines the
/// [`FIXTURE_TEST_NAME`] test.
pub fn fixture_config(name: &str, mode: Option<&str>, timeout_ms: Option<u64>) -> McpServerConfig {
    let exe = std::env::current_exe().expect("current_exe for the MCP fixture");
    let mut env = BTreeMap::new();
    env.insert(FIXTURE_ENV.to_string(), "1".to_string());
    if let Some(mode) = mode {
        env.insert(FIXTURE_MODE_ENV.to_string(), mode.to_string());
    }
    McpServerConfig {
        name: name.to_string(),
        command: exe.to_string_lossy().into_owned(),
        args: vec![
            "--exact".to_string(),
            FIXTURE_TEST_NAME.to_string(),
            "--nocapture".to_string(),
        ],
        env,
        timeout_ms,
    }
}

/// The synchronous server loop (newline-delimited JSON-RPC on stdio).
fn serve(mode: &str) {
    use std::io::{BufRead, Write};

    if mode == "fail" {
        std::process::exit(3);
    }

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    // Terminate any partial harness line ("test mcp_fixture ... ") so protocol
    // lines stay intact from the very first message.
    let _ = writeln!(stdout);
    let _ = stdout.flush();
    if mode == "bad-json" {
        let _ = writeln!(stdout, "this line is not valid JSON");
        let _ = stdout.flush();
    }

    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(trimmed) else {
            continue; // ignore junk
        };
        let Some(id) = message.get("id").cloned() else {
            continue; // notification: no response
        };
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();

        let response = match method.as_str() {
            "initialize" => Ok(json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "fixture-server", "version": "0.0.1"},
            })),
            "tools/list" => {
                if mode == "slow" {
                    std::thread::sleep(std::time::Duration::from_secs(4));
                }
                Ok(tools_listing())
            }
            "tools/call" => Ok(call_response(message.get("params"))),
            other => Err((-32601i64, format!("method `{other}` not found"))),
        };

        let payload = match response {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err((code, message)) => {
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
            }
        };
        let _ = writeln!(stdout, "{payload}");
        let _ = stdout.flush();

        if mode == "exit-after-init" && method == "initialize" {
            std::process::exit(0);
        }
    }
}

/// The fixed tool catalog served by the fixture.
fn tools_listing() -> Value {
    json!({
        "tools": [
            {
                "name": "echo",
                "description": "Echoes the given text back",
                "inputSchema": {
                    "type": "object",
                    "properties": {"text": {"type": "string"}},
                    "required": ["text"]
                }
            },
            {
                "name": "fail",
                "description": "Always reports an MCP error",
                "inputSchema": {"type": "object"}
            },
            {
                "name": "sleepy",
                "description": "Sleeps for `seconds` then answers",
                "inputSchema": {
                    "type": "object",
                    "properties": {"seconds": {"type": "number"}}
                }
            }
        ]
    })
}

/// `tools/call` implementation for the fixture catalog.
fn call_response(params: Option<&Value>) -> Value {
    let name = params
        .and_then(|params| params.get("name"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let arguments = params
        .and_then(|params| params.get("arguments"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    match name {
        "echo" => {
            let text = arguments
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default();
            json!({"content": [{"type": "text", "text": format!("echo: {text}")}]})
        }
        "fail" => json!({
            "content": [{"type": "text", "text": "intentional failure"}],
            "isError": true
        }),
        "sleepy" => {
            let seconds = arguments
                .get("seconds")
                .and_then(Value::as_f64)
                .unwrap_or(1.0)
                .clamp(0.0, 30.0);
            std::thread::sleep(std::time::Duration::from_secs_f64(seconds));
            json!({"content": [{"type": "text", "text": format!("slept for {seconds}s")}]})
        }
        other => json!({
            "content": [{"type": "text", "text": format!("unknown tool `{other}`")}],
            "isError": true
        }),
    }
}
