//! Performance benchmarks for the agent's hot paths (Phase 5).
//!
//! Run with `cargo bench -p shell-agent-core`. The targets and current
//! numbers are documented in `docs/BENCHMARKS.md`.

use criterion::{Criterion, criterion_group, criterion_main};
use shell_agent_core::{Agent, AgentConfig};
use shell_key_manager::{KeyStatus, KeyStore};
use shell_llm::{
    ChatMessage, ChatResponse, CompletionRequest, LlmProvider, ProviderError, StreamEvent,
};
use shell_memory::{MemoryStore, embed};

/// Round-robin key selection over a 10-key pool (every request does this).
fn bench_key_lookup(c: &mut Criterion) {
    let mut store = KeyStore::new();
    for index in 0..10 {
        store.add_key("openai", &format!("sk-bench-key-{index:02}-abcdef"));
    }
    c.bench_function("key_lookup_round_robin_10_keys", |b| {
        b.iter(|| {
            let key = store.next_key("openai");
            assert!(key.is_some());
        });
    });
}

/// 429 handling: mark a key rate-limited and pick the next healthy one.
fn bench_key_rotation(c: &mut Criterion) {
    let mut store = KeyStore::new();
    for index in 0..10 {
        store.add_key("openai", &format!("sk-rotate-key-{index:02}-abcdef"));
    }
    c.bench_function("key_rotation_on_429", |b| {
        b.iter(|| {
            let id = store.next_key("openai").expect("healthy key").id.clone();
            store.mark_key_status("openai", &id, KeyStatus::RateLimited, None);
            let next = store.next_key("openai");
            assert!(next.is_some(), "one 429 must not exhaust a 10-key pool");
        });
    });
}

/// The dependency-free 384-dim hashed embedding used for memory recall.
fn bench_embedding(c: &mut Criterion) {
    let text = "refactor the session store to stream tool call results";
    c.bench_function("embed_384_dim", |b| {
        b.iter(|| embed::embed(text));
    });
}

/// SQLite append + semantic memory search against a temp database.
fn bench_sqlite(c: &mut Criterion) {
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let dir = tempfile::tempdir().expect("tempdir");
    let store = runtime
        .block_on(MemoryStore::init(
            &dir.path().join("bench.db").to_string_lossy(),
        ))
        .expect("open store");
    let session = runtime
        .block_on(store.create_session("bench", "gpt-4.1", "openai", "/tmp/bench"))
        .expect("create session");

    let mut counter = 0i64;
    c.bench_function("sqlite_append_message", |b| {
        b.iter(|| {
            counter += 1;
            let message = shell_memory::Message {
                role: "user".to_string(),
                content: Some(format!("benchmark message {counter}")),
                turn_idx: counter,
                ..Default::default()
            };
            runtime
                .block_on(store.append_message(&session, &message))
                .expect("append message");
        });
    });

    for index in 0..100 {
        let content = format!(
            "Task: benchmark memory number {index} about deploy pipelines and firewall rules\n\
             Outcome: recorded for benchmarking\nTools: read"
        );
        runtime
            .block_on(store.add_memory(
                &content,
                Some(&embed::to_bytes(&embed::embed(&content))),
                None,
            ))
            .expect("add memory");
    }
    c.bench_function("memory_search_100_entries", |b| {
        b.iter(|| {
            let embedding = embed::embed("deploy pipeline firewall");
            runtime
                .block_on(store.search_memories(&embedding, 3))
                .expect("search memories");
        });
    });
}

/// Provider that always answers the same way (no network): lets the benchmark
/// measure the agent loop itself - prompt persistence, memory recall, message
/// assembly, and SQLite writes - rather than any real LLM latency.
struct BenchProvider {
    response: ChatResponse,
}

#[async_trait::async_trait]
impl LlmProvider for BenchProvider {
    fn id(&self) -> &str {
        "bench"
    }

    fn count_tokens(&self, _messages: &[ChatMessage]) -> usize {
        0
    }

    fn context_window(&self, _model_id: &str) -> usize {
        8192
    }

    async fn get_completion(
        &self,
        _req: CompletionRequest,
    ) -> Result<Box<dyn tokio::io::AsyncRead + Unpin + Send>, ProviderError> {
        Err(ProviderError::Other(
            "bench provider has no byte stream".into(),
        ))
    }

    async fn chat(&self, _req: CompletionRequest) -> Result<ChatResponse, ProviderError> {
        Ok(self.response.clone())
    }

    async fn chat_streaming(
        &self,
        _req: CompletionRequest,
        on_event: &mut (dyn FnMut(StreamEvent) + Send),
    ) -> Result<ChatResponse, ProviderError> {
        let response = self.response.clone();
        on_event(StreamEvent {
            content: response.content.clone(),
            tool_calls: response.tool_calls.clone(),
            done: true,
        });
        Ok(response)
    }
}

/// One full agent turn against an instant provider (the "LLM → tools → LLM"
/// loop without network latency).
fn bench_agent_turn(c: &mut Criterion) {
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let dir = tempfile::tempdir().expect("tempdir");
    let memory = runtime
        .block_on(MemoryStore::init(
            &dir.path().join("bench-agent.db").to_string_lossy(),
        ))
        .expect("open store");
    let mut store = KeyStore::new();
    store.add_key("bench", "sk-bench-agent-0000");
    let mut config = AgentConfig::new("bench", "bench-model");
    config.workspace = dir.path().to_path_buf();
    let mut agent = Agent::with_provider(
        config,
        memory,
        store,
        std::sync::Arc::new(BenchProvider {
            response: ChatResponse {
                content: "acknowledged".to_string(),
                tool_calls: Vec::new(),
            },
        }),
    );

    c.bench_function("agent_turn_scripted_provider", |b| {
        b.iter(|| {
            runtime
                .block_on(agent.run_turn(Some("benchmark prompt".to_string()), None))
                .expect("agent turn");
        });
    });
}

criterion_group!(
    benches,
    bench_key_lookup,
    bench_key_rotation,
    bench_embedding,
    bench_sqlite,
    bench_agent_turn
);
criterion_main!(benches);
