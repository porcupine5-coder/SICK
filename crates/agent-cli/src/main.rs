//! `sick` CLI entry point.
//!
//! Phase 1: non-interactive (prompt-in / summary-out). Phase 3: interactive
//! TUI (ratatui) when no PROMPT is given or `--interactive` is passed.
//! Phase 7: `sick keys add` provider/key setup. Loads TOML config and API
//! keys, opens the SQLite session store, and runs the agent loop.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::{CommandFactory, Parser, Subcommand};
use shell_agent_core::{
    Agent, AgentConfig, AgentMode, AgentRun, KeyValidation, default_model_for, resolve_provider,
    validate_api_key, validation_model,
};
use shell_common::sick_home;
use shell_config::ShellConfig;
use shell_key_manager::{
    KeyError, KeyStore, KeychainBackend, SaveReport, default_encrypted_path, expected_env_vars,
    save_store_encrypted, secure,
};
use shell_memory::MemoryStore;
use tracing_subscriber::EnvFilter;

mod keys_cmd;

/// S.I.C.K: a BYOK multi-provider shell + coding agent.
#[derive(Parser)]
#[command(
    name = "sick",
    version,
    about = "S.I.C.K: a BYOK multi-provider shell + coding agent"
)]
struct Cli {
    /// Path to config file (short flag is `-C`; `-c` runs a command)
    #[arg(short = 'C', long)]
    config: Option<PathBuf>,

    /// Provider to use (e.g. openai, google)
    #[arg(short, long)]
    provider: Option<String>,

    /// Model to use (e.g. gpt-4.1)
    #[arg(short, long)]
    model: Option<String>,

    /// Run one shell command directly and exit with its exit code
    #[arg(short = 'c', long = "command", value_name = "COMMAND")]
    command_line: Option<String>,

    /// Draft a plan in plan mode (read-only tools) instead of executing (non-interactive)
    #[arg(long)]
    plan: bool,

    /// Show configured API keys and their health status, then exit
    #[arg(long)]
    keys: bool,

    /// Start the interactive TUI (also the default when no PROMPT is given)
    #[arg(short, long)]
    interactive: bool,

    /// Resume a saved session: with PROMPT it continues non-interactively,
    /// otherwise the TUI opens on it (see `sick sessions`)
    #[arg(long, value_name = "SESSION_ID")]
    resume: Option<String>,

    /// Print shell completions for SHELL (bash, zsh, fish, powershell, elvish)
    /// and exit
    #[arg(long, value_name = "SHELL")]
    generate_completions: Option<clap_complete::Shell>,

    /// Prompt to run non-interactively
    prompt: Option<String>,

    #[command(subcommand)]
    command: Option<Command>,
}

/// Subcommands available as `sick <command>`.
#[derive(Subcommand)]
enum Command {
    /// Manage API keys (pick a provider and paste a key)
    Keys {
        #[command(subcommand)]
        action: Option<KeysAction>,
    },
    /// List saved sessions, newest first (alias: ls)
    #[command(alias = "ls")]
    Sessions,
    /// Print a stored session's transcript, or continue it:
    /// resume `<id>` `[PROMPT]`
    Resume {
        /// Session id (see `sick sessions`)
        session_id: String,
        /// Optional prompt to continue the session with
        prompt: Option<String>,
    },
    /// Copy a session and its history into a new session
    Fork {
        /// Session id to copy
        session_id: String,
        /// Title for the copy (default: "fork of `<id>`")
        #[arg(long)]
        title: Option<String>,
    },
    /// Show recent prompt history
    History {
        /// Maximum number of prompts to show
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
    },
    /// Search prompt history and memories
    Search {
        /// Text to search for
        query: String,
    },
    /// Import API keys into the encrypted store (from env or a plaintext file)
    Import {
        /// Import every provider key found in the environment
        #[arg(long)]
        from_env: bool,
        /// Import from a plaintext keys file (keys.toml or keys.json)
        #[arg(short, long, value_name = "PATH")]
        file: Option<PathBuf>,
        /// Also validate each imported key with one live request per key
        #[arg(long)]
        validate: bool,
    },
    /// Export stored keys: plaintext TOML by default, password-encrypted with --encrypt
    Export {
        /// File to write (e.g. keys-backup.toml)
        #[arg(short, long, value_name = "PATH", default_value = "keys-export.toml")]
        output: PathBuf,
        /// Encrypt the export with a master password (PBKDF2 + ChaCha20-Poly1305)
        #[arg(long)]
        encrypt: bool,
    },
    /// Serve the local web UI (chat over WebSocket on 127.0.0.1)
    Web {
        /// Port to listen on (default: `[web] port` from the config, else 8080)
        #[arg(long)]
        port: Option<u16>,
    },
}

/// Actions offered by `sick keys`.
#[derive(Subcommand)]
enum KeysAction {
    /// Pick a provider and paste an API key (validated live, then saved)
    Add {
        /// Provider id (e.g. openai, google, mistral, cohere, aionlabs, zai, groq, ...)
        provider: Option<String>,
        /// Save without making a live validation request
        #[arg(long)]
        no_validate: bool,
    },
    /// List configured keys and their health status
    List,
    /// Remove a stored key by its display id (see `sick keys`)
    Remove {
        /// Provider id (e.g. openai, google, mistral, cohere, aionlabs, zai, groq, ...)
        provider: String,
        /// Non-secret key id shown by `sick keys` (first 8 characters)
        key_id: String,
    },
}

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("error: {err:#}");
        std::process::exit(1);
    }
}

async fn run() -> anyhow::Result<()> {
    init_tracing();
    let mut cli = Cli::parse();

    // `--generate-completions`: print and exit before touching any state.
    if let Some(shell) = cli.generate_completions {
        let mut command = Cli::command();
        clap_complete::generate(shell, &mut command, "sick", &mut std::io::stdout());
        return Ok(());
    }

    // Keys: environment variables, then the encrypted `keys.enc`, then the
    // plaintext legacy `keys.toml` / `keys.json`.
    let mut keys = KeyStore::new();
    let from_env = keys.load_from_env();
    tracing::debug!(from_env, "keys loaded from environment");
    if let Err(error) = load_keys_with_prompt(&mut keys) {
        eprintln!("warning: failed to load keys: {error}");
    }

    // Subcommands (`sick keys …`, `sick sessions`, …) run before the prompt/TUI paths.
    if let Some(command) = cli.command.take() {
        return match command {
            Command::Keys { action } => match action.unwrap_or(KeysAction::List) {
                KeysAction::Add {
                    provider,
                    no_validate,
                } => keys_cmd::add(provider, no_validate).await,
                KeysAction::List => {
                    keys.refresh_key_health();
                    print_key_status(&keys);
                    Ok(())
                }
                KeysAction::Remove { provider, key_id } => {
                    cmd_keys_remove(&provider, &key_id).await
                }
            },
            Command::Sessions => cmd_sessions().await,
            Command::Resume { session_id, prompt } => {
                cmd_resume(&cli, keys, session_id, prompt).await
            }
            Command::Fork { session_id, title } => cmd_fork(session_id, title).await,
            Command::History { limit } => cmd_history(limit).await,
            Command::Search { query } => cmd_search(query).await,
            Command::Import {
                from_env,
                file,
                validate,
            } => cmd_import(from_env, file.as_deref(), validate).await,
            Command::Export { output, encrypt } => cmd_export(output, encrypt).await,
            Command::Web { port } => cmd_web(&cli, keys, port).await,
        };
    }

    if cli.keys {
        keys.refresh_key_health();
        print_key_status(&keys);
        return Ok(());
    }

    // `-c/--command`: run one shell command directly, propagate its exit code.
    if let Some(command) = cli.command_line.clone() {
        let code = run_shell_command(&command).await?;
        std::process::exit(code);
    }

    // Interactive TUI: explicit `-i`, or no PROMPT given (phase-3 spec).
    if cli.interactive || cli.prompt.is_none() {
        if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
            eprintln!("sick: interactive mode requires a terminal (stdin/stdout must be a TTY).");
            eprintln!("usage: sick \"PROMPT\"     (run one prompt non-interactively)");
            eprintln!("   or: sick -i          (force the interactive TUI)");
            eprintln!("   or: sick --keys      (show key status)");
            eprintln!("   or: sick keys add    (pick a provider and paste an API key)");
            std::process::exit(2);
        }

        let keys_plaintext = keys.to_toml_text().ok();
        let (agent, _db_path) = build_agent(&cli, keys).await?;

        // `[web] enabled = true`: also serve the web UI on its own agent.
        let web_config = resolve_config(cli.config.as_deref())?.0.web;
        if web_config.enabled {
            let mut web_keys = KeyStore::new();
            if let Some(plaintext) = &keys_plaintext {
                let _ = web_keys.load_toml(plaintext);
            }
            match build_agent(&cli, web_keys).await {
                Ok((web_agent, _)) => {
                    match tokio::net::TcpListener::bind(("127.0.0.1", web_config.port)).await {
                        Ok(listener) => {
                            eprintln!("web UI: http://127.0.0.1:{}", web_config.port);
                            tokio::spawn(async move {
                                if let Err(error) = shell_web_ui::serve(listener, web_agent).await {
                                    eprintln!("web UI stopped: {error}");
                                }
                            });
                        }
                        Err(error) => eprintln!(
                            "warning: web UI port {} unavailable: {error}",
                            web_config.port
                        ),
                    }
                }
                Err(error) => eprintln!("warning: could not build web UI agent: {error:#}"),
            }
        }

        let mut app = shell_tui::App::new(agent);
        if let Some(session_id) = cli.resume.clone() {
            app.resume_on_startup(session_id);
        }
        app.run().await?;
        if let Some(session_id) = &app.state.session_id {
            eprintln!("session: {session_id}");
        }
        return Ok(());
    }

    let prompt = cli.prompt.clone().expect("prompt is present here");
    let plan = cli.plan;
    let (mut agent, db_path) = build_agent(&cli, keys).await?;
    if plan {
        agent.set_mode(AgentMode::Plan);
        eprintln!("plan mode: read-only tools only; the agent will draft a plan.");
    }
    let result = if let Some(session_id) = cli.resume.clone() {
        agent.resume_session(&session_id).await?;
        let turn = agent.run_turn(Some(prompt), None).await?;
        AgentRun {
            session_id: agent.session_id().to_string(),
            content: turn.content,
            iterations: turn.iterations,
            tool_calls: turn.tool_calls,
        }
    } else {
        agent.run(prompt).await?
    };

    if !result.content.trim().is_empty() {
        println!("{}", result.content);
    }
    eprintln!(
        "session: {} ({} iteration(s), {} tool call(s), db: {})",
        result.session_id,
        result.iterations,
        result.tool_calls,
        db_path.display()
    );

    Ok(())
}

/// Run one shell command with inherited stdio; returns its exit code.
async fn run_shell_command(command: &str) -> anyhow::Result<i32> {
    let cwd = std::env::current_dir()?;
    let mut cmd = shell_shell::inherit_command(command, &cwd);
    let status = cmd.status().await?;
    Ok(status.code().unwrap_or(1))
}

/// Open the default session store (creating the database when missing).
async fn open_store() -> anyhow::Result<MemoryStore> {
    let db_path = MemoryStore::default_db_path();
    Ok(MemoryStore::init(&db_path.to_string_lossy()).await?)
}

/// `sick sessions`: list stored sessions, newest first.
async fn cmd_sessions() -> anyhow::Result<()> {
    let store = open_store().await?;
    let sessions = store.list_sessions().await?;
    if sessions.is_empty() {
        println!(
            "No sessions stored yet ({}).",
            MemoryStore::default_db_path().display()
        );
        return Ok(());
    }

    println!("Sessions ({}):", sessions.len());
    for session in sessions {
        let messages = store.message_count(&session.id).await.unwrap_or(0);
        println!("  {}", session.id);
        println!(
            "    {} · {} messages · updated {} · {}",
            session.title,
            messages,
            session.updated_at,
            session.model.as_deref().unwrap_or("-")
        );
    }
    Ok(())
}

/// `sick resume [PROMPT]`: print a stored transcript, or continue the session.
async fn cmd_resume(
    cli: &Cli,
    keys: KeyStore,
    session_id: String,
    prompt: Option<String>,
) -> anyhow::Result<()> {
    if let Some(prompt) = prompt {
        let (mut agent, db_path) = build_agent(cli, keys).await?;
        agent.resume_session(&session_id).await?;
        let turn = agent.run_turn(Some(prompt), None).await?;
        if !turn.content.trim().is_empty() {
            println!("{}", turn.content);
        }
        eprintln!(
            "session: {} ({} iteration(s), {} tool call(s), db: {})",
            session_id,
            turn.iterations,
            turn.tool_calls,
            db_path.display()
        );
        return Ok(());
    }

    let store = open_store().await?;
    let Some(meta) = store.get_session(&session_id).await? else {
        anyhow::bail!("session `{session_id}` not found — see `sick sessions`");
    };
    let history = store.load_session_history(&session_id).await?;

    println!("Session {} — {}", meta.id, meta.title);
    println!(
        "  model: {} · provider: {} · updated: {}",
        meta.model.as_deref().unwrap_or("-"),
        meta.provider.as_deref().unwrap_or("-"),
        meta.updated_at
    );
    println!();
    for message in &history {
        match message.role.as_str() {
            "user" => println!(
                "You: {}",
                flatten(message.content.as_deref().unwrap_or(""), 400)
            ),
            "assistant" => {
                if let Some(content) = message.content.as_deref()
                    && !content.trim().is_empty()
                {
                    println!("Assistant: {}", flatten(content, 600));
                }
                if message.tool_calls_json.is_some() {
                    println!("Assistant: [tool calls]");
                }
            }
            "tool" => println!(
                "[tool] {}",
                flatten(message.content.as_deref().unwrap_or(""), 200)
            ),
            _ => {}
        }
    }
    Ok(())
}

/// `sick fork [--title]`: copy a session plus its history; prints the new id.
async fn cmd_fork(session_id: String, title: Option<String>) -> anyhow::Result<()> {
    let store = open_store().await?;
    let title = title.unwrap_or_else(|| format!("fork of {}", short_id(&session_id)));
    let new_id = store.fork_session(&session_id, &title).await?;
    println!("{new_id}");
    eprintln!("forked {session_id} → {new_id} ({title})");
    Ok(())
}

/// `sick history`: recent prompts across sessions.
async fn cmd_history(limit: usize) -> anyhow::Result<()> {
    let store = open_store().await?;
    let prompts = store.get_prompt_history(limit).await?;
    if prompts.is_empty() {
        println!("No prompt history yet.");
        return Ok(());
    }

    println!("Recent prompts ({}):", prompts.len());
    for prompt in prompts {
        let session = prompt
            .session_id
            .as_deref()
            .map(short_id)
            .unwrap_or_else(|| "-".to_string());
        println!(
            "  [{}] ({session}) {}",
            prompt.timestamp,
            flatten(&prompt.content, 100)
        );
    }
    Ok(())
}

/// `sick search`: prompt substring hits + semantic memory matches.
async fn cmd_search(query: String) -> anyhow::Result<()> {
    let store = open_store().await?;
    let prompts = store.search_prompts(&query, 20).await?;

    let embedding = shell_memory::embed::embed(&query);
    let mut memories = store.search_memories(&embedding, 10).await?;
    memories.retain(|entry| entry.score.unwrap_or(0.0) >= 0.30);
    if memories.is_empty() {
        memories = store.search_memories_text(&query, 10).await?;
    }

    println!("Prompts matching `{query}` ({}):", prompts.len());
    for prompt in &prompts {
        println!("  [{}] {}", prompt.timestamp, flatten(&prompt.content, 100));
    }
    println!();
    println!("Memories matching `{query}` ({}):", memories.len());
    for memory in &memories {
        let score = memory
            .score
            .map(|value| format!("{value:.2}"))
            .unwrap_or_else(|| "-".to_string());
        println!("  ({score}) {}", flatten(&memory.content, 120));
    }
    Ok(())
}

/// Load the stored keys (encrypted `keys.enc` first, then the plaintext
/// legacy files) into `keys`.
///
/// Password-protected files prompt for the master password when a terminal is
/// available; otherwise set `SICK_KEY_PASSWORD`. Prints a warning when the
/// primary file was unreadable and the `.bak` backup was used instead.
fn load_keys_with_prompt(keys: &mut KeyStore) -> anyhow::Result<()> {
    let encrypted_path = default_encrypted_path();
    match keys.load_encrypted_default(None) {
        Ok(Some(loaded)) => {
            if loaded.recovered {
                eprintln!(
                    "warning: could not read {}; recovered from {}. \
                     Re-run `sick import --from-env` to store a fresh copy.",
                    encrypted_path.display(),
                    secure::backup_path(&encrypted_path).display()
                );
            }
            tracing::debug!(loaded.keys_loaded, "keys loaded from keys.enc");
            Ok(())
        }
        Ok(None) => {
            let legacy = keys.load_legacy_default_files()?;
            if legacy > 0 {
                tracing::debug!(legacy, "keys loaded from plaintext legacy files");
            }
            Ok(())
        }
        Err(KeyError::PasswordRequired) if std::io::stdin().is_terminal() => {
            let password = rpassword::prompt_password("Master password for the keys.enc file: ")?;
            let loaded = keys
                .load_encrypted_default(Some(&password))?
                .expect("PasswordRequired implies the file exists");
            if loaded.recovered {
                eprintln!(
                    "warning: recovered keys from {}",
                    secure::backup_path(&encrypted_path).display()
                );
            }
            tracing::debug!(loaded.keys_loaded, "keys loaded from keys.enc (password)");
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

/// Persist `store` to the encrypted keys file, prompting for a new master
/// password when no OS keychain is available.
fn save_keys_secure(store: &KeyStore) -> anyhow::Result<SaveReport> {
    match save_store_encrypted(store, None) {
        Ok(report) => Ok(report),
        Err(KeyError::PasswordRequired) | Err(KeyError::KeychainUnavailable { .. })
            if std::io::stdin().is_terminal() =>
        {
            let password = prompt_new_password()?;
            Ok(save_store_encrypted(store, Some(&password))?)
        }
        Err(error) => Err(error.into()),
    }
}

/// Prompt twice for a new master password (used when the OS keychain is
/// unavailable and the keys file needs password protection).
pub(crate) fn prompt_new_password() -> anyhow::Result<String> {
    let password = rpassword::prompt_password("Create a master password for keys.enc: ")?;
    if password.is_empty() {
        anyhow::bail!("empty master password — aborting (nothing was saved)");
    }
    let confirm = rpassword::prompt_password("Confirm master password: ")?;
    if password != confirm {
        anyhow::bail!("passwords do not match — aborting (nothing was saved)");
    }
    Ok(password)
}

/// `sick keys remove`: drop one stored key and re-save the encrypted file.
async fn cmd_keys_remove(provider: &str, key_id: &str) -> anyhow::Result<()> {
    // Reload strictly: re-saving a partially loaded store would drop keys.
    let mut keys = KeyStore::new();
    load_keys_with_prompt(&mut keys)?;

    let provider = shell_key_manager::normalize_provider(provider);
    if !keys.remove_key(&provider, key_id) {
        let known: Vec<String> = keys
            .keys_for(&provider)
            .iter()
            .map(|key| key.id.clone())
            .collect();
        if known.is_empty() {
            anyhow::bail!("no keys configured for provider `{provider}`");
        }
        anyhow::bail!(
            "no key `{key_id}` for `{provider}` — known ids: {}",
            known.join(", ")
        );
    }

    let report = save_keys_secure(&keys)?;
    println!(
        "Removed {provider} key {key_id}… ({} left for {provider}).",
        keys.key_count(&provider)
    );
    println!(
        "Saved {} ({})",
        report.path.display(),
        report.source.describe()
    );
    for migrated in &report.migrated {
        println!("Plaintext keys file migrated to {}", migrated.display());
    }
    Ok(())
}

/// `sick import`: merge keys from the environment or a plaintext file into the
/// encrypted store.
async fn cmd_import(from_env: bool, file: Option<&Path>, validate: bool) -> anyhow::Result<()> {
    if !from_env && file.is_none() {
        anyhow::bail!(
            "nothing to import — pass `--from-env` to import environment variables, \
             or `--file <path>` for a plaintext keys file"
        );
    }

    // Merge into the existing store (encrypted or legacy) so an import never
    // drops keys that are already configured.
    let mut keys = KeyStore::new();
    load_keys_with_prompt(&mut keys)?;

    let imported = match (file, from_env) {
        (Some(path), _) => keys.load_from_file(path)?,
        (None, true) => keys.load_from_env(),
        (None, false) => unreachable!("guarded above"),
    };

    // Even with nothing new, an import still encrypts + migrates any
    // plaintext legacy file it found (that is part of the import flow).
    let legacy_present =
        KeyStore::default_keys_toml_path().is_file() || KeyStore::default_keys_path().is_file();
    if imported == 0 && !legacy_present {
        println!("No new keys found to import.");
        return Ok(());
    }

    let report = save_keys_secure(&keys)?;
    if imported > 0 {
        println!("Imported {imported} key(s).");
    } else {
        println!("No new keys found — migrating the existing plaintext keys file.");
    }
    println!(
        "Stored {} (protected by {}).",
        report.path.display(),
        report.source.describe()
    );
    for migrated in &report.migrated {
        println!("Plaintext keys file migrated to {}", migrated.display());
    }

    if validate && imported > 0 {
        validate_stored_keys(&keys).await;
    }
    Ok(())
}

/// Best-effort live validation of every stored key (one minimal request per
/// key). Rejections are reported, never fatal: the keys are already stored.
async fn validate_stored_keys(keys: &KeyStore) {
    println!("Validating stored keys (one request per key)...");
    for provider_id in keys.providers() {
        let model = validation_model(keys, &provider_id);
        let config = AgentConfig::new(&provider_id, model.clone());
        let provider = match resolve_provider(&config, keys, &provider_id) {
            Ok(provider) => provider,
            Err(error) => {
                println!("  ⚠ {provider_id}: cannot validate ({error})");
                continue;
            }
        };
        for key in keys.keys_for(&provider_id) {
            let id = &key.id;
            match validate_api_key(&provider, &model, key.value.as_str()).await {
                Ok(KeyValidation::Verified) => println!("  ✓ {provider_id} {id}… verified"),
                Ok(KeyValidation::Unverified(reason)) => {
                    println!("  ⚠ {provider_id} {id}… stored, verification inconclusive ({reason})")
                }
                Err(error) => {
                    println!("  ✗ {provider_id} {id}… rejected by the provider ({error})")
                }
            }
        }
    }
}

/// `sick export`: write stored keys as plaintext TOML, or password-encrypted
/// with `--encrypt`.
async fn cmd_export(output: PathBuf, encrypt: bool) -> anyhow::Result<()> {
    let mut keys = KeyStore::new();
    load_keys_with_prompt(&mut keys)?;
    if !keys.has_keys() {
        anyhow::bail!(
            "no stored keys to export — run `sick keys add` or `sick import --from-env` first"
        );
    }
    let providers = keys.providers().len();
    let text = keys.to_toml_text()?;

    if encrypt {
        // Password-only protection: a keyring file would not travel with the
        // export, so the export must be self-contained.
        let report = match secure::save_encrypted(&output, &text, None, &KeychainBackend::None) {
            Ok(report) => report,
            Err(KeyError::PasswordRequired) | Err(KeyError::KeychainUnavailable { .. })
                if std::io::stdin().is_terminal() =>
            {
                let password = prompt_new_password()?;
                secure::save_encrypted(&output, &text, Some(&password), &KeychainBackend::None)?
            }
            Err(error) => return Err(error.into()),
        };
        println!(
            "Exported keys for {providers} provider(s) to {} — password-encrypted ({}).",
            report.path.display(),
            report.source.describe()
        );
    } else {
        if let Some(parent) = output.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&output, &text)?;
        eprintln!(
            "warning: {} contains unencrypted API keys — keep it private.",
            output.display()
        );
        println!(
            "Exported keys for {providers} provider(s) to {} (plaintext TOML).",
            output.display()
        );
    }
    Ok(())
}

/// `sick web`: serve the local web UI (the agent is moved into the server).
async fn cmd_web(cli: &Cli, keys: KeyStore, port: Option<u16>) -> anyhow::Result<()> {
    let (config, _) = resolve_config(cli.config.as_deref())?;
    let port = port.unwrap_or(config.web.port);
    let (agent, _db_path) = build_agent(cli, keys).await?;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|error| anyhow::anyhow!("cannot bind 127.0.0.1:{port}: {error}"))?;
    println!("S.I.C.K web UI on http://127.0.0.1:{port} (Ctrl+C to stop)");
    shell_web_ui::serve(listener, agent)
        .await
        .map_err(|error| anyhow::anyhow!("web UI server error: {error}"))?;
    Ok(())
}

/// Collapse whitespace onto one line, truncating to `max` chars.
fn flatten(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        flat.chars().take(max).collect::<String>() + "…"
    }
}

/// First 8 chars of an id, for compact display.
fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

/// Resolve config/model/keys and open the session store into a ready agent.
///
/// Shared by the non-interactive and TUI paths so both honor the same
/// priority order: CLI flags > config file > provider-aware defaults.
async fn build_agent(cli: &Cli, mut keys: KeyStore) -> anyhow::Result<(Agent, PathBuf)> {
    let (config, config_from_file) = resolve_config(cli.config.as_deref())?;
    let provider = cli
        .provider
        .clone()
        .unwrap_or_else(|| config.llm.default_provider.clone());
    let model = match cli.model.clone() {
        Some(model) => model,
        None if config_from_file => config.llm.default_model.clone(),
        None => default_model_for(&provider).to_string(),
    };
    let fallback_chain: Vec<(String, String)> = config
        .llm
        .fallback_chain
        .iter()
        .map(|entry| (entry.provider.clone(), entry.model.clone()))
        .collect();

    // Extensions (Phase 6): load plugin libraries before building the agent,
    // so a provider offered by an extension can replace provider resolution.
    let extensions_dir = shell_extensions::resolve_extensions_dir(&config.extensions.dir);
    let extension_host = shell_extensions::ExtensionHost::load_dir(
        &extensions_dir,
        &shell_extensions::ExtensionContext {
            workspace: std::env::current_dir()?,
            state_dir: shell_common::sick_home(),
            extension_dir: extensions_dir.clone(),
        },
    );
    for (path, error) in extension_host.errors() {
        eprintln!("warning: extension {} not loaded: {error}", path.display());
    }
    let extension_provider = extension_host.provider();

    // At least one of the primary/fallback providers must have a key — unless
    // an extension provides the LLM provider (then no keys are needed).
    if extension_provider.is_none() {
        let mut candidates = vec![provider.clone()];
        candidates.extend(fallback_chain.iter().map(|(p, _)| p.clone()));
        if !candidates.iter().any(|p| keys.key_count(p) > 0) {
            let keys_path = KeyStore::default_keys_toml_path();
            anyhow::bail!(missing_key_message(&provider, &keys_path));
        }
    }

    // Session store.
    let db_path = MemoryStore::default_db_path();
    let memory = MemoryStore::init(&db_path.to_string_lossy()).await?;

    let agent_config = AgentConfig {
        provider,
        model,
        fallback_chain,
        temperature: config.llm.temperature,
        max_tokens: config.llm.max_tokens,
        system_prompt: String::new(),
        workspace: std::env::current_dir()?,
        base_url: config.llm.base_url.clone().unwrap_or_default(),
        sandbox: config.sandbox.clone(),
        mcp: config.mcp.servers.clone(),
    };

    if let Err(reason) = shell_shell::sandbox::validate(&config.sandbox) {
        eprintln!("warning: {reason}");
    }

    let mut agent = match extension_provider {
        Some(extension_provider) => {
            // Extension supplies the provider; still honor `[[mcp.servers]]`.
            // Rotation first requires a healthy key for the configured
            // provider id before the (custom) provider is called; extension
            // providers authenticate themselves, so register a placeholder
            // (in-memory only, never persisted).
            let configured_provider = agent_config.provider.clone();
            if keys.key_count(&configured_provider) == 0 {
                keys.add_key(&configured_provider, "extension-provider");
            }
            let mut agent = Agent::with_provider(agent_config, memory, keys, extension_provider);
            agent.connect_mcp().await;
            agent
        }
        None => Agent::new(agent_config, memory, keys).await?,
    };
    agent.attach_extension_host(std::sync::Arc::new(extension_host));

    for status in agent.mcp_status() {
        if !status.ready {
            eprintln!(
                "warning: MCP server `{}` unavailable ({})",
                status.name, status.state
            );
        }
    }
    Ok((agent, db_path))
}

/// Print every configured key and its health status.
fn print_key_status(keys: &KeyStore) {
    let report = keys.get_status_report();
    if report.is_empty() {
        println!("No API keys configured.");
        println!("  Run `sick keys add` to pick a provider and paste a key (stored encrypted).");
        println!("  Or import every provider key from your environment: `sick import --from-env`.");
        println!(
            "  Or set SICK_<PROVIDER>_API_KEY env vars, or write a plaintext {} \
             (migrated to encrypted on the next save).",
            KeyStore::default_keys_toml_path().display()
        );
        return;
    }

    let mut current_provider = String::new();
    for info in report {
        if info.provider != current_provider {
            if !current_provider.is_empty() {
                println!();
            }
            println!("Provider: {}", info.provider);
            current_provider = info.provider.clone();
        }

        let mut line = format!("  Key {}...  [{}]", info.key_id, info.status);
        if let Some(reset_in) = info.reset_in() {
            line.push_str(&format!("  reset in: {}", format_duration(reset_in)));
        }
        match info.last_used_ago() {
            Some(ago) => line.push_str(&format!("  last used: {} ago", format_duration(ago))),
            None => line.push_str("  last used: never"),
        }
        println!("{line}");
    }
}

/// Compact duration formatting: `45s`, `2m`, `3h`.
fn format_duration(duration: Duration) -> String {
    let secs = duration.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h", secs / 3600)
    }
}

/// Configure tracing to stderr; `RUST_LOG` selects the level (default `warn`).
fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

/// Resolve the config file: explicit `--config` wins, then workspace scope
/// (`.sick/settings.toml`, legacy `.shell-agent/settings.toml`), then the user
/// scope (`<SICK_HOME>/config.toml`), then defaults.
/// The bool reports whether a config file was actually loaded.
fn resolve_config(explicit: Option<&Path>) -> anyhow::Result<(ShellConfig, bool)> {
    if let Some(path) = explicit {
        return Ok((ShellConfig::load(path)?, true));
    }

    for scope in [".sick", ".shell-agent"] {
        let workspace = PathBuf::from(scope).join("settings.toml");
        if workspace.is_file() {
            return Ok((ShellConfig::load(&workspace)?, true));
        }
    }

    let user = sick_home().join("config.toml");
    if user.is_file() {
        return Ok((ShellConfig::load(&user)?, true));
    }

    Ok((ShellConfig::default(), false))
}

/// Helpful, actionable message when a provider has no configured key.
fn missing_key_message(provider: &str, keys_path: &Path) -> String {
    let vars = expected_env_vars(provider).join("` or `");
    format!(
        "no API key found for provider `{provider}`.\n  \
         Run `sick keys add` to paste one (stored encrypted in keys.enc), \
         or `sick import --from-env`, or set `{vars}` in your environment, \
         or add the key to `{}`.",
        keys_path.display()
    )
}
