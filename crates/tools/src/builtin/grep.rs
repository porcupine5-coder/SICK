//! `grep` tool: regex search across files.

use async_trait::async_trait;
use regex::Regex;
use serde_json::{Value, json};

use crate::tool::{Tool, ToolContext, ToolResult, optional_str, optional_usize, required_str};

/// Default maximum number of matches returned.
const DEFAULT_MAX_RESULTS: usize = 200;

/// Maximum characters of combined matches.
const MAX_OUTPUT_CHARS: usize = 40_000;

/// Recursively searches file contents with a regular expression.
pub struct GrepTool;

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &str {
        "Search file contents with a regular expression. Respects .gitignore \
         and skips hidden files. Returns `path:line: text` matches."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Regular expression to search for."
                },
                "path": {
                    "type": "string",
                    "description": "Directory (or file) to search, relative to the working directory (default '.')."
                },
                "include": {
                    "type": "string",
                    "description": "Only search files whose name matches this glob, e.g. '*.rs'."
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of matches to return (default 200)."
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
        let include = optional_str(&args, "include");
        let max_results = optional_usize(&args, "max_results").unwrap_or(DEFAULT_MAX_RESULTS);

        let regex = match Regex::new(&pattern) {
            Ok(re) => re,
            Err(e) => return ToolResult::err(format!("invalid regex `{pattern}`: {e}")),
        };

        let include_matcher = match &include {
            Some(glob) => match globset::Glob::new(glob) {
                Ok(g) => Some(g.compile_matcher()),
                Err(e) => return ToolResult::err(format!("invalid include glob `{glob}`: {e}")),
            },
            None => None,
        };

        let root = ctx.resolve(&rel_path);
        let base = if root.is_file() {
            root.parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| root.clone())
        } else {
            root.clone()
        };

        let walker = ignore::WalkBuilder::new(&root).build();
        let mut matches: Vec<String> = Vec::new();
        let mut truncated = false;
        let mut scanned_chars = 0usize;

        'outer: for entry in walker.filter_map(|e| e.ok()) {
            let path = entry.path();
            if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                continue;
            }
            if let Some(matcher) = &include_matcher {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                if !matcher.is_match(name.as_ref()) {
                    continue;
                }
            }

            let bytes = match std::fs::read(path) {
                Ok(b) => b,
                Err(_) => continue,
            };
            let text = String::from_utf8_lossy(&bytes);
            let display = display_path(&base, path);

            for (idx, line) in text.lines().enumerate() {
                if regex.is_match(line) {
                    let rendered = format!("{}:{}: {}\n", display, idx + 1, line.trim_end());
                    scanned_chars += rendered.len();
                    matches.push(rendered);
                    if matches.len() >= max_results || scanned_chars >= MAX_OUTPUT_CHARS {
                        truncated = true;
                        break 'outer;
                    }
                }
            }
        }

        if matches.is_empty() {
            return ToolResult::ok("(no matches)");
        }

        let mut out: String = matches.concat();
        if truncated {
            out.push_str("... [more matches not shown]\n");
        } else {
            out.push_str(&format!("({} match(es))\n", matches.len()));
        }

        ToolResult::ok(out)
    }
}

fn display_path(base: &std::path::Path, path: &std::path::Path) -> String {
    let relative = path.strip_prefix(base).unwrap_or(path);
    relative.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::ToolContext;
    use serde_json::json;

    #[tokio::test]
    async fn finds_matches_with_line_numbers() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "first\nneedle here\nlast\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "nothing\n").unwrap();

        let ctx = ToolContext::new(dir.path());
        let result = GrepTool.execute(json!({"pattern": "needle"}), &ctx).await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("a.txt:2: needle here"),
            "{}",
            result.output
        );
        assert!(!result.output.contains("b.txt"));
    }

    #[tokio::test]
    async fn include_glob_filters_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "let x = 1;\n").unwrap();
        std::fs::write(dir.path().join("a.txt"), "let x = 1;\n").unwrap();

        let ctx = ToolContext::new(dir.path());
        let result = GrepTool
            .execute(json!({"pattern": "let x", "include": "*.rs"}), &ctx)
            .await;
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("a.rs"));
        assert!(!result.output.contains("a.txt"));
    }

    #[tokio::test]
    async fn invalid_regex_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(dir.path());
        let result = GrepTool.execute(json!({"pattern": "("}), &ctx).await;
        assert!(!result.success);
        assert!(result.output.contains("invalid regex"));
    }

    #[tokio::test]
    async fn no_matches_reports_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello\n").unwrap();
        let ctx = ToolContext::new(dir.path());
        let result = GrepTool.execute(json!({"pattern": "zzz"}), &ctx).await;
        assert!(result.success);
        assert_eq!(result.output, "(no matches)");
    }
}
