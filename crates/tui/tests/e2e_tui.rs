//! End-to-end TUI tests: synthetic key events against a real agent actor
//! backed by a scripted provider, rendered on a `TestBackend`.

mod common;

use common::{key, pump_until, render_to_string, scripted_app, scripted_app_with_keys, type_text};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use shell_key_manager::secure::{KeychainBackend, default_encrypted_path, load_encrypted};
use shell_llm::{ChatResponse, ToolCall};
use shell_tui::{InputMode, SetupStage, Theme, ThemePickerItem, ThemePickerMode};
use std::time::{Duration, Instant};

fn text(content: &str) -> ChatResponse {
    ChatResponse {
        content: content.to_string(),
        tool_calls: Vec::new(),
    }
}

#[tokio::test]
async fn streaming_turn_renders_tokens_tools_and_final_answer() {
    let dir = tempfile::tempdir().unwrap();
    let script = vec![
        ChatResponse {
            content: "Creating the file.".to_string(),
            tool_calls: vec![ToolCall {
                id: "call_1".to_string(),
                tool_name: "write".to_string(),
                args: serde_json::json!({"path": "hello.txt", "content": "hi"}),
                thought_signature: None,
            }],
        },
        text("Done — hello.txt is ready."),
    ];
    let mut app = scripted_app(dir.path(), script).await;

    let welcome = render_to_string(&mut app, 110, 34);
    assert!(
        welcome.contains("A I   C O D I N G"),
        "idle welcome shows the SICK splash: {welcome}"
    );

    type_text(&mut app, "create hello.txt");
    app.handle_key(key(KeyCode::Enter));
    assert!(app.is_busy(), "submitting starts a turn");

    assert!(
        pump_until(&mut app, |app| !app.is_busy()).await,
        "turn completes"
    );

    let messages = &app.state.messages;
    assert!(
        messages
            .iter()
            .any(|m| m.role == "user" && m.content == "create hello.txt")
    );
    assert_eq!(
        messages
            .iter()
            .filter(|m| m.role == "assistant" && m.content == "Creating the file.")
            .count(),
        1,
        "streamed tokens are not duplicated by the turn reply"
    );
    assert!(messages.iter().any(|m| m.role == "tool_call"
        && m.tool_name.as_deref() == Some("write")
        && m.content.contains("hello.txt")));
    assert!(
        messages
            .iter()
            .any(|m| m.role == "tool_result" && m.content.contains("[ok"))
    );
    assert!(
        messages
            .iter()
            .any(|m| m.role == "assistant" && m.content == "Done — hello.txt is ready.")
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("hello.txt")).unwrap(),
        "hi",
        "the agent actually executed the write tool"
    );
    assert!(app.state.session_id.is_some(), "session id captured");

    let rendered = render_to_string(&mut app, 110, 34);
    assert!(
        !rendered.contains("A I   C O D I N G"),
        "welcome splash is hidden after work starts"
    );
    assert!(rendered.contains("S.I.C.K"), "header title");
    assert!(rendered.contains("scripted-model"), "header model");
    assert!(rendered.contains("1 key(s)"), "header key count");
    assert!(rendered.contains("Creating the file."), "streamed text");
    assert!(rendered.contains("hello.txt is ready"), "final answer");
    assert!(rendered.contains("Input"), "input box");
}

#[tokio::test]
async fn active_response_shows_animated_thinking_status() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = scripted_app(dir.path(), vec![]).await;
    app.state.busy = true;
    app.state.busy_since = Some(Instant::now() - Duration::from_millis(250));

    let rendered = render_to_string(&mut app, 110, 34);
    assert!(
        rendered.contains("Contemplating"),
        "thinking phrase: {rendered}"
    );
    assert!(
        ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏']
            .iter()
            .any(|frame| rendered.contains(*frame)),
        "animated spinner frame: {rendered}"
    );
}

#[tokio::test]
async fn keysrm_requires_provider_and_display_id() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = scripted_app(dir.path(), vec![]).await;

    app.execute_slash_command("/keysrm");
    assert!(app.state.messages.iter().any(|message| {
        message
            .content
            .contains("Usage: /keysrm <provider> <key-id>")
    }));
    app.execute_slash_command("/keyssm");
    assert!(app.state.messages.iter().any(|message| {
        message
            .content
            .contains("Usage: /keysrm <provider> <key-id>")
    }));
}

#[tokio::test]
async fn cloudflare_key_setup_requests_account_id_separately_from_token() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = scripted_app(dir.path(), vec![]).await;
    app.execute_slash_command("/keys add cloudflare");
    app.handle_paste("cf-api-token-value\n".to_string());
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(
        app.state.key_setup.as_ref().unwrap().stage,
        SetupStage::EnterAccountId
    );
    assert_eq!(
        app.state.key_setup.as_ref().unwrap().key_input,
        "cf-api-token-value"
    );

    app.handle_paste("not-an-account-id".to_string());
    app.handle_key(key(KeyCode::Enter));
    let setup = app.state.key_setup.as_ref().unwrap();
    assert_eq!(setup.stage, SetupStage::EnterAccountId);
    assert!(
        setup
            .status
            .as_deref()
            .is_some_and(|status| status.contains("32-character"))
    );
}

#[tokio::test]
async fn cloudflare_command_rejects_token_id_as_account_id() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = scripted_app(dir.path(), vec![]).await;
    app.execute_slash_command("/cloudflare token-id");
    assert!(app.state.messages.iter().any(|message| {
        message.content.contains("account ID, not the API token ID")
            && message.content.contains("32-character")
    }));
}

#[tokio::test]
async fn commands_history_and_multiline_behave() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = scripted_app(dir.path(), vec![text("ack")]).await;

    // /help renders the command list and clears the input.
    type_text(&mut app, "/help");
    assert_eq!(app.state.input_mode, InputMode::Command);
    assert!(app.state.command_palette);
    assert_eq!(app.state.matching_commands(), vec!["/help"]);
    app.handle_key(key(KeyCode::Enter));
    assert!(app.input_text().is_empty());
    assert!(
        app.state
            .messages
            .iter()
            .any(|m| m.content.contains("/resume")),
        "help lists /resume"
    );

    // The command palette renders while typing a prefix; Esc cancels.
    type_text(&mut app, "/res");
    let rendered = render_to_string(&mut app, 100, 30);
    assert!(rendered.contains("/resume"), "palette shows /resume");
    assert!(
        rendered.contains("load a saved session"),
        "palette description"
    );
    app.handle_key(key(KeyCode::Esc));
    assert!(
        !app.state.should_quit,
        "Esc cancels command mode, not quits"
    );
    assert_eq!(app.state.input_mode, InputMode::Normal);
    assert!(app.input_text().is_empty());

    // Ctrl+C also cancels command mode (rather than quitting).
    type_text(&mut app, "/mod");
    app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert!(
        !app.state.should_quit,
        "Ctrl+C cancels command mode, not quits"
    );
    assert!(app.input_text().is_empty());

    // Built-in themes can be selected directly and listed.
    app.execute_slash_command("/theme light");
    assert_eq!(app.state.theme, Theme::Light);
    app.execute_slash_command("/theme list");
    assert!(
        app.state
            .messages
            .iter()
            .any(|message| message.content.contains("frost-dark"))
    );
    app.execute_slash_command("/theme dark");
    assert_eq!(app.state.theme, Theme::Dark);

    // Ctrl+Enter inserts a newline; Enter submits the whole buffer.
    type_text(&mut app, "line one");
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));
    type_text(&mut app, "line two");
    assert_eq!(app.input_text(), "line one\nline two");
    app.handle_key(key(KeyCode::Enter));
    assert!(pump_until(&mut app, |app| !app.is_busy()).await);
    assert!(
        app.state
            .messages
            .iter()
            .any(|m| m.role == "user" && m.content == "line one\nline two")
    );
    assert_eq!(
        app.state.history.back().map(String::as_str),
        Some("line one\nline two")
    );

    // Up/Down recall history when the input is empty.
    app.handle_key(key(KeyCode::Up));
    assert_eq!(app.input_text(), "line one\nline two");
    app.handle_key(key(KeyCode::Down));
    assert!(app.input_text().is_empty());

    // Ctrl+R opens the recall overlay; Enter loads the selection.
    app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
    assert!(app.state.show_history);
    app.handle_key(key(KeyCode::Enter));
    assert!(!app.state.show_history);
    assert_eq!(app.input_text(), "line one\nline two");

    // Bare /model reports the current selection.
    app.execute_slash_command("/model");
    assert!(
        app.state
            .messages
            .iter()
            .any(|m| m.content.contains("Current model: scripted-model")),
        "bare /model reports the active model"
    );

    // /clear empties the chat pane (leaving the confirmation message).
    app.execute_slash_command("/clear");
    assert_eq!(app.state.messages.len(), 1);
    assert!(app.state.messages[0].content.contains("Chat cleared."));

    // /quit exits.
    app.execute_slash_command("/quit");
    assert!(app.state.should_quit);
}

#[tokio::test]
async fn escape_and_ctrl_c_quit_in_normal_mode() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = scripted_app(dir.path(), vec![]).await;

    app.handle_key(key(KeyCode::Esc));
    assert!(app.state.should_quit, "Esc quits in normal mode");

    app.state.should_quit = false;
    app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert!(app.state.should_quit, "Ctrl+C quits");
}

#[tokio::test]
async fn theme_picker_filters_selects_and_cancels() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = scripted_app(dir.path(), vec![]).await;

    app.execute_slash_command("/themes");
    assert!(app.state.themes_picker.is_some());
    for character in "frost".chars() {
        app.handle_key(key(KeyCode::Char(character)));
    }
    let rendered = render_to_string(&mut app, 100, 30);
    assert!(rendered.contains("Frost Dark"));

    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.state.theme, Theme::FrostDark);
    assert!(app.state.themes_picker.is_none());

    app.execute_slash_command("/theme");
    assert!(app.state.themes_picker.is_some());
    app.handle_key(key(KeyCode::Esc));
    assert!(app.state.themes_picker.is_none());
}

#[tokio::test]
async fn custom_theme_color_wheel_and_json_import_and_persistence() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = scripted_app(dir.path(), vec![]).await;

    // 1. Test persistence of selected theme (in-memory; file round-trip is
    //    covered by unit tests in theme.rs — the e2e tests share SICK_HOME so
    //    reading back the file is racy).
    app.execute_slash_command("/theme vaporwave-sunset");
    assert_eq!(app.state.theme, Theme::VaporwaveSunset);
    // activate_theme calls Theme::save under the hood; verify success via the
    // system message it posts.
    assert!(
        app.state
            .messages
            .iter()
            .any(|m| m.content.contains("Theme: vaporwave-sunset")),
        "theme was activated and persisted"
    );

    // 2. Test Color Wheel launch from picker
    app.execute_slash_command("/themes");
    let picker = app.state.themes_picker.as_ref().unwrap();
    assert_eq!(
        picker.filtered_items()[0],
        ThemePickerItem::CreateCustomWheel
    );
    assert_eq!(
        picker.filtered_items()[1],
        ThemePickerItem::ImportCustomJson
    );

    // Select CreateCustomWheel (index 0) and hit Enter
    app.state.themes_picker.as_mut().unwrap().selection = 0;
    app.handle_key(key(KeyCode::Enter));
    assert!(matches!(
        app.state.themes_picker.as_ref().unwrap().mode,
        ThemePickerMode::ColorWheel(_)
    ));

    // Save custom theme from Color Wheel studio via Ctrl+S
    app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
    assert!(app.state.themes_picker.is_none());
    assert_eq!(app.state.theme.name(), "my-custom-theme");
    assert!(
        app.state
            .themes
            .iter()
            .any(|t| t.name() == "my-custom-theme"),
        "custom wheel theme is in the themes list"
    );

    // 3. Test JSON Theme Importer
    app.execute_slash_command("/themes");
    app.state.themes_picker.as_mut().unwrap().selection = 1; // ImportCustomJson
    app.handle_key(key(KeyCode::Enter));
    assert!(matches!(
        app.state.themes_picker.as_ref().unwrap().mode,
        ThemePickerMode::JsonImport(_)
    ));

    let json = r##"{
        "name": "synth-glow",
        "background": "#110022",
        "primary": "#EEEEEE",
        "accent": "#FF00AA",
        "secondary": "#550088",
        "success": "#00FF88",
        "warning": "#FFAA00",
        "error": "#FF0044",
        "metadata": "#00CCFF"
    }"##;
    app.handle_paste(json.to_string());
    // Press Ctrl+S to save and apply
    app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
    assert!(app.state.themes_picker.is_none());
    assert_eq!(app.state.theme.name(), "synth-glow");

    // Verify custom theme is in the themes list and was activated.
    assert!(
        app.state.themes.iter().any(|t| t.name() == "synth-glow"),
        "synth-glow should be in the themes list"
    );
    assert!(
        app.state
            .messages
            .iter()
            .any(|m| m.content.contains("Theme: synth-glow")),
        "synth-glow theme was activated"
    );
}

#[tokio::test]
async fn skills_picker_opens_and_cancels() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = scripted_app(dir.path(), vec![]).await;

    app.execute_slash_command("/skills");
    assert!(app.state.skills_picker.is_some());
    app.handle_key(key(KeyCode::Esc));
    assert!(app.state.skills_picker.is_none());
}

#[tokio::test]
async fn agent_commands_reach_the_actor() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = scripted_app(dir.path(), vec![text("hello there")]).await;

    // /keys shows the configured key with its health.
    type_text(&mut app, "/keys");
    app.handle_key(key(KeyCode::Enter));
    assert!(
        pump_until(&mut app, |app| {
            app.state
                .messages
                .iter()
                .any(|m| m.content.contains("[Active]"))
        })
        .await,
        "key status shown"
    );
    assert_eq!(app.state.key_status.len(), 1);

    // /config prints the current configuration.
    type_text(&mut app, "/config");
    app.handle_key(key(KeyCode::Enter));
    assert!(
        pump_until(&mut app, |app| {
            app.state
                .messages
                .iter()
                .any(|m| m.content.contains("provider: scripted"))
        })
        .await,
        "config shown"
    );

    // /mcp reports that no MCP servers are configured for this agent.
    type_text(&mut app, "/mcp");
    app.handle_key(key(KeyCode::Enter));
    assert!(
        pump_until(&mut app, |app| {
            app.state
                .messages
                .iter()
                .any(|m| m.content.contains("no servers configured"))
        })
        .await,
        "mcp status shown"
    );

    // Run a turn, then /sessions lists the session.
    type_text(&mut app, "remember me");
    app.handle_key(key(KeyCode::Enter));
    assert!(
        pump_until(&mut app, |app| !app.is_busy()).await,
        "turn completes"
    );
    let session_id = app.state.session_id.clone().expect("session id set");

    type_text(&mut app, "/sessions");
    app.handle_key(key(KeyCode::Enter));
    assert!(
        pump_until(&mut app, |app| {
            app.state
                .messages
                .iter()
                .any(|m| m.content.contains(&session_id))
        })
        .await,
        "session listed"
    );

    // /resume reloads the stored history into the chat pane.
    app.state.messages.clear();
    type_text(&mut app, &format!("/resume {session_id}"));
    app.handle_key(key(KeyCode::Enter));
    assert!(
        pump_until(&mut app, |app| {
            app.state
                .messages
                .iter()
                .any(|m| m.content.contains("Resumed session"))
        })
        .await,
        "resume works"
    );
    assert!(
        app.state
            .messages
            .iter()
            .any(|m| m.role == "user" && m.content == "remember me"),
        "resumed history contains the earlier prompt"
    );
    assert_eq!(app.state.session_id.as_deref(), Some(session_id.as_str()));

    // /model switches provider/model (validated through the actor).
    type_text(&mut app, "/model anthropic:claude-3-7-sonnet-20250219");
    app.handle_key(key(KeyCode::Enter));
    assert!(
        pump_until(&mut app, |app| app.state.current_provider == "anthropic").await,
        "model switched"
    );
    assert_eq!(app.state.current_model, "claude-3-7-sonnet-20250219");

    // Unknown commands produce an error message.
    type_text(&mut app, "/nope");
    app.handle_key(key(KeyCode::Enter));
    assert!(
        app.state
            .messages
            .iter()
            .any(|m| m.role == "error" && m.content.contains("Unknown command")),
        "unknown command reported"
    );
}

#[tokio::test]
async fn first_run_without_keys_opens_key_setup_wizard() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = scripted_app_with_keys(dir.path(), vec![], &[]).await;

    // No keys configured: the wizard auto-opens on the provider picker.
    let setup = app.state.key_setup.as_ref().expect("wizard auto-opens");
    assert_eq!(setup.stage, SetupStage::ChooseProvider);
    assert!(
        app.state
            .messages
            .iter()
            .any(|m| m.content.contains("No API keys"))
    );

    let rendered = render_to_string(&mut app, 100, 30);
    assert!(
        rendered.contains("choose a provider"),
        "picker overlay rendered: {rendered}"
    );
    assert!(
        rendered.contains("Anthropic Claude"),
        "provider labels shown"
    );

    // Arrow keys move the highlight; Enter advances to the key prompt.
    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Down));
    assert_eq!(
        app.state.key_setup.as_ref().unwrap().provider(),
        "google",
        "highlight follows Down presses"
    );
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(
        app.state.key_setup.as_ref().unwrap().stage,
        SetupStage::EnterKey
    );

    // Esc goes back to the picker, then skips setup entirely.
    app.handle_key(key(KeyCode::Esc));
    assert_eq!(
        app.state.key_setup.as_ref().unwrap().stage,
        SetupStage::ChooseProvider
    );
    app.handle_key(key(KeyCode::Esc));
    assert!(app.state.key_setup.is_none(), "Esc skips the wizard");
    assert!(!app.state.should_quit, "Esc in the wizard does not quit");

    // `/keys add` re-opens the wizard later.
    app.execute_slash_command("/keys add");
    assert!(app.state.key_setup.is_some());
}

#[tokio::test]
async fn keys_add_command_validates_saves_and_registers() {
    let dir = tempfile::tempdir().unwrap();
    // The scripted provider's first response is consumed by validation.
    let mut app = scripted_app_with_keys(dir.path(), vec![text("pong")], &[]).await;

    // Opened without keys, so close the auto-wizard first.
    app.execute_slash_command("/keys add scripted");
    let setup = app.state.key_setup.as_ref().expect("wizard open");
    assert_eq!(setup.stage, SetupStage::EnterKey);
    assert_eq!(setup.provider(), "scripted");

    // Paste the secret (bracketed paste), then submit with Enter.
    app.handle_paste("sk-scripted-e2e-123\n".to_string());
    assert_eq!(
        app.state.key_setup.as_ref().unwrap().key_input,
        "sk-scripted-e2e-123"
    );
    let rendered = render_to_string(&mut app, 100, 30);
    assert!(
        !rendered.contains("sk-scripted-e2e-123"),
        "the secret is never rendered"
    );
    assert!(rendered.contains("••"), "masked dots rendered: {rendered}");

    app.handle_key(key(KeyCode::Enter));
    assert_eq!(
        app.state.key_setup.as_ref().unwrap().stage,
        SetupStage::Validating
    );
    assert!(
        pump_until(&mut app, |app| app.state.key_setup.is_none()).await,
        "wizard closes after the reply"
    );

    assert!(
        app.state
            .messages
            .iter()
            .any(|m| m.content.contains("scripted key saved") && m.content.contains("saved")),
        "success message: {:?}",
        app.state
            .messages
            .iter()
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>()
    );
    assert_eq!(app.state.key_status.len(), 1, "key status refreshed");
    assert_eq!(app.state.key_status[0].provider, "scripted");

    // The key landed in the encrypted keys file under SICK_HOME (isolated
    // temp dir); the plaintext file must not exist, and the ciphertext must
    // not contain the secret.
    let keys_path = default_encrypted_path();
    assert!(keys_path.is_file(), "keys.enc written");
    let raw = std::fs::read_to_string(&keys_path).expect("read keys.enc");
    assert!(
        !raw.contains("sk-scripted-e2e-123"),
        "secret must not appear in ciphertext: {raw}"
    );
    let (plaintext, source) = load_encrypted(&keys_path, None, &KeychainBackend::from_env())
        .expect("decrypt keys.enc")
        .expect("file exists");
    assert!(
        plaintext.contains("sk-scripted-e2e-123"),
        "decrypted document holds the secret"
    );
    assert_eq!(
        source.describe(),
        format!(
            "keyring file {}",
            std::env::var("SICK_KEYRING_FILE").unwrap()
        )
    );
    assert!(
        !shell_key_manager::KeyStore::default_keys_toml_path().exists(),
        "no plaintext keys.toml is left behind"
    );
}

#[tokio::test]
async fn keysadd_command_opens_the_wizard_at_the_provider_picker() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = scripted_app(dir.path(), vec![]).await;

    app.execute_slash_command("/keysadd");
    let setup = app.state.key_setup.as_ref().expect("wizard open");
    assert_eq!(setup.stage, SetupStage::ChooseProvider, "picker first");
    assert_eq!(setup.provider(), "openai", "starts at the first provider");

    // The picker renders provider labels; Down moves the highlight, Enter
    // advances to the key prompt.
    let rendered = render_to_string(&mut app, 100, 30);
    assert!(
        rendered.contains("Google Gemini"),
        "provider picker rendered: {rendered}"
    );
    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Enter));
    let setup = app.state.key_setup.as_ref().unwrap();
    assert_eq!(setup.stage, SetupStage::EnterKey);
    assert_eq!(
        setup.provider(),
        "google",
        "highlight followed the Down presses"
    );
}

#[tokio::test]
async fn models_command_opens_picker_for_providers_with_keys() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = scripted_app_with_keys(
        dir.path(),
        vec![],
        &[("google", "gkey-123"), ("openai", "okey-456")],
    )
    .await;

    app.execute_slash_command("/models");

    let picker = app.state.models_picker.as_ref().expect("picker open");
    assert!(picker.loading, "live lookup dispatched");
    let providers: Vec<&str> = picker
        .options
        .iter()
        .map(|option| option.provider.as_str())
        .collect();
    assert!(providers.contains(&"google"), "google rows: {providers:?}");
    assert!(providers.contains(&"openai"), "openai rows: {providers:?}");
    assert!(
        !providers.contains(&"anthropic") && !providers.contains(&"qwen"),
        "only configured providers are listed: {providers:?}"
    );
    assert!(
        picker
            .options
            .iter()
            .any(|option| option.model == "gemini-3.8-flash"),
        "google catalog rows"
    );

    // The overlay renders rows plus the switch hint.
    let rendered = render_to_string(&mut app, 100, 30);
    assert!(
        rendered.contains("gemini-3.8-flash"),
        "row rendered: {rendered}"
    );
    assert!(rendered.contains("Enter switch"), "hint rendered");

    // Down moves the highlight; Enter dispatches the switch and closes.
    app.handle_key(key(KeyCode::Down));
    assert_eq!(app.state.models_picker.as_ref().unwrap().selection, 1);
    app.handle_key(key(KeyCode::Enter));
    assert!(app.state.models_picker.is_none(), "picker closes on Enter");
    assert!(
        pump_until(&mut app, |app| app.state.current_model
            == "gemini-3.7-flash")
        .await,
        "selected model applied"
    );
    assert_eq!(app.state.current_provider, "google");

    // Without any keys the command points at /keysadd instead.
    let mut bare = scripted_app_with_keys(dir.path(), vec![], &[]).await;
    bare.execute_slash_command("/models");
    assert!(
        bare.state
            .messages
            .iter()
            .any(|m| m.content.contains("/keysadd")),
        "empty key state suggests /keysadd"
    );
}

#[tokio::test]
async fn command_palette_arrows_pick_and_run_a_command() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = scripted_app(dir.path(), vec![]).await;

    type_text(&mut app, "/ke");
    assert_eq!(
        app.state.matching_commands(),
        vec!["/keys", "/keysrm", "/keyssm", "/keysadd"]
    );

    let rendered = render_to_string(&mut app, 100, 30);
    assert!(
        rendered.contains("/keysadd"),
        "palette lists both matches: {rendered}"
    );

    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Down));
    assert_eq!(app.state.selected_command(), Some("/keysadd"));
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(
        app.state.key_setup.as_ref().map(|setup| setup.stage),
        Some(SetupStage::ChooseProvider),
        "Enter ran the highlighted /keysadd"
    );
}

#[tokio::test]
async fn bracketed_paste_fills_the_textarea_and_keeps_commands_working() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = scripted_app(dir.path(), vec![]).await;

    // Multiline paste lands in the input editor as-is.
    app.handle_paste("first line\nsecond line".to_string());
    assert_eq!(app.input_text(), "first line\nsecond line");

    // A pasted slash command still switches to command mode.
    let dir = tempfile::tempdir().unwrap();
    let mut app = scripted_app(dir.path(), vec![]).await;
    app.handle_paste("/help".to_string());
    assert_eq!(app.state.input_mode, InputMode::Command);
    app.handle_key(key(KeyCode::Enter));
    assert!(
        app.state
            .messages
            .iter()
            .any(|m| m.content.contains("Available commands")),
        "pasted /help executed"
    );
}

#[tokio::test]
async fn plan_command_drafts_a_plan_and_approve_executes_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = scripted_app(
        dir.path(),
        vec![
            text("1. Write hello.txt\n2. Run the tests"),
            text("Executing step 1: wrote hello.txt."),
        ],
    )
    .await;

    type_text(&mut app, "/plan add a hello feature");
    app.handle_key(key(KeyCode::Enter));
    assert!(app.is_busy(), "plan turn dispatched");
    assert!(pump_until(&mut app, |app| !app.is_busy()).await);
    assert_eq!(
        app.state.plan_review.as_deref(),
        Some("1. Write hello.txt\n2. Run the tests"),
        "plan captured for review"
    );

    let rendered = render_to_string(&mut app, 100, 34);
    assert!(rendered.contains("plan review"), "overlay rendered");
    assert!(rendered.contains("plan"), "options rendered");

    // The review is modal: unrelated keys are ignored.
    app.handle_key(key(KeyCode::Char('x')));
    assert!(app.state.plan_review.is_some());

    // Approve runs the plan (normal mode) via the actor.
    app.handle_key(key(KeyCode::Char('a')));
    assert!(app.state.plan_review.is_none());
    assert!(
        pump_until(&mut app, |app| !app.is_busy()).await,
        "approved turn completes"
    );
    assert!(
        app.state
            .messages
            .iter()
            .any(|m| m.role == "assistant" && m.content.contains("Executing step 1")),
        "execution answer rendered"
    );
    assert!(
        app.state
            .messages
            .iter()
            .any(|m| m.content.contains("Plan approved"))
    );
}

#[tokio::test]
async fn plan_review_edit_replans_and_cancel_discards() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = scripted_app(
        dir.path(),
        vec![
            text("first plan"),
            text("revised plan"),
            text("second task plan"),
        ],
    )
    .await;

    // Edit flow: e → type feedback → Enter re-plans with it.
    app.execute_slash_command("/plan do the thing");
    assert!(
        pump_until(&mut app, |app| app.state.plan_review.is_some()).await,
        "first plan ready"
    );
    app.handle_key(key(KeyCode::Char('e')));
    assert!(app.state.plan_review.is_none());
    assert!(app.state.plan_feedback, "next submit is plan feedback");
    type_text(&mut app, "use a different approach");
    app.handle_key(key(KeyCode::Enter));
    assert!(
        pump_until(&mut app, |app| app.state.plan_review.is_some()).await,
        "revised plan ready"
    );
    assert_eq!(app.state.plan_review.as_deref(), Some("revised plan"));
    assert!(!app.state.plan_feedback, "feedback flag consumed");

    // Cancel flow: c discards the pending plan.
    app.handle_key(key(KeyCode::Char('c')));
    assert!(app.state.plan_review.is_none());
    assert!(
        app.state
            .messages
            .iter()
            .any(|m| m.content.contains("Plan discarded"))
    );

    // `/plan` without a task prints usage instead of starting a turn.
    app.execute_slash_command("/plan");
    assert!(
        app.state
            .messages
            .iter()
            .any(|m| m.content.contains("Usage: /plan"))
    );
}

#[tokio::test]
async fn bang_commands_run_directly_and_shell_mode_toggles() {
    let dir = tempfile::tempdir().unwrap();
    // Empty script: any LLM call would fail, so success proves the passthrough
    // never touched the agent.
    let mut app = scripted_app(dir.path(), vec![]).await;

    type_text(&mut app, "!echo sick-passthrough-ok");
    app.handle_key(key(KeyCode::Enter));
    assert!(app.is_busy(), "passthrough marks the app busy");
    assert!(
        pump_until(&mut app, |app| !app.is_busy()).await,
        "command completes"
    );

    let shell_messages: Vec<&str> = app
        .state
        .messages
        .iter()
        .filter(|m| m.role == "shell")
        .map(|m| m.content.as_str())
        .collect();
    assert!(
        shell_messages
            .iter()
            .any(|m| m.contains("$ echo sick-passthrough-ok")),
        "command echoed: {shell_messages:?}"
    );
    assert!(
        shell_messages
            .iter()
            .any(|m| m.contains("sick-passthrough-ok")),
        "output streamed: {shell_messages:?}"
    );
    assert!(
        shell_messages.iter().any(|m| m.contains("[exit code: 0]")),
        "exit code shown: {shell_messages:?}"
    );
    assert_eq!(
        app.state.history.back().map(String::as_str),
        Some("!echo sick-passthrough-ok"),
        "history remembers passthrough commands"
    );

    // /shell toggles persistent mode: no `!` needed.
    app.execute_slash_command("/shell");
    assert!(app.state.shell_mode);
    let rendered = render_to_string(&mut app, 110, 30);
    assert!(rendered.contains("[SHELL]"), "header badge: {rendered}");

    if !cfg!(windows) {
        type_text(&mut app, "echo persistent-mode-ok");
        app.handle_key(key(KeyCode::Enter));
        assert!(pump_until(&mut app, |app| !app.is_busy()).await);
        assert!(
            app.state
                .messages
                .iter()
                .any(|m| m.role == "shell" && m.content.contains("persistent-mode-ok")),
            "shell mode runs bare text as a command"
        );
    }

    // /shell again turns it off.
    app.execute_slash_command("/shell");
    assert!(!app.state.shell_mode);
    let rendered = render_to_string(&mut app, 110, 30);
    assert!(!rendered.contains("[SHELL]"), "badge cleared: {rendered}");

    // Empty `!` prints usage instead of starting a command.
    type_text(&mut app, "!");
    app.handle_key(key(KeyCode::Enter));
    assert!(
        app.state
            .messages
            .iter()
            .any(|m| m.content.contains("Usage: !"))
    );
    assert!(!app.is_busy(), "empty `!` does not start anything");
}

#[tokio::test]
async fn skill_commands_list_scaffold_and_activate() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = scripted_app(dir.path(), vec![text("ack")]).await;

    // Empty skills dir: /skill points at /skill new.
    app.execute_slash_command("/skill");
    assert!(
        app.state
            .messages
            .iter()
            .any(|m| m.content.contains("No skills found")),
        "empty list message"
    );

    // Scaffold a new skill, then list it.
    app.execute_slash_command("/skill new tester");
    assert!(
        app.state
            .messages
            .iter()
            .any(|m| m.content.contains("Created skill template")),
        "scaffold message"
    );
    app.execute_slash_command("/skill list");
    assert!(
        app.state
            .messages
            .iter()
            .any(|m| m.content.contains("tester")),
        "skill listed"
    );

    // Activate it through the actor.
    app.execute_slash_command("/skill tester");
    assert!(
        pump_until(&mut app, |app| {
            app.state
                .messages
                .iter()
                .any(|m| m.content.contains("Skill `tester` activated"))
        })
        .await,
        "activation confirmed"
    );

    // Scaffolding the same skill again fails loudly.
    app.execute_slash_command("/skill new tester");
    assert!(
        app.state
            .messages
            .iter()
            .any(|m| m.role == "error" && m.content.contains("already exists")),
        "duplicate scaffold refused"
    );
}

#[tokio::test]
async fn task_tool_renders_subagent_activity_and_agents_lists_runs() {
    let dir = tempfile::tempdir().unwrap();
    let script = vec![
        ChatResponse {
            content: "Delegating.".to_string(),
            tool_calls: vec![ToolCall {
                id: "call_task".to_string(),
                tool_name: "task".to_string(),
                args: serde_json::json!({"description": "researcher", "prompt": "find the answer"}),
                thought_signature: None,
            }],
        },
        text("The answer is 42."),
        text("Done — the subagent found the answer."),
    ];
    let mut app = scripted_app(dir.path(), script).await;

    type_text(&mut app, "delegate the research");
    app.handle_key(key(KeyCode::Enter));
    assert!(
        pump_until(&mut app, |app| !app.is_busy()).await,
        "parent turn completes"
    );

    let subagent_messages: Vec<&str> = app
        .state
        .messages
        .iter()
        .filter(|m| m.role == "subagent")
        .map(|m| m.content.as_str())
        .collect();
    assert!(
        subagent_messages
            .iter()
            .any(|m| m.contains("researcher") && m.contains("started")),
        "started marker: {subagent_messages:?}"
    );
    assert!(
        subagent_messages
            .iter()
            .any(|m| m.contains("The answer is 42.")),
        "child tokens streamed: {subagent_messages:?}"
    );
    assert!(
        subagent_messages.iter().any(|m| m.contains("finished")),
        "finished marker: {subagent_messages:?}"
    );
    assert!(
        app.state
            .messages
            .iter()
            .any(|m| m.role == "assistant" && m.content.contains("subagent found the answer")),
        "parent final answer rendered"
    );

    // Rendered chat indents subagent output under the role label.
    let rendered = render_to_string(&mut app, 110, 40);
    assert!(
        rendered.contains("subagent"),
        "role label rendered: {rendered}"
    );
    assert!(rendered.contains("The answer is 42."));

    // /agents lists the finished run.
    app.execute_slash_command("/agents");
    assert!(
        app.state
            .messages
            .iter()
            .any(|m| m.content.contains("researcher · [done]")),
        "/agents lists the run: {:?}",
        app.state
            .messages
            .iter()
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn session_commands_history_search_replay_and_fork() {
    let dir = tempfile::tempdir().unwrap();
    let script = vec![
        ChatResponse {
            content: "Creating the file.".to_string(),
            tool_calls: vec![ToolCall {
                id: "call_cmd_1".to_string(),
                tool_name: "write".to_string(),
                args: serde_json::json!({"path": "notes.txt", "content": "phase four"}),
                thought_signature: None,
            }],
        },
        text("Done - notes.txt is ready."),
    ];
    let mut app = scripted_app(dir.path(), script).await;

    // One tool-using turn: a prompt, an extracted memory, and a tool call.
    type_text(&mut app, "write notes about phase four");
    app.handle_key(key(KeyCode::Enter));
    assert!(
        pump_until(&mut app, |app| !app.is_busy()).await,
        "turn completes"
    );
    let original_session = app.state.session_id.clone().expect("session id");

    // /history lists recent prompts.
    type_text(&mut app, "/history");
    app.handle_key(key(KeyCode::Enter));
    assert!(
        pump_until(&mut app, |app| app
            .state
            .messages
            .iter()
            .any(|m| m.content.contains("Recent prompts")))
        .await,
        "/history reply arrives"
    );
    assert!(
        app.state
            .messages
            .iter()
            .any(|m| m.content.contains("write notes about phase four"))
    );

    // /search finds the prompt and the extracted memory.
    type_text(&mut app, "/search write notes about phase four");
    app.handle_key(key(KeyCode::Enter));
    assert!(
        pump_until(&mut app, |app| app
            .state
            .messages
            .iter()
            .any(|m| m.content.contains("Search `write notes")))
        .await,
        "/search reply arrives"
    );
    assert!(
        app.state
            .messages
            .iter()
            .any(|m| m.content.contains("prompt [")),
        "prompt hit listed"
    );
    assert!(
        app.state
            .messages
            .iter()
            .any(|m| m.content.contains("memory (")),
        "memory hit listed"
    );

    // /replay last re-runs the stored write tool call.
    std::fs::write(dir.path().join("notes.txt"), "tampered").unwrap();
    type_text(&mut app, "/replay last");
    app.handle_key(key(KeyCode::Enter));
    assert!(
        pump_until(&mut app, |app| app
            .state
            .messages
            .iter()
            .any(|m| m.content.contains("replayed `write`")))
        .await,
        "/replay output arrives"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("notes.txt")).unwrap(),
        "phase four",
        "replay really re-executed the tool"
    );
    assert!(
        app.state
            .messages
            .iter()
            .any(|m| m.role == "tool_result" && m.tool_name.as_deref() == Some("replay"))
    );

    // /fork copies the history and continues in the copy.
    type_text(&mut app, "/fork experiment");
    app.handle_key(key(KeyCode::Enter));
    assert!(
        pump_until(&mut app, |app| app
            .state
            .messages
            .iter()
            .any(|m| m.content.contains("Forked")))
        .await,
        "/fork reply arrives"
    );
    let forked_session = app.state.session_id.clone().expect("forked id");
    assert_ne!(forked_session, original_session);
    assert!(
        app.state
            .messages
            .iter()
            .any(|m| m.content.contains(&forked_session))
    );

    // Usage hints for missing arguments.
    app.execute_slash_command("/replay");
    assert!(
        app.state
            .messages
            .iter()
            .any(|m| m.content.contains("Usage: /replay"))
    );
    app.execute_slash_command("/search");
    assert!(
        app.state
            .messages
            .iter()
            .any(|m| m.content.contains("Usage: /search"))
    );
}
