//! `edit` tool: exact find-and-replace inside a file.

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tool::{Tool, ToolContext, ToolResult, required_str};

/// Replaces `old_string` with `new_string` in a file.
pub struct EditTool;

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }

    fn description(&self) -> &str {
        "Replace `old_string` with `new_string` in a file. `old_string` must \
         identify a unique piece of text unless `replace_all` is set to true."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file, absolute or relative to the working directory."
                },
                "old_string": {
                    "type": "string",
                    "description": "Exact text to find."
                },
                "new_string": {
                    "type": "string",
                    "description": "Replacement text."
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "Replace every occurrence instead of requiring a unique match (default false)."
                }
            },
            "required": ["path", "old_string", "new_string"]
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let path = match required_str(&args, "path") {
            Ok(p) => p,
            Err(e) => return ToolResult::err(e),
        };
        let old_string = match required_str(&args, "old_string") {
            Ok(s) => s,
            Err(e) => return ToolResult::err(e),
        };
        let new_string = match args.get("new_string").and_then(Value::as_str) {
            Some(s) => s.to_string(),
            None => return ToolResult::err("missing required string argument `new_string`"),
        };
        let replace_all = args
            .get("replace_all")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        if old_string.is_empty() {
            return ToolResult::err("`old_string` must not be empty");
        }

        let full_path = ctx.resolve(&path);
        let content = match tokio::fs::read_to_string(&full_path).await {
            Ok(c) => c,
            Err(e) => return ToolResult::err(format!("failed to read `{path}`: {e}")),
        };

        let occurrences = content.matches(&old_string).count();
        if occurrences == 0 {
            return ToolResult::err(format!("`old_string` not found in {path}"));
        }
        if occurrences > 1 && !replace_all {
            return ToolResult::err(format!(
                "`old_string` occurs {occurrences} times in {path}; provide a larger unique \
                 snippet or set `replace_all` to true"
            ));
        }

        let updated = if replace_all {
            content.replace(&old_string, &new_string)
        } else {
            content.replacen(&old_string, &new_string, 1)
        };

        if let Err(e) = tokio::fs::write(&full_path, updated).await {
            return ToolResult::err(format!("failed to write `{path}`: {e}"));
        }

        ToolResult::ok(format!("Replaced {occurrences} occurrence(s) in {path}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::ToolContext;
    use serde_json::json;

    fn setup(content: &str) -> (tempfile::TempDir, ToolContext) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), content).unwrap();
        let ctx = ToolContext::new(dir.path());
        (dir, ctx)
    }

    #[tokio::test]
    async fn replaces_unique_match() {
        let (dir, ctx) = setup("hello world\n");
        let result = EditTool
            .execute(
                json!({"path": "a.txt", "old_string": "world", "new_string": "rust"}),
                &ctx,
            )
            .await;
        assert!(result.success, "{}", result.output);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "hello rust\n"
        );
    }

    #[tokio::test]
    async fn refuses_ambiguous_match_without_replace_all() {
        let (dir, ctx) = setup("x x\n");
        let result = EditTool
            .execute(
                json!({"path": "a.txt", "old_string": "x", "new_string": "y"}),
                &ctx,
            )
            .await;
        assert!(!result.success);
        assert!(result.output.contains("occurs 2 times"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "x x\n"
        );
    }

    #[tokio::test]
    async fn replace_all_replaces_every_occurrence() {
        let (dir, ctx) = setup("x x\n");
        let result = EditTool
            .execute(
                json!({"path": "a.txt", "old_string": "x", "new_string": "y", "replace_all": true}),
                &ctx,
            )
            .await;
        assert!(result.success, "{}", result.output);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "y y\n"
        );
    }

    #[tokio::test]
    async fn missing_text_is_an_error() {
        let (_dir, ctx) = setup("hello\n");
        let result = EditTool
            .execute(
                json!({"path": "a.txt", "old_string": "nope", "new_string": "y"}),
                &ctx,
            )
            .await;
        assert!(!result.success);
        assert!(result.output.contains("not found"));
    }
}
