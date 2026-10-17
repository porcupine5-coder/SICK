-- Phase 1 schema: sessions, messages, tool_calls.
-- Mirrors the schema in the plan (section 8); semantic-memory and catalog
-- tables arrive in later phases.

PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY,          -- UUID
    title TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    model TEXT,
    provider TEXT,
    workspace TEXT,
    token_budget_prompt INTEGER DEFAULT 0,
    token_budget_candidate INTEGER DEFAULT 0,
    system_prompt TEXT
);

-- Full conversation history
CREATE TABLE IF NOT EXISTS messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    turn_idx INTEGER NOT NULL,
    role TEXT NOT NULL CHECK (role IN ('user', 'assistant', 'system', 'tool')),
    content TEXT,
    tool_calls_json TEXT,         -- JSON array of tool calls
    tool_result TEXT,             -- JSON array of tool results
    token_count INTEGER,
    timestamp TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id);

-- Action history
CREATE TABLE IF NOT EXISTS tool_calls (
    id TEXT PRIMARY KEY,          -- tool call ID from the LLM
    session_id TEXT NOT NULL,
    turn_idx INTEGER,
    tool_name TEXT NOT NULL,
    args_json TEXT,
    result TEXT,
    duration_ms INTEGER,
    exit_code INTEGER,
    timestamp TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_tool_calls_session ON tool_calls(session_id);

-- Prompt history (Phase 4): every user prompt, for `/history` and `sick search`.
CREATE TABLE IF NOT EXISTS prompts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT,
    content TEXT NOT NULL,
    model TEXT,
    timestamp TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_prompts_session ON prompts(session_id);
CREATE INDEX IF NOT EXISTS idx_prompts_timestamp ON prompts(timestamp);

-- Semantic memory (Phase 4): compact facts distilled from completed turns,
-- with a local embedding blob for similarity search (see embed.rs).
CREATE TABLE IF NOT EXISTS memories (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    content TEXT NOT NULL,
    embedding BLOB,               -- little-endian f32 array (embed.rs to_bytes)
    source_session TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Identical memories are stored once (INSERT OR IGNORE dedupe).
CREATE UNIQUE INDEX IF NOT EXISTS idx_memories_content ON memories(content);
CREATE INDEX IF NOT EXISTS idx_memories_created ON memories(created_at);
