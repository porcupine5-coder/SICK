//! Encrypted key storage (Phase 5).
//!
//! The canonical keys file is `<SICK_HOME>/keys.enc`: a JSON envelope whose
//! ciphertext is the same TOML text the plaintext keys file uses, encrypted
//! with ChaCha20-Poly1305. Two protection schemes exist:
//!
//! - `keychain` - a random 32-byte master key lives in the OS keychain
//!   (Windows Credential Manager, `secret-tool` / Secret Service on Linux,
//!   `security` on macOS). No password is needed at runtime.
//! - `password` - the master key is derived from a user password with
//!   PBKDF2-HMAC-SHA256 (random salt, [`PBKDF2_ITERATIONS`] iterations). Used
//!   when no OS keychain is available; the password comes from
//!   `SICK_KEY_PASSWORD` or an interactive prompt.
//!
//! Headless deployments (and tests) can opt into a file-backed keyring with
//! `SICK_KEYRING_FILE=<path>`: the master key is stored in that file instead
//! of the OS keychain. This is deliberate - it keeps servers scriptable while
//! keeping the default path (OS keychain) password-free.
//!
//! Recovery: every save first copies the previous encrypted file to
//! `<name>.enc.bak`; a load that cannot decrypt the primary file retries the
//! backup before giving up. Plaintext `keys.toml` / `keys.json` files are
//! migrated (renamed to `*.bak`) once their contents are stored encrypted.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::{KeyError, KeyStore};

/// Service name used for keychain entries.
pub const KEYCHAIN_SERVICE: &str = "sick";
/// Account name used for the master key entry.
pub const KEYCHAIN_ACCOUNT: &str = "keys-master";
/// PBKDF2 iteration count for password-protected files.
pub const PBKDF2_ITERATIONS: u32 = 210_000;
/// Current envelope version.
pub const ENVELOPE_VERSION: u32 = 1;

/// Env var selecting a file-backed keyring (headless deployments, tests).
pub const KEYRING_FILE_ENV: &str = "SICK_KEYRING_FILE";
/// Legacy-prefixed variant of [`KEYRING_FILE_ENV`].
pub const LEGACY_KEYRING_FILE_ENV: &str = "SHELL_AGENT_KEYRING_FILE";
/// Env var providing the master password non-interactively.
pub const KEY_PASSWORD_ENV: &str = "SICK_KEY_PASSWORD";
/// Legacy-prefixed variant of [`KEY_PASSWORD_ENV`].
pub const LEGACY_KEY_PASSWORD_ENV: &str = "SHELL_AGENT_KEY_PASSWORD";

/// The JSON envelope written to `keys.enc`.
#[derive(Debug, Serialize, Deserialize)]
struct Envelope {
    version: u32,
    /// `"keychain"` or `"password"`.
    scheme: String,
    /// Base64 12-byte nonce.
    nonce: String,
    /// Base64 ciphertext of the TOML keys document.
    ciphertext: String,
    /// Base64 PBKDF2 salt (password scheme only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    salt: Option<String>,
    /// PBKDF2 iterations (password scheme only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    iterations: Option<u32>,
}

/// How the master key is protected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MasterKeySource {
    /// OS keychain; the string names the backend ("Windows Credential Manager", ...).
    Keychain(String),
    /// File-backed keyring (`SICK_KEYRING_FILE`), for headless use.
    KeyringFile(PathBuf),
    /// Master password (PBKDF2).
    Password,
}

impl MasterKeySource {
    /// Human-readable description for CLI/TUI messages.
    pub fn describe(&self) -> String {
        match self {
            MasterKeySource::Keychain(backend) => format!("OS keychain ({backend})"),
            MasterKeySource::KeyringFile(path) => format!("keyring file {}", path.display()),
            MasterKeySource::Password => "master password (PBKDF2)".to_string(),
        }
    }
}

/// Where the master key is kept. Resolved from the environment by
/// [`KeychainBackend::from_env`]; tests use [`KeychainBackend::File`] directly.
#[derive(Debug, Clone)]
pub enum KeychainBackend {
    /// The OS keychain.
    Os,
    /// Store/read the master key in this file (hex-encoded).
    File(PathBuf),
    /// No keyring: only password-protected files can be used.
    None,
}

impl KeychainBackend {
    /// Resolve the backend from `SICK_KEYRING_FILE` (file) or the OS default.
    pub fn from_env() -> Self {
        if let Some(path) =
            env_value(KEYRING_FILE_ENV).or_else(|| env_value(LEGACY_KEYRING_FILE_ENV))
        {
            return KeychainBackend::File(PathBuf::from(path));
        }
        KeychainBackend::Os
    }

    /// Label for [`MasterKeySource`] reporting.
    fn source_label(&self) -> MasterKeySource {
        match self {
            KeychainBackend::Os => MasterKeySource::Keychain(os_keychain_name().to_string()),
            KeychainBackend::File(path) => MasterKeySource::KeyringFile(path.clone()),
            KeychainBackend::None => MasterKeySource::Password,
        }
    }

    /// Fetch the master key, if present.
    fn lookup(&self, account: &str) -> Result<Option<Zeroizing<Vec<u8>>>, KeyError> {
        match self {
            KeychainBackend::None => Ok(None),
            KeychainBackend::File(path) => {
                if !path.is_file() {
                    return Ok(None);
                }
                let raw = std::fs::read_to_string(path)?;
                let hex = raw.trim();
                if hex.is_empty() {
                    return Ok(None);
                }
                Ok(Some(Zeroizing::new(hex_decode(hex).map_err(|reason| {
                    KeyError::KeychainUnavailable {
                        reason: format!(
                            "keyring file {} is not valid hex: {reason}",
                            path.display()
                        ),
                    }
                })?)))
            }
            KeychainBackend::Os => os_keychain_lookup(KEYCHAIN_SERVICE, account),
        }
    }

    /// Store a newly created master key.
    fn store(&self, account: &str, secret: &[u8]) -> Result<(), KeyError> {
        match self {
            KeychainBackend::None => Err(KeyError::KeychainUnavailable {
                reason: "keyring disabled".to_string(),
            }),
            KeychainBackend::File(path) => {
                if let Some(parent) = path.parent()
                    && !parent.as_os_str().is_empty()
                {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(path, hex_encode(secret))?;
                restrict_permissions(path)?;
                Ok(())
            }
            KeychainBackend::Os => os_keychain_store(KEYCHAIN_SERVICE, account, secret),
        }
    }
}

/// Result of a successful encrypted save.
#[derive(Debug)]
pub struct SaveReport {
    /// The encrypted file that was written.
    pub path: PathBuf,
    /// How the master key is protected.
    pub source: MasterKeySource,
    /// Backup of the previous version of the file, when one existed.
    pub backup: Option<PathBuf>,
    /// Plaintext legacy files renamed to `*.bak` after migration.
    pub migrated: Vec<PathBuf>,
}

/// Result of [`add_key_secure`].
#[derive(Debug)]
pub struct AddKeyReport {
    /// Whether the key was actually new (false: identical key already stored).
    pub added: bool,
    /// Details of the encrypted save.
    pub save: SaveReport,
}

/// Result of [`remove_key_secure`].
#[derive(Debug)]
pub struct RemoveKeyReport {
    /// Number of saved keys left for this provider.
    pub remaining_for_provider: usize,
    /// Details of the encrypted save.
    pub save: SaveReport,
}

/// Default encrypted keys file: `<SICK_HOME>/keys.enc`.
pub fn default_encrypted_path() -> PathBuf {
    shell_common::sick_home().join("keys.enc")
}

/// Backup path for an encrypted keys file (`keys.enc` -> `keys.enc.bak`).
pub fn backup_path(path: &Path) -> PathBuf {
    let mut name = OsString::from(path.as_os_str());
    name.push(".bak");
    PathBuf::from(name)
}

/// Encrypt `toml_text` into `path`, provisioning a master key when needed.
///
/// Existing files keep their scheme: password-protected files require
/// `password` (or `SICK_KEY_PASSWORD`), keychain files use the keychain. New
/// files prefer the keychain and fall back to `password` when it is
/// unavailable and a password is provided. A pre-existing file is copied to
/// `<path>.bak` before being replaced.
pub fn save_encrypted(
    path: &Path,
    toml_text: &str,
    password: Option<&str>,
    backend: &KeychainBackend,
) -> Result<SaveReport, KeyError> {
    // Unreadable file: treat as absent and fall through to fresh
    // provisioning. The previous file is still backed up below before being
    // replaced.
    let existing = read_envelope(path).unwrap_or_default();

    let (json, source) = match &existing {
        Some(envelope) if envelope.scheme == "password" => {
            let password = resolve_password(password)?;
            let salt = match envelope.salt.as_deref() {
                Some(encoded) => decode_b64(encoded, path, "salt")?,
                None => random_bytes(16).to_vec(),
            };
            let iterations = envelope.iterations.unwrap_or(PBKDF2_ITERATIONS);
            let key = derive_password_key(password.as_str(), &salt, iterations);
            (
                seal(toml_text, &key, "password", Some(salt), Some(iterations))?,
                MasterKeySource::Password,
            )
        }
        _ => match keychain_or_new(backend, toml_text) {
            Ok(pair) => pair,
            Err(KeyError::KeychainUnavailable { reason }) => {
                // No OS keychain: fall back to a password-protected file when
                // a password is available, otherwise explain both options.
                match resolve_password(password) {
                    Ok(password) => {
                        tracing_warn(&reason);
                        let salt = random_bytes(16);
                        let key = derive_password_key(password.as_str(), &salt, PBKDF2_ITERATIONS);
                        (
                            seal(
                                toml_text,
                                &key,
                                "password",
                                Some(salt.to_vec()),
                                Some(PBKDF2_ITERATIONS),
                            )?,
                            MasterKeySource::Password,
                        )
                    }
                    Err(password_error @ KeyError::PasswordRequired) => {
                        return Err(KeyError::KeychainUnavailable {
                            reason: format!("{reason}; and no master password: {password_error}"),
                        });
                    }
                    Err(other) => return Err(other),
                }
            }
            Err(other) => return Err(other),
        },
    };

    // Back up the previous file before replacing it.
    let mut backup = None;
    if path.is_file() {
        let bak = backup_path(path);
        if bak.exists() {
            let _ = std::fs::remove_file(&bak);
        }
        std::fs::copy(path, &bak)?;
        backup = Some(bak);
    }

    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::File::create(path)?;
    file.write_all(json.as_bytes())?;
    file.write_all(b"\n")?;
    restrict_permissions(path)?;

    Ok(SaveReport {
        path: path.to_path_buf(),
        source,
        backup,
        migrated: Vec::new(),
    })
}

/// Decrypt `path`; returns `None` when the file does not exist.
///
/// `password` (or `SICK_KEY_PASSWORD`) is required for password-protected
/// files. Errors are [`KeyError::PasswordRequired`] or [`KeyError::Decrypt`].
pub fn load_encrypted(
    path: &Path,
    password: Option<&str>,
    backend: &KeychainBackend,
) -> Result<Option<(Zeroizing<String>, MasterKeySource)>, KeyError> {
    let Some(envelope) = read_envelope(path)? else {
        return Ok(None);
    };
    if envelope.version != ENVELOPE_VERSION {
        return Err(KeyError::Decrypt {
            path: path.to_path_buf(),
            reason: format!("unsupported envelope version {}", envelope.version),
        });
    }
    let nonce = decode_b64(&envelope.nonce, path, "nonce")?;
    let ciphertext = decode_b64(&envelope.ciphertext, path, "ciphertext")?;

    let (plaintext, source) = match envelope.scheme.as_str() {
        "keychain" => {
            let master = backend.lookup(KEYCHAIN_ACCOUNT)?.ok_or_else(|| {
                KeyError::Decrypt {
                    path: path.to_path_buf(),
                    reason: format!(
                        "master key not found in the keychain ({}); was it created on another machine?",
                        os_keychain_name()
                    ),
                }
            })?;
            (
                decrypt(&master, &nonce, &ciphertext, path)?,
                backend.source_label(),
            )
        }
        "password" => {
            let password = resolve_password(password)?;
            let salt = envelope
                .salt
                .as_deref()
                .ok_or_else(|| KeyError::Decrypt {
                    path: path.to_path_buf(),
                    reason: "password-protected file is missing its salt".to_string(),
                })
                .and_then(|encoded| decode_b64(encoded, path, "salt"))?;
            let iterations = envelope.iterations.unwrap_or(PBKDF2_ITERATIONS);
            let key = derive_password_key(password.as_str(), &salt, iterations);
            (
                decrypt(&key, &nonce, &ciphertext, path)?,
                MasterKeySource::Password,
            )
        }
        other => {
            return Err(KeyError::Decrypt {
                path: path.to_path_buf(),
                reason: format!("unknown protection scheme `{other}`"),
            });
        }
    };

    let text = String::from_utf8(plaintext.to_vec()).map_err(|_| KeyError::Decrypt {
        path: path.to_path_buf(),
        reason: "decrypted content is not valid UTF-8".to_string(),
    })?;
    Ok(Some((Zeroizing::new(text), source)))
}

/// Load `path`, falling back to `<path>.bak` when the primary file cannot be
/// decrypted. Returns `(toml_text, source, recovered_from_backup)`.
pub fn load_with_backup(
    path: &Path,
    password: Option<&str>,
    backend: &KeychainBackend,
) -> Result<Option<(Zeroizing<String>, MasterKeySource, bool)>, KeyError> {
    match load_encrypted(path, password, backend) {
        Ok(Some((text, source))) => Ok(Some((text, source, false))),
        Ok(None) => Ok(None),
        Err(primary_error) => {
            let backup = backup_path(path);
            if backup.is_file()
                && let Ok(Some((text, source))) = load_encrypted(&backup, password, backend)
            {
                return Ok(Some((text, source, true)));
            }
            Err(primary_error)
        }
    }
}

/// Load the default `keys.enc`, with backup recovery.
pub fn load_default(
    password: Option<&str>,
    backend: &KeychainBackend,
) -> Result<Option<(Zeroizing<String>, MasterKeySource, bool)>, KeyError> {
    load_with_backup(&default_encrypted_path(), password, backend)
}

/// Encrypt `store` into `encrypted_path`, then rename the given plaintext
/// legacy files to `*.bak` (their contents are already inside the encrypted
/// file). Used by [`save_store_encrypted`] and directly by tests.
pub fn save_store_encrypted_to(
    store: &KeyStore,
    encrypted_path: &Path,
    password: Option<&str>,
    backend: &KeychainBackend,
    legacy_paths: &[PathBuf],
) -> Result<SaveReport, KeyError> {
    let text = store.to_toml_text()?;
    let mut report = save_encrypted(encrypted_path, &text, password, backend)?;

    for legacy in legacy_paths {
        if !legacy.is_file() {
            continue;
        }
        let backup = backup_path(legacy);
        if backup.exists() {
            let _ = std::fs::remove_file(&backup);
        }
        std::fs::rename(legacy, &backup)?;
        report.migrated.push(backup);
    }
    Ok(report)
}

/// Save `store` as `<SICK_HOME>/keys.enc`, migrating plaintext legacy files.
pub fn save_store_encrypted(
    store: &KeyStore,
    password: Option<&str>,
) -> Result<SaveReport, KeyError> {
    let backend = KeychainBackend::from_env();
    save_store_encrypted_to(
        store,
        &default_encrypted_path(),
        password,
        &backend,
        &[
            KeyStore::default_keys_toml_path(),
            KeyStore::default_keys_path(),
        ],
    )
}

/// Load the default keys (encrypted first, then plaintext legacy), add one
/// key, and save everything back encrypted. This is the flow behind
/// `sick keys add` and the TUI key wizard.
pub fn add_key_secure(
    provider: &str,
    secret: &str,
    password: Option<&str>,
) -> Result<AddKeyReport, KeyError> {
    add_key_secure_with_base_url(provider, secret, None, password)
}

/// Load the default keys, add a key and optional provider endpoint, then save
/// both inside the encrypted store.
pub fn add_key_secure_with_base_url(
    provider: &str,
    secret: &str,
    base_url: Option<&str>,
    password: Option<&str>,
) -> Result<AddKeyReport, KeyError> {
    let backend = KeychainBackend::from_env();
    let mut store = KeyStore::new();
    if let Some((text, _, _)) = load_default(password, &backend)? {
        store.load_toml(&text)?;
    } else {
        store.load_legacy_default_files()?;
    }

    let added = store.add_key(provider, secret);
    if let Some(base_url) = base_url {
        store.set_provider_base_url(provider, base_url);
    }
    let save = save_store_encrypted(&store, password)?;
    Ok(AddKeyReport { added, save })
}

/// Persist provider endpoint settings in the encrypted key store.
pub fn set_provider_base_url_secure(
    provider: &str,
    base_url: &str,
    password: Option<&str>,
) -> Result<SaveReport, KeyError> {
    let backend = KeychainBackend::from_env();
    let encrypted_path = default_encrypted_path();
    let legacy_paths = [
        KeyStore::default_keys_toml_path(),
        KeyStore::default_keys_path(),
    ];
    set_provider_base_url_secure_to(
        provider,
        base_url,
        password,
        &encrypted_path,
        &backend,
        &legacy_paths,
    )
}

fn set_provider_base_url_secure_to(
    provider: &str,
    base_url: &str,
    password: Option<&str>,
    encrypted_path: &Path,
    backend: &KeychainBackend,
    legacy_paths: &[PathBuf],
) -> Result<SaveReport, KeyError> {
    let mut store = KeyStore::new();
    if let Some((text, _, _)) = load_with_backup(encrypted_path, password, backend)? {
        store.load_toml(&text)?;
    } else {
        for path in legacy_paths {
            if path.is_file() {
                store.load_from_file(path)?;
            }
        }
    }
    store.set_provider_base_url(provider, base_url);
    save_store_encrypted_to(&store, encrypted_path, password, backend, legacy_paths)
}

/// Load the default encrypted key store, remove one saved key, and persist it.
pub fn remove_key_secure(
    provider: &str,
    key_id: &str,
    password: Option<&str>,
) -> Result<RemoveKeyReport, KeyError> {
    let backend = KeychainBackend::from_env();
    let path = default_encrypted_path();
    let legacy_paths = [
        KeyStore::default_keys_toml_path(),
        KeyStore::default_keys_path(),
    ];
    remove_key_secure_from(provider, key_id, password, &path, &backend, &legacy_paths)
}

fn remove_key_secure_from(
    provider: &str,
    key_id: &str,
    password: Option<&str>,
    encrypted_path: &Path,
    backend: &KeychainBackend,
    legacy_paths: &[PathBuf],
) -> Result<RemoveKeyReport, KeyError> {
    let mut store = KeyStore::new();
    if let Some((text, _, _)) = load_with_backup(encrypted_path, password, backend)? {
        store.load_toml(&text)?;
    } else {
        for path in legacy_paths {
            if path.is_file() {
                store.load_from_file(path)?;
            }
        }
    }

    let provider = crate::normalize_provider(provider);
    if !store.remove_key(&provider, key_id) {
        let known_ids = store
            .keys_for(&provider)
            .iter()
            .map(|key| key.id.as_str())
            .collect::<Vec<_>>();
        let detail = if known_ids.is_empty() {
            format!("no saved keys for provider `{provider}`")
        } else {
            format!(
                "no saved key `{key_id}` for `{provider}`; saved ids: {}",
                known_ids.join(", ")
            )
        };
        return Err(KeyError::Invalid(detail));
    }

    let remaining_for_provider = store.key_count(&provider);
    let save = save_store_encrypted_to(&store, encrypted_path, password, backend, legacy_paths)?;
    Ok(RemoveKeyReport {
        remaining_for_provider,
        save,
    })
}

/// One master key (32 random bytes) fetched from the keychain, or created,
/// stored, and returned when absent.
fn keychain_or_new(
    backend: &KeychainBackend,
    toml_text: &str,
) -> Result<(String, MasterKeySource), KeyError> {
    let master = match backend.lookup(KEYCHAIN_ACCOUNT)? {
        Some(master) => master,
        None => {
            let master = Zeroizing::new(random_bytes(32).to_vec());
            backend.store(KEYCHAIN_ACCOUNT, &master)?;
            master
        }
    };
    Ok((
        seal(toml_text, &master, "keychain", None, None)?,
        backend.source_label(),
    ))
}

fn resolve_password(password: Option<&str>) -> Result<Zeroizing<String>, KeyError> {
    if let Some(password) = password {
        return Ok(Zeroizing::new(password.to_string()));
    }
    if let Some(env) = env_value(KEY_PASSWORD_ENV).or_else(|| env_value(LEGACY_KEY_PASSWORD_ENV)) {
        return Ok(Zeroizing::new(env));
    }
    Err(KeyError::PasswordRequired)
}

fn read_envelope(path: &Path) -> Result<Option<Envelope>, KeyError> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(KeyError::Io(error)),
    };
    if raw.trim().is_empty() {
        return Ok(None);
    }
    serde_json::from_str(&raw)
        .map(Some)
        .map_err(|error| KeyError::Decrypt {
            path: path.to_path_buf(),
            reason: format!("not a valid encrypted keys file: {error}"),
        })
}

fn seal(
    toml_text: &str,
    key: &[u8],
    scheme: &str,
    salt: Option<Vec<u8>>,
    iterations: Option<u32>,
) -> Result<String, KeyError> {
    let cipher = ChaCha20Poly1305::new_from_slice(key)
        .map_err(|error| KeyError::Invalid(format!("invalid master key: {error}")))?;
    let mut nonce_bytes = vec![0u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), toml_text.as_bytes())
        .map_err(|error| KeyError::Invalid(format!("encryption failed: {error}")))?;
    let envelope = Envelope {
        version: ENVELOPE_VERSION,
        scheme: scheme.to_string(),
        nonce: BASE64.encode(nonce_bytes),
        ciphertext: BASE64.encode(ciphertext),
        salt: salt.map(|salt| BASE64.encode(salt)),
        iterations,
    };
    serde_json::to_string_pretty(&envelope).map_err(KeyError::from)
}

fn decrypt(
    key: &[u8],
    nonce: &[u8],
    ciphertext: &[u8],
    path: &Path,
) -> Result<Zeroizing<Vec<u8>>, KeyError> {
    if nonce.len() != 12 {
        return Err(KeyError::Decrypt {
            path: path.to_path_buf(),
            reason: "nonce must be 12 bytes".to_string(),
        });
    }
    let cipher = ChaCha20Poly1305::new_from_slice(key)
        .map_err(|error| KeyError::Invalid(format!("invalid master key: {error}")))?;
    let plaintext = cipher
        .decrypt(Nonce::from_slice(nonce), ciphertext)
        .map_err(|_| KeyError::Decrypt {
            path: path.to_path_buf(),
            reason: "wrong password, or the file is corrupted".to_string(),
        })?;
    Ok(Zeroizing::new(plaintext))
}

fn decode_b64(encoded: &str, path: &Path, what: &str) -> Result<Vec<u8>, KeyError> {
    BASE64.decode(encoded).map_err(|error| KeyError::Decrypt {
        path: path.to_path_buf(),
        reason: format!("{what} is not valid base64: {error}"),
    })
}

/// PBKDF2-HMAC-SHA256 key derivation (RFC 8018), used for password-protected
/// files. Implemented directly on `hmac`/`sha2` to avoid another dependency.
fn derive_password_key(password: &str, salt: &[u8], iterations: u32) -> Zeroizing<Vec<u8>> {
    let mut key = Zeroizing::new(vec![0u8; 32]);
    pbkdf2_hmac_sha256(password.as_bytes(), salt, iterations.max(1), &mut key);
    key
}

fn pbkdf2_hmac_sha256(password: &[u8], salt: &[u8], iterations: u32, out: &mut [u8]) {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    let base =
        <Hmac<Sha256> as Mac>::new_from_slice(password).expect("HMAC accepts any key length");
    for (block, chunk) in out.chunks_mut(32).enumerate() {
        let mut mac = base.clone();
        mac.update(salt);
        mac.update(&(block as u32 + 1).to_be_bytes());
        let mut acc = mac.finalize().into_bytes();
        let mut previous = acc;
        for _ in 1..iterations {
            let mut mac = base.clone();
            mac.update(&previous);
            previous = mac.finalize().into_bytes();
            for (a, b) in acc.iter_mut().zip(previous.iter()) {
                *a ^= *b;
            }
        }
        chunk.copy_from_slice(&acc[..chunk.len()]);
    }
}

fn random_bytes(len: usize) -> Zeroizing<Vec<u8>> {
    let mut bytes = Zeroizing::new(vec![0u8; len]);
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bytes
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn hex_decode(text: &str) -> Result<Vec<u8>, String> {
    if !text.len().is_multiple_of(2) {
        return Err("odd number of hex digits".to_string());
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(text.len() / 2);
    for pair in bytes.chunks_exact(2) {
        let hi = (pair[0] as char).to_digit(16).ok_or("non-hex digit")?;
        let lo = (pair[1] as char).to_digit(16).ok_or("non-hex digit")?;
        out.push(((hi << 4) | lo) as u8);
    }
    Ok(out)
}

fn env_value(name: &str) -> Option<String> {
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => Some(value),
        _ => None,
    }
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) -> Result<(), KeyError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> Result<(), KeyError> {
    Ok(())
}

fn os_keychain_name() -> &'static str {
    if cfg!(windows) {
        "Windows Credential Manager"
    } else if cfg!(target_os = "macos") {
        "macOS Keychain"
    } else {
        "Secret Service (secret-tool)"
    }
}

fn tracing_warn(message: &str) {
    // The key manager does not depend on a logging facade; surface via stderr
    // at debug level so it never interferes with TUI rendering.
    if std::env::var_os("SICK_DEBUG_KEYS").is_some() {
        eprintln!("warning: {message}");
    }
}

// ---------------------------------------------------------------------------
// OS keychain backends
// ---------------------------------------------------------------------------

#[cfg(windows)]
fn os_keychain_lookup(
    service: &str,
    account: &str,
) -> Result<Option<Zeroizing<Vec<u8>>>, KeyError> {
    use windows_sys::Win32::Security::Credentials::{
        CRED_TYPE_GENERIC, CREDENTIALW, CredFree, CredReadW,
    };

    let target = wide(&format!("{service}:{account}"));
    let mut credential: *mut CREDENTIALW = std::ptr::null_mut();
    // SAFETY: `target` is a valid NUL-terminated wide string and `credential`
    // is a valid out-pointer; on success the credential must be freed once.
    let found = unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut credential) };
    if found == 0 || credential.is_null() {
        return Ok(None);
    }
    let blob = unsafe {
        let cred = &*credential;
        std::slice::from_raw_parts(cred.CredentialBlob, cred.CredentialBlobSize as usize).to_vec()
    };
    unsafe { CredFree(credential as *const core::ffi::c_void) };
    Ok(Some(Zeroizing::new(blob)))
}

#[cfg(windows)]
fn os_keychain_store(service: &str, account: &str, secret: &[u8]) -> Result<(), KeyError> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::Security::Credentials::{
        CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredWriteW,
    };

    let mut target = wide(&format!("{service}:{account}"));
    let mut user = wide(&format!("sick\\{account}"));
    let credential = CREDENTIALW {
        Flags: 0,
        Type: CRED_TYPE_GENERIC,
        TargetName: target.as_mut_ptr(),
        Comment: std::ptr::null_mut(),
        LastWritten: FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        },
        CredentialBlobSize: secret.len() as u32,
        CredentialBlob: secret.as_ptr().cast_mut(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        AttributeCount: 0,
        Attributes: std::ptr::null_mut(),
        TargetAlias: std::ptr::null_mut(),
        UserName: user.as_mut_ptr(),
    };
    // SAFETY: the credential struct points at live buffers for the call.
    let ok = unsafe { CredWriteW(&credential, 0) };
    if ok == 0 {
        return Err(KeyError::KeychainUnavailable {
            reason: format!("CredWrite failed ({})", std::io::Error::last_os_error()),
        });
    }
    Ok(())
}

#[cfg(windows)]
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(all(unix, not(target_os = "macos")))]
fn os_keychain_lookup(
    service: &str,
    account: &str,
) -> Result<Option<Zeroizing<Vec<u8>>>, KeyError> {
    use std::process::Command;

    let output = Command::new("secret-tool")
        .args(["lookup", "service", service, "account", account])
        .output();
    match output {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if text.is_empty() {
                return Ok(None);
            }
            match hex_decode(&text) {
                Ok(bytes) => Ok(Some(Zeroizing::new(bytes))),
                Err(reason) => Err(KeyError::KeychainUnavailable {
                    reason: format!("secret-tool returned malformed data: {reason}"),
                }),
            }
        }
        _ => Ok(None),
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn os_keychain_store(service: &str, account: &str, secret: &[u8]) -> Result<(), KeyError> {
    use std::process::{Command, Stdio};

    let mut child = Command::new("secret-tool")
        .args([
            "store",
            "--label",
            "SICK master key",
            "service",
            service,
            "account",
            account,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| KeyError::KeychainUnavailable {
            reason: format!("secret-tool is not available: {error}"),
        })?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(hex_encode(secret).as_bytes())
            .map_err(|error| KeyError::KeychainUnavailable {
                reason: format!("could not write to secret-tool: {error}"),
            })?;
    }
    let status = child.wait()?;
    if !status.success() {
        return Err(KeyError::KeychainUnavailable {
            reason: "secret-tool store failed (is a Secret Service provider running?)".to_string(),
        });
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn os_keychain_lookup(
    service: &str,
    account: &str,
) -> Result<Option<Zeroizing<Vec<u8>>>, KeyError> {
    use std::process::Command;

    let output = Command::new("security")
        .args(["find-generic-password", "-a", account, "-s", service, "-w"])
        .output();
    match output {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if text.is_empty() {
                return Ok(None);
            }
            match hex_decode(&text) {
                Ok(bytes) => Ok(Some(Zeroizing::new(bytes))),
                Err(reason) => Err(KeyError::KeychainUnavailable {
                    reason: format!("macOS keychain returned malformed data: {reason}"),
                }),
            }
        }
        _ => Ok(None),
    }
}

#[cfg(target_os = "macos")]
fn os_keychain_store(service: &str, account: &str, secret: &[u8]) -> Result<(), KeyError> {
    use std::process::{Command, Stdio};

    let status = Command::new("security")
        .args([
            "add-generic-password",
            "-a",
            account,
            "-s",
            service,
            "-w",
            &hex_encode(secret),
            "-U",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|error| KeyError::KeychainUnavailable {
            reason: format!("`security` is not available: {error}"),
        })?;
    if !status.success() {
        return Err(KeyError::KeychainUnavailable {
            reason: "`security add-generic-password` failed".to_string(),
        });
    }
    Ok(())
}

/// Stub for platforms without a keychain implementation.
#[cfg(not(any(windows, unix)))]
fn os_keychain_lookup(
    _service: &str,
    _account: &str,
) -> Result<Option<Zeroizing<Vec<u8>>>, KeyError> {
    Ok(None)
}

#[cfg(not(any(windows, unix)))]
fn os_keychain_store(_service: &str, _account: &str, _secret: &[u8]) -> Result<(), KeyError> {
    Err(KeyError::KeychainUnavailable {
        reason: "no keychain backend for this platform".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::KeyStore;

    fn backend(dir: &Path) -> KeychainBackend {
        KeychainBackend::File(dir.join("keyring"))
    }

    fn sample_store() -> KeyStore {
        let mut store = KeyStore::new();
        store.add_key("openai", "sk-secret-openai-123456");
        store.add_key("anthropic", "sk-ant-secret-987654");
        store
    }

    #[test]
    fn hex_round_trip_and_rejects_bad_input() {
        let bytes = [0u8, 1, 0xab, 0xff];
        let encoded = hex_encode(&bytes);
        assert_eq!(encoded, "0001abff");
        assert_eq!(hex_decode(&encoded).unwrap(), bytes);
        assert!(hex_decode("abc").is_err());
        assert!(hex_decode("zz").is_err());
    }

    #[test]
    fn pbkdf2_matches_known_vectors() {
        // RFC 8018 test vectors for PBKDF2-HMAC-SHA256.
        let mut out = [0u8; 32];
        pbkdf2_hmac_sha256(b"password", b"salt", 1, &mut out);
        assert_eq!(
            hex_encode(&out),
            "120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b"
        );
        pbkdf2_hmac_sha256(b"password", b"salt", 2, &mut out);
        assert_eq!(
            hex_encode(&out),
            "ae4d0c95af6b46d32d0adff928f06dd02a303f8ef3c251dfd6e2d85a95474c43"
        );
    }

    #[test]
    fn keyring_file_round_trip_hides_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys.enc");
        let text = sample_store().to_toml_text().unwrap();

        let report = save_encrypted(&path, &text, None, &backend(dir.path())).unwrap();
        assert!(matches!(report.source, MasterKeySource::KeyringFile(_)));
        assert!(report.backup.is_none());

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("sk-secret-openai"), "ciphertext leaked");
        assert!(!raw.contains("[openai]"), "plaintext leaked");
        assert!(raw.contains("\"scheme\": \"keychain\""));

        let (loaded, source) = load_encrypted(&path, None, &backend(dir.path()))
            .unwrap()
            .expect("file exists");
        assert_eq!(loaded.as_str(), text);
        assert!(matches!(source, MasterKeySource::KeyringFile(_)));

        // The keyring file holds the master key, not the API keys.
        let keyring = std::fs::read_to_string(dir.path().join("keyring")).unwrap();
        assert!(!keyring.contains("sk-secret-openai"));
    }

    #[test]
    fn password_mode_round_trip_and_wrong_password() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys.enc");
        let text = sample_store().to_toml_text().unwrap();

        let report = save_encrypted(
            &path,
            &text,
            Some("hunter2-correct"),
            &KeychainBackend::None,
        )
        .unwrap();
        assert_eq!(report.source, MasterKeySource::Password);

        let (loaded, _) = load_encrypted(&path, Some("hunter2-correct"), &KeychainBackend::None)
            .unwrap()
            .expect("file exists");
        assert_eq!(loaded.as_str(), text);

        let error = load_encrypted(&path, Some("wrong"), &KeychainBackend::None).unwrap_err();
        match error {
            KeyError::Decrypt { reason, .. } => assert!(reason.contains("wrong password")),
            other => panic!("expected Decrypt, got {other:?}"),
        }

        // Without any password the load asks for one.
        let error = load_encrypted(&path, None, &KeychainBackend::None).unwrap_err();
        assert!(matches!(error, KeyError::PasswordRequired));

        // Saving without a password over a password file is also refused.
        let error = save_encrypted(&path, &text, None, &KeychainBackend::None).unwrap_err();
        assert!(matches!(error, KeyError::PasswordRequired));
    }

    #[test]
    fn save_keeps_backup_and_load_recovers_from_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys.enc");
        let first = "good version";
        let second = "second version";

        save_encrypted(&path, first, None, &backend(dir.path())).unwrap();
        let report = save_encrypted(&path, second, None, &backend(dir.path())).unwrap();
        let backup = report.backup.expect("second save keeps a backup");
        assert!(backup.is_file());
        assert!(backup.to_string_lossy().ends_with("keys.enc.bak"));

        // Corrupt the primary file: the load falls back to the backup.
        std::fs::write(&path, "{ not json").unwrap();
        let (text, _, recovered) = load_with_backup(&path, None, &backend(dir.path()))
            .unwrap()
            .unwrap();
        assert!(recovered);
        assert_eq!(text.as_str(), first, "backup holds the previous version");

        // With no usable backup the error surfaces.
        std::fs::remove_file(&backup).unwrap();
        let error = load_with_backup(&path, None, &backend(dir.path())).unwrap_err();
        assert!(matches!(error, KeyError::Decrypt { .. }));
    }

    #[test]
    fn save_store_encrypted_migrates_plaintext_files() {
        let dir = tempfile::tempdir().unwrap();
        let encrypted = dir.path().join("keys.enc");
        let legacy_toml = dir.path().join("keys.toml");
        std::fs::write(&legacy_toml, "[openai]\nkeys = [\"sk-plain-legacy\"]\n").unwrap();

        let report = save_store_encrypted_to(
            &sample_store(),
            &encrypted,
            None,
            &backend(dir.path()),
            &[legacy_toml.clone(), dir.path().join("keys.json")],
        )
        .unwrap();

        assert_eq!(
            report.migrated,
            vec![legacy_toml.with_extension("toml.bak")]
        );
        assert!(!legacy_toml.exists(), "plaintext file moved away");
        assert!(legacy_toml.with_extension("toml.bak").is_file());
        assert!(encrypted.is_file());
    }

    #[test]
    fn keychain_unavailable_without_password_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys.enc");
        let error = save_encrypted(&path, "x", None, &KeychainBackend::None).unwrap_err();
        match error {
            KeyError::KeychainUnavailable { reason } => {
                assert!(reason.contains("no master password"), "{reason}");
            }
            other => panic!("expected KeychainUnavailable, got {other:?}"),
        }
    }

    #[test]
    fn add_key_secure_round_trips_through_default_paths() {
        // Uses explicit paths instead of the default (env-dependent) ones: the
        // store round trip is identical.
        let dir = tempfile::tempdir().unwrap();
        let encrypted = dir.path().join("keys.enc");
        let backend = backend(dir.path());

        let mut store = KeyStore::new();
        store.add_key("openai", "sk-first");
        save_store_encrypted_to(&store, &encrypted, None, &backend, &[]).unwrap();

        // Load, add a second key, save again - both survive.
        let (text, _, _) = load_with_backup(&encrypted, None, &backend)
            .unwrap()
            .unwrap();
        let mut store = KeyStore::new();
        store.load_toml(&text).unwrap();
        assert!(store.add_key("openai", "sk-second"));
        assert!(!store.add_key("openai", "sk-first"), "duplicate rejected");
        save_store_encrypted_to(&store, &encrypted, None, &backend, &[]).unwrap();

        let (text, _, _) = load_with_backup(&encrypted, None, &backend)
            .unwrap()
            .unwrap();
        assert!(text.contains("sk-first"));
        assert!(text.contains("sk-second"));
    }

    #[test]
    fn remove_key_secure_updates_encrypted_store() {
        let dir = tempfile::tempdir().unwrap();
        let encrypted = dir.path().join("keys.enc");
        let backend = backend(dir.path());
        let mut store = KeyStore::new();
        store.add_key("openai", "sk-remove-me");
        store.add_key("openai", "sk-keep-me");
        store.add_key("google", "google-key");
        save_store_encrypted_to(&store, &encrypted, None, &backend, &[]).unwrap();

        let report =
            remove_key_secure_from("openai", "sk-remov", None, &encrypted, &backend, &[]).unwrap();
        assert_eq!(report.remaining_for_provider, 1);

        let (text, _, _) = load_with_backup(&encrypted, None, &backend)
            .unwrap()
            .unwrap();
        assert!(!text.contains("sk-remove-me"));
        assert!(text.contains("sk-keep-me"));
        assert!(text.contains("google-key"));
    }

    #[test]
    fn secure_provider_endpoint_update_preserves_saved_keys() {
        let dir = tempfile::tempdir().unwrap();
        let encrypted = dir.path().join("keys.enc");
        let backend = backend(dir.path());
        save_store_encrypted_to(&sample_store(), &encrypted, None, &backend, &[]).unwrap();

        let base_url =
            "https://api.cloudflare.com/client/v4/accounts/0123456789abcdef0123456789abcdef/ai/v1";
        set_provider_base_url_secure_to("cloudflare", base_url, None, &encrypted, &backend, &[])
            .unwrap();

        let (text, _, _) = load_with_backup(&encrypted, None, &backend)
            .unwrap()
            .unwrap();
        let mut loaded = KeyStore::new();
        loaded.load_toml(&text).unwrap();
        assert_eq!(
            loaded.provider_base_url("cloudflare").as_deref(),
            Some(base_url)
        );
        assert_eq!(loaded.key_count("openai"), 1);
        assert_eq!(loaded.key_count("anthropic"), 1);
    }
}
