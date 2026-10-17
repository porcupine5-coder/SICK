//! Workspace-level end-to-end tests.
//!
//! NOTE: The workspace root is a *virtual* workspace (no root package), so Cargo
//! does not compile this file as a test target yet. The live end-to-end smoke
//! tests for the CLI binary live in `crates/agent-cli/tests/e2e_basic.rs`, where
//! Cargo can wire them to the `shell-agent` binary.
//!
//! This file is the reserved home for workspace-wide E2E tests once/if the root
//! gains a package.

/// Sanity check for the shipped example config.
#[test]
fn example_config_contains_llm_section() {
    let config =
        std::fs::read_to_string("config/example-config.toml").expect("example config exists");
    assert!(config.contains("[llm]"), "config was: {config}");
}
