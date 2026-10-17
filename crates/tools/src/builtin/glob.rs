//! `glob` tool: find files by glob pattern.

use async_trait::async_trait;
use globset::GlobBuilder;
use serde_json::{Value, json};

use crate::tool::{Tool, ToolContext, ToolResult, optional_str, required_str};

/// Maximum number of matched paths returned.
const MAX_PATHS: usize = 1000;

/// Finds files whose paths match a glob pattern (e.g. `**/*.rs`).
pub struct GlobTool;

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }

    fn description(&self) -> &str {
        "Find files matching a glob pattern such as `**/*.rs` or `src/**/*.toml`. \
         Respects .gitignore and skips hidden files."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern to match against relative paths."
                },
                "path": {
                    "type": "string",
                    "description": "Base directory to search, relative to the working directory (default '.')."
                }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let pattern = match required_str(&args, "pattern") {
            Ok(p) => p,
            Err(e) => return ToolResult::err(e),
        };
        let rel_path = optional_str(&args, "path").unwrap_or_else(|| ".".to_string());

        let matcher = match GlobBuilder::new(&pattern).build() {
            Ok(glob) => glob.compile_matcher(),
            Err(e) => return ToolResult::err(format!("invalid glob `{pattern}`: {e}")),
        };

        let root = ctx.resolve(&rel_path);
        if !root.exists() {
            return ToolResult::err(format!("path `{rel_path}` does not exist"));
        }

        let walker = ignore::WalkBuilder::new(&root).build();
        let mut matches: Vec<String> = Vec::new();
        let mut truncated = false;

        for entry in walker.filter_map(|e| e.ok()) {
            if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                continue;
            }
            let path = entry.path();
            let relative = path.strip_prefix(&root).unwrap_or(path);
            let display = relative.to_string_lossy().replace('\\', "/");

            if matcher.is_match(&display) {
                matches.push(display);
                if matches.len() >= MAX_PATHS {
                    truncated = true;
                    break;
                }
            }
        }

        if matches.is_empty() {
            return ToolResult::ok("(no files matched)");
        }

        matches.sort();
        let mut out = matches.join("\n");
        out.push('\n');
        if truncated {
            out.push_str("... [match list truncated]\n");
        } else {
            out.push_str(&format!("({} file(s))\n", matches.len()));
        }

        ToolResult::ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::ToolContext;
    use serde_json::json;

    #[tokio::test]
    async fn matches_nested_pattern() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/deep")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "").unwrap();
        std::fs::write(dir.path().join("src/deep/lib.rs"), "").unwrap();
        std::fs::write(dir.path().join("README.md"), "").unwrap();

        let ctx = ToolContext::new(dir.path());
        let result = GlobTool.execute(json!({"pattern": "**/*.rs"}), &ctx).await;
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("src/main.rs"), "{}", result.output);
        assert!(
            result.output.contains("src/deep/lib.rs"),
            "{}",
            result.output
        );
        assert!(result.output.contains("2 file(s)"), "{}", result.output);
        assert!(!result.output.contains("README.md"));
    }

    #[tokio::test]
    async fn no_matches_reports_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(dir.path());
        let result = GlobTool.execute(json!({"pattern": "*.zzz"}), &ctx).await;
        assert!(result.success);
        assert_eq!(result.output, "(no files matched)");
    }

    #[tokio::test]
    async fn missing_base_path_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(dir.path());
        let result = GlobTool
            .execute(json!({"pattern": "*.rs", "path": "no-such-dir"}), &ctx)
            .await;
        assert!(!result.success);
    }
}
