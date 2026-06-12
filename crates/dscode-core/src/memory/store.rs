//! Memory store — SQLite database for the 3-tier memory system.
//! Phase 1: minimal implementation.

use rusqlite::Connection;
use std::path::PathBuf;

pub struct MemoryStore {
    conn: Connection,
}

impl MemoryStore {
    pub fn new(path: PathBuf) -> Result<Self, rusqlite::Error> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        Ok(Self { conn })
    }

    pub fn conn(&self) -> &Connection { &self.conn }
}
