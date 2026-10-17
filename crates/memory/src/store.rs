//! SQLite-backed session store.

use std::path::{Path, PathBuf};

use chrono::Utc;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

/// Phase 1 schema, embedded at compile time.
pub const SCHEMA: &str = include_str!("schema.sql");

/// Errors surfaced by the memory store.
#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("session `{0}` not found")]
    SessionNotFound(String),
}

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, MemoryError>;

/// A message persisted in the `messages` table.
#[derive(Debug, Clone, Default)]
pub struct Message {
    /// `user`, `assistant`, `system`, or `tool`.
    pub role: String,
    pub content: Option<String>,
    /// JSON array of tool calls (for assistant messages).
    pub tool_calls_json: Option<String>,
    /// JSON tool result payload (for tool messages).
    pub tool_result: Option<String>,
    pub token_count: Option<i64>,
    /// Conversation turn this message belongs to.
    pub turn_idx: i64,
}

/// A record persisted in the `tool_calls` table.
#[derive(Debug, Clone, Default)]
pub struct ToolCallRecord {
    /// Tool call id assigned by the LLM.
    pub id: String,
    pub session_id: String,
    pub turn_idx: Option<i64>,
    pub tool_name: String,
    pub args_json: Option<String>,
    pub result: Option<String>,
    pub duration_ms: Option<i64>,
    pub exit_code: Option<i32>,
}

/// A stored user prompt (Phase 4 prompt history).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptRecord {
    pub id: i64,
    pub session_id: Option<String>,
    pub content: String,
    pub model: Option<String>,
    pub timestamp: String,
}

/// A stored semantic memory (Phase 4).
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryEntry {
    pub id: i64,
    pub content: String,
    pub source_session: Option<String>,
    pub created_at: String,
    /// Cosine similarity to the search query (set by [`MemoryStore::search_memories`]).
    pub score: Option<f32>,
}

/// Metadata for a stored session.
#[derive(Debug, Clone)]
pub struct SessionMeta {
    pub id: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub workspace: Option<String>,
}

/// SQLite-backed session/message store.
#[derive(Clone)]
pub struct MemoryStore {
    db: SqlitePool,
}

impl MemoryStore {
    /// Open (creating if needed) the SQLite database at `db_path` and apply
    /// the schema.
    pub async fn init(db_path: &str) -> Result<Self> {
        if let Some(parent) = Path::new(db_path).parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }

        let options = SqliteConnectOptions::new()
            .filename(db_path)
            .create_if_missing(true)
            .foreign_keys(true)
            // WAL + synchronous=NORMAL avoids an fsync per message write
            // (~13 ms on Windows with the default FULL): the session log is a
            // local, regenerable artifact, and power loss can lose the last
            // few transactions but never corrupt the database.
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal);

        let db = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await?;

        sqlx::raw_sql(SCHEMA).execute(&db).await?;

        Ok(Self { db })
    }

    /// Default database location: `<SICK_HOME>/sessions.db`
    /// (defaults to `~/.sick/sessions.db`).
    pub fn default_db_path() -> PathBuf {
        shell_common::sick_home().join("sessions.db")
    }

    /// Insert a new session row, returning its generated UUID.
    pub async fn create_session(
        &self,
        title: &str,
        model: &str,
        provider: &str,
        workspace: &str,
    ) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        sqlx::query(
            "INSERT INTO sessions (id, title, created_at, updated_at, model, provider, workspace)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(title)
        .bind(&now)
        .bind(&now)
        .bind(model)
        .bind(provider)
        .bind(workspace)
        .execute(&self.db)
        .await?;

        Ok(id)
    }

    /// Append a message to a session's history.
    pub async fn append_message(&self, session_id: &str, msg: &Message) -> Result<()> {
        sqlx::query(
            "INSERT INTO messages
                (session_id, turn_idx, role, content, tool_calls_json, tool_result, token_count)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(session_id)
        .bind(msg.turn_idx)
        .bind(&msg.role)
        .bind(&msg.content)
        .bind(&msg.tool_calls_json)
        .bind(&msg.tool_result)
        .bind(msg.token_count)
        .execute(&self.db)
        .await?;

        self.touch_session(session_id).await?;
        Ok(())
    }

    /// Append a tool-call record to the action history.
    pub async fn append_tool_call(&self, call: &ToolCallRecord) -> Result<()> {
        sqlx::query(
            "INSERT OR REPLACE INTO tool_calls
                (id, session_id, turn_idx, tool_name, args_json, result, duration_ms, exit_code)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&call.id)
        .bind(&call.session_id)
        .bind(call.turn_idx)
        .bind(&call.tool_name)
        .bind(&call.args_json)
        .bind(&call.result)
        .bind(call.duration_ms)
        .bind(call.exit_code)
        .execute(&self.db)
        .await?;

        Ok(())
    }

    /// Load the full ordered history of a session.
    pub async fn load_session_history(&self, session_id: &str) -> Result<Vec<Message>> {
        let rows = sqlx::query(
            "SELECT role, content, tool_calls_json, tool_result, token_count, turn_idx
             FROM messages WHERE session_id = ? ORDER BY id",
        )
        .bind(session_id)
        .fetch_all(&self.db)
        .await?;

        Ok(rows
            .into_iter()
            .map(|row| Message {
                role: row.get("role"),
                content: row.get("content"),
                tool_calls_json: row.get("tool_calls_json"),
                tool_result: row.get("tool_result"),
                token_count: row.get("token_count"),
                turn_idx: row.get("turn_idx"),
            })
            .collect())
    }

    /// List all sessions, newest first.
    pub async fn list_sessions(&self) -> Result<Vec<SessionMeta>> {
        let rows = sqlx::query(
            "SELECT id, title, created_at, updated_at, model, provider, workspace
             FROM sessions ORDER BY created_at DESC",
        )
        .fetch_all(&self.db)
        .await?;

        Ok(rows.into_iter().map(row_to_session).collect())
    }

    /// Fetch a single session by id.
    pub async fn get_session(&self, session_id: &str) -> Result<Option<SessionMeta>> {
        let row = sqlx::query(
            "SELECT id, title, created_at, updated_at, model, provider, workspace
             FROM sessions WHERE id = ?",
        )
        .bind(session_id)
        .fetch_optional(&self.db)
        .await?;

        Ok(row.map(row_to_session))
    }

    /// Add token usage to a session's cumulative budgets and bump `updated_at`.
    pub async fn update_session_usage(
        &self,
        session_id: &str,
        prompt_tokens: i64,
        candidate_tokens: i64,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "UPDATE sessions
             SET updated_at = ?,
                 token_budget_prompt = token_budget_prompt + ?,
                 token_budget_candidate = token_budget_candidate + ?
             WHERE id = ?",
        )
        .bind(&now)
        .bind(prompt_tokens)
        .bind(candidate_tokens)
        .bind(session_id)
        .execute(&self.db)
        .await?;

        Ok(())
    }

    /// Number of messages stored for a session.
    pub async fn message_count(&self, session_id: &str) -> Result<i64> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM messages WHERE session_id = ?")
            .bind(session_id)
            .fetch_one(&self.db)
            .await?;
        Ok(count)
    }

    /// Number of tool-call records stored for a session.
    pub async fn tool_call_count(&self, session_id: &str) -> Result<i64> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tool_calls WHERE session_id = ?")
            .bind(session_id)
            .fetch_one(&self.db)
            .await?;
        Ok(count)
    }

    /// Rename a session (and bump `updated_at`).
    pub async fn update_session_title(&self, session_id: &str, title: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        sqlx::query("UPDATE sessions SET title = ?, updated_at = ? WHERE id = ?")
            .bind(title)
            .bind(&now)
            .bind(session_id)
            .execute(&self.db)
            .await?;
        Ok(())
    }

    /// Delete a session and its messages, tool calls, and prompt rows.
    pub async fn delete_session(&self, session_id: &str) -> Result<bool> {
        sqlx::query("DELETE FROM tool_calls WHERE session_id = ?")
            .bind(session_id)
            .execute(&self.db)
            .await?;
        sqlx::query("DELETE FROM prompts WHERE session_id = ?")
            .bind(session_id)
            .execute(&self.db)
            .await?;
        // messages cascade via the foreign key; prompts/tool_calls have no FK.
        let result = sqlx::query("DELETE FROM sessions WHERE id = ?")
            .bind(session_id)
            .execute(&self.db)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Copy a session (metadata + ordered messages) into a new session.
    ///
    /// Tool-call rows are not copied: their primary key is the LLM call id, so
    /// copies would collide; tool results already live in the messages table.
    pub async fn fork_session(&self, session_id: &str, title: &str) -> Result<String> {
        let source = self
            .get_session(session_id)
            .await?
            .ok_or_else(|| MemoryError::SessionNotFound(session_id.to_string()))?;

        let new_id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO sessions (id, title, created_at, updated_at, model, provider, workspace)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&new_id)
        .bind(title)
        .bind(&now)
        .bind(&now)
        .bind(&source.model)
        .bind(&source.provider)
        .bind(&source.workspace)
        .execute(&self.db)
        .await?;

        sqlx::query(
            "INSERT INTO messages
                (session_id, turn_idx, role, content, tool_calls_json, tool_result, token_count, timestamp)
             SELECT ?, turn_idx, role, content, tool_calls_json, tool_result, token_count, timestamp
             FROM messages WHERE session_id = ? ORDER BY id",
        )
        .bind(&new_id)
        .bind(session_id)
        .execute(&self.db)
        .await?;

        Ok(new_id)
    }

    /// Store a user prompt in the history; returns its row id.
    pub async fn save_prompt(
        &self,
        session_id: Option<&str>,
        content: &str,
        model: &str,
    ) -> Result<i64> {
        let result =
            sqlx::query("INSERT INTO prompts (session_id, content, model) VALUES (?, ?, ?)")
                .bind(session_id)
                .bind(content)
                .bind(model)
                .execute(&self.db)
                .await?;
        Ok(result.last_insert_rowid())
    }

    /// Most recent prompts, newest first.
    pub async fn get_prompt_history(&self, limit: usize) -> Result<Vec<PromptRecord>> {
        let rows = sqlx::query(
            "SELECT id, session_id, content, model, timestamp
             FROM prompts ORDER BY id DESC LIMIT ?",
        )
        .bind(limit as i64)
        .fetch_all(&self.db)
        .await?;

        Ok(rows.into_iter().map(row_to_prompt).collect())
    }

    /// Most recent prompts created in the last `days` days, newest first.
    pub async fn get_recent_prompt_history(
        &self,
        limit: usize,
        days: u32,
    ) -> Result<Vec<PromptRecord>> {
        let rows = sqlx::query(
            "SELECT id, session_id, content, model, timestamp
             FROM prompts
             WHERE datetime(timestamp) >= datetime('now', '-' || ? || ' days')
             ORDER BY id DESC LIMIT ?",
        )
        .bind(i64::from(days))
        .bind(limit as i64)
        .fetch_all(&self.db)
        .await?;

        Ok(rows.into_iter().map(row_to_prompt).collect())
    }

    /// Prompts whose content contains `query` (case-insensitive), newest first.
    pub async fn search_prompts(&self, query: &str, limit: usize) -> Result<Vec<PromptRecord>> {
        let rows = sqlx::query(
            "SELECT id, session_id, content, model, timestamp
             FROM prompts WHERE content LIKE ? ORDER BY id DESC LIMIT ?",
        )
        .bind(format!("%{query}%"))
        .bind(limit as i64)
        .fetch_all(&self.db)
        .await?;

        Ok(rows.into_iter().map(row_to_prompt).collect())
    }

    /// Insert a memory, deduplicating identical content; returns its row id.
    pub async fn add_memory(
        &self,
        content: &str,
        embedding: Option<&[u8]>,
        source_session: Option<&str>,
    ) -> Result<i64> {
        sqlx::query(
            "INSERT OR IGNORE INTO memories (content, embedding, source_session) VALUES (?, ?, ?)",
        )
        .bind(content)
        .bind(embedding)
        .bind(source_session)
        .execute(&self.db)
        .await?;

        let id: i64 = sqlx::query_scalar("SELECT id FROM memories WHERE content = ?")
            .bind(content)
            .fetch_one(&self.db)
            .await?;
        Ok(id)
    }

    /// Stored memories, newest first (embeddings excluded, `score` unset).
    pub async fn list_memories(&self, limit: usize) -> Result<Vec<MemoryEntry>> {
        let rows = sqlx::query(
            "SELECT id, content, source_session, created_at FROM memories
             ORDER BY id DESC LIMIT ?",
        )
        .bind(limit as i64)
        .fetch_all(&self.db)
        .await?;

        Ok(rows
            .into_iter()
            .map(|row| MemoryEntry {
                id: row.get("id"),
                content: row.get("content"),
                source_session: row.get("source_session"),
                created_at: row.get("created_at"),
                score: None,
            })
            .collect())
    }

    /// Rank memories by cosine similarity to `query_embedding`, best first.
    ///
    /// Rows whose embedding dimension differs from the query are skipped, so
    /// swapping embedding models degrades gracefully instead of erroring.
    pub async fn search_memories(
        &self,
        query_embedding: &[f32],
        limit: usize,
    ) -> Result<Vec<MemoryEntry>> {
        let rows = sqlx::query(
            "SELECT id, content, embedding, source_session, created_at
             FROM memories
             WHERE embedding IS NOT NULL
               AND datetime(created_at) >= datetime('now', '-7 days')
             ORDER BY id DESC LIMIT 1000",
        )
        .fetch_all(&self.db)
        .await?;

        let mut scored: Vec<MemoryEntry> = rows
            .into_iter()
            .filter_map(|row| {
                let blob: Vec<u8> = row.get("embedding");
                let vector = crate::embed::from_bytes(&blob);
                if vector.len() != query_embedding.len() {
                    return None;
                }
                Some(MemoryEntry {
                    id: row.get("id"),
                    content: row.get("content"),
                    source_session: row.get("source_session"),
                    created_at: row.get("created_at"),
                    score: Some(crate::embed::cosine(query_embedding, &vector)),
                })
            })
            .collect();

        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(limit);
        Ok(scored)
    }

    /// Substring search over memory content (case-insensitive fallback).
    pub async fn search_memories_text(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<MemoryEntry>> {
        let rows = sqlx::query(
            "SELECT id, content, source_session, created_at FROM memories
             WHERE content LIKE ? ORDER BY id DESC LIMIT ?",
        )
        .bind(format!("%{query}%"))
        .bind(limit as i64)
        .fetch_all(&self.db)
        .await?;

        Ok(rows
            .into_iter()
            .map(|row| MemoryEntry {
                id: row.get("id"),
                content: row.get("content"),
                source_session: row.get("source_session"),
                created_at: row.get("created_at"),
                score: None,
            })
            .collect())
    }

    /// Delete a memory by id; returns whether a row was removed.
    pub async fn delete_memory(&self, id: i64) -> Result<bool> {
        let result = sqlx::query("DELETE FROM memories WHERE id = ?")
            .bind(id)
            .execute(&self.db)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Most recent tool call in a session (for `/replay last`).
    pub async fn last_tool_call(&self, session_id: &str) -> Result<Option<ToolCallRecord>> {
        let row = sqlx::query(
            "SELECT id, session_id, turn_idx, tool_name, args_json, result, duration_ms, exit_code
             FROM tool_calls WHERE session_id = ? ORDER BY rowid DESC LIMIT 1",
        )
        .bind(session_id)
        .fetch_optional(&self.db)
        .await?;
        Ok(row.map(row_to_tool_call))
    }

    /// Tool call by exact id, else by id prefix (most recent match wins).
    pub async fn find_tool_call(&self, id_or_prefix: &str) -> Result<Option<ToolCallRecord>> {
        let row = sqlx::query(
            "SELECT id, session_id, turn_idx, tool_name, args_json, result, duration_ms, exit_code
             FROM tool_calls WHERE id = ? LIMIT 1",
        )
        .bind(id_or_prefix)
        .fetch_optional(&self.db)
        .await?;
        if let Some(row) = row {
            return Ok(Some(row_to_tool_call(row)));
        }

        // LIKE pattern: escape wildcards so prefixes match literally.
        let escaped = id_or_prefix
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let row = sqlx::query(
            "SELECT id, session_id, turn_idx, tool_name, args_json, result, duration_ms, exit_code
             FROM tool_calls WHERE id LIKE ? ESCAPE '\\' ORDER BY rowid DESC LIMIT 1",
        )
        .bind(format!("{escaped}%"))
        .fetch_optional(&self.db)
        .await?;
        Ok(row.map(row_to_tool_call))
    }

    async fn touch_session(&self, session_id: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        sqlx::query("UPDATE sessions SET updated_at = ? WHERE id = ?")
            .bind(&now)
            .bind(session_id)
            .execute(&self.db)
            .await?;
        Ok(())
    }
}

fn row_to_session(row: sqlx::sqlite::SqliteRow) -> SessionMeta {
    SessionMeta {
        id: row.get("id"),
        title: row.get("title"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        model: row.get("model"),
        provider: row.get("provider"),
        workspace: row.get("workspace"),
    }
}

fn row_to_tool_call(row: sqlx::sqlite::SqliteRow) -> ToolCallRecord {
    ToolCallRecord {
        id: row.get("id"),
        session_id: row.get("session_id"),
        turn_idx: row.get("turn_idx"),
        tool_name: row.get("tool_name"),
        args_json: row.get("args_json"),
        result: row.get("result"),
        duration_ms: row.get("duration_ms"),
        exit_code: row.get("exit_code"),
    }
}

fn row_to_prompt(row: sqlx::sqlite::SqliteRow) -> PromptRecord {
    PromptRecord {
        id: row.get("id"),
        session_id: row.get("session_id"),
        content: row.get("content"),
        model: row.get("model"),
        timestamp: row.get("timestamp"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn temp_store() -> (tempfile::TempDir, MemoryStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("sessions.db");
        let store = MemoryStore::init(&db_path.to_string_lossy())
            .await
            .expect("store init");
        (dir, store)
    }

    fn msg(role: &str, content: &str, turn: i64) -> Message {
        Message {
            role: role.to_string(),
            content: Some(content.to_string()),
            turn_idx: turn,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn init_creates_database_and_is_idempotent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("nested").join("sessions.db");
        let path_str = db_path.to_string_lossy().to_string();

        let first = MemoryStore::init(&path_str).await.expect("first init");
        assert!(db_path.exists());
        drop(first);

        // Re-running init on an existing database must succeed.
        MemoryStore::init(&path_str).await.expect("second init");
    }

    #[tokio::test]
    async fn session_round_trip() {
        let (_dir, store) = temp_store().await;

        let session_id = store
            .create_session("test session", "gpt-4.1", "openai", "/tmp/ws")
            .await
            .expect("create session");

        store
            .append_message(&session_id, &msg("system", "you are helpful", 0))
            .await
            .expect("append system");
        store
            .append_message(&session_id, &msg("user", "create hello.txt", 1))
            .await
            .expect("append user");
        store
            .append_message(
                &session_id,
                &Message {
                    role: "assistant".to_string(),
                    content: None,
                    tool_calls_json: Some(
                        r#"[{"id":"call_1","name":"write","args":{"path":"hello.txt"}}]"#
                            .to_string(),
                    ),
                    turn_idx: 1,
                    ..Default::default()
                },
            )
            .await
            .expect("append assistant");
        store
            .append_message(
                &session_id,
                &Message {
                    role: "tool".to_string(),
                    content: Some("Wrote 13 bytes".to_string()),
                    tool_result: Some(r#"{"success":true}"#.to_string()),
                    turn_idx: 1,
                    ..Default::default()
                },
            )
            .await
            .expect("append tool");

        let history = store
            .load_session_history(&session_id)
            .await
            .expect("load history");
        assert_eq!(history.len(), 4);
        assert_eq!(history[0].role, "system");
        assert_eq!(history[1].content.as_deref(), Some("create hello.txt"));
        assert!(history[2].tool_calls_json.is_some());
        assert_eq!(history[3].role, "tool");
        assert_eq!(store.message_count(&session_id).await.unwrap(), 4);

        let sessions = store.list_sessions().await.expect("list sessions");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, session_id);
        assert_eq!(sessions[0].title, "test session");
        assert_eq!(sessions[0].model.as_deref(), Some("gpt-4.1"));
        assert_eq!(sessions[0].provider.as_deref(), Some("openai"));

        let fetched = store
            .get_session(&session_id)
            .await
            .expect("get session")
            .expect("session exists");
        assert_eq!(fetched.title, "test session");
        assert!(store.get_session("missing").await.unwrap().is_none());

        store
            .update_session_usage(&session_id, 100, 50)
            .await
            .expect("update usage");
    }

    #[tokio::test]
    async fn tool_call_records_round_trip() {
        let (_dir, store) = temp_store().await;
        let session_id = store
            .create_session("tools", "gpt-4.1", "openai", "/tmp/ws")
            .await
            .unwrap();

        store
            .append_tool_call(&ToolCallRecord {
                id: "call_1".to_string(),
                session_id: session_id.clone(),
                turn_idx: Some(1),
                tool_name: "write".to_string(),
                args_json: Some(r#"{"path":"hello.txt"}"#.to_string()),
                result: Some("ok".to_string()),
                duration_ms: Some(3),
                exit_code: Some(0),
            })
            .await
            .expect("append tool call");

        assert_eq!(store.tool_call_count(&session_id).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn sessions_are_cascade_deleted() {
        let (_dir, store) = temp_store().await;
        let session_id = store
            .create_session("t", "m", "p", "/tmp/ws")
            .await
            .unwrap();
        store
            .append_message(&session_id, &msg("user", "hi", 1))
            .await
            .unwrap();

        sqlx::query("DELETE FROM sessions WHERE id = ?")
            .bind(&session_id)
            .execute(&store.db)
            .await
            .unwrap();

        assert_eq!(store.message_count(&session_id).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn prompts_are_stored_listed_and_searched() {
        let (_dir, store) = temp_store().await;
        let session_id = store
            .create_session("t", "m", "p", "/tmp/ws")
            .await
            .unwrap();

        store
            .save_prompt(Some(&session_id), "refactor the parser", "gpt-4.1")
            .await
            .unwrap();
        store
            .save_prompt(Some(&session_id), "write a rust CLI", "gpt-4.1")
            .await
            .unwrap();
        store
            .save_prompt(None, "orphan prompt", "gpt-4.1")
            .await
            .unwrap();

        let history = store.get_prompt_history(10).await.unwrap();
        assert_eq!(history.len(), 3);
        assert_eq!(history[0].content, "orphan prompt", "newest first");
        assert_eq!(history[2].session_id.as_deref(), Some(session_id.as_str()));

        // Case-insensitive substring search.
        let hits = store.search_prompts("PARSER", 10).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].content, "refactor the parser");
        assert!(
            store
                .search_prompts("nothing", 10)
                .await
                .unwrap()
                .is_empty()
        );

        // Limit is honored.
        assert_eq!(store.get_prompt_history(1).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn recent_prompt_history_excludes_entries_older_than_requested_window() {
        let (_dir, store) = temp_store().await;
        store
            .save_prompt(None, "recent command", "gpt-4.1")
            .await
            .unwrap();
        store
            .save_prompt(None, "old command", "gpt-4.1")
            .await
            .unwrap();
        sqlx::query(
            "UPDATE prompts SET timestamp = datetime('now', '-8 days') WHERE content = 'old command'",
        )
        .execute(&store.db)
        .await
        .unwrap();

        let recent = store.get_recent_prompt_history(20, 7).await.unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].content, "recent command");
    }

    #[tokio::test]
    async fn memories_dedupe_and_rank_by_similarity() {
        use crate::embed;

        let (_dir, store) = temp_store().await;

        let deploy = "deploy key lives in the vault under prod-deploy-key";
        let smoothie = "banana smoothie recipe with mango and yogurt";
        let first = store
            .add_memory(deploy, Some(&embed::to_bytes(&embed::embed(deploy))), None)
            .await
            .unwrap();
        store
            .add_memory(
                smoothie,
                Some(&embed::to_bytes(&embed::embed(smoothie))),
                None,
            )
            .await
            .unwrap();
        store
            .add_memory(
                "old failed deploy retry should not be recalled",
                Some(&embed::to_bytes(&embed::embed(
                    "old failed deploy retry should not be recalled",
                ))),
                None,
            )
            .await
            .unwrap();
        sqlx::query(
            "UPDATE memories SET created_at = datetime('now', '-8 days')
             WHERE content LIKE 'old failed deploy%'",
        )
        .execute(&store.db)
        .await
        .unwrap();
        // Identical content is deduplicated (same row id, one row).
        let again = store
            .add_memory(deploy, Some(&embed::to_bytes(&embed::embed(deploy))), None)
            .await
            .unwrap();
        assert_eq!(first, again);
        assert_eq!(store.list_memories(10).await.unwrap().len(), 3);

        let query = embed::embed("where is the deploy key vault");
        let ranked = store.search_memories(&query, 2).await.unwrap();
        assert_eq!(ranked.len(), 2);
        assert!(
            ranked
                .iter()
                .all(|memory| !memory.content.starts_with("old failed"))
        );
        assert!(ranked[0].content.contains("deploy key"), "{ranked:?}");
        assert!(ranked[0].score.unwrap() > ranked[1].score.unwrap());

        // Text fallback and delete.
        assert_eq!(
            store.search_memories_text("mango", 5).await.unwrap().len(),
            1
        );
        assert!(store.delete_memory(ranked[1].id).await.unwrap());
        assert!(!store.delete_memory(ranked[1].id).await.unwrap());
        assert_eq!(store.list_memories(10).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn fork_copies_messages_under_a_new_session() {
        let (_dir, store) = temp_store().await;
        let original = store
            .create_session("original", "gpt-4.1", "openai", "/tmp/ws")
            .await
            .unwrap();
        store
            .append_message(&original, &msg("user", "step one", 1))
            .await
            .unwrap();
        store
            .append_message(&original, &msg("assistant", "done", 1))
            .await
            .unwrap();

        let forked = store.fork_session(&original, "branch").await.unwrap();
        assert_ne!(forked, original);
        assert_eq!(store.message_count(&forked).await.unwrap(), 2);
        assert_eq!(store.message_count(&original).await.unwrap(), 2);

        let meta = store.get_session(&forked).await.unwrap().unwrap();
        assert_eq!(meta.title, "branch");
        assert_eq!(meta.workspace.as_deref(), Some("/tmp/ws"));

        // New messages in the fork do not leak into the original.
        store
            .append_message(&forked, &msg("user", "extra", 2))
            .await
            .unwrap();
        assert_eq!(store.message_count(&original).await.unwrap(), 2);

        // Rename + delete round trip.
        store
            .update_session_title(&forked, "renamed")
            .await
            .unwrap();
        assert_eq!(
            store.get_session(&forked).await.unwrap().unwrap().title,
            "renamed"
        );
        assert!(store.delete_session(&forked).await.unwrap());
        assert!(store.get_session(&forked).await.unwrap().is_none());
        assert!(store.fork_session("missing", "x").await.is_err());
    }

    #[tokio::test]
    async fn tool_calls_are_looked_up_by_id_prefix_and_last() {
        let (_dir, store) = temp_store().await;
        let session_id = store
            .create_session("t", "m", "p", "/tmp/ws")
            .await
            .unwrap();

        for (id, name) in [("call_aaa_1", "read"), ("call_bbb_2", "write")] {
            store
                .append_tool_call(&ToolCallRecord {
                    id: id.to_string(),
                    session_id: session_id.clone(),
                    turn_idx: Some(1),
                    tool_name: name.to_string(),
                    args_json: Some(r#"{"path":"x"}"#.to_string()),
                    result: Some("ok".to_string()),
                    duration_ms: Some(1),
                    exit_code: None,
                })
                .await
                .unwrap();
        }

        let exact = store.find_tool_call("call_aaa_1").await.unwrap().unwrap();
        assert_eq!(exact.tool_name, "read");

        let prefixed = store.find_tool_call("call_bbb").await.unwrap().unwrap();
        assert_eq!(prefixed.tool_name, "write");

        let last = store.last_tool_call(&session_id).await.unwrap().unwrap();
        assert_eq!(last.id, "call_bbb_2");

        assert!(store.find_tool_call("nope").await.unwrap().is_none());
        assert!(store.last_tool_call("missing").await.unwrap().is_none());
    }
}
