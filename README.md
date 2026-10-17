# S.I.C.K

**A BYOK (bring-your-own-key) multi-provider shell + coding agent for the terminal.**

S.I.C.K is a Claude-Code-style CLI/TUI: it chats with OpenAI, Anthropic, Google
Gemini, DeepSeek, OpenRouter, or Qwen models, runs shell commands and file
tools for you, remembers what it did across sessions, and keeps your API keys
encrypted at rest. It's written in Rust and ships as a single binary named
`sick`.

![build](https://img.shields.io/badge/build-passing-brightgreen)
![version](https://img.shields.io/badge/version-0.1.0-blue)
![license](https://img.shields.io/badge/license-Apache--2.0-green)

---

## Features

- **Any provider, your keys** — OpenAI, Anthropic, Google (Gemini), DeepSeek,
  OpenRouter, Qwen; OpenAI-compatible custom endpoints via `base_url`.
- **Encrypted key storage** — keys live in `keys.enc`, encrypted with
  ChaCha20-Poly1305; the master key is kept in the OS keychain (Windows
  Credential Manager, Secret Service on Linux, Keychain on macOS) or derived
  from a master password when no keychain exists.
- **Key rotation & health tracking** — multiple keys per provider, round-robin
  selection, automatic cooldowns for 429s and flaky keys, permanent skip for
  rejected/exhausted keys, optional provider fallback chains.
- **Full TUI** — streaming responses, tool-call cards, command palette,
  plan review, memory browser, 20+ slash commands, shell passthrough
  (`!command`) and Kali WSL mode (`/shell`) on Windows.
- **Customizable TUI** — eight built-in color themes, persistent custom
  palettes, and concise progress messages while longer turns are running.
- **Plan mode** — `/plan <task>` drafts a step-by-step plan with read-only
  tools; approve, edit, or cancel before anything runs.
- **Skills** — reusable instruction bundles loaded with `/skill`.
- **Subagents** — the `task` tool spawns focused child agents for research and
  reports back; `/agent <task>` explicitly delegates focused work and
  `/agents` lists runs.
- **MCP servers** — connect Model Context Protocol servers via `[mcp]` config;
  their tools join the registry as `<server>-<tool>` (per-server failure
  isolation; `/mcp` shows status).
- **Sandboxed shell** — optionally run the `bash` tool inside `bwrap`,
  `firejail`, or a Docker container; fail-closed when the backend is missing.
- **Extensions** — dynamic-library plugins that add tools, observe every
  message, or provide their own LLM provider (see `crates/example-extension`).
- **Web UI** — `sick web` serves a localhost chat page (WebSocket streaming of
  tokens, tool calls, and results) with no build step.
- **Memory & sessions** — every session is persisted to SQLite; prompts are
  searchable and available for keyboard recall for seven days, tool calls are
  replayable (`/replay last`), and the agent recalls recent outcomes and failed
  tool approaches automatically.
- **Shell completions** — generated for bash, zsh, fish, PowerShell, elvish.
- **No plaintext secrets left behind** — importing from env or a plaintext
  `keys.toml` migrates the file to `*.bak` once the contents are encrypted.

## Installation

Requires Rust 1.96+ (edition 2024).

```bash
# From a checkout of this repository
cargo install --path crates/agent-cli --locked

# or build a release binary at target/release/sick(.exe)
cargo build --release
```

After installing, run the CLI directly with `sick` (no `cargo run` required).
On Windows, Cargo installs `sick.exe` in `%USERPROFILE%\.cargo\bin`; make sure
that directory is on your `PATH`. On Linux and macOS it installs into
`~/.cargo/bin`.

Pre-built binaries: none are published yet — build from source above.

## Quick start

```bash
# 1. Store a key, encrypted (pick a provider, paste the key, it is validated)
sick keys add

# ...or import every provider key already in your environment
export OPENAI_API_KEY=sk-...
sick import --from-env

# 2. Check what is configured
sick --keys

# 3. Ask for something (non-interactive)
sick "create a hello.txt file containing hi"

# 4. Or open the full TUI
sick
```

In the TUI, type `/` to open the command palette, `/help` for every command
and key binding, and start any message with `!` to run it directly in the host
shell. On Windows, `/shell` switches to the `kali-linux` WSL distribution so
you can run Kali commands without the LLM; install it first with
`wsl --install -d kali-linux`. Set `SICK_KALI_WSL_DISTRO` if your Kali
distribution has a different WSL name. On Linux and macOS, `/shell` runs the
configured platform shell.

### TUI themes, memory, and delegation

- While the agent is responding, the header animates a spinner and rotates
  through witty thinking phrases.
- `/themes` (or `/theme`) opens a searchable palette picker with color previews.
  `/theme list` prints the available palettes, and `/theme frost-dark` selects
  one directly.
- Create and activate a custom palette with:

  ```text
  /theme create ocean #0B1220 #E0F2FE #38BDF8 #64748B #34D399 #FBBF24 #F87171 #7DD3FC
  ```

  The colors are background, primary text, accent, secondary, success,
  warning, error, and metadata. Custom themes are stored in
  `<SICK_HOME>/themes.toml`; their active selection is remembered there too.
- Prior prompts from the past seven days are loaded into Up/Down input history
  at startup. `/history` and `/search` remain available for browsing stored
  work; sessions are not automatically deleted.
- `/skills` opens the searchable skill picker; `/skill` and `/skill list` print
  the available skills, while `/skill new <name>` scaffolds one. Use
  `/agent <task>` to ask the agent to delegate a focused task to a subagent, or
  let the agent use the `task` tool during normal work.
- `/keys` lists configured providers and key ids. Remove a saved key with
  `/keysrm <provider> <key-id>` or its `/keyssm` alias
  (environment-variable keys must be removed by
  clearing their environment variable instead).
- Cloudflare Workers AI needs a 32-character account ID in addition to its API
  token. Set it once with `/cloudflare <account-id>`; SICK saves the endpoint
  in the encrypted key store, so it works after restart without an environment
  variable. `/keys add cloudflare` collects both values during setup. The
  account ID is not the API token ID.

## CLI reference

| Command | What it does |
| --- | --- |
| `sick "PROMPT"` | Run one prompt non-interactively (streams to stdout) |
| `sick` / `sick -i` | Open the interactive TUI |
| `sick --keys` | Show every configured key and its health status |
| `sick keys add [PROVIDER]` | Interactive provider picker + hidden key paste (+ live validation) |
| `sick keys [list]` | Same as `--keys` |
| `sick keys remove <PROVIDER> <KEY_ID>` | Remove one key (id shown by `sick keys`) |
| `sick import --from-env` | Import every provider key found in the environment |
| `sick import --file keys.toml [--validate]` | Import a plaintext keys file (`--validate` makes one live request per key) |
| `sick export -o out.toml [--encrypt]` | Export stored keys (plaintext TOML, or password-encrypted with `--encrypt`) |
| `sick sessions` (`ls`) | List saved sessions, newest first |
| `sick resume <ID> [PROMPT]` | Print a transcript, or continue the session with PROMPT |
| `sick fork <ID> [--title T]` | Copy a session (and its history) into a new one |
| `sick history [-l N]` | Recent prompt history (default 20) |
| `sick search <QUERY>` | Search prompts + semantic memory matches |
| `sick --resume <ID>` | Continue a session; with `-i` the TUI opens on it |
| `sick --plan "PROMPT"` | Draft a plan with read-only tools instead of executing |
| `sick -c "COMMAND"` | Run one shell command directly and propagate its exit code |
| `sick -C config.toml ...` | Use a specific config file |
| `sick web [--port N]` | Serve the local web UI (chat over WebSocket on 127.0.0.1) |
| `sick --generate-completions <SHELL>` | Print completions (bash, zsh, fish, powershell, elvish) |

> Note: `-C` (capital) selects a config file; `-c` (lowercase) runs a command.

Shell completions:

```bash
sick --generate-completions bash > ~/.local/share/bash-completion/completions/sick
sick --generate-completions powershell > sick.ps1
```

## Configuration

Configuration is resolved in this order (first hit wins):

1. `--config <PATH>` / `-C <PATH>`
2. `.sick/settings.toml` in the workspace (legacy `.shell-agent/settings.toml` also read)
3. `<SICK_HOME>/config.toml` (default `~/.sick/config.toml`)
4. built-in defaults

`SICK_HOME` overrides the state directory (`~/.sick` by default; the legacy
`SHELL_AGENT_HOME` is also read). It holds `config.toml`, `keys.enc`,
`sessions.db`, and `skills/`.

Example `config.toml` (see `config/example-config.toml`):

```toml
[llm]
default_provider = "google"
default_model = "Gemini 3.5 Flash-Lite"
temperature = 0.2
max_tokens = 64000
# base_url = "https://api.openai.com/v1"   # OpenAI-compatible endpoints

# Optional: try these when every key of the primary provider is unusable.
# fallback_chain = [
#   { provider = "anthropic", model = "claude-3-7-sonnet-20250219" },
#   { provider = "google", model = "gemini-2.0-flash" },
# ]
```

CLI flags override the config file: `--provider`, `--model`.

Beyond `[llm]`, the config file supports the Phase 6 sections `[mcp]`
(Model Context Protocol servers), `[sandbox]` (sandboxed `bash`), and
`[extensions]` (dynamic-library plugins), plus `[web]` for the local web UI —
see [`docs/advanced.md`](docs/advanced.md) and `config/example-config.toml`.

## Key management

### Where keys come from

Priority order at startup:

1. **Environment variables** — `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`,
   `GOOGLE_API_KEY` / `GEMINI_API_KEY`, `DEEPSEEK_API_KEY`,
   `OPENROUTER_API_KEY`, `QWEN_API_KEY`. Numbered variants (`_1`…`_9`),
   `SICK_<BASE>` and legacy `SHELL_AGENT_<BASE>` names are also accepted.
2. **`<SICK_HOME>/keys.enc`** — the encrypted store (preferred).
3. **`<SICK_HOME>/keys.toml`**, then `keys.json` — plaintext legacy files
   (read for compatibility; migrated to `*.bak` on the next encrypted save).

### Encrypted storage

`keys.enc` is a JSON envelope; the ciphertext is the TOML keys document
encrypted with **ChaCha20-Poly1305**. The 32-byte master key is stored in the
OS keychain (`sick:keys-master`). When no keychain is available SICK falls
back to a **master password** (PBKDF2-HMAC-SHA256, 210 000 iterations, random
salt):

- `SICK_KEY_PASSWORD` — supply the master password non-interactively.
- `SICK_KEYRING_FILE=<path>` — headless/file-based keyring: the master key is
  stored (hex, 0600 on unix) at `<path>`. Useful for servers and CI; skip real
  keychain integration entirely.

Every save keeps a backup at `keys.enc.bak`; if the primary file is ever
unreadable, SICK recovers from the backup automatically and tells you to
re-import. On the first encrypted save after an import, plaintext
`keys.toml` / `keys.json` files are renamed to `*.bak` so secrets don't linger
in plaintext.

### Import / export

```bash
sick import --from-env              # every provider key from the environment
sick import --file keys.toml        # from a plaintext keys file
sick import --from-env --validate   # + one live request per key to verify it

sick export -o keys-backup.toml               # plaintext TOML (backup/restore format)
sick export -o keys-backup.enc --encrypt      # password-encrypted export
```

A plaintext export prints a loud warning on stderr; keep those files private
(they are valid `keys.toml` inputs for `sick import --file`). An encrypted
export is in the same format as `keys.enc`: to restore it, copy it to
`<SICK_HOME>/keys.enc` and provide the password (`SICK_KEY_PASSWORD` or the
interactive prompt).

### Rotation and health

- `sick keys` shows one line per key: `Key <id>...  [Status]  reset in: 45s  last used: 2m ago`.
- Statuses: `Active`, `RateLimited` (HTTP 429, cooldown from the provider's
  retry hint or 60 s), `UnknownError` (5xx/network; after more than 3
  consecutive failures a 30 s cooldown), `Invalid` (401/403, skipped forever),
  `Exhausted` (quota/context, skipped forever).
- Selection is round-robin across healthy keys; expired cooldowns are promoted
  back to `Active` automatically (and by `Agent::refresh_key_health`).
- When every key of the primary provider is unusable, the optional
  `fallback_chain` providers are tried in order.

## Provider setup

| Provider | `--provider` id | Env vars | Notes |
| --- | --- | --- | --- |
| OpenAI | `openai` | `OPENAI_API_KEY` | default model family `gpt-*` |
| Anthropic | `anthropic` | `ANTHROPIC_API_KEY` | Claude models |
| Google Gemini | `google` (alias `gemini`) | `GOOGLE_API_KEY`, `GEMINI_API_KEY` | uses the OpenAI-compatible endpoint; `base_url` set automatically |
| DeepSeek | `deepseek` | `DEEPSEEK_API_KEY` | OpenAI-compatible |
| OpenRouter | `openrouter` | `OPENROUTER_API_KEY` | hundreds of models via one key |
| Qwen | `qwen` | `QWEN_API_KEY` | DashScope compatible mode |
| Cloudflare Workers AI | `cloudflare` | `CLOUDFLARE_API_KEY`, optional `CLOUDFLARE_ACCOUNT_ID` | `/cloudflare <account-id>` stores the account ID encrypted; `/keys add cloudflare` asks for it |
| Anything else | any id + `base_url` in config | `<ID>_API_KEY` | OpenAI-compatible endpoints |

All of these work through the same OpenAI-compatible chat API, so custom
endpoints (local llama.cpp, vLLM, LM Studio, …) only need a `base_url` and a
key entry. See `docs/guide.md` for the full provider reference.

## Memory & sessions

Everything the agent does is persisted to `<SICK_HOME>/sessions.db` (SQLite):

- **Sessions** — id, title, model, provider, workspace, timestamps.
- **Messages** — the full transcript, per turn.
- **Tool calls** — id, name, args, result, duration, exit code. Re-runnable
  with `/replay <id|last>` (runs the tool directly, no LLM round-trip).
- **Prompts** — every user prompt, searchable via `sick search` / `/search`.
- **Memories** — after any turn that used tools, a compact
  `Task/Outcome/Tools` summary is embedded (384-dim, dependency-free hashed
  unigram+bigram vectors) and stored. On later turns the top 3 memories with
  cosine ≥ 0.30 from *other* sessions are injected as an ephemeral system
  addendum — they are never persisted into the transcript, and sessions never
  recall their own memories.

## TUI usage

```
 Enter send · Ctrl+Enter newline · Up/Down history · Ctrl+R history recall
 PgUp/PgDn scroll · Esc cancel/quit · Ctrl+C quit · type / for the palette
```

Slash commands include:

| Command | What it does |
| --- | --- |
| `/help` | Commands + key bindings |
| `/plan <task>` | Draft a plan; then approve · edit · cancel |
| `/skill [list \| new <name> \| <name>]` | Manage and load skills |
| `/agents` | Subagent runs spawned by the `task` tool |
| `/model [provider:]model`, `/models` | Switch / list models |
| `/keys`, `/keysadd` | Key status; paste a new key (encrypted save) |
| `/sessions`, `/resume <id>`, `/fork [title]` | Session management |
| `/history`, `/search <query>`, `/replay <id\|last>` | Prompt/memory/tool-call history |
| `/shell` | Toggle shell passthrough (`!command` also works) |
| `/mcp` | MCP server status and their exposed tools |
| `/clear`, `/theme [dark\|light]`, `/config`, `/quit` | Display and misc |

Any input starting with `!` runs directly in the shell (with your approval
prompt shown in the transcript) and feeds the result back to the model.

## Benchmarks

```bash
cargo bench -p shell-agent-core
```

Covers key lookup/rotation, embedding, SQLite writes, memory search, and the
full agent turn against an instant provider. Results and targets live in
[`docs/BENCHMARKS.md`](docs/BENCHMARKS.md).

## FAQ

**Is my API key ever written to disk in plaintext?**
Not by S.I.C.K. Saved keys go to `keys.enc` (ChaCha20-Poly1305). Two deliberate
exceptions: `sick export` without `--encrypt` writes a plaintext backup (with
a warning), and plaintext legacy files you created yourself are only migrated
to `*.bak` — you can delete the backups any time.

**Where is the master key?**
In your OS keychain (`sick:keys-master`) by default. With
`SICK_KEYRING_FILE` it's in that file; with a master password it's derived from
`SICK_KEY_PASSWORD` / the interactive prompt and never stored.

**What if I lose the master key / move machines?**
The encrypted file is not portable without its key. Re-import your keys
(`sick import --from-env` or `--file`) on the new machine — or set
`SICK_KEYRING_FILE` and export/import the keyring file yourself.

**Do I need a key for every provider?**
No. Configure one provider's key and S.I.C.K uses it. The `fallback_chain` is
optional.

**Does the agent execute commands without asking?**
In the TUI, shell commands surface in the transcript and sensitive tools ask
for confirmation. Plan mode runs *only* read-only tools (`read`, `grep`,
`glob`, `ls`) and never mutates anything.

**How do I see what a session did?**
`sick sessions`, then `sick resume <id>` (transcript) — or in the TUI,
`/sessions`, `/history`, `/search`, and `/replay last`.

**Which Rust version?** 1.96+ (edition 2024).

## License

Apache-2.0.
