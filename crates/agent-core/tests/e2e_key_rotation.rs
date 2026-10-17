//! Phase 2 integration tests: transparent key rotation and cross-provider
//! fallback, using a local `mockito` HTTP server (no real API keys).
//!
//! Covered completion criteria:
//! - 429 → key marked `RateLimited`, retried with the next key
//! - 401 → key marked `Invalid`, retried
//! - context-window error → key marked `Exhausted`, history truncated, retried
//! - provider A all keys fail → provider B serves the request

use shell_agent_core::{Agent, AgentConfig};
use shell_key_manager::{KeyStatus, KeyStore};
use shell_llm::ChatMessage;
use shell_memory::MemoryStore;

/// SSE body for a plain text success response.
fn success_sse(text: &str) -> String {
    format!(
        concat!(
            "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"role\":\"assistant\",\"content\":\"{text}\"}},\"finish_reason\":null}}]}}\n\n",
            "data: {{\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"stop\"}}]}}\n\n",
            "data: [DONE]\n\n",
        ),
        text = text
    )
}

/// Create an agent backed by a temp SQLite store.
async fn test_agent(
    provider: &str,
    model: &str,
    fallback_chain: Vec<(String, String)>,
    base_url: String,
    keys: KeyStore,
) -> (Agent, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("sessions.db");
    let memory = MemoryStore::init(&db.to_string_lossy())
        .await
        .expect("memory store");

    let mut config = AgentConfig::new(provider, model);
    config.fallback_chain = fallback_chain;
    config.base_url = base_url;

    let agent = Agent::new(config, memory, keys).await.expect("agent");
    (agent, dir)
}

fn user_messages(text: &str) -> Vec<ChatMessage> {
    vec![
        ChatMessage::new("system", "You are helpful."),
        ChatMessage::new("user", text),
    ]
}

fn status_of(agent: &Agent, provider: &str, key_value: &str) -> KeyStatus {
    let key_id = shell_key_manager::key_id(key_value);
    agent
        .key_manager()
        .get_status_report()
        .into_iter()
        .find(|info| info.provider == provider && info.key_id == key_id)
        .unwrap_or_else(|| panic!("no status for {provider}/{key_id}"))
        .status
}

#[tokio::test]
async fn rate_limited_key_rotates_to_next_key() {
    let mut server = mockito::Server::new_async().await;

    let limited = server
        .mock("POST", "/v1/chat/completions")
        .match_header("authorization", "Bearer sk-key-1")
        .with_status(429)
        .with_header("content-type", "application/json")
        .with_body(r#"{"error":{"message":"rate limit exceeded","type":"rate_limit_exceeded"}}"#)
        .expect(1)
        .create_async()
        .await;

    let served = server
        .mock("POST", "/v1/chat/completions")
        .match_header("authorization", "Bearer sk-key-2")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(success_sse("hello from key two"))
        .expect(1)
        .create_async()
        .await;

    let mut keys = KeyStore::new();
    keys.add_key("openai", "sk-key-1");
    keys.add_key("openai", "sk-key-2");

    let (mut agent, _dir) = test_agent(
        "openai",
        "test-model",
        Vec::new(),
        format!("{}/v1", server.url()),
        keys,
    )
    .await;

    let response = agent
        .send_with_rotation(&user_messages("hi"))
        .await
        .expect("rotation succeeds with the second key");

    assert_eq!(response.content, "hello from key two");
    assert_eq!(
        status_of(&agent, "openai", "sk-key-1"),
        KeyStatus::RateLimited
    );
    assert_eq!(status_of(&agent, "openai", "sk-key-2"), KeyStatus::Active);
    assert!(agent.key_manager().healthy_keys_exist("openai"));

    limited.assert_async().await;
    served.assert_async().await;
}

#[tokio::test]
async fn unauthorized_key_is_marked_invalid_and_retried() {
    let mut server = mockito::Server::new_async().await;

    let rejected = server
        .mock("POST", "/v1/chat/completions")
        .match_header("authorization", "Bearer sk-bad-1")
        .with_status(401)
        .with_header("content-type", "application/json")
        .with_body(r#"{"error":{"message":"invalid api key"}}"#)
        .expect(1)
        .create_async()
        .await;

    let served = server
        .mock("POST", "/v1/chat/completions")
        .match_header("authorization", "Bearer sk-good-1")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(success_sse("hello from the good key"))
        .expect(1)
        .create_async()
        .await;

    let mut keys = KeyStore::new();
    keys.add_key("openai", "sk-bad-1");
    keys.add_key("openai", "sk-good-1");

    let (mut agent, _dir) = test_agent(
        "openai",
        "test-model",
        Vec::new(),
        format!("{}/v1", server.url()),
        keys,
    )
    .await;

    let response = agent
        .send_with_rotation(&user_messages("hi"))
        .await
        .expect("rotation skips the invalid key");

    assert_eq!(response.content, "hello from the good key");
    assert_eq!(status_of(&agent, "openai", "sk-bad-1"), KeyStatus::Invalid);
    assert_eq!(status_of(&agent, "openai", "sk-good-1"), KeyStatus::Active);

    rejected.assert_async().await;
    served.assert_async().await;
}

#[tokio::test]
async fn context_window_error_marks_key_exhausted_and_retries() {
    let mut server = mockito::Server::new_async().await;

    let too_long = server
        .mock("POST", "/v1/chat/completions")
        .match_header("authorization", "Bearer sk-old-1")
        .with_status(400)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"error":{"message":"This model's maximum context length is 8192 tokens, however you requested 99999 tokens"}}"#,
        )
        .expect(1)
        .create_async()
        .await;

    let served = server
        .mock("POST", "/v1/chat/completions")
        .match_header("authorization", "Bearer sk-new-1")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(success_sse("hello after truncation"))
        .expect(1)
        .create_async()
        .await;

    let mut keys = KeyStore::new();
    keys.add_key("openai", "sk-old-1");
    keys.add_key("openai", "sk-new-1");

    let (mut agent, _dir) = test_agent(
        "openai",
        "test-model",
        Vec::new(),
        format!("{}/v1", server.url()),
        keys,
    )
    .await;

    let response = agent
        .send_with_rotation(&user_messages("a long request"))
        .await
        .expect("rotation truncates and retries");

    assert_eq!(response.content, "hello after truncation");
    assert_eq!(
        status_of(&agent, "openai", "sk-old-1"),
        KeyStatus::Exhausted
    );
    assert_eq!(status_of(&agent, "openai", "sk-new-1"), KeyStatus::Active);

    too_long.assert_async().await;
    served.assert_async().await;
}

#[tokio::test]
async fn all_keys_rate_limited_falls_back_to_next_provider() {
    let mut server = mockito::Server::new_async().await;

    let openai_limited = server
        .mock("POST", "/v1/chat/completions")
        .match_header("authorization", "Bearer sk-opena1")
        .with_status(429)
        .with_header("content-type", "application/json")
        .with_body(r#"{"error":{"message":"rate limit exceeded"}}"#)
        .expect(1)
        .create_async()
        .await;

    let deepseek_served = server
        .mock("POST", "/v1/chat/completions")
        .match_header("authorization", "Bearer sk-deeps1")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(success_sse("hello from deepseek"))
        .expect(1)
        .create_async()
        .await;

    let mut keys = KeyStore::new();
    keys.add_key("openai", "sk-opena1");
    keys.add_key("deepseek", "sk-deeps1");
    keys.set_provider_base_url("deepseek", format!("{}/v1", server.url()));

    let (mut agent, _dir) = test_agent(
        "openai",
        "test-model",
        vec![("deepseek".to_string(), "deepseek-chat".to_string())],
        format!("{}/v1", server.url()),
        keys,
    )
    .await;

    let response = agent
        .send_with_rotation(&user_messages("hi"))
        .await
        .expect("fallback provider serves the request");

    assert_eq!(response.content, "hello from deepseek");
    assert_eq!(
        status_of(&agent, "openai", "sk-opena1"),
        KeyStatus::RateLimited
    );
    assert_eq!(
        status_of(&agent, "deepseek", "sk-deeps1"),
        KeyStatus::Active
    );

    openai_limited.assert_async().await;
    deepseek_served.assert_async().await;
}

#[tokio::test]
async fn all_providers_exhausted_returns_error() {
    let mut server = mockito::Server::new_async().await;

    // Both providers always rate-limit their single key.
    server
        .mock("POST", "/v1/chat/completions")
        .match_header("authorization", "Bearer sk-opena1")
        .with_status(429)
        .with_header("content-type", "application/json")
        .with_body(r#"{"error":{"message":"rate limit exceeded"}}"#)
        .expect(1)
        .create_async()
        .await;

    let mut keys = KeyStore::new();
    keys.add_key("openai", "sk-opena1");
    keys.set_provider_base_url("deepseek", format!("{}/v1", server.url()));

    // Fallback provider has no keys at all → rotation must fail cleanly.
    let (mut agent, _dir) = test_agent(
        "openai",
        "test-model",
        vec![("deepseek".to_string(), "deepseek-chat".to_string())],
        format!("{}/v1", server.url()),
        keys,
    )
    .await;

    let err = agent
        .send_with_rotation(&user_messages("hi"))
        .await
        .expect_err("all keys exhausted");
    assert!(
        err.to_string().contains("exhausted"),
        "error should explain exhaustion: {err}"
    );
    assert_eq!(
        status_of(&agent, "openai", "sk-opena1"),
        KeyStatus::RateLimited
    );
}
