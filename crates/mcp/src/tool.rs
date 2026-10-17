//! `Tool` adapter: MCP server tools look like built-in tools to the agent.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use shell_tools::{Tool, ToolContext, ToolResult};

use crate::manager::{McpManager, McpToolInfo};

/// One tool offered by an MCP server, exposed as a regular agent tool.
///
/// Calls go back through the owning [`McpManager`] to the server's
/// `tools/call`; `isError` results arrive as failed [`ToolResult`]s.
pub struct McpTool {
    manager: Arc<McpManager>,
    exposed_name: String,
    server: String,
    tool: String,
    description: String,
    schema: Value,
}

impl McpTool {
    /// Wrap discovered tool metadata into a callable tool.
    pub fn new(manager: Arc<McpManager>, info: McpToolInfo) -> Self {
        let description = if info.description.trim().is_empty() {
            format!("MCP tool `{}` on server `{}`", info.tool, info.server)
        } else {
            format!("[MCP: {}] {}", info.server, info.description)
        };
        Self {
            manager,
            exposed_name: info.exposed_name,
            server: info.server,
            tool: info.tool,
            description,
            schema: info.schema,
        }
    }
}

impl McpManager {
    /// Every discovered tool, ready to register with a
    /// [`shell_tools::ToolRegistry`] (config order; empty when no server is
    /// connected).
    pub fn tools(self: &Arc<Self>) -> Vec<Arc<dyn Tool>> {
        self.tool_infos()
            .into_iter()
            .map(|info| Arc::new(McpTool::new(Arc::clone(self), info)) as Arc<dyn Tool>)
            .collect()
    }
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.exposed_name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Value {
        self.schema.clone()
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        match self.manager.call_tool(&self.server, &self.tool, args).await {
            Ok(result) => {
                let text = content_to_text(result.get("content"));
                let is_error = result
                    .get("isError")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if is_error {
                    ToolResult::err(text)
                } else {
                    ToolResult::ok(text)
                }
            }
            Err(error) => ToolResult::err(format!("MCP error: {error}")),
        }
    }
}

/// Flatten an MCP result `content` array into plain text for the model.
pub fn content_to_text(content: Option<&Value>) -> String {
    let mut parts = Vec::new();
    if let Some(items) = content.and_then(Value::as_array) {
        for item in items {
            match item.get("type").and_then(Value::as_str) {
                Some("text") => parts.push(
                    item.get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                ),
                Some(other) => parts.push(format!("[{other} content]")),
                None => parts.push(item.to_string()),
            }
        }
    }
    if parts.is_empty() {
        "(no output)".to_string()
    } else {
        parts.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn flattens_text_content() {
        let content = json!([
            {"type": "text", "text": "first"},
            {"type": "text", "text": "second"},
        ]);
        assert_eq!(content_to_text(Some(&content)), "first\nsecond");
    }

    #[test]
    fn renders_non_text_content_placeholders() {
        let content = json!([{"type": "image", "data": "..."}]);
        assert_eq!(content_to_text(Some(&content)), "[image content]");
    }

    #[test]
    fn empty_content_is_placeholder() {
        assert_eq!(content_to_text(None), "(no output)");
        assert_eq!(content_to_text(Some(&json!([]))), "(no output)");
    }
}
