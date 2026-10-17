//! Provider-override path of the extensions system.
//!
//! This file is a dedicated test binary with a single test, so setting
//! `EXAMPLE_EXTENSION_PROVIDER` in the process environment (read by the
//! fixture library at construction) cannot race parallel tests.

use std::path::{Path, PathBuf};

use shell_extensions::{ExtensionContext, ExtensionHost};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn fixture_library() -> Option<PathBuf> {
    let target = workspace_root().join("target");
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let suffix = if cfg!(windows) {
        "dll"
    } else if cfg!(target_os = "macos") {
        "dylib"
    } else {
        "so"
    };
    let candidates: Vec<PathBuf> = ["example_extension", "libexample_extension"]
        .into_iter()
        .flat_map(|stem| {
            [
                target.join(profile).join(format!("{stem}.{suffix}")),
                target
                    .join(profile)
                    .join("deps")
                    .join(format!("{stem}.{suffix}")),
            ]
        })
        .collect();
    if let Some(found) = candidates.iter().find(|candidate| candidate.is_file()) {
        return Some(found.clone());
    }
    let status = std::process::Command::new(env!("CARGO"))
        .args(["build", "-p", "shell-extension-example"])
        .current_dir(workspace_root())
        .status();
    if !matches!(status, Ok(status) if status.success()) {
        return None;
    }
    candidates.into_iter().find(|candidate| candidate.is_file())
}

#[test]
fn extension_can_provide_the_llm_provider() {
    // Safety: single-test binary; nothing else reads the environment here.
    unsafe { std::env::set_var("EXAMPLE_EXTENSION_PROVIDER", "1") };

    let Some(library) = fixture_library() else {
        eprintln!("skipping: example extension library was not built");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let file_name = library.file_name().unwrap();
    std::fs::copy(&library, dir.path().join(file_name)).unwrap();

    let ctx = ExtensionContext {
        workspace: dir.path().to_path_buf(),
        state_dir: dir.path().to_path_buf(),
        extension_dir: dir.path().to_path_buf(),
    };
    let host = ExtensionHost::load_dir(dir.path(), &ctx);
    assert!(host.errors().is_empty(), "{:?}", host.errors());

    let provider = host.provider().expect("extension-provided provider");
    assert_eq!(provider.id(), "example-extension");
}
