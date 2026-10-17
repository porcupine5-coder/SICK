//! End-to-end tests: real MCP client against the stdio fixture server.
//!
//! The fixture is this test executable itself in `--exact mcp_fixture` mode;
//! see [`shell_mcp::test_support`].

use std::sync::Arc;

use serde_json::json;
use shell_config::McpServerConfig;
use shell_mcp::McpManager;
use shell_mcp::test_support::fixture_config;
use shell_tools::ToolContext;

#[test]
fn mcp_fixture() {
    shell_mcp::test_support::serve_if_requested();
}

async fn connect(configs: Vec<McpServerConfig>) -> Arc<McpManager> {
    Arc::new(McpManager::connect_all(&configs).await)
}

#[tokio::test]
async fn connects_lists_and_calls_tools() {
    let manager = connect(vec![fixture_config("fixture", None, None)]).await;

    let statuses = manager.statuses();
    assert_eq!(statuses.len(), 1);
    assert!(statuses[0].ready, "{}", statuses[0].state);
    assert_eq!(statuses[0].tool_count, 3);

    let tools = manager.tools();
    let mut names: Vec<String> = tools.iter().map(|tool| tool.name().to_string()).collect();
    names.sort();
    assert_eq!(names, ["fixture-echo", "fixture-fail", "fixture-sleepy"]);

    let ctx = ToolContext::new(std::env::temp_dir());
    let echo = tools
        .iter()
        .find(|tool| tool.name() == "fixture-echo")
        .expect("echo tool");
    assert!(
        echo.description().contains("Echoes the given text back"),
        "{}",
        echo.description()
    );
    let result = echo.execute(json!({"text": "hello"}), &ctx).await;
    assert!(result.success, "{}", result.output);
    assert_eq!(result.output, "echo: hello");

    let fail = tools
        .iter()
        .find(|tool| tool.name() == "fixture-fail")
        .expect("fail tool");
    let result = fail.execute(json!({}), &ctx).await;
    assert!(!result.success);
    assert!(
        result.output.contains("intentional failure"),
        "{}",
        result.output
    );
}

#[tokio::test]
async fn failed_server_does_not_block_others() {
    let manager = connect(vec![
        fixture_config("broken", Some("fail"), Some(10_000)),
        fixture_config("usable", None, None),
    ])
    .await;

    let statuses = manager.statuses();
    assert_eq!(statuses.len(), 2);
    assert!(!statuses[0].ready);
    assert!(
        statuses[0].state.starts_with("failed:"),
        "{}",
        statuses[0].state
    );
    assert_eq!(statuses[0].tool_count, 0);
    assert!(statuses[1].ready, "{}", statuses[1].state);
    assert_eq!(statuses[1].tool_count, 3);

    let error = manager
        .call_tool("broken", "echo", json!({"text": "x"}))
        .await
        .expect_err("failed server must not accept calls");
    assert!(error.contains("not running"), "{error}");
}

#[tokio::test]
async fn server_exiting_after_initialize_is_reported() {
    let manager = connect(vec![fixture_config(
        "short-lived",
        Some("exit-after-init"),
        Some(10_000),
    )])
    .await;

    let statuses = manager.statuses();
    assert!(!statuses[0].ready, "{}", statuses[0].state);
    assert!(
        statuses[0].state.contains("tools/list failed"),
        "{}",
        statuses[0].state
    );
    assert_eq!(manager.tool_count(), 0);
}

#[tokio::test]
async fn slow_server_request_times_out() {
    let manager = connect(vec![fixture_config("slow", Some("slow"), Some(2000))]).await;
    let statuses = manager.statuses();
    assert!(!statuses[0].ready, "{}", statuses[0].state);
    assert!(
        statuses[0].state.contains("timed out"),
        "{}",
        statuses[0].state
    );
}

#[tokio::test]
async fn non_json_noise_from_the_server_is_ignored() {
    let manager = connect(vec![fixture_config("noisy", Some("bad-json"), None)]).await;
    let statuses = manager.statuses();
    assert!(statuses[0].ready, "{}", statuses[0].state);
    assert_eq!(manager.tool_count(), 3);
}

#[tokio::test]
async fn missing_command_is_a_failed_server_not_a_panic() {
    let config = McpServerConfig {
        name: "missing".to_string(),
        command: "definitely-not-a-real-mcp-server-binary".to_string(),
        ..Default::default()
    };
    let manager = connect(vec![config]).await;
    let statuses = manager.statuses();
    assert!(!statuses[0].ready);
    assert!(
        statuses[0].state.contains("failed to start") || statuses[0].state.contains("failed"),
        "{}",
        statuses[0].state
    );
}

#[tokio::test]
async fn configured_servers_without_name_or_command_fail_gracefully() {
    let config = McpServerConfig {
        name: "incomplete".to_string(),
        command: String::new(),
        ..Default::default()
    };
    let manager = connect(vec![config]).await;
    let statuses = manager.statuses();
    assert!(!statuses[0].ready);
    assert!(
        statuses[0].state.contains("needs a name and a command"),
        "{}",
        statuses[0].state
    );
}
