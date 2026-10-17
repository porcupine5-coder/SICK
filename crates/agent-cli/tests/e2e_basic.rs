//! End-to-end smoke tests for the `sick` binary.

use std::path::PathBuf;
use std::process::Command;

fn sick() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sick"));
    // Isolate from the host environment: no provider keys (new or legacy
    // names), no state directory overrides.
    for var in [
        "OPENAI_API_KEY",
        "OPENAI_API_KEY_1",
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
        "SICK_KEYRING_FILE",
        "SHELL_AGENT_KEYRING_FILE",
        "SICK_KEY_PASSWORD",
        "SHELL_AGENT_KEY_PASSWORD",
    ] {
        cmd.env_remove(var);
    }
    cmd
}

fn temp_home(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "sick-e2e-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("create temp home");
    dir
}

#[test]
fn version_prints_version_string() {
    let output = sick()
        .arg("--version")
        .output()
        .expect("failed to run sick --version");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("sick"), "stdout was: {stdout}");
    assert!(
        stdout.contains(env!("CARGO_PKG_VERSION")),
        "stdout was: {stdout}"
    );
}

#[test]
fn help_prints_all_flags() {
    let output = sick()
        .arg("--help")
        .output()
        .expect("failed to run sick --help");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    for flag in [
        "--config",
        "--provider",
        "--model",
        "--keys",
        "--interactive",
        "--resume",
        "--command",
        "--plan",
        "--generate-completions",
        "--help",
        "--version",
    ] {
        assert!(
            stdout.contains(flag),
            "help output missing {flag}: {stdout}"
        );
    }
    assert!(
        stdout.contains("keys"),
        "help output should list the keys subcommand: {stdout}"
    );
    assert!(
        stdout.contains("import") && stdout.contains("export"),
        "help output should list the import/export subcommands: {stdout}"
    );
}

#[test]
fn missing_api_key_reports_helpful_error() {
    let home = temp_home("nokey");
    let output = sick()
        .arg("create a hello.txt file")
        .env("SICK_HOME", &home)
        .output()
        .expect("failed to run sick");

    assert!(
        !output.status.success(),
        "expected failure without an API key"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("OPENAI_API_KEY"),
        "stderr should point at the missing env var: {stderr}"
    );
    assert!(
        stderr.contains("keys.enc"),
        "stderr should mention the encrypted keys file: {stderr}"
    );
    assert!(
        stderr.contains("sick keys add"),
        "stderr should suggest the setup flow: {stderr}"
    );
}

#[test]
fn no_prompt_prints_usage_and_exits_nonzero() {
    let home = temp_home("noprompt");
    let output = sick()
        .env("SICK_HOME", &home)
        .output()
        .expect("failed to run sick");

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("PROMPT"), "stderr was: {stderr}");
}

#[test]
fn interactive_without_tty_reports_terminal_requirement() {
    let home = temp_home("tty");
    let output = sick()
        .arg("--interactive")
        .env("SICK_HOME", &home)
        .output()
        .expect("failed to run sick --interactive");

    // `Command::output` never attaches a TTY to stdin, so the interactive
    // path must fail fast with a usage hint instead of hanging.
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("terminal"),
        "stderr should explain the terminal requirement: {stderr}"
    );
    assert!(stderr.contains("PROMPT"), "stderr was: {stderr}");
}

#[test]
fn sick_prefixed_env_key_is_listed() {
    let home = temp_home("sickenv");
    let output = sick()
        .arg("--keys")
        .env("SICK_HOME", &home)
        .env("SICK_OPENAI_API_KEY", "sk-sick-test-123456")
        .output()
        .expect("failed to run sick --keys");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("openai"), "stdout was: {stdout}");
    assert!(stdout.contains("sk-sick-"), "stdout was: {stdout}");
    assert!(stdout.contains("[Active]"), "stdout was: {stdout}");
}

#[test]
fn legacy_shell_agent_env_and_home_still_work() {
    // Old environment variable names and the old home directory override keep
    // working for existing setups.
    let home = temp_home("legacyenv");
    let output = sick()
        .arg("--keys")
        .env("SHELL_AGENT_HOME", &home)
        .env("SHELL_AGENT_OPENAI_API_KEY", "sk-legacy-test-123456")
        .output()
        .expect("failed to run sick --keys");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("openai"), "stdout was: {stdout}");
    assert!(stdout.contains("sk-legac"), "stdout was: {stdout}");
    assert!(stdout.contains("[Active]"), "stdout was: {stdout}");
}

#[test]
fn keys_subcommand_lists_and_add_requires_terminal() {
    let home = temp_home("keyscmd");

    // `sick keys list` (and bare `sick keys`) behave like `--keys`.
    for args in [vec!["keys", "list"], vec!["keys"]] {
        let output = sick()
            .args(&args)
            .env("SICK_HOME", &home)
            .env("SICK_OPENAI_API_KEY", "sk-sick-test-123456")
            .output()
            .expect("failed to run sick keys");
        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("sk-sick-"), "stdout was: {stdout}");
    }

    // `sick keys add` is interactive: without a TTY it must fail fast.
    let output = sick()
        .args(["keys", "add", "openai"])
        .env("SICK_HOME", &home)
        .output()
        .expect("failed to run sick keys add");
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("terminal"),
        "stderr should explain the terminal requirement: {stderr}"
    );
}

#[test]
fn command_flag_runs_shell_and_propagates_exit_code() {
    let home = temp_home("command");

    // `-c` runs one shell command and exits 0 — no API key needed.
    let output = sick()
        .args(["-c", "echo hello-from-sick-cli"])
        .env("SICK_HOME", &home)
        .output()
        .expect("failed to run sick -c");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("hello-from-sick-cli"),
        "stdout was: {stdout}"
    );

    // The long form works too, and non-zero exit codes propagate.
    let output = sick()
        .args(["--command", "exit 3"])
        .env("SICK_HOME", &home)
        .output()
        .expect("failed to run sick --command");
    assert_eq!(output.status.code(), Some(3));
}

#[test]
fn plan_flag_requires_a_prompt_and_mentions_plan_mode() {
    // Without a prompt `--plan` falls through to the interactive path; with a
    // non-TTY stdin that exits 2 with the terminal hint (same as no prompt).
    let home = temp_home("planflag");
    let output = sick()
        .arg("--plan")
        .env("SICK_HOME", &home)
        .output()
        .expect("failed to run sick --plan");
    assert_eq!(output.status.code(), Some(2));
}

/// Seed `<home>/sessions.db` with one stored session (prompt, memory, and a
/// short transcript); returns the session id.
async fn seed_session(home: &std::path::Path) -> String {
    let store = shell_memory::MemoryStore::init(&home.join("sessions.db").to_string_lossy())
        .await
        .expect("seed store");
    let session_id = store
        .create_session("deploy checklist", "gpt-4.1", "openai", "/tmp/ws")
        .await
        .expect("seed session");
    store
        .save_prompt(
            Some(&session_id),
            "check the deploy checklist for friday",
            "gpt-4.1",
        )
        .await
        .expect("seed prompt");
    store
        .add_memory(
            "Task: check the deploy checklist\nOutcome: verified the firewall rules\nTools: read",
            Some(&shell_memory::embed::to_bytes(&shell_memory::embed::embed(
                "check the deploy checklist",
            ))),
            Some(&session_id),
        )
        .await
        .expect("seed memory");
    for (role, content) in [
        ("user", "check the deploy checklist for friday"),
        ("assistant", "Deploy checklist verified."),
    ] {
        store
            .append_message(
                &session_id,
                &shell_memory::Message {
                    role: role.to_string(),
                    content: Some(content.to_string()),
                    turn_idx: 1,
                    ..Default::default()
                },
            )
            .await
            .expect("seed message");
    }
    session_id
}

#[tokio::test]
async fn sessions_history_search_and_resume_subcommands_work() {
    let home = temp_home("sessions");
    let session_id = seed_session(&home).await;

    // `sick sessions` lists the session; `sick ls` is an alias.
    for sub in ["sessions", "ls"] {
        let output = sick()
            .env("SICK_HOME", &home)
            .arg(sub)
            .output()
            .expect("run sick sessions");
        assert!(output.status.success(), "`sick {sub}` failed");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains(&session_id), "stdout: {stdout}");
        assert!(stdout.contains("deploy checklist"), "stdout: {stdout}");
    }

    // `sick history` shows the stored prompt.
    let output = sick()
        .env("SICK_HOME", &home)
        .arg("history")
        .output()
        .expect("run sick history");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("check the deploy checklist for friday"),
        "stdout: {stdout}"
    );

    // `sick search` finds the prompt and the memory.
    let output = sick()
        .env("SICK_HOME", &home)
        .args(["search", "deploy checklist"])
        .output()
        .expect("run sick search");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Prompts matching"), "stdout: {stdout}");
    assert!(stdout.contains("Memories matching"), "stdout: {stdout}");
    assert!(stdout.contains("firewall rules"), "stdout: {stdout}");

    // `sick resume <id>` (no prompt) prints the transcript.
    let output = sick()
        .env("SICK_HOME", &home)
        .args(["resume", &session_id])
        .output()
        .expect("run sick resume");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&format!("Session {session_id}")),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("You: check the deploy checklist"),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("Assistant: Deploy checklist verified."),
        "stdout: {stdout}"
    );
}

#[tokio::test]
async fn fork_subcommand_copies_the_session() {
    let home = temp_home("fork");
    let session_id = seed_session(&home).await;

    let output = sick()
        .env("SICK_HOME", &home)
        .args(["fork", &session_id, "--title", "branch experiment"])
        .output()
        .expect("run sick fork");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let forked_id = stdout.trim().to_string();
    assert_ne!(forked_id, session_id);
    assert_eq!(forked_id.len(), 36, "new UUID printed: {stdout}");

    // The copy carries the transcript and the requested title.
    let store = shell_memory::MemoryStore::init(&home.join("sessions.db").to_string_lossy())
        .await
        .expect("open store");
    assert_eq!(store.message_count(&forked_id).await.unwrap(), 2);
    let meta = store.get_session(&forked_id).await.unwrap().unwrap();
    assert_eq!(meta.title, "branch experiment");
    assert_eq!(store.message_count(&session_id).await.unwrap(), 2);
}

#[test]
fn resume_unknown_session_errors() {
    let home = temp_home("resume-missing");
    let output = sick()
        .env("SICK_HOME", &home)
        .args(["resume", "does-not-exist"])
        .output()
        .expect("run sick resume");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not found"), "stderr: {stderr}");
}

#[test]
fn import_from_env_stores_keys_encrypted() {
    let home = temp_home("import-enc");
    let keyring = home.join("keyring");
    let output = sick()
        .env("SICK_HOME", &home)
        .env("SICK_KEYRING_FILE", &keyring)
        .env("OPENAI_API_KEY", "sk-e2e-openai-secret")
        .env("ANTHROPIC_API_KEY", "sk-e2e-anthropic-secret")
        .args(["import", "--from-env"])
        .output()
        .expect("run sick import");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Imported 2 key(s)"), "stdout: {stdout}");
    assert!(stdout.contains("keyring file"), "stdout: {stdout}");

    // keys.enc exists, is not plaintext, and decrypts to both secrets.
    let encrypted = home.join("keys.enc");
    assert!(encrypted.is_file(), "keys.enc written");
    let raw = std::fs::read_to_string(&encrypted).unwrap();
    assert!(
        !raw.contains("sk-e2e-openai-secret"),
        "ciphertext leaked a secret: {raw}"
    );
    assert!(
        !raw.contains("OPENAI_API_KEY"),
        "ciphertext leaked metadata: {raw}"
    );

    let backend = shell_key_manager::secure::KeychainBackend::File(keyring.clone());
    let (text, source) = shell_key_manager::secure::load_encrypted(&encrypted, None, &backend)
        .expect("decrypt keys.enc")
        .expect("file exists");
    assert!(text.contains("sk-e2e-openai-secret"));
    assert!(text.contains("sk-e2e-anthropic-secret"));
    assert!(matches!(
        source,
        shell_key_manager::MasterKeySource::KeyringFile(_)
    ));

    // A fresh process (`sick keys`) lists both keys from the encrypted file.
    let output = sick()
        .env("SICK_HOME", &home)
        .env("SICK_KEYRING_FILE", &keyring)
        .arg("keys")
        .output()
        .expect("run sick keys");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("openai"), "stdout: {stdout}");
    assert!(stdout.contains("anthropic"), "stdout: {stdout}");
    assert!(stdout.contains("sk-e2e-o"), "key id shown: {stdout}");
}

#[test]
fn import_from_plaintext_file_migrates_and_encrypts() {
    let home = temp_home("import-file");
    let plaintext = home.join("keys.toml");
    std::fs::write(&plaintext, "[deepseek]\nkeys = [\"ds-e2e-plain-secret\"]\n")
        .expect("write plaintext keys.toml");

    let output = sick()
        .env("SICK_HOME", &home)
        .env("SICK_KEYRING_FILE", home.join("keyring"))
        .args(["import", "--file"])
        .arg(&plaintext)
        .output()
        .expect("run sick import --file");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // The plaintext file is migrated to *.bak and the secret lives encrypted.
    assert!(!plaintext.exists(), "plaintext file migrated away");
    let migrated = home.join("keys.toml.bak");
    assert!(migrated.is_file(), "migration backup exists");
    let raw = std::fs::read_to_string(home.join("keys.enc")).unwrap();
    assert!(!raw.contains("ds-e2e-plain-secret"), "ciphertext leaked");
}

#[test]
fn export_writes_plaintext_and_encrypted_files() {
    let home = temp_home("export");
    let keyring = home.join("keyring");
    let imported = sick()
        .env("SICK_HOME", &home)
        .env("SICK_KEYRING_FILE", &keyring)
        .env("OPENAI_API_KEY", "sk-e2e-export-secret")
        .args(["import", "--from-env"])
        .output()
        .expect("run sick import");
    assert!(imported.status.success());

    // Plaintext export: readable TOML with a loud warning on stderr.
    let plain = home.join("backup.toml");
    let output = sick()
        .env("SICK_HOME", &home)
        .env("SICK_KEYRING_FILE", &keyring)
        .args(["export", "-o"])
        .arg(&plain)
        .output()
        .expect("run sick export");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("plaintext TOML"), "stdout: {stdout}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unencrypted"), "stderr: {stderr}");
    let text = std::fs::read_to_string(&plain).unwrap();
    assert!(text.contains("sk-e2e-export-secret"));
    assert!(text.contains("[openai]"));

    // Encrypted export: password comes from SICK_KEY_PASSWORD (no TTY) and the
    // file decrypts with that password only.
    let encrypted = home.join("backup.enc");
    let output = sick()
        .env("SICK_HOME", &home)
        .env("SICK_KEYRING_FILE", &keyring)
        .env("SICK_KEY_PASSWORD", "master-password-e2e")
        .args(["export", "-o"])
        .arg(&encrypted)
        .arg("--encrypt")
        .output()
        .expect("run sick export --encrypt");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("password-encrypted"), "stdout: {stdout}");
    let raw = std::fs::read_to_string(&encrypted).unwrap();
    assert!(!raw.contains("sk-e2e-export-secret"), "ciphertext leaked");

    let decrypt = |password: &str| {
        shell_key_manager::secure::load_encrypted(
            &encrypted,
            Some(password),
            &shell_key_manager::secure::KeychainBackend::None,
        )
    };
    let (text, _) = decrypt("master-password-e2e")
        .expect("decrypt export")
        .expect("file exists");
    assert!(text.contains("sk-e2e-export-secret"));
    assert!(
        decrypt("wrong-password").is_err(),
        "wrong password rejected"
    );
}

#[test]
fn keys_remove_deletes_one_key_and_keeps_the_rest() {
    let home = temp_home("keys-remove");
    let keyring = home.join("keyring");
    let imported = sick()
        .env("SICK_HOME", &home)
        .env("SICK_KEYRING_FILE", &keyring)
        .env("OPENAI_API_KEY", "sk-rm-openai-secret")
        .env("ANTHROPIC_API_KEY", "sk-ant-rm-secret")
        .args(["import", "--from-env"])
        .output()
        .expect("run sick import");
    assert!(imported.status.success());

    let output = sick()
        .env("SICK_HOME", &home)
        .env("SICK_KEYRING_FILE", &keyring)
        .args(["keys", "remove", "openai", "sk-rm-op"])
        .output()
        .expect("run sick keys remove");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Removed openai key"), "stdout: {stdout}");

    // The openai key is gone; the anthropic key survives.
    let output = sick()
        .env("SICK_HOME", &home)
        .env("SICK_KEYRING_FILE", &keyring)
        .arg("keys")
        .output()
        .expect("run sick keys");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("sk-rm-op"), "stdout: {stdout}");
    assert!(stdout.contains("sk-ant-r"), "stdout: {stdout}");

    // Removing the same id again explains what is known.
    let output = sick()
        .env("SICK_HOME", &home)
        .env("SICK_KEYRING_FILE", &keyring)
        .args(["keys", "remove", "openai", "sk-rm-op"])
        .output()
        .expect("run sick keys remove again");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no keys configured"), "stderr: {stderr}");
}

#[test]
fn completions_generate_for_supported_shells() {
    for shell in ["bash", "zsh", "fish", "powershell"] {
        let output = sick()
            .args(["--generate-completions", shell])
            .output()
            .expect("run sick --generate-completions");
        assert!(output.status.success(), "`{shell}` generation failed");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("sick"),
            "`{shell}` completions should mention sick: {stdout:.200}"
        );
        assert!(
            stdout.len() > 200,
            "`{shell}` completions look empty: {stdout:.200}"
        );
    }

    let output = sick()
        .args(["--generate-completions", "not-a-shell"])
        .output()
        .expect("run sick --generate-completions bad");
    assert!(!output.status.success(), "unknown shells are rejected");
}
