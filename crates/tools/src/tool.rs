//! The `Tool` trait and shared execution types.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::Value;

/// Execution context handed to every tool invocation.
#[derive(Debug, Clone)]
pub struct ToolContext {
    /// Directory that relative paths and shell commands resolve against.
    pub working_dir: PathBuf,
}

impl ToolContext {
    /// Create a context rooted at `working_dir`.
    pub fn new(working_dir: impl Into<PathBuf>) -> Self {
        Self {
            working_dir: working_dir.into(),
        }
    }

    /// Resolve a user/model supplied path against the working directory.
    pub fn resolve(&self, path: &str) -> PathBuf {
        let path = Path::new(path);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.working_dir.join(path)
        }
    }
}

/// Result of a tool execution.
#[derive(Debug, Clone)]
pub struct ToolResult {
    /// Whether the tool completed successfully.
    pub success: bool,
    /// Human/model readable output (error text when `success` is false).
    pub output: String,
    /// Process exit code, when the tool ran a process.
    pub exit_code: Option<i32>,
}

impl ToolResult {
    /// Successful result.
    pub fn ok(output: impl Into<String>) -> Self {
        Self {
            success: true,
            output: output.into(),
            exit_code: None,
        }
    }

    /// Failed result; `output` carries the error message.
    pub fn err(output: impl Into<String>) -> Self {
        Self {
            success: false,
            output: output.into(),
            exit_code: None,
        }
    }

    /// Attach a process exit code.
    pub fn with_exit_code(mut self, code: Option<i32>) -> Self {
        self.exit_code = code;
        self
    }
}

/// A built-in or dynamically registered tool.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Unique tool name exposed to the model.
    fn name(&self) -> &str;

    /// Description shown to the model.
    fn description(&self) -> &str;

    /// JSON Schema describing the tool parameters.
    fn parameters_schema(&self) -> Value;

    /// Execute the tool with parsed JSON arguments.
    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult;
}

/// Helper: fetch a required string argument.
pub(crate) fn required_str(args: &Value, field: &str) -> Result<String, String> {
    args.get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("missing required string argument `{field}`"))
}

/// Helper: fetch an optional string argument.
pub(crate) fn optional_str(args: &Value, field: &str) -> Option<String> {
    args.get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|s| !s.trim().is_empty())
}

/// Helper: fetch an optional usize argument.
pub(crate) fn optional_usize(args: &Value, field: &str) -> Option<usize> {
    args.get(field).and_then(Value::as_u64).map(|v| v as usize)
}
