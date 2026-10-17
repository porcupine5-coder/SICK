//! API key storage, health tracking, and rotation for SICK.
//!
//! Keys are loaded from environment variables and from a local keys file, and
//! grouped into per-provider pools. The canonical file is the encrypted
//! `<SICK_HOME>/keys.enc` (see [`secure`]); the plaintext `keys.toml` /
//! `keys.json` files are still read for backwards compatibility and are
//! migrated to `*.bak` once their contents are stored encrypted.
//!
//! [`KeyStore::next_key`] performs round-robin selection with health tracking:
//! rate-limited, invalid, and exhausted keys are skipped until they recover
//! (if ever), so the agent can transparently rotate keys.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use zeroize::Zeroizing;

pub mod secure;

pub use secure::{
    AddKeyReport, KeychainBackend, MasterKeySource, RemoveKeyReport, SaveReport, add_key_secure,
    add_key_secure_with_base_url, default_encrypted_path, remove_key_secure, save_store_encrypted,
    set_provider_base_url_secure,
};

/// Standard env vars per provider, mirroring the provider table in the plan.
/// Providers may accept several base variable names (e.g. Google accepts both
/// `GOOGLE_API_KEY` and `GEMINI_API_KEY`).
pub const PROVIDER_ENV_KEYS: &[(&str, &[&str])] = &[
    ("openai", &["OPENAI_API_KEY"]),
    ("anthropic", &["ANTHROPIC_API_KEY"]),
    ("google", &["GOOGLE_API_KEY", "GEMINI_API_KEY"]),
    ("deepseek", &["DEEPSEEK_API_KEY"]),
    ("openrouter", &["OPENROUTER_API_KEY"]),
    ("qwen", &["QWEN_API_KEY", "DASHSCOPE_API_KEY"]),
    ("aionlabs", &["AIONLABS_API_KEY", "AION_API_KEY"]),
    ("cohere", &["COHERE_API_KEY", "CO_API_KEY"]),
    ("mistral", &["MISTRAL_API_KEY", "MISTRALAI_API_KEY"]),
    ("zai", &["ZHIPU_API_KEY", "ZAI_API_KEY", "ZHIPUAI_API_KEY"]),
    ("groq", &["GROQ_API_KEY"]),
    ("cloudflare", &["CLOUDFLARE_API_KEY", "CF_API_KEY"]),
    (
        "huggingface",
        &["HUGGINGFACE_API_KEY", "HF_TOKEN", "HF_API_KEY"],
    ),
    ("kilo", &["KILO_API_KEY", "KILO_CODE_API_KEY"]),
    ("llm7", &["LLM7_API_KEY", "LLM7_TOKEN"]),
    ("modelscope", &["MODELSCOPE_API_KEY"]),
    ("nvidia", &["NVIDIA_API_KEY", "NIM_API_KEY"]),
    ("ollama", &["OLLAMA_API_KEY"]),
    ("ovhcloud", &["OVHCLOUD_API_KEY", "OVH_API_KEY"]),
    ("siliconflow", &["SILICONFLOW_API_KEY"]),
    ("cerebras", &["CEREBRAS_API_KEY"]),
    ("sambanova", &["SAMBANOVA_API_KEY", "SAMBA_API_KEY"]),
    ("together", &["TOGETHER_API_KEY", "TOGETHERAI_API_KEY"]),
    ("chutes", &["CHUTES_API_KEY"]),
    ("github", &["GITHUB_TOKEN", "GITHUB_MODELS_API_KEY"]),
];

/// Default cooldown for rate-limited keys when the provider sends no hint.
pub const DEFAULT_RATE_LIMIT_COOLDOWN: Duration = Duration::from_secs(60);

/// Cooldown applied to a key after too many consecutive transient errors.
pub const DEFAULT_ERROR_COOLDOWN: Duration = Duration::from_secs(30);

/// Number of consecutive transient errors tolerated before a cooldown.
pub const MAX_CONSECUTIVE_ERRORS: u32 = 3;

/// Canonicalize provider aliases (`gemini` → `google`, etc.).
pub fn normalize_provider(provider: &str) -> String {
    let provider = provider.trim().to_lowercase();
    match provider.as_str() {
        "gemini" => "google".to_string(),
        "aion" | "aion-labs" | "aion_labs" => "aionlabs".to_string(),
        "co" => "cohere".to_string(),
        "mistralai" | "mistral-ai" | "mistral_ai" => "mistral".to_string(),
        "zhipu" | "zhipuai" | "zhipu-ai" | "z-ai" | "z_ai" => "zai".to_string(),
        "workers-ai" | "cf" => "cloudflare".to_string(),
        "hf" => "huggingface".to_string(),
        "kilo-code" | "kilo_code" | "kiloai" => "kilo".to_string(),
        "llm7.io" | "llm7-io" => "llm7".to_string(),
        "nim" | "nvidia-nim" | "nvidia_nim" => "nvidia".to_string(),
        "ollama-cloud" | "ollama_cloud" => "ollama".to_string(),
        "ovh" => "ovhcloud".to_string(),
        "silicon-flow" | "silicon_flow" => "siliconflow".to_string(),
        "samba-nova" | "samba_nova" => "sambanova".to_string(),
        "togetherai" | "together-ai" | "together_ai" => "together".to_string(),
        "chutesai" | "chutes-ai" | "chutes_ai" => "chutes".to_string(),
        "github-models" | "github_models" => "github".to_string(),
        _ => provider,
    }
}

/// Check if a provider name or alias is recognized in the key-manager catalog.
pub fn is_known_provider(name: &str) -> bool {
    let norm = normalize_provider(name);
    PROVIDER_ENV_KEYS.iter().any(|(p, _)| *p == norm)
}

/// Errors surfaced by the key manager.
#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid keys file: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid keys file: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("invalid keys file: {0}")]
    Invalid(String),
    #[error(
        "a master password is required for the encrypted keys file \
         (set SICK_KEY_PASSWORD, or run in a terminal to be prompted)"
    )]
    PasswordRequired,
    #[error("could not decrypt `{path}`: {reason}")]
    Decrypt { path: PathBuf, reason: String },
    #[error(
        "OS keychain unavailable ({reason}); set SICK_KEYRING_FILE=<path> for a \
         file-backed keyring, or SICK_KEY_PASSWORD=<password> for password protection"
    )]
    KeychainUnavailable { reason: String },
}

/// Health status of a single API key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyStatus {
    /// Usable.
    Active,
    /// Hit an HTTP 429 / rate limit; unhealthy until `rate_limit_reset_at`.
    RateLimited,
    /// Token quota / context window exhausted; skipped permanently.
    Exhausted,
    /// Rejected by the provider (401/403); skipped permanently.
    Invalid,
    /// Recoverable errors (5xx, network). Kept usable until too many
    /// consecutive errors force a short cooldown.
    UnknownError,
}

impl std::fmt::Display for KeyStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            KeyStatus::Active => "Active",
            KeyStatus::RateLimited => "RateLimited",
            KeyStatus::Exhausted => "Exhausted",
            KeyStatus::Invalid => "Invalid",
            KeyStatus::UnknownError => "UnknownError",
        };
        f.write_str(name)
    }
}

/// A single API key for a provider. The secret is zeroized on drop; `id` is a
/// non-secret display identifier (first 8 characters of the value).
#[derive(Debug, Clone)]
pub struct ApiKey {
    /// Provider identifier, e.g. `"openai"`.
    pub provider: String,
    /// The secret; zeroized when dropped.
    pub value: Zeroizing<String>,
    /// Non-secret identifier: the first 8 characters of the value.
    pub id: String,
    /// Env var it was loaded from, if any.
    pub env_var: Option<String>,
    /// Current health status.
    pub status: KeyStatus,
    /// When the cooldown expires (rate limits, transient-error backoff).
    pub rate_limit_reset_at: Option<Instant>,
    /// When the key was last used in a request.
    pub last_used: Option<Instant>,
    /// Consecutive transient errors since the last success.
    consecutive_errors: u32,
}

impl ApiKey {
    /// Create a key from a literal value.
    pub fn new(provider: impl Into<String>, value: impl Into<String>) -> Self {
        let value = value.into();
        Self {
            provider: provider.into(),
            id: key_id(&value),
            value: Zeroizing::new(value),
            env_var: None,
            status: KeyStatus::Active,
            rate_limit_reset_at: None,
            last_used: None,
            consecutive_errors: 0,
        }
    }

    /// Create a key loaded from an environment variable.
    pub fn from_env(
        provider: impl Into<String>,
        env_var: impl Into<String>,
        value: String,
    ) -> Self {
        let mut key = Self::new(provider, value);
        key.env_var = Some(env_var.into());
        key
    }
}

/// First 8 characters of a secret, used as a non-secret display id.
pub fn key_id(value: &str) -> String {
    value.chars().take(8).collect()
}

/// A snapshot of one key's health, for status displays (`--keys`, `/keys`).
#[derive(Debug, Clone)]
pub struct KeyStatusInfo {
    /// Provider this key belongs to.
    pub provider: String,
    /// Non-secret key identifier (first 8 chars of the value).
    pub key_id: String,
    /// Env var the key came from, if any.
    pub env_var: Option<String>,
    /// Current health status.
    pub status: KeyStatus,
    /// When the key was last used, if ever.
    pub last_used: Option<Instant>,
    /// When the cooldown expires, if the key is cooling down.
    pub reset_at: Option<Instant>,
}

impl KeyStatusInfo {
    /// Remaining cooldown, if the key is currently cooling down.
    pub fn reset_in(&self) -> Option<Duration> {
        let reset = self.reset_at?;
        let remaining = reset.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            None
        } else {
            Some(remaining)
        }
    }

    /// Time since the key was last used, if ever.
    pub fn last_used_ago(&self) -> Option<Duration> {
        self.last_used.map(|t| t.elapsed())
    }
}

/// Per-provider settings read from the keys file (base URL, defaults).
#[derive(Debug, Clone, Default)]
pub struct ProviderKeyConfig {
    /// Base URL override for this provider.
    pub base_url: Option<String>,
    /// Default model for this provider.
    pub default_model: Option<String>,
    /// Context window override for this provider.
    pub context_window: Option<usize>,
}

/// Outcome of [`KeyStore::load_encrypted_default`].
#[derive(Debug)]
pub struct LoadedEncrypted {
    /// How the master key protecting `keys.enc` is stored.
    pub source: MasterKeySource,
    /// True when the primary file failed to decrypt and the `*.bak` backup
    /// was used instead.
    pub recovered: bool,
    /// Number of keys loaded.
    pub keys_loaded: usize,
}

/// Holds all configured keys grouped by provider, plus rotation state.
#[derive(Debug, Default, Clone)]
pub struct KeyStore {
    pools: HashMap<String, Vec<ApiKey>>,
    current_index: HashMap<String, usize>,
    provider_config: HashMap<String, ProviderKeyConfig>,
}

impl KeyStore {
    /// Create an empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a literal key for a provider.
    ///
    /// Blank keys and duplicates (same provider + value) are ignored.
    /// Returns `true` when the key was added.
    pub fn add_key(&mut self, provider: &str, key: &str) -> bool {
        let provider = normalize_provider(provider);
        let key = key.trim();
        if key.is_empty() {
            return false;
        }
        let pool = self.pools.entry(provider.clone()).or_default();
        if pool.iter().any(|existing| existing.value.as_str() == key) {
            return false;
        }
        pool.push(ApiKey::new(provider, key));
        true
    }

    /// Attach per-provider settings (base URL, defaults) from a keys file.
    pub fn configure_provider(&mut self, provider: &str, config: ProviderKeyConfig) {
        let provider = normalize_provider(provider);
        self.provider_config.insert(provider, config);
    }

    /// Convenience: set only the base URL for a provider.
    pub fn set_provider_base_url(&mut self, provider: &str, base_url: impl Into<String>) {
        let provider = normalize_provider(provider);
        let entry = self.provider_config.entry(provider).or_default();
        entry.base_url = Some(base_url.into().trim_end_matches('/').to_string());
    }

    /// Base URL override configured for a provider, if any.
    pub fn provider_base_url(&self, provider: &str) -> Option<String> {
        self.provider_config
            .get(&normalize_provider(provider))
            .and_then(|c| c.base_url.clone())
    }

    /// Full per-provider settings, if any were configured.
    pub fn provider_config(&self, provider: &str) -> Option<&ProviderKeyConfig> {
        self.provider_config.get(&normalize_provider(provider))
    }

    /// Load every supported provider key found in the process environment.
    ///
    /// For each provider this checks every base variable (e.g. Google checks
    /// both `GOOGLE_API_KEY` and `GEMINI_API_KEY`), numbered variants
    /// (`<BASE>_1`, `_2`, ... up to `_9`), and the `SICK_<BASE>` fallback
    /// (with the legacy `SHELL_AGENT_<BASE>` name still accepted).
    /// Returns the number of keys loaded.
    pub fn load_from_env(&mut self) -> usize {
        let mut loaded = 0;

        for (provider, base_vars) in PROVIDER_ENV_KEYS {
            for base_var in *base_vars {
                for var in env_var_candidates(base_var) {
                    if let Some(value) = non_empty_env(&var) {
                        let value = value.trim().to_string();
                        let key = ApiKey::from_env(*provider, var.clone(), value);
                        let pool = self.pools.entry((*provider).to_string()).or_default();
                        if pool.iter().any(|existing| existing.value == key.value) {
                            continue;
                        }
                        pool.push(key);
                        loaded += 1;
                    }
                }
            }
        }

        loaded
    }

    /// Load keys from a keys file.
    ///
    /// `.toml` files use the provider-section format:
    /// ```toml
    /// [openai]
    /// keys = ["sk-...", { env = "OPENAI_API_KEY_2" }, { value = "sk-..." }]
    /// base_url = "https://api.openai.com/v1"
    /// ```
    ///
    /// `.json` files keep the legacy shapes:
    /// ```json
    /// { "openai": { "keys": ["sk-..."], "base_url": "..." } }
    /// ```
    ///
    /// Returns the number of keys loaded (entries pointing at unset env vars
    /// or duplicates are skipped).
    pub fn load_from_file(&mut self, path: &Path) -> Result<usize, KeyError> {
        let raw = std::fs::read_to_string(path)?;
        let is_toml = path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"));
        if is_toml {
            self.load_toml(&raw)
        } else {
            self.load_json(&raw)
        }
    }

    /// Load keys from a TOML string in the keys-file format.
    ///
    /// See [`KeyStore::load_from_file`] for the format. Returns the number of
    /// keys loaded.
    pub fn load_toml(&mut self, raw: &str) -> Result<usize, KeyError> {
        let sections: HashMap<String, TomlProviderSection> = toml::from_str(raw)?;
        let mut loaded = 0;

        for (provider, section) in sections {
            let provider = normalize_provider(&provider);
            for entry in &section.keys {
                match entry {
                    KeyEntry::Literal(value) => {
                        if self.add_key(&provider, value) {
                            loaded += 1;
                        }
                    }
                    KeyEntry::Value { value } => {
                        if self.add_key(&provider, value) {
                            loaded += 1;
                        }
                    }
                    KeyEntry::Env { env } => {
                        if let Some(value) = non_empty_env(env) {
                            let key = ApiKey::from_env(provider.clone(), env.clone(), value);
                            let pool = self.pools.entry(provider.clone()).or_default();
                            if pool.iter().any(|existing| existing.value == key.value) {
                                continue;
                            }
                            pool.push(key);
                            loaded += 1;
                        }
                    }
                }
            }

            if section.base_url.is_some()
                || section.default_model.is_some()
                || section.context_window.is_some()
            {
                self.configure_provider(
                    &provider,
                    ProviderKeyConfig {
                        base_url: section
                            .base_url
                            .map(|url| url.trim_end_matches('/').to_string()),
                        default_model: section.default_model,
                        context_window: section.context_window,
                    },
                );
            }
        }

        Ok(loaded)
    }

    fn load_json(&mut self, raw: &str) -> Result<usize, KeyError> {
        let value: serde_json::Value = serde_json::from_str(raw)?;
        let obj = value
            .as_object()
            .ok_or_else(|| KeyError::Invalid("top-level JSON must be an object".to_string()))?;

        let mut loaded = 0;
        for (provider, spec) in obj {
            let mut settings = ProviderKeyConfig::default();
            let entries: Vec<&serde_json::Value> = match spec {
                serde_json::Value::Array(items) => items.iter().collect(),
                serde_json::Value::Object(map) => {
                    settings.base_url = map
                        .get("base_url")
                        .and_then(|v| v.as_str())
                        .map(|url| url.trim_end_matches('/').to_string());
                    settings.default_model = map
                        .get("default_model")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                    settings.context_window = map
                        .get("context_window")
                        .and_then(|v| v.as_u64())
                        .map(|n| n as usize);
                    map.get("keys")
                        .and_then(serde_json::Value::as_array)
                        .map(|items| items.iter().collect())
                        .unwrap_or_default()
                }
                _ => continue,
            };

            for entry in entries {
                match entry {
                    serde_json::Value::String(key) => {
                        if self.add_key(provider, key) {
                            loaded += 1;
                        }
                    }
                    serde_json::Value::Object(fields) => {
                        if let Some(env_var) = fields.get("env").and_then(|v| v.as_str()) {
                            if let Some(value) = non_empty_env(env_var) {
                                let provider = normalize_provider(provider);
                                let key = ApiKey::from_env(
                                    provider.clone(),
                                    env_var.to_string(),
                                    value.trim().to_string(),
                                );
                                let pool = self.pools.entry(provider).or_default();
                                if pool.iter().any(|existing| existing.value == key.value) {
                                    continue;
                                }
                                pool.push(key);
                                loaded += 1;
                            }
                        } else if let Some(value) = fields.get("value").and_then(|v| v.as_str())
                            && self.add_key(provider, value)
                        {
                            loaded += 1;
                        }
                    }
                    _ => {}
                }
            }

            if settings.base_url.is_some()
                || settings.default_model.is_some()
                || settings.context_window.is_some()
            {
                self.configure_provider(provider, settings);
            }
        }

        Ok(loaded)
    }

    /// Load the default encrypted keys file, if one exists.
    ///
    /// Reads `<SICK_HOME>/keys.enc`. `password` overrides the
    /// `SICK_KEY_PASSWORD` environment variable for password-protected files;
    /// when neither is available the call fails with
    /// [`KeyError::PasswordRequired`]. A corrupt primary file falls back to
    /// `<SICK_HOME>/keys.enc.bak` when that backup decrypts.
    pub fn load_encrypted_default(
        &mut self,
        password: Option<&str>,
    ) -> Result<Option<LoadedEncrypted>, KeyError> {
        let backend = KeychainBackend::from_env();
        match secure::load_default(password, &backend)? {
            Some((text, source, recovered)) => {
                let keys_loaded = self.load_toml(&text)?;
                Ok(Some(LoadedEncrypted {
                    source,
                    recovered,
                    keys_loaded,
                }))
            }
            None => Ok(None),
        }
    }

    /// Load the plaintext legacy files (`keys.toml`, then `keys.json`) when
    /// present. Returns the number of keys loaded.
    pub fn load_legacy_default_files(&mut self) -> Result<usize, KeyError> {
        let mut loaded = 0;
        let toml_path = Self::default_keys_toml_path();
        if toml_path.is_file() {
            loaded += self.load_from_file(&toml_path)?;
        }
        let json_path = Self::default_keys_path();
        if json_path.is_file() {
            loaded += self.load_from_file(&json_path)?;
        }
        Ok(loaded)
    }

    /// Load the default keys file if it exists.
    ///
    /// Prefers the encrypted `<SICK_HOME>/keys.enc`, then the plaintext
    /// `keys.toml`, then the legacy `keys.json`. Password-protected encrypted
    /// files surface [`KeyError::PasswordRequired`] unless
    /// `SICK_KEY_PASSWORD` is set; retry with
    /// [`KeyStore::load_encrypted_default`] and an explicit password.
    pub fn load_default_file(&mut self) -> Result<usize, KeyError> {
        if let Some(loaded) = self.load_encrypted_default(None)? {
            return Ok(loaded.keys_loaded);
        }
        self.load_legacy_default_files()
    }

    /// Select the next healthy key for a provider (round-robin).
    ///
    /// - Skips `RateLimited` / `UnknownError` keys whose cooldown has not
    ///   elapsed, and `Invalid` / `Exhausted` keys permanently.
    /// - Expired cooldowns are promoted back to `Active`.
    /// - Returns `None` when every key is unusable.
    pub fn next_key(&mut self, provider: &str) -> Option<&ApiKey> {
        let provider = normalize_provider(provider);
        if self.pools.get(&provider).is_none_or(Vec::is_empty) {
            return None;
        }
        self.refresh_key_health();
        let now = Instant::now();

        let keys = self.pools.get(&provider)?;
        let len = keys.len();
        let start = *self.current_index.get(&provider).unwrap_or(&0) % len;

        let mut selected = None;
        for offset in 0..len {
            let idx = (start + offset) % len;
            if is_usable(&keys[idx], now) {
                selected = Some(idx);
                break;
            }
        }

        let idx = selected?;
        self.current_index.insert(provider.clone(), (idx + 1) % len);
        self.pools.get(&provider)?.get(idx)
    }

    /// Update the health status of a specific key.
    ///
    /// `retry_after` sets the cooldown for `RateLimited` / `UnknownError`
    /// statuses (defaults apply when `None`).
    pub fn mark_key_status(
        &mut self,
        provider: &str,
        key_id: &str,
        status: KeyStatus,
        retry_after: Option<Duration>,
    ) {
        let provider = normalize_provider(provider);
        let now = Instant::now();
        let Some(keys) = self.pools.get_mut(&provider) else {
            return;
        };
        let Some(key) = keys.iter_mut().find(|k| k.id == key_id) else {
            return;
        };

        key.last_used = Some(now);
        match status {
            KeyStatus::Active => {
                key.status = KeyStatus::Active;
                key.rate_limit_reset_at = None;
                key.consecutive_errors = 0;
            }
            KeyStatus::RateLimited => {
                key.status = KeyStatus::RateLimited;
                key.rate_limit_reset_at =
                    Some(now + retry_after.unwrap_or(DEFAULT_RATE_LIMIT_COOLDOWN));
            }
            KeyStatus::Exhausted | KeyStatus::Invalid => {
                key.status = status;
                key.rate_limit_reset_at = None;
            }
            KeyStatus::UnknownError => {
                key.consecutive_errors += 1;
                key.status = KeyStatus::UnknownError;
                if let Some(delay) = retry_after {
                    key.rate_limit_reset_at = Some(now + delay);
                } else if key.consecutive_errors > MAX_CONSECUTIVE_ERRORS {
                    key.rate_limit_reset_at = Some(now + DEFAULT_ERROR_COOLDOWN);
                }
            }
        }
    }

    /// True when at least one key is currently usable.
    pub fn healthy_keys_exist(&self, provider: &str) -> bool {
        let provider = normalize_provider(provider);
        let now = Instant::now();
        self.pools
            .get(&provider)
            .is_some_and(|keys| keys.iter().any(|key| is_usable(key, now)))
    }

    /// True when every configured key is permanently unusable
    /// (`Invalid` or `Exhausted`).
    pub fn all_keys_exhausted(&self, provider: &str) -> bool {
        let provider = normalize_provider(provider);
        self.pools.get(&provider).is_some_and(|keys| {
            !keys.is_empty()
                && keys
                    .iter()
                    .all(|k| matches!(k.status, KeyStatus::Invalid | KeyStatus::Exhausted))
        })
    }

    /// Number of keys configured for a provider.
    pub fn key_count(&self, provider: &str) -> usize {
        self.pools
            .get(&normalize_provider(provider))
            .map(Vec::len)
            .unwrap_or(0)
    }

    /// Providers with at least one key, sorted.
    pub fn providers(&self) -> Vec<String> {
        let mut providers: Vec<String> = self
            .pools
            .iter()
            .filter(|(_, keys)| !keys.is_empty())
            .map(|(provider, _)| provider.clone())
            .collect();
        providers.sort();
        providers
    }

    /// True when at least one key is configured for any provider.
    pub fn has_keys(&self) -> bool {
        !self.providers().is_empty()
    }

    /// Snapshot of every key's health, grouped by provider (sorted).
    pub fn get_status_report(&self) -> Vec<KeyStatusInfo> {
        let mut report = Vec::new();
        for provider in self.providers() {
            if let Some(keys) = self.pools.get(&provider) {
                for key in keys {
                    report.push(KeyStatusInfo {
                        provider: provider.clone(),
                        key_id: key.id.clone(),
                        env_var: key.env_var.clone(),
                        status: key.status,
                        last_used: key.last_used,
                        reset_at: key.rate_limit_reset_at,
                    });
                }
            }
        }
        report
    }

    /// Promote keys whose cooldown elapsed back to `Active`.
    ///
    /// Called automatically by [`KeyStore::next_key`]; exposed so status
    /// displays (`sick keys`, TUI `/keys`) show recovered keys as active.
    /// Returns how many keys recovered.
    pub fn refresh_key_health(&mut self) -> usize {
        let now = Instant::now();
        let mut refreshed = 0;
        for keys in self.pools.values_mut() {
            for key in keys.iter_mut() {
                if matches!(key.status, KeyStatus::RateLimited | KeyStatus::UnknownError)
                    && let Some(reset) = key.rate_limit_reset_at
                    && reset <= now
                {
                    key.status = KeyStatus::Active;
                    key.rate_limit_reset_at = None;
                    key.consecutive_errors = 0;
                    refreshed += 1;
                }
            }
        }
        refreshed
    }

    /// The keys configured for a provider (empty slice when none).
    pub fn keys_for(&self, provider: &str) -> &[ApiKey] {
        self.pools
            .get(&normalize_provider(provider))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Remove the key whose non-secret display id matches `key_id` from
    /// `provider`. Returns `true` when a key was removed.
    pub fn remove_key(&mut self, provider: &str, key_id: &str) -> bool {
        let provider = normalize_provider(provider);
        let Some(keys) = self.pools.get_mut(&provider) else {
            return false;
        };
        let before = keys.len();
        keys.retain(|key| key.id != key_id);
        keys.len() < before
    }

    /// Serialize the store to the keys-file TOML format (secrets included).
    ///
    /// This is the plaintext document that gets encrypted into `keys.enc`; it
    /// is also what `sick export` writes. Env-referenced keys are materialized
    /// as literal values so an export is self-contained.
    pub fn to_toml_text(&self) -> Result<String, KeyError> {
        let mut root = toml::Table::new();
        let mut providers = self.providers();
        providers.extend(self.provider_config.keys().cloned());
        providers.sort();
        providers.dedup();
        for provider in providers {
            let keys = self.pools.get(&provider).map(Vec::as_slice).unwrap_or(&[]);
            let mut table = toml::Table::new();
            let entries = keys
                .iter()
                .map(|key| toml::Value::String(key.value.as_str().to_string()))
                .collect();
            table.insert("keys".to_string(), toml::Value::Array(entries));
            if let Some(config) = self.provider_config.get(&provider) {
                if let Some(base_url) = &config.base_url {
                    table.insert(
                        "base_url".to_string(),
                        toml::Value::String(base_url.clone()),
                    );
                }
                if let Some(default_model) = &config.default_model {
                    table.insert(
                        "default_model".to_string(),
                        toml::Value::String(default_model.clone()),
                    );
                }
                if let Some(context_window) = config.context_window {
                    table.insert(
                        "context_window".to_string(),
                        toml::Value::Integer(context_window as i64),
                    );
                }
            }
            root.insert(provider, toml::Value::Table(table));
        }
        toml::to_string_pretty(&root)
            .map_err(|error| KeyError::Invalid(format!("failed to serialize keys: {error}")))
    }

    /// Default location of the legacy keys file:
    /// `<SICK_HOME>/keys.json` (defaults to `~/.sick/keys.json`).
    pub fn default_keys_path() -> PathBuf {
        shell_common::sick_home().join("keys.json")
    }

    /// Default location of the keys file:
    /// `<SICK_HOME>/keys.toml` (defaults to `~/.sick/keys.toml`).
    pub fn default_keys_toml_path() -> PathBuf {
        shell_common::sick_home().join("keys.toml")
    }

    /// Default location of the encrypted keys file:
    /// `<SICK_HOME>/keys.enc` (defaults to `~/.sick/keys.enc`).
    pub fn default_encrypted_path() -> PathBuf {
        secure::default_encrypted_path()
    }
}

/// Append `secret` to the keys file at `path`, creating the file (and its
/// parent directory) when needed and preserving every other provider/entry.
///
/// Returns `Ok(true)` when the key was written, `Ok(false)` when an identical
/// key for the provider was already present (the file is left untouched).
pub fn save_key_to_file(path: &Path, provider: &str, secret: &str) -> Result<bool, KeyError> {
    let provider = normalize_provider(provider);
    let secret = secret.trim();
    if secret.is_empty() {
        return Err(KeyError::Invalid("API key is empty".to_string()));
    }

    let mut root: toml::Table = if path.is_file() {
        let raw = std::fs::read_to_string(path)?;
        if raw.trim().is_empty() {
            toml::Table::new()
        } else {
            raw.parse::<toml::Table>()?
        }
    } else {
        toml::Table::new()
    };

    let section = root
        .entry(provider.clone())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let table = section
        .as_table_mut()
        .ok_or_else(|| KeyError::Invalid(format!("`{provider}` section is not a table")))?;
    let keys = table
        .entry("keys".to_string())
        .or_insert_with(|| toml::Value::Array(Vec::new()));
    let entries = keys
        .as_array_mut()
        .ok_or_else(|| KeyError::Invalid(format!("`{provider}.keys` is not an array")))?;

    let already_present = entries.iter().any(|entry| match entry {
        toml::Value::String(existing) => existing == secret,
        toml::Value::Table(table) => {
            table.get("value").and_then(|value| value.as_str()) == Some(secret)
        }
        _ => false,
    });
    if already_present {
        return Ok(false);
    }
    entries.push(toml::Value::String(secret.to_string()));

    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let text = toml::to_string_pretty(&root)
        .map_err(|error| KeyError::Invalid(format!("failed to serialize keys file: {error}")))?;
    std::fs::write(path, text)?;
    Ok(true)
}

/// Whether a key can serve a request right now.
fn is_usable(key: &ApiKey, now: Instant) -> bool {
    match key.status {
        KeyStatus::Active => true,
        KeyStatus::UnknownError => key.rate_limit_reset_at.is_none_or(|reset| reset <= now),
        KeyStatus::RateLimited | KeyStatus::Exhausted | KeyStatus::Invalid => false,
    }
}

/// Env var names checked for one provider base variable, in priority order:
/// the standard name, numbered variants `_1.._9`, then the `SICK_` prefixed
/// form and the legacy `SHELL_AGENT_` form.
pub fn env_var_candidates(base_var: &str) -> Vec<String> {
    let mut vars = Vec::with_capacity(12);
    vars.push(base_var.to_string());
    for n in 1..=9 {
        vars.push(format!("{base_var}_{n}"));
    }
    vars.push(format!("SICK_{base_var}"));
    vars.push(format!("SHELL_AGENT_{base_var}"));
    vars
}

/// Env var names worth checking for a provider, in order (for error hints).
pub fn expected_env_vars(provider: &str) -> Vec<String> {
    let provider = normalize_provider(provider);
    match PROVIDER_ENV_KEYS.iter().find(|(p, _)| *p == provider) {
        Some((_, base_vars)) => {
            let mut vars = Vec::new();
            for base in *base_vars {
                vars.push((*base).to_string());
                vars.push(format!("{base}_1"));
            }
            vars.push(format!("SICK_{}", base_vars[0]));
            vars.push(format!("SHELL_AGENT_{}", base_vars[0]));
            vars
        }
        None => vec![format!("{}_API_KEY", provider.to_uppercase())],
    }
}

fn non_empty_env(var: &str) -> Option<String> {
    match std::env::var(var) {
        Ok(value) if !value.trim().is_empty() => Some(value),
        _ => None,
    }
}

/// One entry in a TOML keys file: `"sk-..."`, `{ env = "..." }`, or
/// `{ value = "..." }`.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(untagged)]
enum KeyEntry {
    Literal(String),
    Env { env: String },
    Value { value: String },
}

/// One `[provider]` section of a TOML keys file.
#[derive(Debug, Clone, Default, serde::Deserialize)]
struct TomlProviderSection {
    #[serde(default)]
    keys: Vec<KeyEntry>,
    base_url: Option<String>,
    default_model: Option<String>,
    context_window: Option<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn value_of(key: &ApiKey) -> &str {
        key.value.as_str()
    }

    #[test]
    fn add_and_select_round_robin() {
        let mut store = KeyStore::new();
        assert!(store.add_key("openai", "sk-first"));
        assert!(store.add_key("openai", "sk-second"));
        assert_eq!(store.key_count("openai"), 2);
        assert_eq!(value_of(store.next_key("openai").unwrap()), "sk-first");
        assert_eq!(value_of(store.next_key("openai").unwrap()), "sk-second");
        assert_eq!(value_of(store.next_key("openai").unwrap()), "sk-first");
        assert!(store.next_key("anthropic").is_none());
    }

    #[test]
    fn deduplicates_identical_keys() {
        let mut store = KeyStore::new();
        assert!(store.add_key("openai", "sk-same"));
        assert!(!store.add_key("openai", "sk-same"));
        assert_eq!(store.key_count("openai"), 1);
    }

    #[test]
    fn key_id_is_first_eight_chars() {
        let mut store = KeyStore::new();
        store.add_key("openai", "sk-abcdefghij");
        let key = store.next_key("openai").unwrap();
        assert_eq!(key.id, "sk-abcde");
        assert_eq!(key.status, KeyStatus::Active);
    }

    #[test]
    fn ignores_blank_keys_and_normalizes_provider() {
        let mut store = KeyStore::new();
        assert!(!store.add_key("OpenAI", "   "));
        assert!(store.add_key("OpenAI", " sk-live "));
        assert_eq!(store.key_count("openai"), 1);
        assert_eq!(value_of(store.next_key("openai").unwrap()), "sk-live");
    }

    #[test]
    fn providers_lists_only_configured() {
        let mut store = KeyStore::new();
        store.add_key("openai", "sk-a");
        store.add_key("deepseek", "sk-b");
        assert_eq!(store.providers(), vec!["deepseek", "openai"]);
        assert!(store.has_keys());
    }

    #[test]
    fn gemini_is_an_alias_for_google() {
        let mut store = KeyStore::new();
        store.add_key("gemini", "gm-live");
        assert_eq!(store.key_count("google"), 1);
        assert_eq!(value_of(store.next_key("google").unwrap()), "gm-live");
        assert_eq!(store.providers(), vec!["google"]);

        store.add_key("google", "gm-second");
        assert_eq!(store.key_count("gemini"), 2);
    }

    #[test]
    fn rate_limited_key_is_skipped_until_cooldown_expires() {
        let mut store = KeyStore::new();
        store.add_key("openai", "sk-one");
        let id = store.next_key("openai").unwrap().id.clone();
        store.mark_key_status(
            "openai",
            &id,
            KeyStatus::RateLimited,
            Some(Duration::from_secs(60)),
        );

        assert!(store.next_key("openai").is_none());
        assert!(!store.healthy_keys_exist("openai"));

        // Default cooldown applies when no retry_after hint is given.
        store.mark_key_status("openai", &id, KeyStatus::RateLimited, None);
        assert!(store.next_key("openai").is_none());
        let report = store.get_status_report();
        let remaining = report[0].reset_in().expect("default cooldown set");
        assert!(remaining <= DEFAULT_RATE_LIMIT_COOLDOWN);

        // An explicit success clears the cooldown immediately.
        store.mark_key_status("openai", &id, KeyStatus::Active, None);
        assert!(store.next_key("openai").is_some());

        // A zero-length cooldown is promoted back to Active immediately.
        store.mark_key_status("openai", &id, KeyStatus::RateLimited, Some(Duration::ZERO));
        let key = store
            .next_key("openai")
            .expect("expired cooldown re-usable");
        assert_eq!(key.status, KeyStatus::Active);
        assert!(key.rate_limit_reset_at.is_none());
    }

    #[test]
    fn invalid_and_exhausted_are_permanently_skipped() {
        let mut store = KeyStore::new();
        store.add_key("openai", "sk-bad");
        store.add_key("openai", "sk-good");
        let bad = store.next_key("openai").unwrap().id.clone();
        store.mark_key_status("openai", &bad, KeyStatus::Invalid, None);

        assert_eq!(value_of(store.next_key("openai").unwrap()), "sk-good");
        assert_eq!(value_of(store.next_key("openai").unwrap()), "sk-good");

        let good = store.next_key("openai").unwrap().id.clone();
        store.mark_key_status("openai", &good, KeyStatus::Exhausted, None);
        assert!(store.next_key("openai").is_none());
        assert!(store.all_keys_exhausted("openai"));

        // One invalid + one exhausted → still all exhausted (permanent).
        store.mark_key_status("openai", &good, KeyStatus::Active, None);
        assert!(!store.all_keys_exhausted("openai"));
        assert!(store.healthy_keys_exist("openai"));
    }

    #[test]
    fn unknown_error_gets_cooldown_after_consecutive_failures() {
        let mut store = KeyStore::new();
        store.add_key("openai", "sk-flaky");
        let id = store.next_key("openai").unwrap().id.clone();

        for _ in 0..MAX_CONSECUTIVE_ERRORS {
            store.mark_key_status("openai", &id, KeyStatus::UnknownError, None);
        }
        // Still usable at the tolerance threshold.
        assert!(store.next_key("openai").is_some());

        // The next consecutive error crosses the threshold → cooldown.
        store.mark_key_status("openai", &id, KeyStatus::UnknownError, None);
        assert!(
            store.next_key("openai").is_none(),
            "key should be cooling down after >3 consecutive errors"
        );
        assert!(!store.healthy_keys_exist("openai"));

        // A success resets the counter and clears the cooldown.
        store.mark_key_status("openai", &id, KeyStatus::Active, None);
        assert!(store.next_key("openai").is_some());
    }

    #[test]
    fn status_report_reflects_health() {
        let mut store = KeyStore::new();
        store.add_key("openai", "sk-aaaa1111");
        store.add_key("openai", "sk-bbbb2222");
        store.add_key("anthropic", "sk-ant-3333");
        let first = store.next_key("openai").unwrap().id.clone();
        store.mark_key_status(
            "openai",
            &first,
            KeyStatus::RateLimited,
            Some(Duration::from_secs(45)),
        );

        let report = store.get_status_report();
        assert_eq!(report.len(), 3);

        let openai: Vec<&KeyStatusInfo> =
            report.iter().filter(|i| i.provider == "openai").collect();
        assert_eq!(openai.len(), 2);
        assert_eq!(openai[0].key_id, first);
        assert_eq!(openai[0].status, KeyStatus::RateLimited);
        assert!(openai[0].reset_in().is_some());
        assert!(openai[0].last_used_ago().is_some());
        assert_eq!(openai[1].status, KeyStatus::Active);
        assert!(openai[1].last_used_ago().is_none());

        let anthropic = report.iter().find(|i| i.provider == "anthropic").unwrap();
        assert_eq!(anthropic.key_id, "sk-ant-3");
        assert_eq!(anthropic.status, KeyStatus::Active);
    }

    #[test]
    fn loads_keys_from_json_file_and_reads_settings() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("keys.json");
        let mut file = std::fs::File::create(&path).expect("create keys.json");
        writeln!(
            file,
            r#"{{
                "openai": {{ "keys": ["sk-one", "sk-two"], "base_url": "http://localhost:9999/v1/" }},
                "anthropic": {{ "keys": [ {{ "value": "sk-ant" }} ] }}
            }}"#
        )
        .expect("write keys.json");

        let mut store = KeyStore::new();
        let loaded = store.load_from_file(&path).expect("keys load");
        assert_eq!(loaded, 3);
        assert_eq!(value_of(store.next_key("openai").unwrap()), "sk-one");
        assert_eq!(value_of(store.next_key("anthropic").unwrap()), "sk-ant");
        assert_eq!(
            store.provider_base_url("openai").as_deref(),
            Some("http://localhost:9999/v1")
        );
    }

    #[test]
    fn loads_keys_from_toml_file_with_env_and_value_entries() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("keys.toml");
        std::fs::write(
            &path,
            r#"
            [openai]
            keys = ["sk-literal", { value = "sk-inline" }, { env = "SHELL_AGENT_TEST_UNSET_ENV_XYZ" }]
            base_url = "https://api.openai.com/v1/"
            default_model = "gpt-4.1"
            context_window = 1047576

            [deepseek]
            keys = ["ds-1", "ds-2"]
            "#,
        )
        .expect("write keys.toml");

        let mut store = KeyStore::new();
        let loaded = store.load_from_file(&path).expect("keys load");
        assert_eq!(loaded, 4, "unset env entry is skipped");
        assert_eq!(store.key_count("openai"), 2);
        assert_eq!(store.key_count("deepseek"), 2);
        assert_eq!(value_of(store.next_key("openai").unwrap()), "sk-literal");
        assert_eq!(value_of(store.next_key("openai").unwrap()), "sk-inline");
        assert_eq!(
            store.provider_base_url("openai").as_deref(),
            Some("https://api.openai.com/v1")
        );
        let settings = store.provider_config("openai").unwrap();
        assert_eq!(settings.default_model.as_deref(), Some("gpt-4.1"));
        assert_eq!(settings.context_window, Some(1047576));
    }

    #[test]
    fn provider_endpoint_settings_persist_without_literal_keys() {
        let mut store = KeyStore::new();
        store.set_provider_base_url(
            "cloudflare",
            "https://api.cloudflare.com/client/v4/accounts/0123456789abcdef0123456789abcdef/ai/v1",
        );
        let serialized = store.to_toml_text().unwrap();
        let mut loaded = KeyStore::new();
        assert_eq!(loaded.load_toml(&serialized).unwrap(), 0);
        assert_eq!(
            loaded.provider_base_url("cloudflare").as_deref(),
            Some(
                "https://api.cloudflare.com/client/v4/accounts/0123456789abcdef0123456789abcdef/ai/v1"
            )
        );
    }

    #[test]
    fn skips_env_entries_for_unset_vars() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("keys.json");
        std::fs::write(
            &path,
            r#"{ "openai": [ { "env": "SHELL_AGENT_TEST_DEFINITELY_UNSET_VAR_123" } ] }"#,
        )
        .expect("write keys.json");

        let mut store = KeyStore::new();
        let loaded = store.load_from_file(&path).expect("keys load");
        assert_eq!(loaded, 0);
        assert_eq!(store.key_count("openai"), 0);
    }

    #[test]
    fn rejects_non_object_json() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("keys.json");
        std::fs::write(&path, r#"["sk"]"#).expect("write keys.json");
        let mut store = KeyStore::new();
        assert!(store.load_from_file(&path).is_err());
    }

    #[test]
    fn expected_env_vars_covers_known_and_unknown_providers() {
        assert_eq!(
            expected_env_vars("openai"),
            vec![
                "OPENAI_API_KEY",
                "OPENAI_API_KEY_1",
                "SICK_OPENAI_API_KEY",
                "SHELL_AGENT_OPENAI_API_KEY"
            ]
        );
        assert_eq!(
            expected_env_vars("gemini"),
            vec![
                "GOOGLE_API_KEY",
                "GOOGLE_API_KEY_1",
                "GEMINI_API_KEY",
                "GEMINI_API_KEY_1",
                "SICK_GOOGLE_API_KEY",
                "SHELL_AGENT_GOOGLE_API_KEY"
            ]
        );
        assert_eq!(expected_env_vars("my-local"), vec!["MY-LOCAL_API_KEY"]);
    }

    #[test]
    fn env_var_candidates_cover_numbered_sick_and_legacy_names() {
        assert_eq!(
            env_var_candidates("OPENAI_API_KEY"),
            vec![
                "OPENAI_API_KEY",
                "OPENAI_API_KEY_1",
                "OPENAI_API_KEY_2",
                "OPENAI_API_KEY_3",
                "OPENAI_API_KEY_4",
                "OPENAI_API_KEY_5",
                "OPENAI_API_KEY_6",
                "OPENAI_API_KEY_7",
                "OPENAI_API_KEY_8",
                "OPENAI_API_KEY_9",
                "SICK_OPENAI_API_KEY",
                "SHELL_AGENT_OPENAI_API_KEY",
            ]
        );
    }

    #[test]
    fn save_key_to_file_creates_appends_and_dedupes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested").join("keys.toml");

        // Creates the file (and parent dirs) from scratch.
        assert!(save_key_to_file(&path, "openai", "sk-first").expect("save"));
        let raw = std::fs::read_to_string(&path).expect("read");
        assert!(raw.contains("openai"));
        assert!(raw.contains("sk-first"));

        // Appends without disturbing other providers, and normalizes aliases.
        assert!(save_key_to_file(&path, "gemini", "gm-second").expect("save"));
        assert!(save_key_to_file(&path, "openai", "sk-second").expect("save"));
        let raw = std::fs::read_to_string(&path).expect("read");
        assert!(raw.contains("sk-first"));
        assert!(raw.contains("sk-second"));
        assert!(raw.contains("gm-second"));
        assert!(
            raw.contains("[google]"),
            "gemini normalizes to google: {raw}"
        );

        // Identical key: no write, reported as already present.
        assert!(!save_key_to_file(&path, "openai", "sk-first").expect("dedupe"));
        let after = std::fs::read_to_string(&path).expect("read");
        assert_eq!(raw, after);

        // The file is a valid keys file for the loader.
        let mut store = KeyStore::new();
        let loaded = store.load_from_file(&path).expect("load");
        assert_eq!(loaded, 3);
        assert_eq!(store.key_count("openai"), 2);
        assert_eq!(store.key_count("google"), 1);
    }

    #[test]
    fn save_key_to_file_rejects_empty_and_malformed_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(save_key_to_file(&dir.path().join("k.toml"), "openai", "  ").is_err());

        let path = dir.path().join("bad.toml");
        std::fs::write(&path, "not = [valid").expect("write");
        assert!(save_key_to_file(&path, "openai", "sk-x").is_err());
    }

    #[test]
    fn is_known_provider_identifies_all_known_providers_and_aliases() {
        assert!(is_known_provider("openai"));
        assert!(is_known_provider("google"));
        assert!(is_known_provider("gemini"));
        assert!(is_known_provider("cerebras"));
        assert!(is_known_provider("sambanova"));
        assert!(is_known_provider("samba-nova"));
        assert!(is_known_provider("together"));
        assert!(is_known_provider("chutes"));
        assert!(is_known_provider("github"));
        assert!(is_known_provider("github-models"));
        assert!(!is_known_provider("unknown-provider-xyz"));
    }
}
