//! Memory store — SQLite database for the 3-tier memory system.
//! Connection is held under a Mutex so the store is `Send + Sync` safe.

use rusqlite::{params, Connection, Transaction, TransactionBehavior};
use std::path::PathBuf;
use std::sync::Mutex;
use tracing::{debug, warn};

use super::fact::Fact;
use super::fts::{ensure_fts, query_tokens, search_memory};
use super::pattern::Pattern;
use super::raw::RawMessage;

/// Schema version of `memory.db`, tracked in `PRAGMA user_version`.
/// 1 = base tables + FTS index + the one-time FTS reconciliation.
const SCHEMA_VERSION: i64 = 1;

/// Raw messages older than this are pruned on ingest. Raw messages are the
/// verbatim audit tier: the largest table and the least useful once stale, but
/// still real user data, so the window is deliberately generous.
pub const RAW_RETENTION_DAYS: i64 = 90;

const SCHEMA_DDL: &str = "
    CREATE TABLE IF NOT EXISTS raw_messages (
       id TEXT PRIMARY KEY,
       session_id TEXT NOT NULL,
       role TEXT NOT NULL,
       content TEXT NOT NULL,
       created_at INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS facts (
       id TEXT PRIMARY KEY,
       session_id TEXT NOT NULL,
       subject TEXT NOT NULL,
       predicate TEXT NOT NULL,
       object TEXT NOT NULL,
       confidence REAL NOT NULL DEFAULT 0.7,
       created_at INTEGER NOT NULL
    );
    CREATE UNIQUE INDEX IF NOT EXISTS idx_facts_spo
       ON facts(session_id, subject, predicate, object);
    -- Supports the session-scoped FTS search, which probes facts by SPO.
    CREATE INDEX IF NOT EXISTS idx_facts_spo_obj
       ON facts(subject, predicate, object);
    CREATE TABLE IF NOT EXISTS patterns (
       id TEXT PRIMARY KEY,
       name TEXT NOT NULL UNIQUE,
       description TEXT NOT NULL,
       occurrence_count INTEGER NOT NULL DEFAULT 1,
       last_seen_at INTEGER NOT NULL,
       tags TEXT NOT NULL DEFAULT '[]'
    );
    CREATE INDEX IF NOT EXISTS idx_facts_session ON facts(session_id);
    CREATE INDEX IF NOT EXISTS idx_raw_session ON raw_messages(session_id);
    -- Retention prunes raw_messages by created_at.
    CREATE INDEX IF NOT EXISTS idx_raw_created ON raw_messages(created_at);
    CREATE UNIQUE INDEX IF NOT EXISTS idx_patterns_name ON patterns(name);
";

/// One-time reconciliation between `facts` and `memory_fts`.
///
/// `insert_fact` used to write the two tables in three separate autocommit
/// statements, so a crash between them could leave a fact that can never be
/// found. Index rows whose fact is gone are left alone — the session-scoped
/// search ignores them, and deleting derived-but-present data is the riskier
/// direction.
const SCHEMA_BACKFILL: &str = "
    INSERT INTO memory_fts (subject, predicate, object, content)
    SELECT f.subject, f.predicate, f.object,
           f.subject || ' ' || f.predicate || ' ' || f.object
    FROM facts f
    WHERE NOT EXISTS (
        SELECT 1 FROM memory_fts m
        WHERE m.subject = f.subject
          AND m.predicate = f.predicate
          AND m.object = f.object
    );
";

pub struct MemoryStore {
    conn: Mutex<Connection>,
}

impl MemoryStore {
    pub fn new(path: PathBuf) -> Result<Self, rusqlite::Error> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        Self::init(Connection::open(path)?)
    }

    /// In-memory store, used as a degraded fallback when the on-disk database
    /// cannot be opened. Nothing persists, but no operation fails.
    pub fn open_in_memory() -> Result<Self, rusqlite::Error> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self, rusqlite::Error> {
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;

        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version < SCHEMA_VERSION {
            // One transaction and one version bump. A failure anywhere —
            // including a concurrent writer holding the lock — rolls the whole
            // thing back and leaves user_version at 0, so the next open retries
            // instead of running against a half-built schema.
            let tx = Transaction::new_unchecked(&conn, TransactionBehavior::Immediate)?;
            tx.execute_batch(SCHEMA_DDL)?;
            // The FTS table must exist before the backfill that fills it. On a
            // database that migrated before FTS existed this is the statement
            // that creates it.
            ensure_fts(&tx)?;
            tx.execute_batch(SCHEMA_BACKFILL)?;
            tx.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))?;
            tx.commit()?;
            debug!(from = version, to = SCHEMA_VERSION, "memory.db schema migrated");
        }

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn open_default() -> Result<Self, String> {
        let path = crate::config::settings::Config::data_dir()
            .map_err(|e| e.to_string())?
            .join("memory.db");
        Self::new(path).map_err(|e| e.to_string())
    }

    /// Lock the connection, recovering from a poisoned mutex.
    ///
    /// A panic while holding the lock used to turn every later memory call
    /// into a panic. Recovering is safe here because every write path uses an
    /// RAII transaction, so an unwinding panic rolls the statement back before
    /// the guard is dropped.
    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn insert_raw(&self, msg: &RawMessage) -> Result<(), rusqlite::Error> {
        {
            let conn = self.lock();
            conn.execute(
                "INSERT OR REPLACE INTO raw_messages (id, session_id, role, content, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![msg.id, msg.session_id, msg.role, msg.content, msg.created_at],
            )?;
        }
        // Retention runs on the write path, never on open: opening a store
        // must not destroy user data. The indexed range DELETE is a no-op
        // (microseconds) until something actually ages out.
        if let Err(e) = self.prune_raw(RAW_RETENTION_DAYS) {
            warn!(error = %e, "raw message retention prune failed");
        }
        Ok(())
    }

    /// Delete raw messages older than `days` days; returns how many rows went
    /// away. `days <= 0` keeps everything.
    pub fn prune_raw(&self, days: i64) -> Result<usize, rusqlite::Error> {
        if days <= 0 {
            return Ok(0);
        }
        let cutoff = chrono::Utc::now().timestamp() - days * 86_400;
        let conn = self.lock();
        let removed = conn.execute(
            "DELETE FROM raw_messages WHERE created_at < ?1",
            params![cutoff],
        )?;
        if removed > 0 {
            debug!(removed, days, "pruned stale raw memory messages");
        }
        Ok(removed)
    }

    pub fn insert_fact(&self, fact: &Fact) -> Result<(), rusqlite::Error> {
        let conn = self.lock();
        let content = format!("{} {} {}", fact.subject, fact.predicate, fact.object);
        // One transaction: `facts` and `memory_fts` must never disagree about
        // what exists. Three autocommit statements meant a failure in the
        // middle left a fact that could never be found (or an index row with
        // nothing behind it).
        let tx = Transaction::new_unchecked(&conn, TransactionBehavior::Immediate)?;
        // Remove prior FTS rows for the same SPO to avoid unbounded growth.
        tx.execute(
            "DELETE FROM memory_fts WHERE subject = ?1 AND predicate = ?2 AND object = ?3",
            params![fact.subject, fact.predicate, fact.object],
        )?;
        tx.execute(
            "INSERT INTO facts (id, session_id, subject, predicate, object, confidence, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(session_id, subject, predicate, object) DO UPDATE SET
               confidence = MAX(facts.confidence, excluded.confidence),
               created_at = excluded.created_at",
            params![
                fact.id,
                fact.session_id,
                fact.subject,
                fact.predicate,
                fact.object,
                fact.confidence,
                fact.created_at
            ],
        )?;
        tx.execute(
            "INSERT INTO memory_fts (subject, predicate, object, content) VALUES (?1, ?2, ?3, ?4)",
            params![fact.subject, fact.predicate, fact.object, content],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn insert_pattern(&self, pat: &Pattern) -> Result<(), rusqlite::Error> {
        let tags = serde_json::to_string(&pat.tags).unwrap_or_else(|_| "[]".into());
        let conn = self.lock();
        // Upsert by stable name (business key). `occurrence_count` is
        // ACCUMULATED, not max-ed: `promote_patterns` hands over the increment
        // for this pass, and `MAX(...)` made the counter a sliding-window
        // maximum that could never exceed one 50-fact window.
        conn.execute(
            "INSERT INTO patterns (id, name, description, occurrence_count, last_seen_at, tags)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(name) DO UPDATE SET
               description = excluded.description,
               occurrence_count = patterns.occurrence_count + excluded.occurrence_count,
               last_seen_at = excluded.last_seen_at,
               tags = excluded.tags",
            params![
                pat.id,
                pat.name,
                pat.description,
                pat.occurrence_count,
                pat.last_seen_at,
                tags
            ],
        )?;
        Ok(())
    }

    pub fn list_facts(&self, session_id: &str, limit: usize) -> Result<Vec<Fact>, rusqlite::Error> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, session_id, subject, predicate, object, confidence, created_at
             FROM facts WHERE session_id = ?1 ORDER BY created_at DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![session_id, limit as i64], |row| {
            Ok(Fact {
                id: row.get(0)?,
                session_id: row.get(1)?,
                subject: row.get(2)?,
                predicate: row.get(3)?,
                object: row.get(4)?,
                confidence: row.get(5)?,
                created_at: row.get(6)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Full-text search. `session_id` scopes the hits to one conversation
    /// (see `fts::search_memory`); `None` searches every session.
    pub fn search(
        &self,
        query: &str,
        session_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(String, String, String, f64)>, rusqlite::Error> {
        let conn = self.lock();
        search_memory(&conn, query, session_id, limit)
    }

    /// Keyword (LIKE) search, used when the FTS index is unavailable or errors.
    ///
    /// Terms come from the same tokenizer as the FTS path, so the pattern can
    /// never contain a LIKE metacharacter and needs no escaping.
    pub fn search_lexical(
        &self,
        query: &str,
        session_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(String, String, String, f64)>, rusqlite::Error> {
        let terms = query_tokens(query);
        let Some(term) = terms.iter().max_by_key(|t| t.chars().count()) else {
            return Ok(Vec::new());
        };
        let pattern = format!("%{term}%");
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT subject, predicate, object FROM facts
             WHERE (?1 IS NULL OR session_id = ?1)
               AND (subject LIKE ?2 OR predicate LIKE ?2 OR object LIKE ?2)
             ORDER BY created_at DESC
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![session_id, pattern, limit as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                0.0,
            ))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
}
