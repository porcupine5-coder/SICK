//! `write` tool: write text to a file.

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tool::{Tool, ToolContext, ToolResult, required_str};

/// Writes (and overwrites) text files, creating parent directories.
pub struct WriteTool;

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &str {
        "write"
    }

    fn description(&self) -> &str {
        "Write text to a file, creating parent directories as needed. \
         Overwrites the file if it already exists."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file, absolute or relative to the working directory."
                },
                "content": {
                    "type": "string",
                    "description": "Full text content to write."
                }
            },
            "required": ["path", "content"]
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let path = match required_str(&args, "path") {
            Ok(p) => p,
            Err(e) => return ToolResult::err(e),
        };
        let content = match args.get("content").and_then(Value::as_str) {
            Some(c) => c,
            None => return ToolResult::err("missing required string argument `content`"),
        };

        let full_path = ctx.resolve(&path);

        if let Some(parent) = full_path.parent()
            && !parent.as_os_str().is_empty()
            && let Err(e) = tokio::fs::create_dir_all(parent).await
        {
            return ToolResult::err(format!(
                "failed to create parent directories for `{path}`: {e}"
            ));
        }

        if let Err(e) = tokio::fs::write(&full_path, content).await {
            return ToolResult::err(format!("failed to write `{path}`: {e}"));
        }

        let lines = content.lines().count();
        ToolResult::ok(format!(
            "Wrote {} bytes ({lines} lines) to {path}",
            content.len()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::ToolContext;
    use serde_json::json;

    #[tokio::test]
    async fn writes_file_and_creates_parents() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(dir.path());
        let result = WriteTool
            .execute(
                json!({"path": "nested/hello.txt", "content": "Hello, World!"}),
                &ctx,
            )
            .await;
        assert!(result.success, "{}", result.output);
        let written = std::fs::read_to_string(dir.path().join("nested/hello.txt")).unwrap();
        assert_eq!(written, "Hello, World!");
    }

    #[tokio::test]
    async fn overwrites_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "old").unwrap();
        let ctx = ToolContext::new(dir.path());
        let result = WriteTool
            .execute(json!({"path": "a.txt", "content": "new"}), &ctx)
            .await;
        assert!(result.success);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "new"
        );
    }

    #[tokio::test]
    async fn missing_content_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(dir.path());
        let result = WriteTool.execute(json!({"path": "a.txt"}), &ctx).await;
        assert!(!result.success);
        assert!(result.output.contains("content"));
    }
}
