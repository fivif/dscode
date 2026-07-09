//! Memory store — SQLite database for the 3-tier memory system.
//! Connection is held under a Mutex so the store is `Send + Sync` safe.

use rusqlite::{params, Connection};
use std::path::PathBuf;
use std::sync::Mutex;

use super::fact::Fact;
use super::fts::{ensure_fts, search_memory};
use super::pattern::Pattern;
use super::raw::RawMessage;

pub struct MemoryStore {
    conn: Mutex<Connection>,
}

impl MemoryStore {
    pub fn new(path: PathBuf) -> Result<Self, rusqlite::Error> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys=ON;
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
             CREATE UNIQUE INDEX IF NOT EXISTS idx_patterns_name ON patterns(name);",
        )?;
        // Best-effort for DBs created before UNIQUE(name)
        let _ = conn.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_patterns_name ON patterns(name);",
        );
        ensure_fts(&conn)?;
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

    pub fn insert_raw(&self, msg: &RawMessage) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO raw_messages (id, session_id, role, content, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![msg.id, msg.session_id, msg.role, msg.content, msg.created_at],
        )?;
        Ok(())
    }

    pub fn insert_fact(&self, fact: &Fact) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        // Remove prior FTS rows for same SPO to avoid unbounded growth
        let content = format!("{} {} {}", fact.subject, fact.predicate, fact.object);
        let _ = conn.execute(
            "DELETE FROM memory_fts WHERE subject = ?1 AND predicate = ?2 AND object = ?3",
            params![fact.subject, fact.predicate, fact.object],
        );
        conn.execute(
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
        conn.execute(
            "INSERT INTO memory_fts (subject, predicate, object, content) VALUES (?1, ?2, ?3, ?4)",
            params![fact.subject, fact.predicate, fact.object, content],
        )?;
        Ok(())
    }

    pub fn insert_pattern(&self, pat: &Pattern) -> Result<(), rusqlite::Error> {
        let tags = serde_json::to_string(&pat.tags).unwrap_or_else(|_| "[]".into());
        let conn = self.conn.lock().unwrap();
        // Upsert by stable name (business key)
        conn.execute(
            "INSERT INTO patterns (id, name, description, occurrence_count, last_seen_at, tags)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(name) DO UPDATE SET
               description = excluded.description,
               occurrence_count = MAX(patterns.occurrence_count, excluded.occurrence_count),
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
        let conn = self.conn.lock().unwrap();
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

    pub fn search(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<(String, String, String, f64)>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        search_memory(&conn, query, limit)
    }
}
