//! Terminal UI (Phase 3): ratatui + crossterm interactive mode.
//!
//! [`App`] owns the UI state and talks to the agent through a background
//! actor task, rendering streamed tokens and tool activity as they arrive.

pub mod app;
pub mod layout;
pub mod state;
pub mod theme;

pub use app::{App, TuiTerminal};
pub use state::{
    COMMANDS, ColorWheelState, InputMode, JsonImportState, KeySetup, ModelOption, ModelsPicker,
    SETUP_PROVIDERS, SetupStage, SkillOption, SkillsPicker, ThemePickerItem, ThemePickerMode,
    ThemesPicker, TuiState, UiMessage,
};
pub use theme::{Theme, ThemeStyles};
