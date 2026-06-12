//! Knowledge node CRUD with dual SQLite databases (global + session) and FTS5 indexing.
//!
//! # Architecture
//!
//! - `global.db` in `~/.dscode/wiki/` — persists across all sessions.
//! - `session.db` in `~/.dscode/wiki/<session_id>/` — scoped to one session.
//!
//! Both databases share the same schema: a `knowledge_nodes` table plus an
//! FTS5 virtual table (`knowledge_nodes_fts`) for full-text search.  The
//! [`Engine`] provides a unified API that queries both layers, preferring
//! session-local matches when they exist.

use chrono::Utc;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

use crate::config::settings::Config;

// ── Domain types ────────────────────────────────────────────────────────────

/// Classification of a knowledge node.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeType {
    /// An abstract concept or topic.
    Concept,
    /// A concrete, verifiable fact.
    Fact,
    /// A recurring coding pattern or idiom.
    Pattern,
    /// A decision the agent made and why.
    Decision,
    /// A rule or constraint the agent follows.
    Rule,
}

impl NodeType {
    pub fn as_str(&self) -> &'static str {
        match self {
            NodeType::Concept => "concept",
            NodeType::Fact => "fact",
            NodeType::Pattern => "pattern",
            NodeType::Decision => "decision",
            NodeType::Rule => "rule",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "concept" => Some(NodeType::Concept),
            "fact" => Some(NodeType::Fact),
            "pattern" => Some(NodeType::Pattern),
            "decision" => Some(NodeType::Decision),
            "rule" => Some(NodeType::Rule),
            _ => None,
        }
    }
}

/// A single unit of knowledge stored in the wiki.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeNode {
    /// Unique identifier (UUID v4).
    pub id: String,
    /// Short human-readable title.
    pub title: String,
    /// Full text content of the node.
    pub content: String,
    /// Type classification.
    pub node_type: NodeType,
    /// Free-form tags for categorization.
    pub tags: Vec<String>,
    /// Unix timestamp (seconds) when the node was created.
    pub created_at: i64,
    /// Session in which this node was created (empty for global).
    #[serde(default)]
    pub session_id: String,
}

impl KnowledgeNode {
    /// Create a new node with a generated UUID and the current timestamp.
    pub fn new(title: String, content: String, node_type: NodeType, tags: Vec<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            title,
            content,
            node_type,
            tags,
            created_at: Utc::now().timestamp(),
            session_id: String::new(),
        }
    }

    /// Attach this node to a specific session.
    pub fn with_session(mut self, session_id: String) -> Self {
        self.session_id = session_id;
        self
    }

    /// Render the node as a compact markdown snippet for LLM context injection.
    pub fn to_context_snippet(&self) -> String {
        format!(
            "**[{}]** {} — {}",
            self.node_type.as_str().to_uppercase(),
            self.title,
            self.content
        )
    }
}

// ── Database initialisation helpers ─────────────────────────────────────────

fn create_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS knowledge_nodes (
            id          TEXT PRIMARY KEY,
            title       TEXT NOT NULL,
            content     TEXT NOT NULL,
            node_type   TEXT NOT NULL DEFAULT 'concept',
            tags        TEXT NOT NULL DEFAULT '[]',
            created_at  INTEGER NOT NULL,
            session_id  TEXT NOT NULL DEFAULT ''
        );

        CREATE VIRTUAL TABLE IF NOT EXISTS knowledge_nodes_fts
            USING fts5(id, title, content, tags, tokenize='porter unicode61');

        CREATE INDEX IF NOT EXISTS idx_nodes_type
            ON knowledge_nodes(node_type);

        CREATE INDEX IF NOT EXISTS idx_nodes_created
            ON knowledge_nodes(created_at);

        CREATE INDEX IF NOT EXISTS idx_nodes_session
            ON knowledge_nodes(session_id);",
    )
    .map_err(|e| format!("Wiki schema migration failed: {}", e))?;
    Ok(())
}

// ── Engine ──────────────────────────────────────────────────────────────────

/// The central wiki engine managing dual SQLite databases (global + per-session).
pub struct Engine {
    global_conn: Connection,
}

impl Engine {
    /// Open (or create) the global wiki database at `~/.dscode/wiki/global.db`.
    pub fn new() -> Result<Self, String> {
        let wiki_dir = Config::wiki_dir().map_err(|e| e.to_string())?;
        std::fs::create_dir_all(&wiki_dir)
            .map_err(|e| format!("Failed to create wiki dir {:?}: {}", wiki_dir, e))?;

        let global_path = wiki_dir.join("global.db");
        let global_conn = Connection::open(&global_path)
            .map_err(|e| format!("Failed to open global wiki db {:?}: {}", global_path, e))?;

        global_conn
            .execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .map_err(|e| format!("WAL pragma failed: {}", e))?;

        create_schema(&global_conn)?;

        Ok(Self { global_conn })
    }

    /// Open (or create) a session-scoped wiki database.
    pub fn session_conn(session_id: &str) -> Result<Connection, String> {
        let session_dir = Config::wiki_dir()
            .map_err(|e| e.to_string())?
            .join(session_id);
        std::fs::create_dir_all(&session_dir)
            .map_err(|e| format!("Failed to create session wiki dir {:?}: {}", session_dir, e))?;

        let conn = Connection::open(session_dir.join("session.db"))
            .map_err(|e| format!("Failed to open session wiki db: {}", e))?;

        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .map_err(|e| format!("WAL pragma failed: {}", e))?;

        create_schema(&conn)?;
        Ok(conn)
    }

    // ── CRUD: global layer ───────────────────────────────────────────────

    /// Insert a node into the global database.
    pub fn insert_global(&self, node: &KnowledgeNode) -> Result<(), String> {
        insert_node(&self.global_conn, node)
    }

    /// Retrieve a global node by id.
    pub fn get_global(&self, id: &str) -> Result<Option<KnowledgeNode>, String> {
        get_node(&self.global_conn, id)
    }

    /// List all global nodes, newest first.
    pub fn list_global(&self) -> Result<Vec<KnowledgeNode>, String> {
        list_nodes(&self.global_conn)
    }

    /// Update an existing global node.
    pub fn update_global(&self, node: &KnowledgeNode) -> Result<(), String> {
        update_node(&self.global_conn, node)
    }

    /// Delete a global node by id.
    pub fn delete_global(&self, id: &str) -> Result<(), String> {
        delete_node(&self.global_conn, id)
    }

    // ── CRUD: session layer ──────────────────────────────────────────────

    /// Insert a node into a session database.
    pub fn insert_session(session_id: &str, node: &KnowledgeNode) -> Result<(), String> {
        let conn = Self::session_conn(session_id)?;
        insert_node(&conn, node)
    }

    /// Retrieve a session node by id.
    pub fn get_session(session_id: &str, id: &str) -> Result<Option<KnowledgeNode>, String> {
        let conn = Self::session_conn(session_id)?;
        get_node(&conn, id)
    }

    /// List all nodes for a session.
    pub fn list_session(session_id: &str) -> Result<Vec<KnowledgeNode>, String> {
        let conn = Self::session_conn(session_id)?;
        list_nodes(&conn)
    }

    /// Update a session node.
    pub fn update_session(session_id: &str, node: &KnowledgeNode) -> Result<(), String> {
        let conn = Self::session_conn(session_id)?;
        update_node(&conn, node)
    }

    /// Delete a session node by id.
    pub fn delete_session(session_id: &str, id: &str) -> Result<(), String> {
        let conn = Self::session_conn(session_id)?;
        delete_node(&conn, id)
    }

    // ── Unified queries ──────────────────────────────────────────────────

    /// Search both global and session layers. Session results are returned first.
    pub fn search_unified(
        &self,
        session_id: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<KnowledgeNode>, String> {
        let mut results: Vec<KnowledgeNode> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();

        // Session layer first.
        if let Ok(conn) = Self::session_conn(session_id) {
            if let Ok(session_results) = search_fts(&conn, query, limit) {
                for node in session_results {
                    seen.insert(node.id.clone());
                    results.push(node);
                }
            }
        }

        // Global layer (skip duplicates).
        if results.len() < limit {
            let remaining = limit - results.len();
            if let Ok(global_results) = search_fts(&self.global_conn, query, remaining) {
                for node in global_results {
                    if !seen.contains(&node.id) {
                        results.push(node);
                    }
                }
            }
        }

        Ok(results)
    }

    /// Load knowledge snippets for LLM context injection.
    pub fn load_context_snippets(
        &self,
        session_id: &str,
        query: &str,
        max_snippets: usize,
    ) -> Result<Vec<String>, String> {
        let nodes = self.search_unified(session_id, query, max_snippets)?;
        Ok(nodes.iter().map(|n| n.to_context_snippet()).collect())
    }
}

// ── Low-level helpers ───────────────────────────────────────────────────────

fn insert_node(conn: &Connection, node: &KnowledgeNode) -> Result<(), String> {
    let tags_json = serde_json::to_string(&node.tags)
        .map_err(|e| format!("Serialize tags: {}", e))?;

    conn.execute(
        "INSERT INTO knowledge_nodes (id, title, content, node_type, tags, created_at, session_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            node.id,
            node.title,
            node.content,
            node.node_type.as_str(),
            tags_json,
            node.created_at,
            node.session_id,
        ],
    )
    .map_err(|e| format!("Insert node: {}", e))?;

    // Mirror into FTS5 index.
    conn.execute(
        "INSERT INTO knowledge_nodes_fts (id, title, content, tags)
         VALUES (?1, ?2, ?3, ?4)",
        params![node.id, node.title, node.content, node.title], // tags indexed as title text
    )
    .map_err(|e| format!("Insert FTS: {}", e))?;

    Ok(())
}

fn get_node(conn: &Connection, id: &str) -> Result<Option<KnowledgeNode>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, title, content, node_type, tags, created_at, session_id
             FROM knowledge_nodes WHERE id = ?1",
        )
        .map_err(|e| format!("Prepare get_node: {}", e))?;

    stmt.query_row(params![id], |row| {
        Ok(KnowledgeNode {
            id: row.get(0)?,
            title: row.get(1)?,
            content: row.get(2)?,
            node_type: NodeType::from_str(&row.get::<_, String>(3)?)
                .unwrap_or(NodeType::Concept),
            tags: serde_json::from_str(&row.get::<_, String>(4)?).unwrap_or_default(),
            created_at: row.get(5)?,
            session_id: row.get(6)?,
        })
    })
    .optional()
    .map_err(|e| format!("get_node: {}", e))
}

fn list_nodes(conn: &Connection) -> Result<Vec<KnowledgeNode>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, title, content, node_type, tags, created_at, session_id
             FROM knowledge_nodes ORDER BY created_at DESC",
        )
        .map_err(|e| format!("Prepare list_nodes: {}", e))?;

    let rows = stmt
        .query_map([], |row| {
            Ok(KnowledgeNode {
                id: row.get(0)?,
                title: row.get(1)?,
                content: row.get(2)?,
                node_type: NodeType::from_str(&row.get::<_, String>(3)?)
                    .unwrap_or(NodeType::Concept),
                tags: serde_json::from_str(&row.get::<_, String>(4)?).unwrap_or_default(),
                created_at: row.get(5)?,
                session_id: row.get(6)?,
            })
        })
        .map_err(|e| format!("list_nodes: {}", e))?;

    let mut nodes = Vec::new();
    for row in rows {
        nodes.push(row.map_err(|e| format!("Row error: {}", e))?);
    }
    Ok(nodes)
}

fn update_node(conn: &Connection, node: &KnowledgeNode) -> Result<(), String> {
    let tags_json = serde_json::to_string(&node.tags)
        .map_err(|e| format!("Serialize tags: {}", e))?;

    let affected = conn
        .execute(
            "UPDATE knowledge_nodes SET title=?1, content=?2, node_type=?3, tags=?4
             WHERE id=?5",
            params![
                node.title,
                node.content,
                node.node_type.as_str(),
                tags_json,
                node.id,
            ],
        )
        .map_err(|e| format!("Update node: {}", e))?;

    if affected == 0 {
        return Err(format!("Node {} not found", node.id));
    }

    // Update FTS index: delete old, insert new.
    conn.execute(
        "DELETE FROM knowledge_nodes_fts WHERE id=?1",
        params![node.id],
    )
    .map_err(|e| format!("Delete FTS: {}", e))?;
    conn.execute(
        "INSERT INTO knowledge_nodes_fts (id, title, content, tags)
         VALUES (?1, ?2, ?3, ?4)",
        params![node.id, node.title, node.content, node.title],
    )
    .map_err(|e| format!("Insert FTS: {}", e))?;

    Ok(())
}

fn delete_node(conn: &Connection, id: &str) -> Result<(), String> {
    let affected = conn
        .execute("DELETE FROM knowledge_nodes WHERE id=?1", params![id])
        .map_err(|e| format!("Delete node: {}", e))?;
    if affected == 0 {
        return Err(format!("Node {} not found", id));
    }
    conn.execute(
        "DELETE FROM knowledge_nodes_fts WHERE id=?1",
        params![id],
    )
    .map_err(|e| format!("Delete FTS: {}", e))?;
    Ok(())
}

/// FTS5 search on a connection. Returns nodes ranked by BM25.
fn search_fts(conn: &Connection, query: &str, limit: usize) -> Result<Vec<KnowledgeNode>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT k.id, k.title, k.content, k.node_type, k.tags, k.created_at, k.session_id
             FROM knowledge_nodes_fts f
             JOIN knowledge_nodes k ON k.id = f.id
             WHERE knowledge_nodes_fts MATCH ?1
             ORDER BY rank
             LIMIT ?2",
        )
        .map_err(|e| format!("Prepare search_fts: {}", e))?;

    let rows = stmt
        .query_map(params![query, limit as i64], |row| {
            Ok(KnowledgeNode {
                id: row.get(0)?,
                title: row.get(1)?,
                content: row.get(2)?,
                node_type: NodeType::from_str(&row.get::<_, String>(3)?)
                    .unwrap_or(NodeType::Concept),
                tags: serde_json::from_str(&row.get::<_, String>(4)?).unwrap_or_default(),
                created_at: row.get(5)?,
                session_id: row.get(6)?,
            })
        })
        .map_err(|e| format!("search_fts query: {}", e))?;

    let mut nodes = Vec::new();
    for row in rows {
        nodes.push(row.map_err(|e| format!("Row error: {}", e))?);
    }
    Ok(nodes)
}

// ── Optional helper ─────────────────────────────────────────────────────────

/// Small helper to turn a rusqlite `Result` into an `Option`.
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

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Create a throwaway Engine backed by a temp directory.
    fn temp_engine() -> (TempDir, Engine) {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("global.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL;").unwrap();
        create_schema(&conn).unwrap();
        let engine = Engine {
            global_conn: conn,
        };
        (dir, engine)
    }

    #[test]
    fn test_insert_and_get_global() {
        let (_dir, engine) = temp_engine();
        let node = KnowledgeNode::new(
            "Test Concept".into(),
            "This is a test concept about Rust traits.".into(),
            NodeType::Concept,
            vec!["rust".into(), "traits".into()],
        );

        engine.insert_global(&node).unwrap();
        let fetched = engine.get_global(&node.id).unwrap().expect("should exist");
        assert_eq!(fetched.title, "Test Concept");
        assert_eq!(fetched.content, "This is a test concept about Rust traits.");
        assert_eq!(fetched.node_type, NodeType::Concept);
        assert_eq!(fetched.tags.len(), 2);
    }

    #[test]
    fn test_list_global() {
        let (_dir, engine) = temp_engine();
        for i in 0..3 {
            let node = KnowledgeNode::new(
                format!("Node {}", i),
                format!("Content {}", i),
                NodeType::Fact,
                    vec![],
                );
            engine.insert_global(&node).unwrap();
        }
        let all = engine.list_global().unwrap();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn test_update_global() {
        let (_dir, engine) = temp_engine();
        let mut node = KnowledgeNode::new(
            "Original".into(),
            "Original content.".into(),
            NodeType::Concept,
            vec![],
        );
        engine.insert_global(&node).unwrap();

        node.title = "Updated".into();
        node.content = "Updated content.".into();
        engine.update_global(&node).unwrap();

        let fetched = engine.get_global(&node.id).unwrap().unwrap();
        assert_eq!(fetched.title, "Updated");
        assert_eq!(fetched.content, "Updated content.");
    }

    #[test]
    fn test_delete_global() {
        let (_dir, engine) = temp_engine();
        let node = KnowledgeNode::new(
            "To Delete".into(),
            "Will be removed.".into(),
            NodeType::Fact,
            vec![],
        );
        engine.insert_global(&node).unwrap();
        engine.delete_global(&node.id).unwrap();
        assert!(engine.get_global(&node.id).unwrap().is_none());
    }

    #[test]
    fn test_search_fts() {
        let (_dir, engine) = temp_engine();
        let node = KnowledgeNode::new(
            "Rust Async".into(),
            "Understanding async await in Rust using Tokio.".into(),
            NodeType::Concept,
            vec!["rust".into(), "async".into()],
        );
        engine.insert_global(&node).unwrap();

        // FTS5 search for "async"
        let results = search_fts(&engine.global_conn, "async", 10).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].title, "Rust Async");
    }

    #[test]
    fn test_search_no_match() {
        let (_dir, engine) = temp_engine();
        let node = KnowledgeNode::new(
            "Python".into(),
            "Python is a scripting language.".into(),
            NodeType::Fact,
            vec![],
        );
        engine.insert_global(&node).unwrap();
        let results = search_fts(&engine.global_conn, "zzzzzz_nonexistent", 10).unwrap();
        assert!(results.is_empty());
    }
}
