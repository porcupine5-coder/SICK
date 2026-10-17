//! Integration tests: load the example extension library and exercise it.

use std::path::{Path, PathBuf};

use shell_extensions::{ExtensionContext, ExtensionHost};
use shell_llm::ChatMessage;
use shell_tools::ToolContext;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Locate the example-extension library, building it on demand.
///
/// `cargo test --workspace` builds it as a workspace member; a standalone
/// `cargo test -p shell-extensions` run triggers a nested `cargo build`.
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

/// Copy the fixture into an isolated extensions directory and load it.
fn load_fixture() -> Option<(ExtensionHost, tempfile::TempDir, PathBuf)> {
    let library = fixture_library()?;
    let dir = tempfile::tempdir().ok()?;
    let file_name = library.file_name()?;
    std::fs::copy(&library, dir.path().join(file_name)).ok()?;
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).ok()?;
    let ctx = ExtensionContext {
        workspace: workspace.clone(),
        state_dir: dir.path().to_path_buf(),
        extension_dir: dir.path().to_path_buf(),
    };
    let host = ExtensionHost::load_dir(dir.path(), &ctx);
    Some((host, dir, workspace))
}

#[test]
fn loads_the_example_extension_and_calls_its_tools() {
    let Some((host, _dir, workspace)) = load_fixture() else {
        eprintln!("skipping: example extension library was not built");
        return;
    };

    assert!(host.errors().is_empty(), "{:?}", host.errors());
    assert_eq!(host.len(), 1);
    assert_eq!(
        host.info(),
        vec![("example".to_string(), "0.1.0".to_string())]
    );
    // No provider override unless enabled via env var.
    assert!(host.provider().is_none());

    let tools = host.tools();
    let names: Vec<&str> = tools.iter().map(|tool| tool.name()).collect();
    for expected in ["example_echo", "example_message_count", "example_context"] {
        assert!(names.contains(&expected), "missing {expected} in {names:?}");
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let ctx = ToolContext::new(&workspace);

        let echo = tools
            .iter()
            .find(|tool| tool.name() == "example_echo")
            .unwrap();
        let result = echo.execute(serde_json::json!({"text": "hi"}), &ctx).await;
        assert!(result.success, "{}", result.output);
        assert_eq!(result.output, "example echo: hi");

        // `init` handed the extension the workspace directory.
        let context = tools
            .iter()
            .find(|tool| tool.name() == "example_context")
            .unwrap();
        let result = context.execute(serde_json::json!({}), &ctx).await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains(&workspace.display().to_string()),
            "{}",
            result.output
        );

        // `on_message` reaches the loaded extension (shared counter).
        host.on_message(&ChatMessage::new("user", "one"));
        host.on_message(&ChatMessage::new("assistant", "two"));
        let counter = tools
            .iter()
            .find(|tool| tool.name() == "example_message_count")
            .unwrap();
        let result = counter.execute(serde_json::json!({}), &ctx).await;
        assert_eq!(result.output, "2");
    });
}

#[test]
fn garbage_library_files_are_reported_not_fatal() {
    let dir = tempfile::tempdir().unwrap();
    let suffix = if cfg!(windows) {
        "dll"
    } else if cfg!(target_os = "macos") {
        "dylib"
    } else {
        "so"
    };
    std::fs::write(
        dir.path().join(format!("garbage.{suffix}")),
        b"this is not a dynamic library",
    )
    .unwrap();
    // A non-library file must be ignored entirely.
    std::fs::write(dir.path().join("readme.txt"), b"hello").unwrap();

    let ctx = ExtensionContext {
        workspace: dir.path().to_path_buf(),
        state_dir: dir.path().to_path_buf(),
        extension_dir: dir.path().to_path_buf(),
    };
    let host = ExtensionHost::load_dir(dir.path(), &ctx);
    assert!(host.is_empty());
    assert_eq!(host.errors().len(), 1, "{:?}", host.errors());
    assert!(host.tools().is_empty());
}
