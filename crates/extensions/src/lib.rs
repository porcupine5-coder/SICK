//! Extensions system for SICK (Phase 6).
//!
//! Extensions are dynamic libraries (`.dll` / `.so` / `.dylib`) that plug
//! custom tools and — optionally — a custom LLM provider into the agent.
//! They are loaded from `[extensions] dir` (default `<SICK_HOME>/extensions`,
//! `~` expands to the home directory) at CLI/TUI startup.
//!
//! Writing an extension:
//!
//! ```ignore
//! use std::sync::Arc;
//! use shell_extensions::{export_extension, Extension, ExtensionContext, ExtensionResult};
//! use shell_llm::{ChatMessage, LlmProvider};
//! use shell_tools::Tool;
//!
//! struct MyExtension;
//!
//! impl Extension for MyExtension {
//!     fn name(&self) -> &str { "my-extension" }
//!     fn version(&self) -> &str { "0.1.0" }
//!     fn init(&mut self, _ctx: &ExtensionContext) -> ExtensionResult<()> { Ok(()) }
//!     fn on_message(&self, _msg: &ChatMessage) -> ExtensionResult<()> { Ok(()) }
//!     fn provide_tools(&self) -> Vec<Arc<dyn Tool>> { Vec::new() }
//!     fn provide_provider(&self) -> Option<Arc<dyn LlmProvider>> { None }
//! }
//!
//! export_extension!(MyExtension);
//! ```
//!
//! Build with `crate-type = ["cdylib"]` and drop the library into the
//! extensions directory. A complete, tested example lives in
//! `crates/example-extension/`.
//!
//! # ABI notes
//!
//! The host and the extension exchange Rust trait objects, so both sides must
//! be built with the same Rust toolchain, target, and compatible versions of
//! the `shell-*` crates (the same rule as `libloading`'s own plugin example).
//! Extensions are trusted code: a loaded library runs with full process
//! privileges.
//!
//! Loading never fails the agent: a library that cannot be loaded (or whose
//! `init` fails) is reported in [`ExtensionHost::errors`] and skipped.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use shell_llm::{ChatMessage, LlmProvider};
use shell_tools::Tool;

/// Extension API revision implemented by this crate.
pub const EXTENSION_API_VERSION: u32 = 1;

/// Name of the exported factory symbol expected in every extension library.
pub const EXTENSION_ENTRY_SYMBOL: &str = "sick_extension_v1";

/// Error type handed back by extension hooks.
pub type ExtensionError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Result alias for extension hooks.
pub type ExtensionResult<T> = Result<T, ExtensionError>;

/// Context passed to [`Extension::init`] once after loading.
#[derive(Debug, Clone)]
pub struct ExtensionContext {
    /// Directory the agent treats as the working directory.
    pub workspace: PathBuf,
    /// SICK state directory (`<SICK_HOME>`).
    pub state_dir: PathBuf,
    /// Directory the extension was loaded from.
    pub extension_dir: PathBuf,
}

/// A plugin loaded from a dynamic library.
pub trait Extension: Send + Sync {
    /// Stable extension name (shown in warnings and status output).
    fn name(&self) -> &str;

    /// Extension version string.
    fn version(&self) -> &str;

    /// Called once after loading, before any tool or message hook runs.
    fn init(&mut self, ctx: &ExtensionContext) -> ExtensionResult<()>;

    /// Called for every message appended to the conversation.
    fn on_message(&self, msg: &ChatMessage) -> ExtensionResult<()>;

    /// Tools contributed to the agent. Registering a tool whose name matches
    /// a built-in replaces the built-in for this session.
    fn provide_tools(&self) -> Vec<Arc<dyn Tool>>;

    /// Optional custom LLM provider. When any extension provides one, the
    /// agent uses it instead of resolving the configured provider (API keys
    /// are then optional).
    fn provide_provider(&self) -> Option<Arc<dyn LlmProvider>>;

    /// ABI revision the library was built against (do not override).
    fn api_version(&self) -> u32 {
        EXTENSION_API_VERSION
    }
}

/// Factory signature exported as [`EXTENSION_ENTRY_SYMBOL`].
///
/// The extension boxes the value and transfers ownership to the host, which
/// reconstructs the `Box<dyn Extension>` and drops it before unloading the
/// library.
///
/// The `allow` silences `improper_ctypes_definitions`: a trait object is not
/// C-ABI-safe, which is exactly why host and extension must use the same Rust
/// toolchain (see the crate docs).
#[allow(improper_ctypes_definitions)]
pub type ExtensionCreateFn = unsafe extern "C" fn() -> *mut dyn Extension;

/// Export the entry point for an extension type.
///
/// ```ignore
/// export_extension!(MyExtension::new());
/// ```
#[macro_export]
macro_rules! export_extension {
    ($construct:expr) => {
        /// Extension factory called by the SICK host after loading the library.
        #[allow(improper_ctypes_definitions)]
        #[unsafe(no_mangle)]
        pub extern "C" fn sick_extension_v1() -> *mut dyn $crate::Extension {
            ::std::boxed::Box::into_raw(::std::boxed::Box::new($construct))
                as *mut dyn $crate::Extension
        }
    };
}

/// One successfully loaded extension library.
///
/// Must stay alive while the extension is used: dropping the handle unloads
/// the code. The [`Extension`] box itself is released before the library, so
/// destruction is safe.
pub struct LoadedExtension {
    /// Dropped before `library` (field order matters: the code must stay
    /// mapped while the trait object is alive).
    extension: Box<dyn Extension>,
    #[allow(dead_code)] // kept alive for the extension's lifetime
    library: libloading::Library,
    path: PathBuf,
}

impl LoadedExtension {
    /// The extension trait object.
    pub fn extension(&self) -> &dyn Extension {
        &*self.extension
    }

    /// Path of the library this extension was loaded from.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Holds every loaded extension for a session.
#[derive(Default)]
pub struct ExtensionHost {
    extensions: Vec<LoadedExtension>,
    errors: Vec<(PathBuf, String)>,
}

impl ExtensionHost {
    /// An empty host (no extensions directory or nothing to load).
    pub fn empty() -> Self {
        Self::default()
    }

    /// Scan `dir` for extension libraries and load them all.
    ///
    /// Never fails: unreadable directories and individual load failures are
    /// recorded in [`ExtensionHost::errors`] and reported via `tracing`.
    pub fn load_dir(dir: &Path, ctx: &ExtensionContext) -> Self {
        let mut host = Self::default();
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) => {
                if error.kind() != std::io::ErrorKind::NotFound {
                    host.errors
                        .push((dir.to_path_buf(), format!("cannot read directory: {error}")));
                }
                return host;
            }
        };

        let mut paths: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_file() && is_library_file(path))
            .collect();
        paths.sort();
        for path in paths {
            match load_library(&path, ctx) {
                Ok(loaded) => {
                    tracing::info!(
                        extension = %loaded.extension().name(),
                        version = %loaded.extension().version(),
                        "extension loaded"
                    );
                    host.extensions.push(loaded);
                }
                Err(error) => {
                    tracing::warn!(path = %path.display(), "extension not loaded: {error}");
                    host.errors.push((path, error));
                }
            }
        }
        host
    }

    /// Whether no extensions are loaded.
    pub fn is_empty(&self) -> bool {
        self.extensions.is_empty()
    }

    /// Number of loaded extensions.
    pub fn len(&self) -> usize {
        self.extensions.len()
    }

    /// `(name, version)` for every loaded extension, in load order.
    pub fn info(&self) -> Vec<(String, String)> {
        self.extensions
            .iter()
            .map(|loaded| {
                (
                    loaded.extension().name().to_string(),
                    loaded.extension().version().to_string(),
                )
            })
            .collect()
    }

    /// Load failures: `(library path, reason)`.
    pub fn errors(&self) -> &[(PathBuf, String)] {
        &self.errors
    }

    /// Tools provided by every loaded extension.
    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.extensions
            .iter()
            .flat_map(|loaded| loaded.extension().provide_tools())
            .collect()
    }

    /// The first provider offered by any extension, if any.
    pub fn provider(&self) -> Option<Arc<dyn LlmProvider>> {
        self.extensions
            .iter()
            .find_map(|loaded| loaded.extension().provide_provider())
    }

    /// Deliver a message to every extension, logging hook failures.
    pub fn on_message(&self, message: &ChatMessage) {
        for loaded in &self.extensions {
            if let Err(error) = loaded.extension().on_message(message) {
                tracing::warn!(
                    extension = %loaded.extension().name(),
                    path = %loaded.path().display(),
                    "extension on_message failed: {error}"
                );
            }
        }
    }
}

/// Load one extension library and run its `init` hook.
///
/// The returned handle keeps the library mapped; drop it to unload.
pub fn load_library(path: &Path, ctx: &ExtensionContext) -> Result<LoadedExtension, String> {
    // SAFETY: loading a dynamic library runs arbitrary code; extensions are
    // trusted files from the user's extensions directory by design.
    unsafe {
        let library = libloading::Library::new(path)
            .map_err(|error| format!("cannot load library: {error}"))?;
        let raw = {
            let create: libloading::Symbol<ExtensionCreateFn> = library
                .get(format!("{EXTENSION_ENTRY_SYMBOL}\0").as_bytes())
                .map_err(|error| format!("missing `{EXTENSION_ENTRY_SYMBOL}` export: {error}"))?;
            create()
        };
        if raw.is_null() {
            return Err(format!(
                "`{EXTENSION_ENTRY_SYMBOL}` returned a null pointer"
            ));
        }
        let mut extension = Box::from_raw(raw);
        if extension.api_version() != EXTENSION_API_VERSION {
            return Err(format!(
                "extension API version {} is not supported (host expects {EXTENSION_API_VERSION})",
                extension.api_version()
            ));
        }
        extension
            .init(ctx)
            .map_err(|error| format!("init failed: {error}"))?;
        Ok(LoadedExtension {
            extension,
            library,
            path: path.to_path_buf(),
        })
    }
}

/// Resolve the extensions directory: `dir` from `[extensions]` (with `~`
/// expansion), or `<SICK_HOME>/extensions` when empty.
pub fn resolve_extensions_dir(dir: &str) -> PathBuf {
    let dir = dir.trim();
    if dir.is_empty() {
        return shell_common::sick_home().join("extensions");
    }
    if let Some(rest) = dir.strip_prefix('~') {
        let rest = rest.trim_start_matches(['/', '\\']);
        if let Some(home) = dirs::home_dir() {
            return if rest.is_empty() {
                home
            } else {
                home.join(rest)
            };
        }
    }
    PathBuf::from(dir)
}

/// Whether a file looks like a loadable library on this platform.
fn is_library_file(path: &Path) -> bool {
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match std::env::consts::OS {
        "windows" => extension == "dll",
        "macos" => extension == "dylib",
        _ => extension == "so",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_file_filter_matches_platform() {
        let name = if cfg!(windows) {
            "plugin.dll"
        } else if cfg!(target_os = "macos") {
            "plugin.dylib"
        } else {
            "plugin.so"
        };
        assert!(is_library_file(Path::new(name)));
        assert!(!is_library_file(Path::new("plugin.txt")));
        assert!(!is_library_file(Path::new("plugin")));
    }

    #[test]
    fn missing_directory_is_an_empty_host() {
        let dir = std::env::temp_dir().join("sick-no-such-extensions-dir-xyz");
        let host = ExtensionHost::load_dir(
            &dir,
            &ExtensionContext {
                workspace: PathBuf::from("."),
                state_dir: PathBuf::from("."),
                extension_dir: dir.clone(),
            },
        );
        assert!(host.is_empty());
        assert!(host.errors().is_empty(), "{:?}", host.errors());
        assert!(host.tools().is_empty());
        assert!(host.provider().is_none());
    }

    #[test]
    fn resolve_defaults_to_sick_home() {
        let resolved = resolve_extensions_dir("");
        assert!(resolved.ends_with("extensions"));
        assert!(resolved.starts_with(shell_common::sick_home()));
    }

    #[test]
    fn resolve_expands_tilde() {
        let home = dirs::home_dir().expect("home dir");
        assert_eq!(resolve_extensions_dir("~"), home);
        assert_eq!(
            resolve_extensions_dir("~/.sick/extensions"),
            home.join(".sick").join("extensions")
        );
        assert_eq!(
            resolve_extensions_dir("/absolute/place"),
            PathBuf::from("/absolute/place")
        );
    }
}
