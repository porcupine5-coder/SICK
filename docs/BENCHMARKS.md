# Benchmarks

Run them all:

```bash
cargo bench -p shell-agent-core      # measured with criterion
```

Source: `crates/agent-core/benches/agent_bench.rs`. Reports are written to
`target/criterion/` (HTML).

## Targets & current numbers

Measured on Windows 11, Rust 1.96 release profile, using the workspace's
`[profile.bench]` defaults. Treat them as ballpark: what matters is keeping
them in the target range and catching regressions.

| Benchmark | What it measures | Target | Measured |
| --- | --- | --- | --- |
| `key_lookup_round_robin_10_keys` | `KeyStore::next_key` over a 10-key pool (runs before every request) | < 2 µs | ~0.4 µs |
| `key_rotation_on_429` | mark one key rate-limited, select the next healthy key | < 5 µs | ~1.0 µs |
| `embed_384_dim` | memory embedding for one sentence (runs per recall + per stored memory) | < 20 µs | ~3.6 µs |
| `sqlite_append_message` | one transcript message written to SQLite | < 2 ms | ~0.5 ms |
| `memory_search_100_entries` | cosine search over 100 stored memories | < 2 ms | ~0.6 ms |
| `agent_turn_scripted_provider` | full turn loop (prompt persistence → recall → LLM call → transcript writes) against an instant in-process provider | < 15 ms | ~4 ms |

The agent-turn benchmark deliberately uses a local provider: it measures
S.I.C.K's own overhead, not network latency. Real-world turn times are dominated
by the provider's response time.

## Profiling notes (Phase 5)

Initial runs exposed a real bottleneck: `sqlite_append_message` took
**~13 ms** per message, and the agent turn ~34 ms, because every write paid a
full `fsync` (SQLite defaults to `synchronous=FULL` even in WAL mode; on
Windows a flush is expensive).

Fix in `crates/memory/src/store.rs`: open databases with explicit
`journal_mode=WAL` + `synchronous=NORMAL`. Durability trade-off: a power loss
can lose the last few *session log* transactions — never corrupt the database,
and API keys are not in SQLite. After the fix:

| Benchmark | Before | After |
| --- | --- | --- |
| `sqlite_append_message` | ~13.4 ms | ~0.5 ms (**~25× faster**) |
| `agent_turn_scripted_provider` | ~33.6 ms | ~4.2 ms (**~8× faster**) |
| `memory_search_100_entries` | ~0.78 ms | ~0.58 ms |

## Notes

- `cargo bench` in the workspace root runs this bench target only (it is the
  only crate with a `[[bench]]`), plus any doc-tests configured there.
- Numbers include criterion's warm-up/statistics; individual samples vary by
  machine. For regression tracking, compare against the previous run with
  `cargo bench -- --save-baseline before` / `--baseline before`.
- No live API keys or network calls are used by any benchmark.
