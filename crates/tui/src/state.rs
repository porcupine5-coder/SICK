//! TUI state: messages, input mode, command history, key status.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use shell_key_manager::KeyStatusInfo;

use crate::theme::Theme;

const THINKING_PHRASES: &[&str] = &[
    "Contemplating",
    "Deliberating",
    "Simmering",
    "Incubating",
    "Percolating",
    "Noodling",
    "Cerebrating",
    "Ruminating",
    "Masticating",
    "Fermenting",
    "Gesticulating",
    "Whispering to electrons",
    "Calibrating flux capacitors",
    "Untangling logic strings",
    "Manifesting text",
    "Flipping digital bits",
    "Consulting the mainframe",
    "Distilling thoughts",
    "Weaving code tapestry",
    "Brewing context",
    "Realigning flux arrays",
    "Reticulating splines",
    "Marinating the context",
    "Churning data butter",
    "Herding digital cats",
    "Whispering to the compiler",
    "Massaging data streams",
    "Untangling logic knots",
    "Consulting the algorithmic oracle",
    "Mining for context clues",
    "Polishing output tokens",
    "Decoding universal constants",
    "Bending spacetime vectors",
    "Stoking the engine room",
    "Flipping magnetic bits",
    "Sweeping memory leak corridors",
    "Sorting chaotic packets",
    "Dusting off old registers",
    "Tuning the feedback loops",
    "Defragmenting synapses",
    "Polishing raw syntax",
    "Buffering cosmic rays",
    "Straining text filters",
    "Baking data pies",
    "Spinning thread pools",
    "Tickling CPU transistors",
    "Shaking the logic tree",
    "Stretching string buffers",
    "Distilling pure abstractions",
    "Calibrating heuristic arrays",
    "Waking sleepy subroutines",
    "Combusting binary fuel",
    "Scouring code repositories",
    "Sifting text sediment",
    "Chasing rogue pointers",
    "Threading needle loops",
    "Weaving bit patterns",
    "Stretching memory stacks",
    "Unpacking dense semantic crates",
    "Brewing digital espresso",
];

const SPINNER_FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Slash commands with descriptions (command palette + `/help`).
pub const COMMANDS: &[(&str, &str)] = &[
    ("/help", "show available commands and key bindings"),
    (
        "/plan",
        "draft a plan first: /plan <task> (approve · edit · cancel)",
    ),
    ("/skill", "list/load skills: /skill [new <name> | <name>]"),
    ("/skills", "interactive skill picker with search"),
    ("/agents", "list subagent runs spawned via the task tool"),
    (
        "/shell",
        "toggle shell mode (run commands directly in your terminal; cd changes directory)",
    ),
    ("/kali", "toggle Kali Linux WSL shell mode (Windows only)"),
    (
        "/model",
        "switch model: /model <provider> <model> (or [provider:]model)",
    ),
    (
        "/models",
        "open model picker with search, or set model: /models <provider> <model>",
    ),
    ("/keys", "show key status · `/keys add` pastes a new key"),
    (
        "/keysrm",
        "remove one saved key: /keysrm <provider> <key-id> (see /keys)",
    ),
    (
        "/keyssm",
        "alias for /keysrm: remove one saved key by provider and key id",
    ),
    (
        "/keysadd",
        "add an API key: choose a provider from the list, then paste it",
    ),
    (
        "/cloudflare",
        "save your Cloudflare account ID: /cloudflare <32-character-account-id>",
    ),
    ("/sessions", "list saved sessions"),
    ("/resume", "load a saved session: /resume <session-id>"),
    (
        "/fork",
        "copy this session and continue in the copy: /fork [title]",
    ),
    ("/history", "show recent prompt history"),
    ("/search", "search prompts and memories: /search <query>"),
    (
        "/replay",
        "re-run a stored tool call: /replay <tool-call-id | last>",
    ),
    ("/clear", "clear the chat display"),
    (
        "/theme",
        "interactive theme picker or set theme: /theme [name|create …]",
    ),
    ("/themes", "browse and select color themes with search"),
    ("/agent", "delegate a focused task: /agent <task>"),
    ("/config", "print the current agent configuration"),
    ("/mcp", "show MCP server status and their tools"),
    ("/quit", "exit the TUI"),
];

/// Providers offered by the key-setup wizard (id, display label).
pub const SETUP_PROVIDERS: &[(&str, &str)] = &[
    ("openai", "OpenAI"),
    ("anthropic", "Anthropic Claude"),
    ("google", "Google Gemini"),
    ("deepseek", "DeepSeek"),
    ("openrouter", "OpenRouter"),
    ("qwen", "Qwen (DashScope)"),
    ("aionlabs", "Aion Labs"),
    ("cohere", "Cohere"),
    ("mistral", "Mistral AI"),
    ("zai", "Z AI (Zhipu AI)"),
    ("groq", "Groq"),
    ("cloudflare", "Cloudflare Workers AI"),
    ("huggingface", "Hugging Face"),
    ("kilo", "Kilo Code"),
    ("llm7", "LLM7.io"),
    ("modelscope", "ModelScope"),
    ("nvidia", "NVIDIA NIM"),
    ("ollama", "Ollama Cloud"),
    ("ovhcloud", "OVHcloud AI"),
    ("siliconflow", "SiliconFlow"),
    ("cerebras", "Cerebras"),
    ("sambanova", "SambaNova"),
    ("together", "Together AI"),
    ("chutes", "Chutes AI"),
    ("github", "GitHub Models"),
];

/// Description for a slash command (empty string when unknown).
pub fn command_description(command: &str) -> &'static str {
    COMMANDS
        .iter()
        .find(|(name, _)| *name == command)
        .map(|(_, description)| *description)
        .unwrap_or("")
}

/// `/help` message body: commands plus key bindings.
pub fn command_help_text() -> String {
    let mut text = String::from("Available commands:");
    for (name, description) in COMMANDS {
        text.push_str(&format!("\n  {name:<10} {description}"));
    }
    text.push_str(
        "\n\nKeys: Enter sends · Ctrl+Enter newline · Up/Down history · \
         Ctrl+R history recall · PgUp/PgDn scroll · Esc cancel/quit · Ctrl+C quit",
    );
    text
}

/// Whether the input line holds a normal message or a slash command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InputMode {
    /// Regular chat input.
    #[default]
    Normal,
    /// Input starts with `/`: slash-command mode (palette visible).
    Command,
}

/// A message rendered in the chat pane.
#[derive(Debug, Clone, PartialEq)]
pub struct UiMessage {
    /// `user`, `assistant`, `tool_call`, `tool_result`, `system`, `error`.
    pub role: String,
    /// Display text.
    pub content: String,
    /// Tool name for tool messages.
    pub tool_name: Option<String>,
    /// Wall-clock time the message was appended (`HH:MM:SS`).
    pub timestamp: String,
}

impl UiMessage {
    /// Create a message stamped with the current local time.
    pub fn new(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
            tool_name: None,
            timestamp: now_stamp(),
        }
    }

    /// Create a tool-related message carrying the tool name.
    pub fn tool(
        role: impl Into<String>,
        name: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        let name = name.into();
        Self {
            role: role.into(),
            content: content.into(),
            tool_name: Some(name),
            timestamp: now_stamp(),
        }
    }
}

fn now_stamp() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

/// Stage of the interactive API-key setup flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupStage {
    /// Picking a provider from the list.
    ChooseProvider,
    /// Typing/pasting the API key (masked).
    EnterKey,
    /// Entering the Cloudflare account ID (not the API token ID).
    EnterAccountId,
    /// Waiting for validation + save to finish.
    Validating,
}

/// Key-setup wizard state (`/keys add`, or auto-opened on first run).
#[derive(Debug, Clone)]
pub struct KeySetup {
    /// Current stage of the flow.
    pub stage: SetupStage,
    /// Highlighted index in the provider list.
    pub selection: usize,
    /// Provider passed to `/keys add <id>` (skips list rendering).
    pub custom_provider: Option<String>,
    /// The pasted key (shown masked).
    pub key_input: String,
    /// Cloudflare account id for Workers AI, stored with provider settings.
    pub account_id_input: String,
    /// Status/error line shown in the overlay.
    pub status: Option<String>,
}

impl Default for KeySetup {
    fn default() -> Self {
        Self::new()
    }
}

impl KeySetup {
    /// Start at the provider picker.
    pub fn new() -> Self {
        Self {
            stage: SetupStage::ChooseProvider,
            selection: 0,
            custom_provider: None,
            key_input: String::new(),
            account_id_input: String::new(),
            status: None,
        }
    }

    /// Open directly at the key prompt for `provider`. Unknown ids are still
    /// accepted (the provider resolver reports a clear error when validating).
    pub fn for_provider(provider: &str) -> Self {
        let normalized = shell_key_manager::normalize_provider(provider);
        let selection = SETUP_PROVIDERS
            .iter()
            .position(|(id, _)| *id == normalized)
            .unwrap_or(0);
        Self {
            stage: SetupStage::EnterKey,
            selection,
            custom_provider: Some(normalized),
            key_input: String::new(),
            account_id_input: String::new(),
            status: None,
        }
    }

    /// Provider id currently targeted.
    pub fn provider(&self) -> &str {
        if let Some(custom) = &self.custom_provider {
            return custom;
        }
        SETUP_PROVIDERS
            .get(self.selection.min(SETUP_PROVIDERS.len() - 1))
            .map(|(id, _)| *id)
            .unwrap_or("openai")
    }

    /// Provider display label.
    pub fn provider_label(&self) -> &str {
        let provider = self.provider();
        if let Some((_, label)) = SETUP_PROVIDERS.iter().find(|(id, _)| *id == provider) {
            return label;
        }
        provider
    }

    /// The key input rendered as dots (never show the secret).
    pub fn masked_key(&self) -> String {
        "•".repeat(self.key_input.chars().count())
    }

    /// Move the provider highlight by `delta`, clamped to the list.
    pub fn move_selection(&mut self, delta: i32) {
        let last = SETUP_PROVIDERS.len() - 1;
        let next = self.selection as i32 + delta;
        self.selection = next.clamp(0, last as i32) as usize;
    }
}

/// A subagent run tracked for the `/agents` overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentRun {
    /// Name from the `task` tool's description argument.
    pub name: String,
    /// `running`, `done`, or `failed`.
    pub status: &'static str,
}

/// One selectable row in the `/models` picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelOption {
    /// Provider id the model belongs to.
    pub provider: String,
    /// Model id.
    pub model: String,
}

/// Interactive model picker opened by `/models`.
#[derive(Debug, Clone)]
pub struct ModelsPicker {
    /// Selectable rows — providers with configured keys only.
    pub options: Vec<ModelOption>,
    /// Highlighted row in the filtered view.
    pub selection: usize,
    /// Whether a live provider lookup is still in flight.
    pub loading: bool,
    /// Status line (live-lookup failures, fallbacks).
    pub status: Option<String>,
    /// Search filter query.
    pub filter: String,
}

impl ModelsPicker {
    /// Create a picker over `options`, highlighting the first row.
    pub fn new(options: Vec<ModelOption>) -> Self {
        Self {
            options,
            selection: 0,
            loading: false,
            status: None,
            filter: String::new(),
        }
    }

    /// Options filtered by the search query.
    pub fn filtered_options(&self) -> Vec<&ModelOption> {
        let query = self.filter.trim().to_ascii_lowercase();
        if query.is_empty() {
            self.options.iter().collect()
        } else {
            self.options
                .iter()
                .filter(|opt| {
                    opt.provider.to_ascii_lowercase().contains(&query)
                        || opt.model.to_ascii_lowercase().contains(&query)
                })
                .collect()
        }
    }

    /// Move selection highlight in the filtered view.
    pub fn move_selection(&mut self, delta: i32) {
        let count = self.filtered_options().len();
        if count == 0 {
            self.selection = 0;
            return;
        }
        let next = self.selection as i32 + delta;
        self.selection = next.clamp(0, count as i32 - 1) as usize;
    }

    /// Currently highlighted model in the filtered view.
    pub fn selected_model(&self) -> Option<ModelOption> {
        let filtered = self.filtered_options();
        filtered.get(self.selection).copied().cloned()
    }

    /// Highlight the row for `provider`/`model` when it is in the list.
    pub fn highlight_model(&mut self, provider: &str, model: &str) {
        let filtered = self.filtered_options();
        if let Some(index) = filtered
            .iter()
            .position(|option| option.provider == provider && option.model == model)
        {
            self.selection = index;
        }
    }
}

/// Selectable row in the `/themes` picker (actions, custom themes, built-in themes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThemePickerItem {
    /// Action: Create a custom theme using the visual color wheel studio.
    CreateCustomWheel,
    /// Action: Import a custom theme from JSON code.
    ImportCustomJson,
    /// An existing custom theme.
    CustomTheme(Theme),
    /// An existing built-in theme.
    BuiltinTheme(Theme),
}

/// Interactive mode inside `/themes`.
#[derive(Debug, Clone)]
pub enum ThemePickerMode {
    /// Browsing the theme list with search.
    Browse,
    /// Visual Color Wheel / Palette builder studio.
    ColorWheel(ColorWheelState),
    /// JSON theme importer.
    JsonImport(JsonImportState),
}

/// Interactive Color Wheel / Palette builder studio state.
#[derive(Debug, Clone)]
pub struct ColorWheelState {
    pub name: String,
    /// Active field: 0 = Name, 1..=8 = Color roles (0..7), 9 = Save button.
    pub active_field: usize,
    /// 8 hex colors [Background, Primary, Accent, Secondary, Success, Warning, Error, Metadata].
    pub colors: [String; 8],
    /// Hue in degrees (0.0 .. 360.0) for active color role.
    pub hue: f32,
    /// Lightness (0.05 .. 0.95) for active color role.
    pub lightness: f32,
    /// Saturation (0.0 .. 1.0) for active color role.
    pub saturation: f32,
    /// Optional status or error message (text, is_error).
    pub message: Option<(String, bool)>,
}

impl ColorWheelState {
    pub const ROLE_LABELS: [&'static str; 8] = [
        "Background",
        "Primary (Text)",
        "Accent (Assistant)",
        "Secondary (Border)",
        "Success (Shell)",
        "Warning (Tools)",
        "Error",
        "Metadata (Info)",
    ];

    pub fn from_styles(styles: &crate::theme::ThemeStyles) -> Self {
        let colors = [
            crate::theme::color_hex(styles.bg).unwrap_or_else(|_| "#0B130E".to_string()),
            crate::theme::color_hex(styles.fg).unwrap_or_else(|_| "#E6F4EA".to_string()),
            crate::theme::color_hex(styles.assistant_color)
                .unwrap_or_else(|_| "#00FF66".to_string()),
            crate::theme::color_hex(styles.border_color).unwrap_or_else(|_| "#4A6B53".to_string()),
            crate::theme::color_hex(styles.success_color).unwrap_or_else(|_| "#10B981".to_string()),
            crate::theme::color_hex(styles.warning_color).unwrap_or_else(|_| "#EAB308".to_string()),
            crate::theme::color_hex(styles.error_color).unwrap_or_else(|_| "#F43F5E".to_string()),
            crate::theme::color_hex(styles.metadata_color)
                .unwrap_or_else(|_| "#00E5FF".to_string()),
        ];
        let (hue, saturation, lightness) = crate::theme::Theme::hex_to_hsl(&colors[2]);
        Self {
            name: "my-custom-theme".to_string(),
            active_field: 0,
            colors,
            hue,
            lightness,
            saturation,
            message: None,
        }
    }

    pub fn sync_hsl_from_active_color(&mut self) {
        if self.active_field >= 1 && self.active_field <= 8 {
            let idx = self.active_field - 1;
            let (h, s, l) = crate::theme::Theme::hex_to_hsl(&self.colors[idx]);
            self.hue = h;
            self.saturation = s;
            self.lightness = l;
        }
    }

    pub fn apply_hsl_to_active_color(&mut self) {
        if self.active_field >= 1 && self.active_field <= 8 {
            let idx = self.active_field - 1;
            self.colors[idx] =
                crate::theme::Theme::hsl_to_hex(self.hue, self.saturation, self.lightness);
        }
    }
}

/// JSON Theme Importer state.
const JSON_THEME_TEMPLATE: &str = r##"{
  "name": "my-custom-theme",
  "background": "#0B130E",
  "primary": "#E6F4EA",
  "accent": "#00FF66",
  "secondary": "#4A6B53",
  "success": "#10B981",
  "warning": "#EAB308",
  "error": "#F43F5E",
  "metadata": "#00E5FF"
}"##;

#[derive(Debug, Clone)]
pub struct JsonImportState {
    pub buffer: String,
    pub status: Result<(String, Theme), String>,
}

impl JsonImportState {
    pub fn new() -> Self {
        let template = JSON_THEME_TEMPLATE.to_string();
        let status = Theme::from_json(&template).map(|t| (t.name().to_string(), t));
        Self {
            buffer: template,
            status,
        }
    }

    pub fn update_status(&mut self) {
        self.status = Theme::from_json(&self.buffer).map(|t| (t.name().to_string(), t));
    }

    pub fn paste(&mut self, text: &str) {
        if self.buffer == JSON_THEME_TEMPLATE {
            self.buffer.clear();
        }
        self.buffer.push_str(text);
        self.update_status();
    }
}

/// Interactive theme picker opened by `/theme` or `/themes`.
#[derive(Debug, Clone)]
pub struct ThemesPicker {
    /// Available themes (builtins + custom).
    pub options: Vec<Theme>,
    /// Highlighted row in the filtered view.
    pub selection: usize,
    /// Search filter query.
    pub filter: String,
    /// Theme active when the picker opened (for Esc cancellation).
    pub initial_theme: Theme,
    /// Current sub-mode (Browse, ColorWheel, or JsonImport).
    pub mode: ThemePickerMode,
}

impl ThemesPicker {
    pub fn new(options: Vec<Theme>, active: Theme) -> Self {
        let mut picker = Self {
            options,
            selection: 0,
            filter: String::new(),
            initial_theme: active.clone(),
            mode: ThemePickerMode::Browse,
        };
        let items = picker.filtered_items();
        if let Some(pos) = items.iter().position(|item| match item {
            ThemePickerItem::BuiltinTheme(t) | ThemePickerItem::CustomTheme(t) => {
                t.name() == active.name()
            }
            _ => false,
        }) {
            picker.selection = pos;
        }
        picker
    }

    /// Items matching search filter (actions, custom themes, built-in themes).
    pub fn filtered_items(&self) -> Vec<ThemePickerItem> {
        let query = self.filter.trim().to_ascii_lowercase();
        let mut items = Vec::new();

        // 1. Custom creation action items
        let wheel_matches = query.is_empty()
            || "create custom theme (color wheel)".contains(&query)
            || "color wheel".contains(&query)
            || "custom".contains(&query)
            || "wheel".contains(&query)
            || "palette".contains(&query);
        if wheel_matches {
            items.push(ThemePickerItem::CreateCustomWheel);
        }

        let json_matches = query.is_empty()
            || "import custom theme from json".contains(&query)
            || "json".contains(&query)
            || "import".contains(&query)
            || "code".contains(&query);
        if json_matches {
            items.push(ThemePickerItem::ImportCustomJson);
        }

        // 2. Existing Custom Themes
        for theme in &self.options {
            if Theme::parse(theme.name()).is_none() {
                if query.is_empty()
                    || theme.name().to_ascii_lowercase().contains(&query)
                    || theme.display_name().to_ascii_lowercase().contains(&query)
                    || theme.vibe().to_ascii_lowercase().contains(&query)
                {
                    items.push(ThemePickerItem::CustomTheme(theme.clone()));
                }
            }
        }

        // 3. Built-in Themes
        for theme in &self.options {
            if Theme::parse(theme.name()).is_some() {
                if query.is_empty()
                    || theme.name().to_ascii_lowercase().contains(&query)
                    || theme.display_name().to_ascii_lowercase().contains(&query)
                    || theme.vibe().to_ascii_lowercase().contains(&query)
                {
                    items.push(ThemePickerItem::BuiltinTheme(theme.clone()));
                }
            }
        }

        items
    }

    /// Move selection highlight in the filtered view.
    pub fn move_selection(&mut self, delta: i32) {
        let count = self.filtered_items().len();
        if count == 0 {
            self.selection = 0;
            return;
        }
        let next = self.selection as i32 + delta;
        self.selection = next.clamp(0, count as i32 - 1) as usize;
    }

    /// Currently highlighted item in the filtered view.
    pub fn selected_item(&self) -> Option<ThemePickerItem> {
        self.filtered_items().get(self.selection).cloned()
    }

    /// Currently highlighted theme if an existing theme is selected.
    pub fn selected_theme(&self) -> Option<Theme> {
        match self.selected_item()? {
            ThemePickerItem::CustomTheme(t) | ThemePickerItem::BuiltinTheme(t) => Some(t),
            _ => None,
        }
    }
}

/// Selectable skill row for the `/skills` picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillOption {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
}

/// Interactive skill picker opened by `/skills` or `/skill [list]`.
#[derive(Debug, Clone)]
pub struct SkillsPicker {
    pub options: Vec<SkillOption>,
    pub selection: usize,
    pub filter: String,
}

impl SkillsPicker {
    pub fn new(options: Vec<SkillOption>) -> Self {
        Self {
            options,
            selection: 0,
            filter: String::new(),
        }
    }

    /// Skills matching the search query (name or description).
    pub fn filtered_options(&self) -> Vec<&SkillOption> {
        let query = self.filter.trim().to_ascii_lowercase();
        if query.is_empty() {
            self.options.iter().collect()
        } else {
            self.options
                .iter()
                .filter(|skill| {
                    skill.name.to_ascii_lowercase().contains(&query)
                        || skill.description.to_ascii_lowercase().contains(&query)
                })
                .collect()
        }
    }

    /// Move selection highlight in the filtered view.
    pub fn move_selection(&mut self, delta: i32) {
        let count = self.filtered_options().len();
        if count == 0 {
            self.selection = 0;
            return;
        }
        let next = self.selection as i32 + delta;
        self.selection = next.clamp(0, count as i32 - 1) as usize;
    }

    /// Currently highlighted skill in the filtered view.
    pub fn selected_skill(&self) -> Option<SkillOption> {
        let filtered = self.filtered_options();
        filtered.get(self.selection).copied().cloned()
    }
}

/// Everything the UI needs to render a frame.
#[derive(Debug)]
pub struct TuiState {
    /// Rendered chat messages.
    pub messages: Vec<UiMessage>,
    /// Current input mode.
    pub input_mode: InputMode,
    /// Whether the command palette is visible.
    pub command_palette: bool,
    /// Text typed after `/` (palette filter).
    pub command_text: String,
    /// Submitted-input history (most recent last).
    pub history: VecDeque<String>,
    /// Position while browsing history (`None` = editing a new input).
    pub history_index: Option<usize>,
    /// Set to exit the event loop.
    pub should_quit: bool,
    /// Model shown in the header.
    pub current_model: String,
    /// Provider shown in the header.
    pub current_provider: String,
    /// Latest key-health snapshot (`/keys`).
    pub key_status: Vec<KeyStatusInfo>,
    /// Active session id, once a turn has run.
    pub session_id: Option<String>,
    /// Active color theme.
    pub theme: Theme,
    /// Available built-in and user-defined themes.
    pub themes: Vec<Theme>,
    /// Whether the agent is currently working on a turn.
    pub busy: bool,
    /// Time the current agent turn began (for long-running status hints).
    pub busy_since: Option<Instant>,
    /// Chat scroll offset in lines (top-anchored).
    pub scroll_offset: u16,
    /// Total scrollable lines in chat.
    pub max_scroll: u16,
    /// Whether to stick to the bottom of the chat pane.
    pub stick_to_bottom: bool,
    /// Whether the Ctrl+R history recall overlay is open.
    pub show_history: bool,
    /// Selected row in the history overlay.
    pub history_selection: usize,
    /// Open key-setup wizard, when running.
    pub key_setup: Option<KeySetup>,
    /// Plan awaiting approve/edit/cancel (Step 4).
    pub plan_review: Option<String>,
    /// Whether the in-flight turn is a plan turn (its reply opens the review).
    pub plan_pending: bool,
    /// Whether the next submitted input is feedback for a re-plan.
    pub plan_feedback: bool,
    /// Persistent shell passthrough mode (`/shell`, Step 7).
    pub shell_mode: bool,
    /// Whether shell mode targets Kali Linux WSL (otherwise host terminal).
    pub shell_use_kali: bool,
    /// Subagent runs observed this session (`/agents`, Step 6).
    pub subagents: Vec<SubagentRun>,
    /// Highlighted row in the command palette.
    pub command_selection: usize,
    /// Open `/models` picker, when running.
    pub models_picker: Option<ModelsPicker>,
    /// Open `/themes` picker, when running.
    pub themes_picker: Option<ThemesPicker>,
    /// Open `/skills` picker, when running.
    pub skills_picker: Option<SkillsPicker>,
}

fn thinking_phrase(elapsed: Duration) -> &'static str {
    let phrase_index = (elapsed.as_secs() / 3) as usize % THINKING_PHRASES.len();
    THINKING_PHRASES[phrase_index]
}

fn spinner_frame(elapsed: Duration) -> char {
    let frame_index = (elapsed.as_millis() / 100) as usize % SPINNER_FRAMES.len();
    SPINNER_FRAMES[frame_index]
}

impl TuiState {
    /// Create fresh state for the given provider/model.
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            messages: Vec::new(),
            input_mode: InputMode::Normal,
            command_palette: false,
            command_text: String::new(),
            history: VecDeque::new(),
            history_index: None,
            should_quit: false,
            current_model: model.into(),
            current_provider: provider.into(),
            key_status: Vec::new(),
            session_id: None,
            theme: Theme::dark(),
            themes: Theme::builtins(),
            busy: false,
            busy_since: None,
            scroll_offset: 0,
            max_scroll: 0,
            stick_to_bottom: true,
            show_history: false,
            history_selection: 0,
            key_setup: None,
            plan_review: None,
            plan_pending: false,
            plan_feedback: false,
            shell_mode: false,
            shell_use_kali: false,
            subagents: Vec::new(),
            command_selection: 0,
            models_picker: None,
            themes_picker: None,
            skills_picker: None,
        }
    }

    /// Append a chat message and stick the view to the bottom.
    pub fn add_message(&mut self, role: &str, content: impl Into<String>) {
        self.messages.push(UiMessage::new(role, content));
        self.stick_to_bottom = true;
    }

    /// Merge prompts loaded from persistent history into keyboard recall.
    pub fn extend_history(&mut self, prompts: impl IntoIterator<Item = String>) {
        let mut combined = prompts.into_iter().collect::<Vec<_>>();
        combined.extend(self.history.drain(..));
        self.history.clear();
        for prompt in combined {
            self.push_history(&prompt);
        }
    }

    /// Return the current witty progress phrase for an in-flight agent turn.
    pub fn working_status(&self) -> Option<String> {
        let elapsed = self.busy_since?.elapsed();
        Some(thinking_phrase(elapsed).to_string())
    }

    /// Current spinner glyph, changing on every animation tick while busy.
    pub fn working_spinner(&self) -> char {
        let elapsed = self
            .busy_since
            .map_or(Duration::ZERO, |start| start.elapsed());
        spinner_frame(elapsed)
    }

    /// Remember a submitted input (consecutive duplicates collapse).
    pub fn push_history(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        if self
            .history
            .back()
            .map(|last| last == text)
            .unwrap_or(false)
        {
            self.history_index = None;
            return;
        }
        self.history.push_back(text.to_string());
        while self.history.len() > 200 {
            self.history.pop_front();
        }
        self.history_index = None;
    }

    /// Move to the previous history entry (Up). Returns the text to load.
    pub fn history_prev(&mut self) -> Option<String> {
        if self.history.is_empty() {
            return None;
        }
        let index = match self.history_index {
            None => self.history.len() - 1,
            Some(0) => 0,
            Some(index) => index - 1,
        };
        self.history_index = Some(index);
        self.history.get(index).cloned()
    }

    /// Move to the next history entry (Down). `Some("")` when past the end.
    pub fn history_next(&mut self) -> Option<String> {
        match self.history_index {
            None => None,
            Some(index) if index + 1 >= self.history.len() => {
                self.history_index = None;
                Some(String::new())
            }
            Some(index) => {
                self.history_index = Some(index + 1);
                self.history.get(index + 1).cloned()
            }
        }
    }

    /// Commands matching the current palette filter.
    pub fn matching_commands(&self) -> Vec<&'static str> {
        let filter = self.command_text.trim().to_ascii_lowercase();
        COMMANDS
            .iter()
            .map(|(name, _)| *name)
            .filter(|name| filter.is_empty() || name.trim_start_matches('/').starts_with(&filter))
            .collect()
    }

    /// The command currently highlighted in the palette.
    pub fn selected_command(&self) -> Option<&'static str> {
        let matches = self.matching_commands();
        if matches.is_empty() {
            return None;
        }
        matches
            .get(self.command_selection.min(matches.len() - 1))
            .copied()
    }

    /// Move the palette highlight by `delta`, clamped to the matches.
    pub fn move_command_selection(&mut self, delta: i32) {
        let len = self.matching_commands().len();
        if len == 0 {
            self.command_selection = 0;
            return;
        }
        let next = self.command_selection as i32 + delta;
        self.command_selection = next.clamp(0, len as i32 - 1) as usize;
    }

    /// Move the models-picker highlight by `delta`, clamped to its filtered rows.
    pub fn move_models_selection(&mut self, delta: i32) {
        if let Some(picker) = self.models_picker.as_mut() {
            picker.move_selection(delta);
        }
    }

    /// The model highlighted in the picker, if any row is selected.
    pub fn selected_model(&self) -> Option<ModelOption> {
        let picker = self.models_picker.as_ref()?;
        picker.selected_model()
    }

    /// Move the themes-picker highlight by `delta`, clamped to its filtered rows.
    pub fn move_themes_selection(&mut self, delta: i32) {
        if let Some(picker) = self.themes_picker.as_mut() {
            picker.move_selection(delta);
        }
    }

    /// The theme highlighted in the picker, if any row is selected.
    pub fn selected_theme(&self) -> Option<Theme> {
        let picker = self.themes_picker.as_ref()?;
        picker.selected_theme()
    }

    /// The item (action or theme) highlighted in the themes picker.
    pub fn selected_theme_item(&self) -> Option<ThemePickerItem> {
        let picker = self.themes_picker.as_ref()?;
        picker.selected_item()
    }

    /// Move the skills-picker highlight by `delta`, clamped to its filtered rows.
    pub fn move_skills_selection(&mut self, delta: i32) {
        if let Some(picker) = self.skills_picker.as_mut() {
            picker.move_selection(delta);
        }
    }

    /// The skill highlighted in the picker, if any row is selected.
    pub fn selected_skill(&self) -> Option<SkillOption> {
        let picker = self.skills_picker.as_ref()?;
        picker.selected_skill()
    }

    /// Scroll the chat pane by `lines` (negative scrolls up).
    pub fn scroll_by(&mut self, lines: i32) {
        let next = i64::from(self.scroll_offset) + i64::from(lines);
        let max = self.max_scroll.max(self.scroll_offset);
        let clamped = next.clamp(0, i64::from(max)) as u16;
        self.scroll_offset = clamped;
        if lines > 0 && self.scroll_offset >= self.max_scroll {
            self.stick_to_bottom = true;
        } else if lines < 0 {
            self.stick_to_bottom = false;
        }
    }

    /// Scroll to the top of the chat history.
    pub fn scroll_to_top(&mut self) {
        self.scroll_offset = 0;
        self.stick_to_bottom = false;
    }

    /// Scroll to the bottom of the chat history.
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = self.max_scroll;
        self.stick_to_bottom = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_navigation_walks_entries_and_back() {
        let mut state = TuiState::new("openai", "gpt-4.1");
        state.push_history("first");
        state.push_history("second");
        state.push_history("second"); // consecutive duplicate collapses

        assert_eq!(state.history.len(), 2);
        assert_eq!(state.history_prev().as_deref(), Some("second"));
        assert_eq!(state.history_prev().as_deref(), Some("first"));
        assert_eq!(state.history_prev().as_deref(), Some("first"));
        assert_eq!(state.history_next().as_deref(), Some("second"));
        assert_eq!(state.history_next().as_deref(), Some(""));
        assert_eq!(state.history_next(), None);
    }

    #[test]
    fn loaded_history_precedes_commands_from_the_current_run() {
        let mut state = TuiState::new("openai", "gpt-4.1");
        state.push_history("typed this session");
        state.extend_history([
            "first saved prompt".to_string(),
            "typed this session".to_string(),
        ]);
        assert_eq!(
            state.history.iter().map(String::as_str).collect::<Vec<_>>(),
            vec!["first saved prompt", "typed this session"]
        );
    }

    #[test]
    fn witty_progress_messages_appear_after_a_short_wait() {
        let mut state = TuiState::new("openai", "gpt-4.1");
        assert_eq!(state.working_status(), None);
        state.busy_since = Some(Instant::now());
        assert_eq!(state.working_status().as_deref(), Some("Contemplating"));
        state.busy_since = Some(Instant::now() - Duration::from_secs(6));
        assert_eq!(state.working_status().as_deref(), Some("Simmering"));
    }

    #[test]
    fn spinner_animates_and_witty_phrases_cycle() {
        let initial = spinner_frame(Duration::ZERO);
        let next_frame = spinner_frame(Duration::from_millis(100));
        assert_ne!(initial, next_frame);
        assert_eq!(
            thinking_phrase(Duration::from_secs(3 * (THINKING_PHRASES.len() as u64 - 1))),
            "Brewing digital espresso"
        );
        assert_eq!(
            thinking_phrase(Duration::from_secs(3 * THINKING_PHRASES.len() as u64)),
            THINKING_PHRASES[0]
        );
    }

    #[test]
    fn command_palette_filters_by_prefix() {
        let mut state = TuiState::new("openai", "gpt-4.1");
        state.command_text.clear();
        assert_eq!(state.matching_commands().len(), COMMANDS.len());

        state.command_text = "mo".to_string();
        assert_eq!(state.matching_commands(), vec!["/model", "/models"]);

        state.command_text = "keysa".to_string();
        assert_eq!(state.matching_commands(), vec!["/keysadd"]);

        state.command_text = "re".to_string();
        assert_eq!(state.matching_commands(), vec!["/resume", "/replay"]);

        state.command_text = "fo".to_string();
        assert_eq!(state.matching_commands(), vec!["/fork"]);

        state.command_text = "zzz".to_string();
        assert!(state.matching_commands().is_empty());
    }

    #[test]
    fn palette_selection_moves_and_clamps() {
        let mut state = TuiState::new("openai", "gpt-4.1");
        state.command_text = "mo".to_string();
        assert_eq!(state.selected_command(), Some("/model"));
        state.move_command_selection(1);
        assert_eq!(state.selected_command(), Some("/models"));
        state.move_command_selection(1);
        assert_eq!(
            state.selected_command(),
            Some("/models"),
            "clamps at the end"
        );
        state.move_command_selection(-5);
        assert_eq!(
            state.selected_command(),
            Some("/model"),
            "clamps at the start"
        );
        state.command_text = "zzz".to_string();
        assert_eq!(state.selected_command(), None);
        state.move_command_selection(1);
        assert_eq!(state.selected_command(), None);
    }

    #[test]
    fn scroll_never_goes_negative() {
        let mut state = TuiState::new("openai", "gpt-4.1");
        state.max_scroll = 42;
        state.scroll_by(-5);
        assert_eq!(state.scroll_offset, 0);
        assert!(!state.stick_to_bottom);
        state.scroll_by(50);
        assert_eq!(state.scroll_offset, 42);
    }

    #[test]
    fn help_text_lists_every_command() {
        let help = command_help_text();
        for (name, _) in COMMANDS {
            assert!(help.contains(name), "help mentions {name}");
        }
        assert!(help.contains("Ctrl+Enter"));
    }

    #[test]
    fn key_setup_masks_input_and_resolves_providers() {
        let mut setup = KeySetup::new();
        assert_eq!(setup.provider(), "openai");
        assert_eq!(setup.masked_key(), "");
        setup.key_input = "sk-secret".to_string();
        assert_eq!(setup.masked_key(), "•••••••••");
        assert!(!setup.masked_key().contains("sk"));

        setup.move_selection(2);
        assert_eq!(setup.provider(), "google");
        setup.move_selection(-10);
        assert_eq!(setup.provider(), "openai");
        setup.move_selection(100);
        assert_eq!(setup.provider(), SETUP_PROVIDERS.last().unwrap().0);

        let direct = KeySetup::for_provider("gemini");
        assert_eq!(direct.stage, SetupStage::EnterKey);
        assert_eq!(direct.provider(), "google");
        assert_eq!(direct.provider_label(), "Google Gemini");

        let custom = KeySetup::for_provider("my-local");
        assert_eq!(custom.provider(), "my-local");
        assert_eq!(custom.provider_label(), "my-local");
    }
}
