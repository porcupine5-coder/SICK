//! Core agent loop for S.I.C.K.
//!
//! Multi-provider with transparent key rotation and a cross-provider fallback
//! chain (Phase 2). Phase 7 adds plan mode, skills, and `task` subagents.

pub mod agent;
pub mod skills;
pub(crate) mod subagent;

pub use agent::{
    Agent, AgentConfig, AgentEvent, AgentMode, AgentRun, AgentTurn, KeyValidation, ModelLookup,
    PLAN_MODE_TOOLS, ProviderModels, default_model_for, fetch_provider_models, resolve_provider,
    validate_api_key, validation_model,
};
pub use shell_llm::cloudflare_base_url;
pub use skills::Skill;
