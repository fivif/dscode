//! SessionManager — persist chat sessions in SQLite.
//!
//! Sessions are stored in ~/.dscode/sessions.db with two tables:
//! - `sessions`: id, title, created_at, updated_at
//! - `messages`: id, session_id, role, content, tool_calls, tool_call_id, reasoning_content, created_at

use chrono::{Datelike, Duration, NaiveDate, Utc};
use rusqlite::{params, Connection};
use serde_json;
use std::path::PathBuf;
use uuid::Uuid;

use crate::config::settings::Config;
use crate::providers::trait_def::{Message, MessageContent, Role, ToolCall};

/// A single chat session with all associated messages.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub workspace: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub messages: Vec<Message>,
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
            .map_err(|e| format!("Failed to open database at {:?}: {}", db_path, e))?;

        // Enable WAL mode for better concurrent read performance.
        conn.execute_batch("PRAGMA journal_mode=WAL;")
            .map_err(|e| format!("Failed to set WAL mode: {}", e))?;

        // Run migrations.
        conn.execute_batch(
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
        .map_err(|e| format!("Migration failed: {}", e))?;

        // Enable foreign keys.
        conn.execute_batch("PRAGMA foreign_keys = ON;")
            .map_err(|e| format!("Failed to enable foreign keys: {}", e))?;

        // Migration: add workspace column if missing (non-fatal if already exists)
        conn.execute_batch("ALTER TABLE sessions ADD COLUMN workspace TEXT NOT NULL DEFAULT ''").ok();

        // Migration: add name column to messages if missing (non-fatal if already exists)
        conn.execute_batch("ALTER TABLE messages ADD COLUMN name TEXT").ok();

        let mgr = Self {
            conn,
            retention_days,
        };

        // Purge sessions past retention on open.
        mgr.purge_old_sessions()?;

        Ok(mgr)
    }

    /// Resolve the database path: ~/.dscode/sessions.db
    fn db_path() -> Result<PathBuf, String> {
        Config::data_dir().map(|d| d.join("sessions.db")).map_err(|e| e.to_string())
    }

    // ── CRUD ──────────────────────────────────────────────────────────────

    /// Create a new session and return it (with empty messages).
    pub fn create_session(&self, title: &str, workspace: &str) -> Result<Session, String> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().timestamp();

        self.conn
            .execute(
                "INSERT INTO sessions (id, title, workspace, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id, title, workspace, now, now],
            )
            .map_err(|e| format!("Failed to create session: {}", e))?;

        Ok(Session {
            id,
            title: title.to_string(),
            workspace: workspace.to_string(),
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
            Err(e) => Err(format!("Failed to get last session: {}", e)),
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
            .map_err(|e| format!("Failed to update workspace: {}", e))?;
        if affected == 0 {
            Err("Session not found".into())
        } else {
            Ok(())
        }
    }

    /// Load a session by id, including all messages ordered by creation time.
    pub fn get_session(&self, session_id: &str) -> Result<Option<Session>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, title, workspace, created_at, updated_at FROM sessions WHERE id = ?1")
            .map_err(|e| format!("Prepare error: {}", e))?;

        let session_row = stmt
            .query_row(params![session_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })
            .optional()
            .map_err(|e| format!("Query error: {}", e))?;

        match session_row {
            None => Ok(None),
            Some((id, title, workspace, created_at, updated_at)) => {
                let messages = self.load_messages(&id)?;
                Ok(Some(Session {
                    id,
                    title,
                    workspace,
                    created_at,
                    updated_at,
                    messages,
                }))
            }
        }
    }

    /// List all sessions, most-recently-updated first. Messages are NOT loaded.
    pub fn list_sessions(&self) -> Result<Vec<Session>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, title, workspace, created_at, updated_at FROM sessions ORDER BY updated_at DESC")
            .map_err(|e| format!("Prepare error: {}", e))?;

        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })
            .map_err(|e| format!("Query error: {}", e))?;

        let mut sessions = Vec::new();
        for row in rows {
            let (id, title, workspace, created_at, updated_at) = row.map_err(|e| format!("Row error: {}", e))?;
            sessions.push(Session {
                id,
                title,
                workspace,
                created_at,
                updated_at,
                messages: Vec::new(),
            });
        }

        Ok(sessions)
    }

    /// Delete a session and all its messages (CASCADE).
    pub fn delete_session(&self, session_id: &str) -> Result<(), String> {
        // SM3: Ensure foreign keys are enforced for CASCADE delete.
        self.conn.execute_batch("PRAGMA foreign_keys = ON;").ok();

        let affected = self
            .conn
            .execute("DELETE FROM sessions WHERE id = ?1", params![session_id])
            .map_err(|e| format!("Delete error: {}", e))?;

        if affected == 0 {
            return Err(format!("Session {} not found", session_id));
        }
        Ok(())
    }

    /// Append a message to a session. Also bumps `updated_at`.
    pub fn add_message(&self, session_id: &str, msg: &Message) -> Result<(), String> {
        // SM4: Pre-check that the session exists before inserting.
        let count: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE id = ?1",
                params![session_id],
                |r| r.get(0),
            )
            .map_err(|e| format!("Session check error: {}", e))?;
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
        self.conn
            .execute_batch("BEGIN;")
            .map_err(|e| format!("Begin transaction error: {}", e))?;

        let insert_result = self.conn.execute(
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
        );
        if let Err(e) = insert_result {
            self.conn.execute_batch("ROLLBACK;").ok();
            return Err(format!("Insert message error: {}", e));
        }

        let update_result = self.conn.execute(
            "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
            params![now, session_id],
        );
        if let Err(e) = update_result {
            self.conn.execute_batch("ROLLBACK;").ok();
            return Err(format!("Update session timestamp error: {}", e));
        }

        self.conn
            .execute_batch("COMMIT;")
            .map_err(|e| format!("Commit transaction error: {}", e))?;

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
    fn purge_old_sessions(&self) -> Result<(), String> {
        // SM13: retention_days=0 means "keep forever".
        if self.retention_days == 0 {
            return Ok(());
        }
        // SM3: Ensure foreign keys are enforced for CASCADE delete.
        self.conn.execute_batch("PRAGMA foreign_keys = ON;").ok();

        let cutoff = Utc::now().timestamp() - (self.retention_days as i64 * 86_400);
        self.conn
            .execute("DELETE FROM sessions WHERE updated_at < ?1", params![cutoff])
            .map_err(|e| format!("Purge error: {}", e))?;
        Ok(())
    }

    /// Manually run the retention purge (e.g. on a timer or explicit user action).
    pub fn purge_now(&self) -> Result<(), String> {
        self.purge_old_sessions()
    }

    // ── Helpers ───────────────────────────────────────────────────────────

    /// Load all messages for a session, ordered by creation time.
    fn load_messages(&self, session_id: &str) -> Result<Vec<Message>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT role, content, tool_calls, tool_call_id, reasoning_content, name, created_at
                 FROM messages WHERE session_id = ?1 ORDER BY created_at ASC",
            )
            .map_err(|e| format!("Prepare messages query: {}", e))?;

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
            .map_err(|e| format!("Query messages error: {}", e))?;

        // SM7: Use iterator with filter_map to skip corrupt rows instead of failing.
        let mut messages: Vec<Message> = rows
            .filter_map(|row| {
                let (role_str, content_json, tool_calls_json, tool_call_id, reasoning_content, name, created_at) =
                    match row {
                        Ok(tuple) => tuple,
                        Err(e) => {
                            eprintln!(
                                "[SessionManager] Skipping corrupt message row: {}",
                                e
                            );
                            return None;
                        }
                    };

                // SM10: str_to_role now returns Result; skip on unknown role.
                let role = match str_to_role(&role_str) {
                    Ok(r) => r,
                    Err(e) => {
                        eprintln!(
                            "[SessionManager] Skipping message with {}",
                            e
                        );
                        return None;
                    }
                };

                let content: MessageContent = match serde_json::from_str(&content_json) {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!(
                            "[SessionManager] Skipping message with corrupt content: {}",
                            e
                        );
                        return None;
                    }
                };

                let tool_calls: Option<Vec<ToolCall>> = match tool_calls_json {
                    Some(ref s) => match serde_json::from_str(s) {
                        Ok(tc) => Some(tc),
                        Err(e) => {
                            eprintln!(
                                "[SessionManager] Skipping message with corrupt tool_calls: {}",
                                e
                            );
                            return None;
                        }
                    },
                    None => None,
                };

                Some(Message {
                    role,
                    content,
                    name,
                    tool_calls,
                    tool_call_id,
                    reasoning_content,
                    created_at,
                })
            })
            .collect();

        Self::validate_tool_chain(&mut messages);

        // SM9: Filter out ghost messages (empty assistant with no content/tools/reasoning).
        messages.retain(|m| {
            if m.role == Role::Assistant
                && m.content.is_empty()
                && m.tool_calls.is_none()
                && m.reasoning_content.is_none()
            {
                eprintln!("[SessionManager] Removing ghost assistant message");
                false
            } else {
                true
            }
        });

        Ok(messages)
    }

    /// Strip orphaned tool_calls and their tool messages.
    fn validate_tool_chain(messages: &mut Vec<Message>) {
        let responded: std::collections::HashSet<String> = messages
            .iter()
            .filter(|m| m.role == Role::Tool)
            .filter_map(|m| m.tool_call_id.clone())
            .collect();
        for m in messages.iter_mut() {
            if let Some(ref mut tc) = m.tool_calls {
                // SM8: Track which tool call IDs are being orphaned.
                let orphaned_ids: std::collections::HashSet<String> = tc
                    .iter()
                    .filter(|t| !responded.contains(&t.id))
                    .map(|t| t.id.clone())
                    .collect();
                tc.retain(|t| responded.contains(&t.id));
                if tc.is_empty() {
                    m.tool_calls = None;
                    // SM8: Only clear tool_call_id on Assistant messages when
                    // ALL their tool_calls are orphaned AND tool_call_id matches
                    // one of the orphaned IDs.
                    if m.role == Role::Assistant {
                        if let Some(ref tci) = m.tool_call_id {
                            if orphaned_ids.contains(tci) {
                                m.tool_call_id = None;
                            }
                        }
                    }
                }
            }
        }
        let valid_ids: std::collections::HashSet<String> = messages
            .iter()
            .filter_map(|m| m.tool_calls.as_ref())
            .flat_map(|tc| tc.iter().map(|t| t.id.clone()))
            .collect();
        messages.retain(|m| {
            if m.role != Role::Tool {
                return true;
            }
            m.tool_call_id
                .as_ref()
                .map_or(false, |id| valid_ids.contains(id))
        });
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
