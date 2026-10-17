//! Built-in tools (shell, file, git, web) for SICK.
//!
//! Phase 1 ships the seven file/execution tools: `read`, `write`, `edit`,
//! `bash`, `ls`, `grep`, `glob`, plus a [`ToolRegistry`].

pub mod builtin;
pub mod registry;
pub mod tool;

pub use registry::{ToolRegistry, ToolSpec};
pub use tool::{Tool, ToolContext, ToolResult};
