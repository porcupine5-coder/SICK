# S.I.C.K — Developer & User Guide

Deep dives into the pieces the README only summarizes. Everything here matches
the code in `crates/`; file paths point at the modules of record.

---

## 1. Key rotation algorithm

Source: `crates/key-manager/src/lib.rs`.

### Pool model

Keys are grouped per provider into a pool (`KeyStore::pools`). Each `ApiKey`
carries:

- `value` — the secret (`Zeroizing<String>`; wiped on drop),
- `id` — the first 8 characters, used as a **non-secret** display identifier
  everywhere (CLI, TUI, logs, `keys.toml`),
- `status` — health state (below),
- `rate_limit_reset_at` / `last_used` / `consecutive_errors` — scheduling
  state.

Loading order: environment variables first, then `keys.enc`, then legacy
plaintext files. Duplicates (same provider + value) are skipped, so the same
key exported and also present in the environment is used once.

### Statuses

| Status | Set when | Consequence |
| --- | --- | --- |
| `Active` | default, or recovery | selectable |
| `RateLimited` | HTTP 429 (or a rate-limit error) | skipped until `rate_limit_reset_at` |
| `UnknownError` | 5xx / network / other transient | usable until more than 3 consecutive failures, then skipped for 30 s |
| `Invalid` | HTTP 401/403 | skipped **permanently** |
| `Exhausted` | quota / context-window/credit exhaustion | skipped **permanently** |

`mark_key_status(provider, key_id, status, retry_after)` performs the
transitions. Cooldowns honour the provider's retry hint when present;
otherwise `DEFAULT_RATE_LIMIT_COOLDOWN` (60 s) applies for rate limits and
`DEFAULT_ERROR_COOLDOWN` (30 s) after `MAX_CONSECUTIVE_ERRORS` (3) transient
failures. A success (`Active`) clears counters and cooldowns immediately.

### Selection

`next_key(provider)`:

1. Promotes every key whose cooldown expired back to `Active`
   (`refresh_key_health`, also exposed on `Agent` and used by status displays).
2. Scans the pool round-robin starting at the stored cursor.
3. Returns the first usable key, advancing the cursor past it — so a pool of
   N healthy keys is used evenly, and a 429 on one key moves load to the next
   without user intervention.

When no key is usable, the agent walks the configured `fallback_chain`
(`llm.provider`/`llm.model` pairs) before failing the turn. `sick keys`
shows the live state, e.g.:

```
Provider: openai
  Key sk-abc123...  [RateLimited]  reset in: 45s  last used: 2m ago
  Key sk-def456...  [Active]  last used: 1m ago
```

### Where clients mark status

The agent marks keys after every request (`agent.rs`): 401/403 → `Invalid`,
429 → `RateLimited` (with the `retry-after` header when sent), quota messages
→ `Exhausted`, 5xx/network → `UnknownError`, success → `Active`. The TUI's
`/keys` and the CLI's `sick keys` are read-only views that first run
`refresh_key_health()` so recovered keys display as active.

---

## 2. Provider configuration reference

### Built-in providers

| Provider | Aliases | Default base URL (OpenAI-compatible) | Env vars (in addition to `SICK_`/`SHELL_AGENT_` variants and `_1.._9`) |
| --- | --- | --- | --- |
| `openai` | — | `https://api.openai.com/v1` | `OPENAI_API_KEY` |
| `anthropic` | — | `https://api.anthropic.com/v1` | `ANTHROPIC_API_KEY` |
| `google` | `gemini` | Gemini OpenAI-compat endpoint | `GOOGLE_API_KEY`, `GEMINI_API_KEY` |
| `deepseek` | — | `https://api.deepseek.com` | `DEEPSEEK_API_KEY` |
| `openrouter` | — | `https://openrouter.ai/api/v1` | `OPENROUTER_API_KEY` |
| `qwen` | — | DashScope compatible mode | `QWEN_API_KEY` |

Env resolution for one base name tries: the name itself, `_1`…`_9`, then the
`SICK_`-prefixed form, then the legacy `SHELL_AGENT_` form. All variants found
are loaded as separate keys of that provider's pool (rotation).

### Keys-file format

The (encrypted) document inside `keys.enc` is the same TOML the plaintext
files use:

```toml
[openai]
keys = [
  "sk-literal",
  { value = "sk-inline" },
  { env = "OPENAI_API_KEY_2" },     # resolved at load time; skipped when unset
]
base_url = "https://api.openai.com/v1"
default_model = "gpt-4.1"
context_window = 1047576

[local-llama]                        # arbitrary provider id
keys = ["not-needed"]
base_url = "http://localhost:8080/v1"
default_model = "llama-3.1-8b"
```

`base_url` overrides the built-in endpoint (trailing `/` normalized away).
`context_window` overrides token accounting when the provider's model table
doesn't know your model.

`keys.json` (legacy) accepts the same data as JSON:
`{ "openai": { "keys": ["sk-..."], "base_url": "..." } }` or the shorthand
`{ "openai": ["sk-..."] }`.

### Config file (`config.toml` / `.sick/settings.toml`)

```toml
[llm]
default_provider = "google"          # any provider id
default_model = "Gemini 3.5 Flash-Lite"
temperature = 0.2
max_tokens = 64000
base_url = ""                        # optional OpenAI-compatible override
fallback_chain = [
  { provider = "anthropic", model = "claude-3-7-sonnet-20250219" },
]
```

Resolution order: `-C <path>` → `.sick/settings.toml` (legacy
`.shell-agent/settings.toml`) → `<SICK_HOME>/config.toml` → defaults. CLI
`--provider`/`--model` beat the file.

### Environment variables

| Variable | Purpose |
| --- | --- |
| `SICK_HOME` (legacy `SHELL_AGENT_HOME`) | State dir (config, keys, sessions, skills). Default `~/.sick`. |
| `<PROVIDER>_API_KEY` | Keys (see above for variants). |
| `SICK_KEY_PASSWORD` | Master password for a password-protected `keys.enc`. |
| `SICK_KEYRING_FILE` | File-backed keyring path (headless/CI; skips the OS keychain). |
| `RUST_LOG` | Log level for stderr (`warn` by default). |

---

## 3. Memory system architecture

Source: `crates/memory/` + `recall_memories`/`remember_turn` in
`crates/agent-core/src/agent.rs`.

### Storage (SQLite at `<SICK_HOME>/sessions.db`)

| Table | Contents |
| --- | --- |
| `sessions` | id (UUID), title, model, provider, workspace, created/updated |
| `messages` | role (`user`/`assistant`/`tool`/`system`), content, tool-call JSON, tool result, turn index |
| `tool_calls` | id (LLM call id), session, tool name, args JSON, result, duration, exit code |
| `prompts` | every user prompt with timestamp + model (survives session deletion) |
| `memories` | distilled `Task/Outcome/Tools` summaries + 384-dim embedding (BLOB) + source session |

Connections use WAL with `synchronous=NORMAL`: message writes are ~25× faster
than the SQLite default without risking corruption (see
`docs/BENCHMARKS.md`). Schema creation is idempotent (`CREATE TABLE IF NOT
EXISTS`), so older databases self-heal on open.

### Embedder (`crates/memory/src/embed.rs`)

Dependency-free, deterministic 384-dim vectors: tokenize into lowercase
unigrams + adjacent bigrams, hash each into one of 384 buckets (signed),
L2-normalize. Similarity is cosine. This is deliberately not a transformer:
recall runs on every turn with zero network calls and zero model downloads,
and its recall quality is sufficient for "what did we do last time?" hints.
It is the TF-IDF-style fallback the phase plan allows.

### Write path (extraction)

After a turn that **used at least one tool**, the agent distills the turn
deterministically:

```
Task: <latest user message, truncated>
Outcome: <summary of the final assistant message, truncated>
Tools: <comma-separated tool names used>
```

The summary is embedded and inserted; duplicates are ignored (unique
constraint on content). No extra LLM call is spent on extraction — a
deliberate deviation from the phase-4 sketch, documented in
`Accomplish/A/phase-4-A.md`.

### Read path (recall)

Before each LLM request the agent embeds the latest user message, asks the
store for the top 5 memories by cosine similarity, then keeps at most **3**
with score **≥ 0.30**, skipping memories whose source session is the current
session. The survivors are injected through the **ephemeral system addendum**
(alongside plan-mode instructions and active skills) — never written into the
transcript, so they never accumulate token cost or leak across sessions.

### Sessions tooling

- `fork_session(id, title)` copies the session row and its messages. Tool-call
  rows are *not* copied (their primary key is the LLM's call id and would
  collide); a forked session keeps the transcript but has no replay history.
- `replay_tool_call(id|last)` re-runs a stored tool call directly — no LLM
  round-trip. Lookup accepts an exact id, a unique prefix, or `last`.
- `search` is two-pronged: substring match over prompts, cosine (≥ 0.30) over
  memories with a text fallback.

---

## 4. Extending with custom tools

Tools live in `crates/tools/` and are registered by `ToolRegistry`.

### Built-ins

| Tool | Kind | Notes |
| --- | --- | --- |
| `read` | filesystem | file/dir reads with limits |
| `write` | filesystem | create/overwrite files |
| `edit` | filesystem | exact-string replacement |
| `ls` | filesystem | directory listing |
| `glob` | filesystem | pattern search (`**/*.rs`) |
| `grep` | filesystem | regex content search |
| `bash` | shell | runs commands through the platform shell |
| `skill` | agent | loads a skill's instructions |
| `task` | agent | spawns a subagent (disabled in subagents: recursion is impossible) |

Plan mode filters the registry to the read-only allowlist
(`read`, `grep`, `glob`, `ls`), and additionally refuses `execute_tools`
calls outright.

### Adding a tool

1. Create `crates/tools/src/builtin/<name>.rs` implementing the `Tool` trait:
   `name()`, `description()`, `parameters_schema()` (JSON Schema), and
   `execute(args, ctx)`. Return human-readable output; errors become tool
   results the model can react to.
2. Export it from `builtin/mod.rs` and register it in
   `ToolRegistry::with_builtins()` (`registry.rs`).
3. If the tool mutates anything, also add it to the plan-mode refusal in
   `agent.rs` so plan review stays safe.
4. Add a unit test next to the tool, and (if it touches the agent loop) an
   e2e case under `crates/tui/tests/` or `crates/agent-cli/tests/`.

Tools receive a `ToolContext` with the workspace path — always resolve
user-provided paths against it, and reject paths escaping the workspace unless
the tool is meant to touch the whole machine (`bash` exempt by design).

### Skills as lightweight extensions

For instructions instead of code: drop `<SICK_HOME>/skills/<name>/SKILL.md`
(with `name:`/`description:` front matter in the body format the `skill` tool
parses) and load it with `/skill <name>` in the TUI. Active skills are
appended to the system prompt for the session. `/skill new <name>` scaffolds
a file for you.

---

## 5. Security model

### What is protected, and how

| Asset | Protection |
| --- | --- |
| API keys at rest | ChaCha20-Poly1305 (`keys.enc`); nonce 12 random bytes per save; envelope holds version/scheme/nonce/ciphertext (+salt/iterations for password mode) |
| Master key (default) | OS keychain under `sick:keys-master` — Windows Credential Manager (`CredWriteW`, local machine scope), Linux Secret Service via `secret-tool`, macOS Keychain via `security` |
| Master key (fallback) | PBKDF2-HMAC-SHA256, 210 000 iterations, 16-byte random salt, 32-byte key — derived from your password, never stored |
| Keys in memory | `Zeroizing` buffers wipe secrets on drop; secrets are never logged (only 8-char non-secret ids) |
| File permissions | `0600` on unix for `keys.enc`, keyring files, and exports |
| Crash safety | Previous file kept as `keys.enc.bak` on every save; automatic backup recovery on decrypt failure |

### Threat model

Protected: casual disk inspection, cloud-synced home directories, backups,
screen sharing (ids only), leaked config files.

Not protected against: malware running as you (it can read the keychain /
prompt memory), a compromised OS keychain, physical access to an unlocked
session, and **plaintext exports** (`sick export` without `--encrypt`) or the
`*.bak` files you choose to keep.

### Deliberate trade-offs

- **`SICK_KEYRING_FILE`** stores the master key as hex in a file. That's the
  documented headless mode (servers, CI, tests) — it trades keychain
  integration for scriptability. Use it only where disk access is trusted.
- **Plaintext migration backups** (`keys.toml.bak`) keep a copy of your
  legacy plaintext file after the first encrypted save. Delete them once
  you've confirmed the encrypted store works.
- **WAL + `synchronous=NORMAL`** SQLite means a power loss can lose the last
  few transactions of the *session log* — never corrupt it, and never touch
  keys (which aren't in SQLite).
- **Password mode has no rekey ceremony**: changing the master password means
  exporting and re-importing. The salt/iteration count live in the envelope so
  future versions can upgrade parameters.

### Error recovery behaviors

- `keys.enc` unreadable → automatic `.bak` recovery, with a warning telling
  you to re-import.
- Wrong/failed password → clear `could not decrypt` error; the file is left
  untouched.
- No keychain and no password → interactive prompt to create a master
  password; non-interactive callers get `SICK_KEY_PASSWORD`/
  `SICK_KEYRING_FILE` instructions instead of a hang.
- Corrupt JSON in `keys.enc` → the save path overwrites it only after
  backing it up, so a broken file never blocks re-import.
- **Restoring an encrypted export**: `sick export --encrypt` files use the
  `keys.enc` format. Copy one to `<SICK_HOME>/keys.enc` and unlock with its
  password (`SICK_KEY_PASSWORD` or the prompt); `SICK_KEYRING_FILE` is not
  involved because the export is password-protected.
