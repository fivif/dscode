//! SessionManager — persist chat sessions in SQLite.
//!
//! Sessions are stored in ~/.dscode/sessions.db with two tables:
//! - `sessions`: id, title, workspace, model, created_at, updated_at
//! - `messages`: id, session_id, role, content, tool_calls, tool_call_id, name, reasoning_content, created_at
//!
//! Schema changes are tracked in `PRAGMA user_version` (see `migrate`).

use chrono::{Datelike, Duration, NaiveDate, Utc};
use rusqlite::{params, Connection, Transaction, TransactionBehavior};
use serde_json;
use std::path::{Path, PathBuf};
use tracing::{debug, warn};
use uuid::Uuid;

use crate::config::settings::Config;
use crate::providers::trait_def::{Message, MessageContent, Role, ToolCall};

/// Schema version of `sessions.db`, tracked in `PRAGMA user_version`.
///
/// 0 = a database created before versioning existed (the three post-hoc
/// `ALTER`s may or may not have been applied); 1 = `workspace`, `model` and
/// `messages.name` are part of the schema; 2 = `idx_sessions_updated` exists.
const SCHEMA_VERSION: i64 = 2;

/// How many `VACUUM INTO` snapshots to keep in `~/.dscode/backups/`.
const BACKUPS_TO_KEEP: usize = 5;

/// A single chat session with all associated messages.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub workspace: String,
    /// Model id bound to this session.
    /// Snapshot of global default at create time; updated when the user picks a model in chat.
    /// Empty string = fall back to config `default_model` (legacy sessions).
    #[serde(default)]
    pub model: String,
    pub created_at: i64,
    /// Last time this session was written to *or opened* (`get_session` bumps
    /// it, see `touch_session`). It is both the sidebar sort key and the
    /// retention key, so opening a session moves it to the top and protects it
    /// from `purge_now` at the same time. An explicit "last message at" column
    /// would decouple the two, at the cost of a schema migration.
    pub updated_at: i64,
    pub messages: Vec<Message>,
}

/// A session plus the count of history rows that could not be decoded.
///
/// `get_session` logs the count at warn level and drops it; callers that can
/// surface it to the user should use `get_session_with_report` instead.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionLoad {
    pub session: Session,
    /// Rows that were unreadable (bad role, corrupt content/tool_calls JSON)
    /// and were therefore left out of `session.messages`. The transcript is
    /// shorter than what is stored by this much, and the model is answering
    /// without them.
    pub skipped_messages: usize,
}

/// Internal result of `load_messages`.
struct LoadedMessages {
    messages: Vec<Message>,
    /// Undecodable rows that were dropped. Repairs performed by
    /// `validate_tool_chain` (dedup, orphan pruning) are not counted here —
    /// they remove rows that carry no usable content.
    skipped: usize,
}

/// Grouping of sessions by recency for UI display.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionGroups {
    pub today: Vec<Session>,
    pub yesterday: Vec<Session>,
    pub this_week: Vec<Session>,
    pub this_month: Vec<Session>,
    pub older: Vec<Session>,
}

/// Manages chat session persistence via SQLite.
pub struct SessionManager {
    conn: Connection,
    retention_days: u32,
}

impl SessionManager {
    /// Open (or create) the database at `db_path` and run migrations.
    /// If `db_path` is relative, it's resolved relative to `~/.dscode/`.
    pub fn new(retention_days: u32) -> Result<Self, String> {
        let db_path = Self::db_path()?;

        // Ensure parent directory exists.
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create sessions dir: {}", e))?;
        }

        let conn = Connection::open(&db_path)
            .map_err(|e| db_err(&format!("Failed to open database at {:?}", db_path), e))?;

        // Enable WAL mode for better concurrent read performance.
        conn.execute_batch("PRAGMA journal_mode=WAL;")
            .map_err(|e| db_err("Failed to set WAL mode", e))?;

        // Run migrations. Errors propagate: a half-migrated database is worse
        // than a refused startup.
        Self::migrate(&conn)?;

        // A single assertion, outside any transaction — the pragma is a no-op
        // inside one. Correctness does not depend on it (child rows are also
        // deleted explicitly), it just keeps the invariant cheap to hold.
        conn.execute_batch("PRAGMA foreign_keys = ON;")
            .map_err(|e| db_err("Failed to enable foreign keys", e))?;

        // NOTE: retention deliberately does NOT run here. It used to be the
        // first thing `new()` did, so merely launching the app destroyed
        // sessions the user had been reading but not writing. `purge_now()` is
        // the explicit path (the desktop app schedules it); opening the
        // database is not destructive.
        Ok(Self {
            conn,
            retention_days,
        })
    }

    /// Bring the database up to `SCHEMA_VERSION`, gated on `PRAGMA user_version`.
    ///
    /// The whole migration is one transaction with the version bump in it, and
    /// every error propagates. Previously the columns the entire codebase
    /// queries were added by three `ALTER TABLE ... .ok()` statements: a single
    /// transient `database is locked` at first launch after an upgrade left the
    /// DB half-migrated, `new()` still returned `Ok`, and the app then opened,
    /// listed sessions, and silently persisted nothing — forever, because there
    /// was no version gate to retry. Now a failure rolls back and the next
    /// launch retries.
    fn migrate(conn: &Connection) -> Result<(), String> {
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(|e| db_err("Failed to read schema version", e))?;

        if version >= SCHEMA_VERSION {
            return Ok(());
        }

        let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
            .map_err(|e| db_err("Failed to start migration transaction", e))?;

        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
                id          TEXT PRIMARY KEY,
                title       TEXT NOT NULL,
                created_at  INTEGER NOT NULL,
                updated_at  INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS messages (
                id                INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id        TEXT NOT NULL,
                role              TEXT NOT NULL,
                content           TEXT NOT NULL,
                tool_calls        TEXT,
                tool_call_id      TEXT,
                reasoning_content TEXT,
                created_at        INTEGER NOT NULL,
                FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS idx_messages_session
                ON messages(session_id, created_at);",
        )
        .map_err(|e| db_err("Migration failed", e))?;

        // Columns added after the original schema shipped. SQLite has no
        // `ADD COLUMN IF NOT EXISTS`, so presence is probed first: an existing
        // 12.7 MB database already has all three and must not be touched.
        for (table, column, sql) in [
            (
                "sessions",
                "workspace",
                "ALTER TABLE sessions ADD COLUMN workspace TEXT NOT NULL DEFAULT ''",
            ),
            (
                "sessions",
                "model",
                "ALTER TABLE sessions ADD COLUMN model TEXT NOT NULL DEFAULT ''",
            ),
            (
                "messages",
                "name",
                "ALTER TABLE messages ADD COLUMN name TEXT",
            ),
        ] {
            if !Self::column_exists(&tx, table, column)? {
                tx.execute_batch(sql)
                    .map_err(|e| db_err(&format!("Migration failed adding {table}.{column}"), e))?;
            }
        }

        // v2: the session list is `ORDER BY updated_at DESC` over the whole
        // table — a fresh install had no index on `sessions` at all, so every
        // sidebar load sorted the full scan in memory. The index matches the
        // query's direction, so SQLite walks it straight instead of sorting.
        if version < 2 {
            tx.execute_batch(
                "CREATE INDEX IF NOT EXISTS idx_sessions_updated
                    ON sessions(updated_at DESC);",
            )
            .map_err(|e| db_err("Migration failed creating idx_sessions_updated", e))?;
        }

        tx.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))
            .map_err(|e| db_err("Failed to record schema version", e))?;
        tx.commit()
            .map_err(|e| db_err("Migration commit failed", e))?;

        debug!(from = version, to = SCHEMA_VERSION, "sessions.db schema migrated");
        Ok(())
    }

    /// Whether `table` has a column named `column` (SQLite has no
    /// `ALTER TABLE ... ADD COLUMN IF NOT EXISTS`). `table` is always a literal
    /// from this module, never user input.
    fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool, String> {
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .map_err(|e| db_err(&format!("Failed to inspect {table}"), e))?;
        let mut rows = stmt
            .query([])
            .map_err(|e| db_err(&format!("Failed to inspect {table}"), e))?;
        while let Some(row) = rows
            .next()
            .map_err(|e| db_err(&format!("Failed to inspect {table}"), e))?
        {
            let name: String = row
                .get(1)
                .map_err(|e| db_err(&format!("Failed to inspect {table}"), e))?;
            if name == column {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Resolve the database path: ~/.dscode/sessions.db
    fn db_path() -> Result<PathBuf, String> {
        Config::data_dir().map(|d| d.join("sessions.db")).map_err(|e| e.to_string())
    }

    // ── CRUD ──────────────────────────────────────────────────────────────

    /// Create a new session and return it (with empty messages).
    /// `model` is usually the current global default; empty means fall back at send time.
    ///
    /// An empty `title` is accepted and filled with [`Self::provisional_title`]:
    /// the desktop modal deliberately creates the session before the user has
    /// typed anything, and a session must never be nameless. The provisional
    /// name is a placeholder, so the first message can still rename it.
    pub fn create_session(
        &self,
        title: &str,
        workspace: &str,
        model: &str,
    ) -> Result<Session, String> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().timestamp();
        let model = model.trim().to_string();
        let title = if title.trim().is_empty() {
            Self::provisional_title(workspace)
        } else {
            title.to_string()
        };

        self.conn
            .execute(
                "INSERT INTO sessions (id, title, workspace, model, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![id, title, workspace, model, now, now],
            )
            .map_err(|e| db_err("Failed to create session", e))?;

        Ok(Session {
            id,
            title,
            workspace: workspace.to_string(),
            model,
            created_at: now,
            updated_at: now,
            messages: Vec::new(),
        })
    }

    /// Get the most recent session.
    pub fn get_last_session(&self) -> Result<Option<Session>, String> {
        let sid: Result<String, _> = self
            .conn
            .query_row(
                "SELECT id FROM sessions ORDER BY updated_at DESC LIMIT 1",
                [],
                |r| r.get(0),
            );
        match sid {
            Ok(id) => self.get_session(&id),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(db_err("Failed to get last session", e)),
        }
    }

    /// Update the workspace for a session.
    pub fn update_workspace(&self, session_id: &str, workspace: &str) -> Result<(), String> {
        let affected = self
            .conn
            .execute(
                "UPDATE sessions SET workspace = ?1, updated_at = ?2 WHERE id = ?3",
                params![workspace, Utc::now().timestamp(), session_id],
            )
            .map_err(|e| db_err("Failed to update workspace", e))?;
        if affected == 0 {
            Err("Session not found".into())
        } else {
            Ok(())
        }
    }

    /// Bind a model id to this session (user changed model in the chat picker).
    pub fn update_model(&self, session_id: &str, model: &str) -> Result<(), String> {
        let model = model.trim().to_string();
        if model.is_empty() {
            return Err("Model must not be empty".into());
        }
        let affected = self
            .conn
            .execute(
                "UPDATE sessions SET model = ?1, updated_at = ?2 WHERE id = ?3",
                params![model, Utc::now().timestamp(), session_id],
            )
            .map_err(|e| db_err("Failed to update model", e))?;
        if affected == 0 {
            Err("Session not found".into())
        } else {
            Ok(())
        }
    }

    /// Rename a session.
    pub fn update_title(&self, session_id: &str, title: &str) -> Result<(), String> {
        let title = title.trim();
        if title.is_empty() {
            return Err("Title must not be empty".into());
        }
        // Keep sidebar readable
        let title: String = title.chars().take(80).collect();
        let affected = self
            .conn
            .execute(
                "UPDATE sessions SET title = ?1, updated_at = ?2 WHERE id = ?3",
                params![title, Utc::now().timestamp(), session_id],
            )
            .map_err(|e| db_err("Failed to update title", e))?;
        if affected == 0 {
            Err("Session not found".into())
        } else {
            Ok(())
        }
    }

    /// Pure mode / control messages that must not steal the session title.
    ///
    /// Note this is an exact-match list, so a command *with trailing text*
    /// (`/plan add a flag`) still titles the session from that text — which is
    /// intended for `/plan` (the text is the goal) but merely tolerated for
    /// `/compact`, whose trailing text is ignored.
    pub fn is_control_only_message(user_message: &str) -> bool {
        let t = user_message.trim().to_lowercase();
        matches!(
            t.as_str(),
            "/teams"
                | "/teams on"
                | "/teams off"
                | "/teams stop"
                | "/plan"
                | "/auto"
                | "/compact"
                | "/teams:"
                | "/plan:"
                | "/auto:"
                | "/compact:"
        )
    }

    /// Whether the title looks auto-generated / placeholder (safe to overwrite).
    pub fn is_placeholder_title(title: &str) -> bool {
        let t = title.trim();
        if t.is_empty() {
            return true;
        }
        let lower = t.to_lowercase();
        lower == "新对话"
            || lower == "new chat"
            || lower == "untitled"
            || lower == "new session"
            || t.starts_with("对话 ") // also the prefix `provisional_title` writes
            || t.starts_with("Chat ")
            || t.starts_with("Session ")
            // workspace-folder-only provisional names from create flow
            || t.starts_with("📂 ") // legacy emoji prefix (migrating away)
            // weak titles from mode-only toggles / empty slash commands
            || t == "Teams · 多 Agent 协作"
            || t == "Plan · 需求规划"
            || t == "Auto · 自动执行"
            || lower == "teams · 多 agent 协作"
    }

    /// Derive a short sidebar title from the first user message.
    pub fn derive_title_from_message(user_message: &str) -> String {
        let raw = user_message.trim();
        if raw.is_empty() || Self::is_control_only_message(raw) {
            return "新对话".into();
        }

        // Prefer first non-empty line
        let first = raw
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or(raw)
            .to_string();

        // Normalize slash commands into readable titles
        let (prefix, body_owned): (&str, String) = if let Some(rest) = first
            .strip_prefix("/plan")
            .filter(|r| r.is_empty() || r.starts_with(|c: char| c.is_whitespace() || c == ':'))
        {
            (
                "Plan · ",
                rest.trim().trim_start_matches(':').trim().to_string(),
            )
        } else if let Some(rest) = first
            .strip_prefix("/auto")
            .filter(|r| r.is_empty() || r.starts_with(|c: char| c.is_whitespace() || c == ':'))
        {
            (
                "Auto · ",
                rest.trim().trim_start_matches(':').trim().to_string(),
            )
        } else if let Some(rest) = first
            .strip_prefix("/teams")
            .filter(|r| r.is_empty() || r.starts_with(|c: char| c.is_whitespace() || c == ':'))
        {
            let rest = rest
                .trim()
                .strip_prefix("on")
                .map(|r| r.trim())
                .unwrap_or(rest.trim())
                .trim_start_matches(':')
                .trim()
                .to_string();
            ("Teams · ", rest)
        } else if first.starts_with('/') {
            // Skill or other slash invoke: "/grill-me clarify auth" → body without command token
            let mut parts = first.splitn(2, char::is_whitespace);
            let _cmd = parts.next();
            let rest = parts.next().unwrap_or("").trim().to_string();
            if rest.is_empty() {
                // bare skill name — use command without leading slash
                let name = first.trim_start_matches('/').to_string();
                ("", name)
            } else {
                ("", rest)
            }
        } else {
            ("", first)
        };

        let body = if body_owned.is_empty() {
            match prefix {
                "Plan · " => "需求规划".to_string(),
                "Auto · " => "自动执行".to_string(),
                "Teams · " => "多 Agent 协作".to_string(),
                _ => "新对话".to_string(),
            }
        } else {
            body_owned
        };

        // Collapse whitespace
        let collapsed: String = body.split_whitespace().collect::<Vec<_>>().join(" ");
        let max_chars = 32usize;
        let mut core: String = collapsed.chars().take(max_chars).collect();
        if collapsed.chars().count() > max_chars {
            core.push('…');
        }

        let title = format!("{prefix}{core}");
        if title.trim().is_empty() {
            "新对话".into()
        } else {
            title
        }
    }

    /// Auto-rename session from first real user message when still using a placeholder title.
    /// Returns `Some(new_title)` if renamed, else `None`.
    ///
    /// This is also the gate for LLM naming: `Some` means the title *was* a
    /// placeholder, so the returned string is what
    /// [`Self::replace_title_if_unchanged`] expects as its `expected` value.
    /// `None` (control-only message, or the user's own title) means no namer
    /// should run at all.
    pub fn maybe_auto_title(&self, session_id: &str, user_message: &str) -> Result<Option<String>, String> {
        // Never name the session after pure mode toggles
        if Self::is_control_only_message(user_message) {
            return Ok(None);
        }

        let current = self
            .conn
            .query_row(
                "SELECT title FROM sessions WHERE id = ?1",
                params![session_id],
                |r| r.get::<_, String>(0),
            )
            .map_err(|e| db_err("Failed to read title", e))?;

        if !Self::is_placeholder_title(&current) {
            return Ok(None);
        }

        let new_title = Self::derive_title_from_message(user_message);
        if Self::is_placeholder_title(&new_title) {
            return Ok(None);
        }
        self.update_title(session_id, &new_title)?;
        Ok(Some(new_title))
    }

    /// Replace the title with `candidate`, but only if the title is still
    /// exactly `expected`.
    ///
    /// The compare-and-swap behind LLM naming. `maybe_auto_title` writes the
    /// deterministic title first and hands it back as `expected`; between that
    /// write and the model's reply the only thing that can change the title is
    /// the user renaming the session, and that must win. A candidate that is
    /// itself a placeholder (the model echoed `新对话`) is refused: it would
    /// make the session look auto-named while being less informative.
    ///
    /// Returns whether the title was replaced.
    pub fn replace_title_if_unchanged(
        &self,
        session_id: &str,
        expected: &str,
        candidate: &str,
    ) -> Result<bool, String> {
        if candidate.trim().is_empty() || Self::is_placeholder_title(candidate) {
            return Ok(false);
        }

        let current = self
            .conn
            .query_row(
                "SELECT title FROM sessions WHERE id = ?1",
                params![session_id],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(|e| db_err("Failed to read title", e))?;

        // Gone (deleted while the namer was in flight) or renamed by hand.
        if current.as_deref() != Some(expected) {
            return Ok(false);
        }

        self.update_title(session_id, candidate)?;
        Ok(true)
    }

    /// Provisional title for a brand-new session (before the first message).
    ///
    /// The `对话 ` prefix is load-bearing, not decoration: `is_placeholder_title`
    /// recognises it, which is what lets the first user message rename the
    /// session. A bare folder name looks hand-set and the overwrite guard would
    /// refuse to touch it.
    pub fn provisional_title(workspace: &str) -> String {
        let folder = std::path::Path::new(workspace)
            .file_name()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty());
        match folder {
            Some(name) => format!("对话 {name}"),
            None => "新对话".into(),
        }
    }

    /// Load a session by id, including all messages ordered by creation time.
    pub fn get_session(&self, session_id: &str) -> Result<Option<Session>, String> {
        Ok(self.get_session_with_report(session_id)?.map(|load| load.session))
    }

    /// Like [`SessionManager::get_session`], but also reports how many history
    /// rows were dropped as unreadable so the caller can tell the user the
    /// transcript is incomplete.
    pub fn get_session_with_report(
        &self,
        session_id: &str,
    ) -> Result<Option<SessionLoad>, String> {
        // Opening a session counts as using it, so retention cannot destroy a
        // conversation the user still reads (see `purge_old_sessions`).
        // Best effort: a failed bump must not fail the read. Done before the
        // SELECT so the `updated_at` handed back matches the row.
        if let Err(e) = self.touch_session(session_id) {
            warn!(session = %session_id, error = %e, "could not bump updated_at on read");
        }

        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, title, workspace, COALESCE(model, ''), created_at, updated_at FROM sessions WHERE id = ?1",
            )
            .map_err(|e| db_err("Prepare error", e))?;

        let session_row = stmt
            .query_row(params![session_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })
            .optional()
            .map_err(|e| db_err("Query error", e))?;

        match session_row {
            None => Ok(None),
            Some((id, title, workspace, model, created_at, updated_at)) => {
                let loaded = self.load_messages(&id)?;
                if loaded.skipped > 0 {
                    // This used to be a bare `eprintln!` per row, which is
                    // invisible in a GUI process with no console: the user saw
                    // a short transcript and nothing else.
                    warn!(
                        session = %id,
                        skipped = loaded.skipped,
                        "session history is missing rows that could not be decoded"
                    );
                }
                Ok(Some(SessionLoad {
                    session: Session {
                        id,
                        title,
                        workspace,
                        model,
                        created_at,
                        updated_at,
                        messages: loaded.messages,
                    },
                    skipped_messages: loaded.skipped,
                }))
            }
        }
    }

    /// Bump `updated_at` for a session (no-op when the id does not exist).
    fn touch_session(&self, session_id: &str) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
                params![Utc::now().timestamp(), session_id],
            )
            .map_err(|e| db_err("Failed to touch session", e))?;
        Ok(())
    }

    /// List all sessions, most-recently-updated first. Messages are NOT loaded.
    ///
    /// Deliberately does NOT bump `updated_at`: it returns every row, so
    /// touching them all would set one timestamp for the whole table and
    /// destroy the `ORDER BY updated_at DESC` ordering this same column
    /// provides. "Read" protection comes from `get_session`.
    pub fn list_sessions(&self) -> Result<Vec<Session>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, title, workspace, COALESCE(model, ''), created_at, updated_at FROM sessions ORDER BY updated_at DESC",
            )
            .map_err(|e| db_err("Prepare error", e))?;

        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })
            .map_err(|e| db_err("Query error", e))?;

        let mut sessions = Vec::new();
        for row in rows {
            let (id, title, workspace, model, created_at, updated_at) =
                row.map_err(|e| db_err("Row error", e))?;
            sessions.push(Session {
                id,
                title,
                workspace,
                model,
                created_at,
                updated_at,
                messages: Vec::new(),
            });
        }

        Ok(sessions)
    }

    /// Delete a session and all its messages.
    ///
    /// Both deletes run in one transaction and the child rows are removed
    /// explicitly. Relying on `ON DELETE CASCADE` alone was unsafe: the
    /// cascade only fires while `PRAGMA foreign_keys` is on, that pragma is a
    /// no-op inside a transaction, and the re-assertion here was `.ok()`-ed, so
    /// `delete_session` could return `Ok(())` while every message row stayed
    /// behind with no owning session.
    pub fn delete_session(&self, session_id: &str) -> Result<(), String> {
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)
            .map_err(|e| db_err("Failed to start delete transaction", e))?;

        tx.execute("DELETE FROM messages WHERE session_id = ?1", params![session_id])
            .map_err(|e| db_err("Delete messages error", e))?;
        let affected = tx
            .execute("DELETE FROM sessions WHERE id = ?1", params![session_id])
            .map_err(|e| db_err("Delete error", e))?;
        tx.commit()
            .map_err(|e| db_err("Delete commit error", e))?;

        if affected == 0 {
            return Err(format!("Session {} not found", session_id));
        }
        Ok(())
    }

    /// Append a message to a session. Also bumps `updated_at`.
    pub fn add_message(&self, session_id: &str, msg: &Message) -> Result<(), String> {
        // SM4: Pre-check that the session exists before inserting. Deliberately
        // OUTSIDE the transaction: it is an autocommit read, so it takes no
        // snapshot for the write to upgrade from.
        let count: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE id = ?1",
                params![session_id],
                |r| r.get(0),
            )
            .map_err(|e| db_err("Session check error", e))?;
        if count == 0 {
            return Err(format!("Session {} not found", session_id));
        }

        let role_str = role_to_str(&msg.role);
        let content_json =
            serde_json::to_string(&msg.content).map_err(|e| format!("Serialize content: {}", e))?;
        let tool_calls_json = msg
            .tool_calls
            .as_ref()
            .map(|tc| serde_json::to_string(tc).map_err(|e| format!("Serialize tool_calls: {}", e)))
            .transpose()?;
        let name = msg.name.as_deref();
        let tool_call_id = msg.tool_call_id.as_deref();
        let reasoning = msg.reasoning_content.as_deref();
        // SM5: Use msg.created_at if set, otherwise use current time.
        let created_at = if msg.created_at > 0 {
            msg.created_at
        } else {
            Utc::now().timestamp()
        };
        let now = Utc::now().timestamp();

        // SM1: Wrap INSERT and UPDATE in a single transaction.
        //
        // IMMEDIATE, not deferred: this connection is shared by every session
        // in the process, so a writer elsewhere (a second app instance, the
        // CLI) can appear between BEGIN and the INSERT. Upgrading a deferred
        // read snapshot to a write returns SQLITE_BUSY *without* consulting the
        // busy handler, so `BEGIN IMMEDIATE` is what actually lets the 5 s
        // busy timeout do its job. The RAII transaction also rolls back if
        // COMMIT fails — the hand-rolled version left the connection inside an
        // open transaction, after which every later write failed with "cannot
        // start a transaction within a transaction".
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)
            .map_err(|e| db_err("Begin transaction error", e))?;

        tx.execute(
            "INSERT INTO messages (session_id, role, content, tool_calls, tool_call_id, reasoning_content, name, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                session_id,
                role_str,
                content_json,
                tool_calls_json,
                tool_call_id,
                reasoning,
                name,
                created_at,
            ],
        )
        .map_err(|e| db_err("Insert message error", e))?;

        tx.execute(
            "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
            params![now, session_id],
        )
        .map_err(|e| db_err("Update session timestamp error", e))?;

        tx.commit()
            .map_err(|e| db_err("Commit transaction error", e))?;

        Ok(())
    }

    // ── Grouping ──────────────────────────────────────────────────────────

    /// Return sessions grouped by recency:
    /// today / yesterday / this week / this month / older
    pub fn get_sessions_grouped(&self) -> Result<SessionGroups, String> {
        let all = self.list_sessions()?;
        let today = Utc::now().date_naive();
        let yesterday = today - Duration::days(1);
        // Monday of the current week (Mon=0, …, Sun=6).
        let weekday = today.weekday().num_days_from_monday();
        let week_start = today - Duration::days(weekday as i64);
        let month_start = NaiveDate::from_ymd_opt(today.year(), today.month(), 1)
            .unwrap_or(today);

        let mut groups = SessionGroups {
            today: Vec::new(),
            yesterday: Vec::new(),
            this_week: Vec::new(),
            this_month: Vec::new(),
            older: Vec::new(),
        };

        for sess in all {
            // SM11: Use non-deprecated from_timestamp_millis.
            let sess_date = chrono::DateTime::from_timestamp_millis(sess.updated_at * 1000)
                .map(|dt| dt.date_naive())
                .unwrap_or(today);

            if sess_date == today {
                groups.today.push(sess);
            } else if sess_date == yesterday {
                groups.yesterday.push(sess);
            } else if sess_date >= week_start {
                groups.this_week.push(sess);
            } else if sess_date >= month_start {
                groups.this_month.push(sess);
            } else {
                groups.older.push(sess);
            }
        }

        Ok(groups)
    }

    // ── Retention ─────────────────────────────────────────────────────────

    /// Remove sessions whose `updated_at` is older than `retention_days` days.
    ///
    /// Destructive by design, so it is only reachable through `purge_now()`
    /// (the desktop app's scheduled cleanup) — never from `new()`. It takes a
    /// `VACUUM INTO` snapshot first, and `get_session` bumps `updated_at`, so a
    /// conversation the user still opens is not eligible.
    fn purge_old_sessions(&self) -> Result<(), String> {
        // SM13: retention_days=0 means "keep forever".
        if self.retention_days == 0 {
            return Ok(());
        }
        let cutoff = Utc::now().timestamp() - (self.retention_days as i64 * 86_400);

        // Count first: the common case (every 6 hours, nothing aged out) must
        // not write a backup file.
        let doomed: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE updated_at < ?1",
                params![cutoff],
                |r| r.get(0),
            )
            .map_err(|e| db_err("Purge count failed", e))?;
        if doomed == 0 {
            return Ok(());
        }

        let backup = self.backup_before_purge()?;
        warn!(
            sessions = doomed,
            backup = %backup.display(),
            retention_days = self.retention_days,
            "purging sessions past retention (snapshot written first)"
        );

        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)
            .map_err(|e| db_err("Failed to start purge transaction", e))?;
        // Children first, explicitly (see `delete_session` for why the FK
        // cascade is not trusted).
        tx.execute(
            "DELETE FROM messages WHERE session_id IN
                 (SELECT id FROM sessions WHERE updated_at < ?1)",
            params![cutoff],
        )
        .map_err(|e| db_err("Purge messages failed", e))?;
        tx.execute("DELETE FROM sessions WHERE updated_at < ?1", params![cutoff])
            .map_err(|e| db_err("Purge sessions failed", e))?;
        tx.commit()
            .map_err(|e| db_err("Purge commit failed", e))?;
        Ok(())
    }

    /// Manually run the retention purge (e.g. on a timer or explicit user action).
    pub fn purge_now(&self) -> Result<(), String> {
        self.purge_old_sessions()
    }

    /// Snapshot `sessions.db` with `VACUUM INTO` before a destructive purge.
    /// Cheap insurance: the purge is irreversible and this is the only copy.
    fn backup_before_purge(&self) -> Result<PathBuf, String> {
        let dir = Config::data_dir()
            .map_err(|e| e.to_string())?
            .join("backups");
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("Failed to create backup dir {}: {}", dir.display(), e))?;

        // Millisecond stamp: a second-resolution name collides when two purges
        // run in the same second, and `VACUUM INTO` refuses an existing target.
        let name = format!("sessions-{}.db", Utc::now().format("%Y%m%d-%H%M%S-%3f"));
        let path = dir.join(name);
        let escaped = path.to_string_lossy().replace('\'', "''");
        self.conn
            .execute_batch(&format!("VACUUM INTO '{escaped}'"))
            .map_err(|e| db_err("Backup (VACUUM INTO) failed", e))?;

        Self::prune_backups(&dir);
        Ok(path)
    }

    /// Keep only the newest `BACKUPS_TO_KEEP` session snapshots.
    fn prune_backups(dir: &Path) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut snapshots: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map_or(false, |n| n.starts_with("sessions-") && n.ends_with(".db"))
            })
            .collect();
        if snapshots.len() <= BACKUPS_TO_KEEP {
            return;
        }
        // Names are timestamp-ordered, so a lexical sort is chronological.
        snapshots.sort();
        for old in &snapshots[..snapshots.len() - BACKUPS_TO_KEEP] {
            if let Err(e) = std::fs::remove_file(old) {
                warn!(path = %old.display(), error = %e, "could not remove old session backup");
            }
        }
    }

    // ── Helpers ───────────────────────────────────────────────────────────

    /// Load all messages for a session, ordered by creation time.
    ///
    /// Rows that cannot be decoded are skipped *and counted* — the count goes
    /// back to the caller (`get_session` logs it) instead of vanishing into an
    /// `eprintln!` that a GUI process has nowhere to print.
    fn load_messages(&self, session_id: &str) -> Result<LoadedMessages, String> {
        let mut stmt = self
            .conn
            .prepare(
                // id (AUTOINCREMENT) breaks ties when many rows share the same
                // second-resolution created_at — critical for tool-chain order.
                "SELECT role, content, tool_calls, tool_call_id, reasoning_content, name, created_at
                 FROM messages WHERE session_id = ?1 ORDER BY created_at ASC, id ASC",
            )
            .map_err(|e| db_err("Prepare messages query", e))?;

        let rows = stmt
            .query_map(params![session_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            })
            .map_err(|e| db_err("Query messages error", e))?;

        let mut skipped = 0usize;
        let mut messages: Vec<Message> = Vec::new();
        for row in rows {
            let (role_str, content_json, tool_calls_json, tool_call_id, reasoning_content, name, created_at) =
                match row {
                    Ok(tuple) => tuple,
                    Err(e) => {
                        debug!(error = %e, "skipping corrupt message row");
                        skipped += 1;
                        continue;
                    }
                };

            // SM10: str_to_role now returns Result; skip on unknown role.
            let role = match str_to_role(&role_str) {
                Ok(r) => r,
                Err(e) => {
                    debug!(%e, "skipping message with unknown role");
                    skipped += 1;
                    continue;
                }
            };

            let content: MessageContent = match serde_json::from_str(&content_json) {
                Ok(c) => c,
                Err(e) => {
                    debug!(error = %e, "skipping message with corrupt content");
                    skipped += 1;
                    continue;
                }
            };

            let tool_calls: Option<Vec<ToolCall>> = match tool_calls_json {
                Some(ref s) => match serde_json::from_str(s) {
                    Ok(tc) => Some(tc),
                    Err(e) => {
                        debug!(error = %e, "skipping message with corrupt tool_calls");
                        skipped += 1;
                        continue;
                    }
                },
                None => None,
            };

            messages.push(Message {
                role,
                content,
                name,
                tool_calls,
                tool_call_id,
                reasoning_content,
                created_at,
            });
        }

        Self::validate_tool_chain(&mut messages);

        // SM9: Filter out ghost messages (empty assistant with no content/tools/reasoning).
        let before_ghosts = messages.len();
        messages.retain(|m| {
            !(m.role == Role::Assistant
                && m.content.is_empty()
                && m.tool_calls.is_none()
                && m.reasoning_content.is_none())
        });
        if messages.len() != before_ghosts {
            debug!(
                removed = before_ghosts - messages.len(),
                "removed empty assistant messages"
            );
        }

        Ok(LoadedMessages { messages, skipped })
    }

    /// Strip orphaned tool_calls and their tool messages.
    ///
    /// Also merges consecutive assistant messages that each carry tool_calls into
    /// a single assistant message. Legacy persistence wrote one assistant per
    /// ToolStart, which OpenAI-compat APIs reject on the next turn:
    /// "assistant message with tool_calls must be followed by tool messages…"
    fn validate_tool_chain(messages: &mut Vec<Message>) {
        // Remove consecutive duplicate messages (same role, same content, same tool metadata).
        // Ignores created_at since duplicates are persisted within the same second.
        let before_count = messages.len();
        let mut i = 1;
        let mut deduped = 0u32;
        while i < messages.len() {
            let same_role = messages[i-1].role == messages[i].role;
            let same_content = messages[i-1].content == messages[i].content;
            // Compare tool_calls by ID only — arguments can differ between copies
            let same_tc_ids = messages[i-1].tool_calls.as_ref().map(|tc| tc.iter().map(|t| &t.id).collect::<Vec<_>>())
                == messages[i].tool_calls.as_ref().map(|tc| tc.iter().map(|t| &t.id).collect::<Vec<_>>());
            let same_tci = messages[i-1].tool_call_id == messages[i].tool_call_id;
            let same_rc = messages[i-1].reasoning_content == messages[i].reasoning_content;
            let same_name = messages[i-1].name == messages[i].name;
            if same_role && same_content && same_tc_ids && same_tci && same_rc && same_name
            {
                // Keep the NEWER row. A failed turn persists nothing
                // (`chat.rs` skips an empty assistant reply), so retrying the
                // same text leaves two adjacent identical user rows — and the
                // model must answer from the retry, not the stale first copy.
                debug!(index = i - 1, "deduplicating duplicate message (keeping the newer row)");
                messages.remove(i - 1);
                deduped += 1;
                // The kept row now sits at `i - 1`; step back so it is compared
                // against its new predecessor.
                i = i.saturating_sub(1).max(1);
            } else {
                i += 1;
            }
        }
        if deduped > 0 {
            debug!(removed = deduped, of = before_count, "dedup summary");
        }

        Self::merge_consecutive_tool_call_assistants(messages);

        let responded: std::collections::HashSet<String> = messages
            .iter()
            .filter(|m| m.role == Role::Tool)
            .filter_map(|m| m.tool_call_id.clone())
            .collect();
        for m in messages.iter_mut() {
            if let Some(ref mut tc) = m.tool_calls {
                tc.retain(|t| responded.contains(&t.id));
                if tc.is_empty() {
                    m.tool_calls = None;
                }
            }
            // CRITICAL: tool_call_id on the Message envelope belongs ONLY to
            // Role::Tool messages. Assistant messages carry tool call IDs inside
            // the tool_calls[].id field. Setting tool_call_id on an Assistant
            // message violates OpenAI protocol and causes DeepSeek 400 errors:
            // "insufficient tool messages following tool_calls message"
            if m.role == Role::Assistant && m.tool_call_id.is_some() {
                m.tool_call_id = None;
            }
        }
        let valid_ids: std::collections::HashSet<String> = messages
            .iter()
            .filter_map(|m| m.tool_calls.as_ref())
            .flat_map(|tc| tc.iter().map(|t| t.id.clone()))
            .collect();
        let before_orphans = messages.len();
        messages.retain(|m| {
            if m.role != Role::Tool {
                return true;
            }
            m.tool_call_id
                .as_ref()
                .map_or(false, |id| valid_ids.contains(id))
        });
        if messages.len() != before_orphans {
            debug!(
                removed = before_orphans - messages.len(),
                "dropped orphaned tool messages"
            );
        }
    }

    /// Collapse `assistant([A]) assistant([B]) tool(A) tool(B)` →
    /// `assistant([A,B]) tool(A) tool(B)`.
    fn merge_consecutive_tool_call_assistants(messages: &mut Vec<Message>) {
        let mut i = 0;
        while i < messages.len() {
            let has_tc = messages[i].role == Role::Assistant
                && messages[i]
                    .tool_calls
                    .as_ref()
                    .map(|tc| !tc.is_empty())
                    .unwrap_or(false);
            if !has_tc {
                i += 1;
                continue;
            }
            let mut j = i + 1;
            while j < messages.len()
                && messages[j].role == Role::Assistant
                && messages[j]
                    .tool_calls
                    .as_ref()
                    .map(|tc| !tc.is_empty())
                    .unwrap_or(false)
            {
                j += 1;
            }
            if j > i + 1 {
                let mut combined = messages[i].tool_calls.take().unwrap_or_default();
                for k in (i + 1)..j {
                    if let Some(tcs) = messages[k].tool_calls.take() {
                        for tc in tcs {
                            if !combined.iter().any(|x| x.id == tc.id) {
                                combined.push(tc);
                            }
                        }
                    }
                    // Fold non-empty content / reasoning into the head message.
                    if messages[i].content.is_empty() && !messages[k].content.is_empty() {
                        messages[i].content = messages[k].content.clone();
                    }
                    if messages[i].reasoning_content.as_ref().map_or(true, |r| r.is_empty()) {
                        if let Some(ref rc) = messages[k].reasoning_content {
                            if !rc.is_empty() {
                                messages[i].reasoning_content = Some(rc.clone());
                            }
                        }
                    }
                }
                messages[i].tool_calls = Some(combined);
                messages.drain((i + 1)..j);
                debug!(
                    merged = j - i,
                    "merged consecutive tool-call assistant messages"
                );
            }
            i += 1;
        }
    }
}

// ── Error formatting ───────────────────────────────────────────────────

/// Format a `rusqlite` error with the SQLite result code preserved.
///
/// `format!("...: {e}")` keeps the message text but throws away the variant and
/// the result code, so `SQLITE_BUSY` (retryable) and `SQLITE_CORRUPT` (fatal)
/// reach every caller as the same opaque string.
fn db_err(context: &str, e: rusqlite::Error) -> String {
    match e.sqlite_error() {
        Some(err) => {
            let retryable = matches!(
                err.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            );
            format!(
                "{context}: SQLite code {} ({:?}, {}): {e}",
                err.extended_code,
                err.code,
                if retryable { "retryable" } else { "not retryable" },
            )
        }
        None => format!("{context}: {e}"),
    }
}

// ── Role serialization helpers ─────────────────────────────────────────

fn role_to_str(role: &Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn str_to_role(s: &str) -> Result<Role, String> {
    match s {
        "system" => Ok(Role::System),
        "user" => Ok(Role::User),
        "assistant" => Ok(Role::Assistant),
        "tool" => Ok(Role::Tool),
        _ => Err(format!("Unknown role '{}'", s)), // SM10: error on unknown roles
    }
}

// ── Extension trait for rusqlite Optional ──────────────────────────────

/// Small helper to turn a rusqlite Result into an Option.
trait Optional<T> {
    fn optional(self) -> Result<Option<T>, rusqlite::Error>;
}

impl<T> Optional<T> for Result<T, rusqlite::Error> {
    fn optional(self) -> Result<Option<T>, rusqlite::Error> {
        match self {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tool_chain_tests {
    use super::SessionManager;
    use crate::providers::trait_def::{
        FunctionCall, Message, MessageContent, Role, ToolCall,
    };

    fn tc(id: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            call_type: "function".into(),
            function: FunctionCall {
                name: "do_web_search".into(),
                arguments: "{}".into(),
            },
        }
    }

    fn assistant_one(id: &str) -> Message {
        Message {
            role: Role::Assistant,
            content: MessageContent::Text(String::new()),
            name: None,
            tool_calls: Some(vec![tc(id)]),
            tool_call_id: None,
            reasoning_content: None,
            created_at: 0,
        }
    }

    fn tool(id: &str) -> Message {
        Message {
            role: Role::Tool,
            content: MessageContent::Text("ok".into()),
            name: None,
            tool_calls: None,
            tool_call_id: Some(id.into()),
            reasoning_content: None,
            created_at: 0,
        }
    }

    #[test]
    fn merge_split_parallel_tool_starts() {
        let mut msgs = vec![
            Message {
                role: Role::User,
                content: MessageContent::Text("search".into()),
                ..Default::default()
            },
            assistant_one("call_1"),
            assistant_one("call_2"),
            tool("call_1"),
            tool("call_2"),
        ];
        SessionManager::merge_consecutive_tool_call_assistants(&mut msgs);
        assert_eq!(msgs.len(), 4);
        let ids: Vec<_> = msgs[1]
            .tool_calls
            .as_ref()
            .unwrap()
            .iter()
            .map(|t| t.id.as_str())
            .collect();
        assert_eq!(ids, vec!["call_1", "call_2"]);
    }

    #[test]
    fn validate_tool_chain_repairs_legacy_history() {
        let mut msgs = vec![
            Message {
                role: Role::User,
                content: MessageContent::Text("q".into()),
                ..Default::default()
            },
            assistant_one("a"),
            assistant_one("b"),
            tool("a"),
            tool("b"),
            Message {
                role: Role::Assistant,
                content: MessageContent::Text("answer".into()),
                ..Default::default()
            },
        ];
        SessionManager::validate_tool_chain(&mut msgs);
        let tc_assts: Vec<_> = msgs
            .iter()
            .filter(|m| m.role == Role::Assistant && m.tool_calls.is_some())
            .collect();
        assert_eq!(tc_assts.len(), 1);
        assert_eq!(tc_assts[0].tool_calls.as_ref().unwrap().len(), 2);
        assert_eq!(msgs.iter().filter(|m| m.role == Role::Tool).count(), 2);
    }
}

#[cfg(test)]
mod migration_tests {
    use super::*;
    use crate::providers::trait_def::{Message, MessageContent, Role};

    fn version(conn: &Connection) -> i64 {
        conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap()
    }

    fn index_exists(conn: &Connection, table: &str, name: &str) -> bool {
        let mut stmt = conn.prepare(&format!("PRAGMA index_list({table})")).unwrap();
        let mut rows = stmt.query([]).unwrap();
        while let Some(row) = rows.next().unwrap() {
            if row.get::<_, String>(1).unwrap() == name {
                return true;
            }
        }
        false
    }

    #[test]
    fn fresh_database_migrates_to_v2_with_every_column() {
        let conn = Connection::open_in_memory().unwrap();
        SessionManager::migrate(&conn).unwrap();
        assert_eq!(version(&conn), SCHEMA_VERSION);
        assert!(SessionManager::column_exists(&conn, "sessions", "workspace").unwrap());
        assert!(SessionManager::column_exists(&conn, "sessions", "model").unwrap());
        assert!(SessionManager::column_exists(&conn, "messages", "name").unwrap());
        assert!(index_exists(&conn, "sessions", "idx_sessions_updated"));
    }

    /// A database already at v1 (real user data, no index on `sessions`) gets
    /// the v2 index and nothing else changes.
    #[test]
    fn v1_database_gains_the_sessions_index() {
        let conn = Connection::open_in_memory().unwrap();
        SessionManager::migrate(&conn).unwrap();
        conn.execute_batch(
            "DROP INDEX idx_sessions_updated; PRAGMA user_version = 1;
             INSERT INTO sessions (id, title, workspace, model, created_at, updated_at)
                 VALUES ('s1', '手动改过的名字', '/w', '', 5, 5);",
        )
        .unwrap();

        SessionManager::migrate(&conn).unwrap();
        assert_eq!(version(&conn), SCHEMA_VERSION);
        assert!(index_exists(&conn, "sessions", "idx_sessions_updated"));
        let title: String = conn
            .query_row("SELECT title FROM sessions WHERE id = 's1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(title, "手动改过的名字");
    }

    /// The shape of the user's existing database: tables present, the three
    /// post-hoc columns missing, `user_version` still 0.
    #[test]
    fn legacy_database_is_upgraded_in_place_and_idempotently() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                 id TEXT PRIMARY KEY, title TEXT NOT NULL,
                 created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
             CREATE TABLE messages (
                 id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL,
                 role TEXT NOT NULL, content TEXT NOT NULL, tool_calls TEXT,
                 tool_call_id TEXT, reasoning_content TEXT, created_at INTEGER NOT NULL);
             INSERT INTO sessions (id, title, created_at, updated_at) VALUES ('s1', 'kept', 1, 1);",
        )
        .unwrap();

        SessionManager::migrate(&conn).unwrap();
        assert_eq!(version(&conn), SCHEMA_VERSION);

        // Existing data survives and picks up the column default.
        let (title, model): (String, String) = conn
            .query_row("SELECT title, COALESCE(model, '') FROM sessions WHERE id = 's1'", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(title, "kept");
        assert_eq!(model, "");

        // A second open sees the current version and does nothing.
        SessionManager::migrate(&conn).unwrap();
        assert_eq!(version(&conn), SCHEMA_VERSION);
    }

    /// Opt-in check against a *copy* of a real `sessions.db`, which is the only
    /// way to be sure the migration is safe on the shapes production actually
    /// produces. Run with:
    ///
    /// ```text
    /// DSCODE_MIGRATION_DB=/path/to/copy.db cargo test -p dscode-core --lib \
    ///     migrates_a_real_database_copy -- --ignored --nocapture
    /// ```
    ///
    /// Never point this at `~/.dscode/sessions.db` itself (see the module docs).
    #[test]
    #[ignore = "manual: needs DSCODE_MIGRATION_DB pointing at a copy of a real sessions.db"]
    fn migrates_a_real_database_copy() {
        let path = std::env::var("DSCODE_MIGRATION_DB")
            .expect("set DSCODE_MIGRATION_DB to a COPY of a real sessions.db");
        let conn = Connection::open(&path).unwrap();

        let before: (i64, i64) = conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM sessions), (SELECT COUNT(*) FROM messages)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();

        SessionManager::migrate(&conn).unwrap();

        let after: (i64, i64) = conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM sessions), (SELECT COUNT(*) FROM messages)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(before, after, "migration changed the row counts");
        assert_eq!(version(&conn), SCHEMA_VERSION);
        assert!(index_exists(&conn, "sessions", "idx_sessions_updated"));

        let sessions: Vec<(String, String)> = {
            let mut stmt = conn
                .prepare("SELECT id, title FROM sessions ORDER BY updated_at DESC")
                .unwrap();
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
            rows.map(|r| r.unwrap()).collect()
        };
        eprintln!(
            "migrated {} sessions / {} messages in {}",
            after.0, after.1, path
        );
        assert!(
            sessions.iter().all(|(_, t)| !t.trim().is_empty()),
            "a session lost its title"
        );
        // The list query the index exists for must still be served from it.
        let plan: String = conn
            .query_row(
                "EXPLAIN QUERY PLAN SELECT id, title FROM sessions ORDER BY updated_at DESC",
                [],
                |r| r.get(3),
            )
            .unwrap();
        eprintln!("query plan: {plan}");
        assert!(
            plan.contains("idx_sessions_updated"),
            "ORDER BY updated_at DESC still does not use the index: {plan}"
        );
    }

    #[test]
    fn dedup_keeps_the_newer_of_two_identical_rows() {
        let msg = |created_at| Message {
            role: Role::User,
            content: MessageContent::Text("继续".into()),
            created_at,
            ..Default::default()
        };
        let mut msgs = vec![msg(100), msg(200)];
        SessionManager::validate_tool_chain(&mut msgs);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].created_at, 200, "the stale copy survived");
    }

    /// `delete_session` must not depend on `PRAGMA foreign_keys`, which is a
    /// no-op inside a transaction.
    #[test]
    fn delete_session_removes_messages_without_the_fk_pragma() {
        let conn = Connection::open_in_memory().unwrap();
        SessionManager::migrate(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO sessions (id, title, workspace, model, created_at, updated_at)
                 VALUES ('s1', 't', '', '', 1, 1);
             INSERT INTO messages (session_id, role, content, created_at)
                 VALUES ('s1', 'user', '\"hi\"', 1);",
        )
        .unwrap();
        // Foreign keys deliberately left OFF.
        let mgr = SessionManager { conn, retention_days: 0 };
        mgr.delete_session("s1").unwrap();
        let left: i64 = mgr
            .conn
            .query_row("SELECT COUNT(*) FROM messages WHERE session_id = 's1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 0);
    }
}

#[cfg(test)]
mod title_tests {
    use super::{Connection, SessionManager};

    #[test]
    fn derive_plain_message() {
        let t = SessionManager::derive_title_from_message("修复登录模块的 token 过期问题");
        assert!(t.contains("修复登录"));
        assert!(t.chars().count() <= 40);
    }

    #[test]
    fn derive_plan_command() {
        let t = SessionManager::derive_title_from_message("/plan 实现用户注册流程");
        assert!(t.starts_with("Plan · "));
        assert!(t.contains("实现用户注册"));
    }

    #[test]
    fn derive_empty_plan() {
        // Bare /plan is control-only — does not produce a sticky title
        assert!(SessionManager::is_control_only_message("/plan"));
        let t = SessionManager::derive_title_from_message("/plan");
        assert_eq!(t, "新对话");
    }

    #[test]
    fn placeholder_detection() {
        assert!(SessionManager::is_placeholder_title("对话 03:08"));
        assert!(SessionManager::is_placeholder_title("📂 DS_code")); // legacy
        assert!(SessionManager::is_placeholder_title("新对话"));
        assert!(SessionManager::is_placeholder_title("Teams · 多 Agent 协作"));
        assert!(!SessionManager::is_placeholder_title("Plan · 实现登录"));
        assert!(!SessionManager::is_placeholder_title("手动改过的名字"));
    }

    #[test]
    fn control_only_skipped() {
        assert!(SessionManager::is_control_only_message("/teams"));
        assert!(SessionManager::is_control_only_message("/teams off"));
        assert!(!SessionManager::is_control_only_message("/teams 做一个番茄钟"));
    }

    #[test]
    fn derive_teams_with_body() {
        let t = SessionManager::derive_title_from_message("/teams 做一个番茄时钟");
        assert!(t.starts_with("Teams · "));
        assert!(t.contains("番茄"));
    }

    #[test]
    fn derive_skill_slash() {
        let t = SessionManager::derive_title_from_message("/grill-me 澄清登录需求");
        assert!(t.contains("澄清登录"));
        assert!(!t.starts_with('/'));
    }

    #[test]
    fn provisional_from_workspace() {
        assert_eq!(
            SessionManager::provisional_title("/Users/zay/Desktop/DS_code"),
            "对话 DS_code"
        );
        // The prefix is what makes it renameable by the first message.
        assert!(SessionManager::is_placeholder_title(
            &SessionManager::provisional_title("/Users/zay/Desktop/DS_code")
        ));
        assert_eq!(SessionManager::provisional_title(""), "新对话");
    }

    /// A session created before its first message must not be nameless — the
    /// desktop modal relies on the manager filling the title in.
    #[test]
    fn create_session_fills_an_empty_title() {
        let conn = Connection::open_in_memory().unwrap();
        SessionManager::migrate(&conn).unwrap();
        let sm = SessionManager { conn, retention_days: 0 };

        let session = sm.create_session("  ", "/tmp/proj", "m").unwrap();
        assert_eq!(session.title, "对话 proj");
        assert!(SessionManager::is_placeholder_title(&session.title));

        // …and the first message can then rename it.
        let renamed = sm
            .maybe_auto_title(&session.id, "修复登录模块的 token 过期")
            .unwrap();
        assert!(renamed.is_some());
    }

    #[test]
    fn empty_title_is_replaced_from_the_first_message() {
        let conn = Connection::open_in_memory().unwrap();
        SessionManager::migrate(&conn).unwrap();
        let sm = SessionManager { conn, retention_days: 0 };
        let session = sm.create_session("对话 proj", "/tmp/proj", "m").unwrap();

        assert_eq!(
            sm.maybe_auto_title(&session.id, "修复登录模块的 token 过期").unwrap(),
            Some("修复登录模块的 token 过期".into())
        );
        // A second message never renames: the title is no longer a placeholder.
        assert_eq!(sm.maybe_auto_title(&session.id, "再改一次").unwrap(), None);
    }

    /// The LLM title lands only on the exact string the deterministic pass
    /// wrote, so a manual rename during the model call always wins.
    #[test]
    fn llm_title_cas_respects_a_manual_rename() {
        let conn = Connection::open_in_memory().unwrap();
        SessionManager::migrate(&conn).unwrap();
        let sm = SessionManager { conn, retention_days: 0 };
        let session = sm.create_session("新对话", "/tmp/proj", "m").unwrap();

        // Happy path: the derived title is still there when the model answers.
        assert!(sm
            .replace_title_if_unchanged(&session.id, "新对话", "修复 token 过期")
            .unwrap());
        assert_eq!(sm.get_session(&session.id).unwrap().unwrap().title, "修复 token 过期");

        // The user renames while a second naming call is in flight.
        sm.update_title(&session.id, "我自己起的名字").unwrap();
        assert!(!sm
            .replace_title_if_unchanged(&session.id, "修复 token 过期", "模型想改的名字")
            .unwrap());
        assert_eq!(sm.get_session(&session.id).unwrap().unwrap().title, "我自己起的名字");
    }

    #[test]
    fn llm_title_refuses_a_placeholder_or_empty_candidate() {
        let conn = Connection::open_in_memory().unwrap();
        SessionManager::migrate(&conn).unwrap();
        let sm = SessionManager { conn, retention_days: 0 };
        let session = sm.create_session("新对话", "/tmp/proj", "m").unwrap();

        assert!(!sm.replace_title_if_unchanged(&session.id, "新对话", "  ").unwrap());
        assert!(!sm.replace_title_if_unchanged(&session.id, "新对话", "新对话").unwrap());
        assert_eq!(sm.get_session(&session.id).unwrap().unwrap().title, "新对话");
    }
}

