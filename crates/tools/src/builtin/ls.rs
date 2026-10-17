//! `ls` tool: list directory contents.

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tool::{Tool, ToolContext, ToolResult, optional_str};

/// Maximum entries returned.
const MAX_ENTRIES: usize = 1000;

/// Lists a directory, marking subdirectories with a trailing `/`.
pub struct LsTool;

#[async_trait]
impl Tool for LsTool {
    fn name(&self) -> &str {
        "ls"
    }

    fn description(&self) -> &str {
        "List the contents of a directory (directories are marked with a \
         trailing `/`). Defaults to the working directory."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Directory to list, absolute or relative to the working directory (default '.')."
                }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let path = optional_str(&args, "path").unwrap_or_else(|| ".".to_string());
        let full_path = ctx.resolve(&path);

        let mut dir = match tokio::fs::read_dir(&full_path).await {
            Ok(dir) => dir,
            Err(e) => return ToolResult::err(format!("failed to list `{path}`: {e}")),
        };

        let mut entries: Vec<(String, bool)> = Vec::new();
        let mut truncated = false;
        loop {
            match dir.next_entry().await {
                Ok(Some(entry)) => {
                    if entries.len() >= MAX_ENTRIES {
                        truncated = true;
                        break;
                    }
                    let is_dir = entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
                    entries.push((entry.file_name().to_string_lossy().to_string(), is_dir));
                }
                Ok(None) => break,
                Err(e) => return ToolResult::err(format!("failed to read `{path}`: {e}")),
            }
        }

        entries.sort_by_key(|a| a.0.to_lowercase());

        if entries.is_empty() {
            return ToolResult::ok("(empty directory)");
        }

        let mut out = String::new();
        for (name, is_dir) in &entries {
            if *is_dir {
                out.push_str(&format!("{name}/\n"));
            } else {
                out.push_str(&format!("{name}\n"));
            }
        }
        if truncated {
            out.push_str("... [entry list truncated]\n");
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
    async fn lists_entries_sorted_with_dir_markers() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("b.txt"), "b").unwrap();
        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();

        let ctx = ToolContext::new(dir.path());
        let result = LsTool.execute(json!({}), &ctx).await;
        assert!(result.success, "{}", result.output);
        let lines: Vec<&str> = result.output.lines().collect();
        assert_eq!(lines, vec!["a.txt", "b.txt", "sub/"]);
    }

    #[tokio::test]
    async fn missing_directory_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(dir.path());
        let result = LsTool.execute(json!({"path": "missing"}), &ctx).await;
        assert!(!result.success);
    }
}
