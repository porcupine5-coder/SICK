//! Tool registry: holds all tools and exposes their definitions.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;

use crate::builtin::{BashTool, EditTool, GlobTool, GrepTool, LsTool, ReadTool, WriteTool};
use crate::tool::Tool;

/// A tool definition ready to be converted into an LLM provider definition.
#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema for the tool parameters.
    pub parameters: Value,
}

/// Holds every registered tool, keyed by name.
#[derive(Default, Clone)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
    order: Vec<String>,
}

impl ToolRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a registry pre-populated with all built-in tools.
    pub fn with_builtins() -> Self {
        Self::with_builtins_sandboxed(shell_config::SandboxConfig::default())
    }

    /// Like [`ToolRegistry::with_builtins`], but runs the `bash` tool under
    /// the given `[sandbox]` settings (Phase 6).
    pub fn with_builtins_sandboxed(sandbox: shell_config::SandboxConfig) -> Self {
        let mut registry = Self::new();
        registry.register(ReadTool);
        registry.register(WriteTool);
        registry.register(EditTool);
        registry.register(BashTool::new(sandbox));
        registry.register(LsTool);
        registry.register(GrepTool);
        registry.register(GlobTool);
        registry
    }

    /// Register a tool (replacing any existing tool with the same name).
    pub fn register<T: Tool + 'static>(&mut self, tool: T) {
        self.register_arc(Arc::new(tool));
    }

    /// Register a shared tool instance.
    pub fn register_arc(&mut self, tool: Arc<dyn Tool>) {
        let name = tool.name().to_string();
        if !self.tools.contains_key(&name) {
            self.order.push(name.clone());
        }
        self.tools.insert(name, tool);
    }

    /// Look a tool up by name.
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    /// Whether a tool with this name is registered.
    pub fn contains(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    /// Tool names in registration order.
    pub fn names(&self) -> Vec<String> {
        self.order.clone()
    }

    /// Number of registered tools.
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// Whether no tools are registered.
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// Definitions for every registered tool, in registration order.
    pub fn specs(&self) -> Vec<ToolSpec> {
        self.order
            .iter()
            .filter_map(|name| self.tools.get(name))
            .map(|tool| ToolSpec {
                name: tool.name().to_string(),
                description: tool.description().to_string(),
                parameters: tool.parameters_schema(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_contains_all_seven_tools() {
        let registry = ToolRegistry::with_builtins();
        assert_eq!(registry.len(), 7);
        let names = registry.names();
        for expected in ["read", "write", "edit", "bash", "ls", "grep", "glob"] {
            assert!(names.contains(&expected.to_string()), "missing {expected}");
            assert!(registry.contains(expected));
        }
    }

    #[test]
    fn specs_expose_name_description_and_schema() {
        let registry = ToolRegistry::with_builtins();
        let specs = registry.specs();
        assert_eq!(specs.len(), 7);

        let write = specs
            .iter()
            .find(|s| s.name == "write")
            .expect("write spec");
        assert!(!write.description.is_empty());
        assert_eq!(write.parameters["type"], "object");
        assert_eq!(write.parameters["required"][0], "path");
    }

    #[test]
    fn get_returns_none_for_unknown_tool() {
        let registry = ToolRegistry::with_builtins();
        assert!(registry.get("nope").is_none());
        assert!(registry.get("read").is_some());
    }
}
