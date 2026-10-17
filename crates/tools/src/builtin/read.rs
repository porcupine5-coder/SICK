//! `read` tool: read a file with line numbers.

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tool::{Tool, ToolContext, ToolResult, optional_usize, required_str};

/// Default maximum number of lines returned.
const DEFAULT_LIMIT: usize = 2000;

/// Reads text files with line numbers, supporting offset/limit windows.
pub struct ReadTool;

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }

    fn description(&self) -> &str {
        "Read a file from the filesystem. Output is prefixed with line numbers. \
         Use `offset` (1-based) and `limit` to read a specific window."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file, absolute or relative to the working directory."
                },
                "offset": {
                    "type": "integer",
                    "description": "1-based line to start reading from (default 1)."
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of lines to read (default 2000)."
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let path = match required_str(&args, "path") {
            Ok(p) => p,
            Err(e) => return ToolResult::err(e),
        };
        let offset = optional_usize(&args, "offset").unwrap_or(1).max(1);
        let limit = optional_usize(&args, "limit")
            .unwrap_or(DEFAULT_LIMIT)
            .max(1);

        let full_path = ctx.resolve(&path);
        let bytes = match tokio::fs::read(&full_path).await {
            Ok(b) => b,
            Err(e) => return ToolResult::err(format!("failed to read `{path}`: {e}")),
        };
        let text = String::from_utf8_lossy(&bytes);

        let total_lines = text.lines().count();
        let mut out = String::new();
        for (idx, line) in text.lines().enumerate().skip(offset - 1).take(limit) {
            out.push_str(&format!("{:>6}\t{}\n", idx + 1, line));
        }

        if out.is_empty() && total_lines == 0 {
            return ToolResult::ok("(empty file)");
        }
        if out.is_empty() {
            return ToolResult::ok(format!(
                "(no lines in window offset={offset} limit={limit}; file has {total_lines} lines)"
            ));
        }

        let shown_end = (offset - 1 + limit).min(total_lines);
        if shown_end < total_lines {
            out.push_str(&format!(
                "... ({} of {} lines shown; use offset/limit to read more)\n",
                shown_end, total_lines
            ));
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
    async fn reads_file_with_line_numbers() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "alpha\nbeta\ngamma\n").unwrap();
        let ctx = ToolContext::new(dir.path());
        let result = ReadTool.execute(json!({"path": "a.txt"}), &ctx).await;
        assert!(result.success);
        assert!(result.output.contains("     1\talpha"));
        assert!(result.output.contains("     3\tgamma"));
    }

    #[tokio::test]
    async fn supports_offset_and_limit() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "1\n2\n3\n4\n5\n").unwrap();
        let ctx = ToolContext::new(dir.path());
        let result = ReadTool
            .execute(json!({"path": "a.txt", "offset": 2, "limit": 2}), &ctx)
            .await;
        assert!(result.success);
        assert!(result.output.contains("     2\t2"));
        assert!(result.output.contains("     3\t3"));
        assert!(!result.output.contains("\t4"));
        assert!(result.output.contains("3 of 5 lines shown"));
    }

    #[tokio::test]
    async fn missing_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(dir.path());
        let result = ReadTool.execute(json!({"path": "nope.txt"}), &ctx).await;
        assert!(!result.success);
        assert!(result.output.contains("nope.txt"));
    }

    #[tokio::test]
    async fn missing_argument_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(dir.path());
        let result = ReadTool.execute(json!({}), &ctx).await;
        assert!(!result.success);
        assert!(result.output.contains("path"));
    }
}
