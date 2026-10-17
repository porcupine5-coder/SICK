//! Session and semantic memory persistence for SICK.
//!
//! Phase 1: SQLite-backed session/message/tool-call storage.

pub mod embed;
pub mod store;

pub use embed::{EMBEDDING_DIM, cosine, embed, from_bytes, to_bytes};
pub use store::{
    MemoryEntry, MemoryError, MemoryStore, Message, PromptRecord, Result, SCHEMA, SessionMeta,
    ToolCallRecord,
};
