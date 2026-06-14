//! Knowledge node CRUD backed by a markdown filesystem at `~/.dscode/wiki/`.
//!
//! # Architecture
//!
//! - `global/` — persists across all sessions, with type subdirectories
//!   (`facts/`, `concepts/`, `patterns/`, `decisions/`, `rules/`).
//! - `sessions/<session_id>/` — scoped to one session (same type layout).
//!
//! Each node is a single `.md` file with YAML frontmatter and markdown
//! content.  The [`Engine`] provides a unified API that queries both layers.
//! An `index.md`, `log.md`, and `schema.md` are maintained automatically.

use chrono::Utc;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
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

    /// Plural directory name for this node type.
    fn plural_dir(&self) -> &'static str {
        match self {
            NodeType::Concept => "concepts",
            NodeType::Fact => "facts",
            NodeType::Pattern => "patterns",
            NodeType::Decision => "decisions",
            NodeType::Rule => "rules",
        }
    }
}

/// All recognised type subdirectories (plural).
const TYPE_DIRS: &[&str] = &["facts", "concepts", "patterns", "decisions", "rules"];

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

    /// Extract [[wikilinks]] from the node's content.
    pub fn extract_wikilinks(&self) -> Vec<String> {
        extract_wikilinks_from_text(&self.content)
    }
}

// ── Frontmatter (YAML envelope for .md files) ──────────────────────────────

/// Serializable YAML frontmatter block inside each `.md` node file.
#[derive(Debug, Serialize, Deserialize)]
struct Frontmatter {
    id: String,
    title: String,
    #[serde(rename = "type")]
    node_type: String,
    tags: Vec<String>,
    created_at: i64,
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    links: Vec<String>,
}

// ── Engine ──────────────────────────────────────────────────────────────────

/// The central wiki engine managing node files on disk under
/// `~/.dscode/wiki/`.
pub struct Engine {
    base_dir: PathBuf,
}

impl Engine {
    // ── Initialisation ────────────────────────────────────────────────────

    /// Open (or create) the wiki directory structure.  Writes `schema.md`
    /// the first time `new()` is called and ensures `index.md` / `log.md`
    /// exist.
    pub fn new() -> Result<Self, String> {
        let base_dir = Config::wiki_dir().map_err(|e| e.to_string())?;
        let global_dir = base_dir.join("global");

        // --- Ensure type subdirectories exist ---
        fs::create_dir_all(&global_dir)
            .map_err(|e| format!("Failed to create global wiki dir {:?}: {}", global_dir, e))?;
        for d in TYPE_DIRS {
            fs::create_dir_all(global_dir.join(d))
                .map_err(|e| format!("Failed to create global/{:?}: {}", d, e))?;
        }
        fs::create_dir_all(base_dir.join("sessions"))
            .map_err(|e| format!("Failed to create sessions dir: {}", e))?;

        // --- schema.md (first run) ---
        let schema_path = global_dir.join("schema.md");
        if !schema_path.exists() {
            fs::write(&schema_path, Self::build_schema())
                .map_err(|e| format!("Failed to write schema.md: {}", e))?;
        }

        // --- index.md (bootstrap if missing) ---
        let index_path = global_dir.join("index.md");
        if !index_path.exists() {
            fs::write(&index_path, "# Wiki Index\n\n_Waiting for nodes..._\n")
                .map_err(|e| format!("Failed to write index.md: {}", e))?;
        }

        // --- log.md (bootstrap if missing) ---
        let log_path = global_dir.join("log.md");
        if !log_path.exists() {
            let header = format!(
                "# Wiki Operation Log\n\n_Created {}_\n\n",
                timestamp_iso(Utc::now().timestamp())
            );
            fs::write(&log_path, &header)
                .map_err(|e| format!("Failed to write log.md: {}", e))?;
        }

        Ok(Self { base_dir })
    }

    /// Ensure a session directory (with type subdirectories and index.md)
    /// exists.
    pub fn session_conn(session_id: &str) -> Result<(), String> {
        let base_dir = Config::wiki_dir().map_err(|e| e.to_string())?;
        let sess_dir = base_dir.join("sessions").join(session_id);
        fs::create_dir_all(&sess_dir)
            .map_err(|e| format!("Failed to create session dir {:?}: {}", sess_dir, e))?;
        for d in TYPE_DIRS {
            fs::create_dir_all(sess_dir.join(d))
                .map_err(|e| format!("Failed to create sessions/{}/{e}: {}", session_id, d))?;
        }
        // Bootstrap session index.md
        let index_path = sess_dir.join("index.md");
        if !index_path.exists() {
            fs::write(&index_path, format!("# Session {}\n\n", session_id))
                .map_err(|e| format!("Failed to write session index.md: {}", e))?;
        }
        Ok(())
    }

    // ── CRUD: global layer ───────────────────────────────────────────────

    /// Insert a node into the global database (write `.md` file).
    pub fn insert_global(&self, node: &KnowledgeNode) -> Result<(), String> {
        let path = self.node_path_global(node);
        write_node_file(&path, node)?;
        self.regenerate_index()?;
        self.append_log(&format!("INSERT {} | {} ({})", node.id, node.title, node.node_type.as_str()))?;
        Ok(())
    }

    /// Retrieve a global node by id.
    pub fn get_global(&self, id: &str) -> Result<Option<KnowledgeNode>, String> {
        match self.find_file_global(id) {
            Some(path) => read_node_file(&path).map(Some),
            None => Ok(None),
        }
    }

    /// List all global nodes, newest first.
    pub fn list_global(&self) -> Result<Vec<KnowledgeNode>, String> {
        let mut nodes: Vec<KnowledgeNode> = Vec::new();
        for d in TYPE_DIRS {
            let dir_path = self.global_dir().join(d);
            if !dir_path.exists() {
                continue;
            }
            for entry in fs::read_dir(&dir_path)
                .map_err(|e| format!("read_dir {:?}: {}", dir_path, e))?
            {
                let entry = entry.map_err(|e| format!("read_dir entry: {}", e))?;
                let path = entry.path();
                if path.extension().map_or(false, |e| e == "md") {
                    if let Ok(node) = read_node_file(&path) {
                        nodes.push(node);
                    }
                }
            }
        }
        nodes.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(nodes)
    }

    /// Update an existing global node.
    pub fn update_global(&self, node: &KnowledgeNode) -> Result<(), String> {
        match self.find_file_global(&node.id) {
            Some(old_path) => {
                // Delete old file (node type may have changed).
                fs::remove_file(&old_path)
                    .map_err(|e| format!("Failed to remove old file {:?}: {}", old_path, e))?;
            }
            None => return Err(format!("Node {} not found", node.id)),
        }
        let new_path = self.node_path_global(node);
        write_node_file(&new_path, node)?;
        self.regenerate_index()?;
        self.append_log(&format!("UPDATE {} | {} ({})", node.id, node.title, node.node_type.as_str()))?;
        Ok(())
    }

    /// Delete a global node by id.
    pub fn delete_global(&self, id: &str) -> Result<(), String> {
        match self.find_file_global(id) {
            Some(path) => {
                fs::remove_file(&path)
                    .map_err(|e| format!("Failed to delete {:?}: {}", path, e))?;
                self.regenerate_index()?;
                self.append_log(&format!("DELETE {}", id))?;
                Ok(())
            }
            None => Err(format!("Node {} not found", id)),
        }
    }

    // ── CRUD: session layer ──────────────────────────────────────────────

    /// Insert a node into a session database.
    pub fn insert_session(session_id: &str, node: &KnowledgeNode) -> Result<(), String> {
        Self::session_conn(session_id)?;
        let base_dir = Config::wiki_dir().map_err(|e| e.to_string())?;
        let session_dir = base_dir.join("sessions").join(session_id);
        let path = session_dir
            .join(node.node_type.plural_dir())
            .join(format!("{}.md", node.id));
        write_node_file(&path, node)?;
        Self::regenerate_session_index(session_id)?;
        Ok(())
    }

    /// Retrieve a session node by id.
    pub fn get_session(session_id: &str, id: &str) -> Result<Option<KnowledgeNode>, String> {
        let base_dir = Config::wiki_dir().map_err(|e| e.to_string())?;
        let session_dir = base_dir.join("sessions").join(session_id);
        if !session_dir.exists() {
            return Ok(None);
        }
        match Self::find_file_in(&session_dir, id) {
            Some(path) => read_node_file(&path).map(Some),
            None => Ok(None),
        }
    }

    /// List all nodes for a session.
    pub fn list_session(session_id: &str) -> Result<Vec<KnowledgeNode>, String> {
        let base_dir = Config::wiki_dir().map_err(|e| e.to_string())?;
        let session_dir = base_dir.join("sessions").join(session_id);
        if !session_dir.exists() {
            return Ok(vec![]);
        }
        list_nodes_in(&session_dir)
    }

    /// Update a session node.
    pub fn update_session(session_id: &str, node: &KnowledgeNode) -> Result<(), String> {
        let base_dir = Config::wiki_dir().map_err(|e| e.to_string())?;
        let session_dir = base_dir.join("sessions").join(session_id);
        match Self::find_file_in(&session_dir, &node.id) {
            Some(old_path) => {
                fs::remove_file(&old_path)
                    .map_err(|e| format!("Failed to remove old file {:?}: {}", old_path, e))?;
            }
            None => return Err(format!("Node {} not found in session {}", node.id, session_id)),
        }
        let new_path = session_dir
            .join(node.node_type.plural_dir())
            .join(format!("{}.md", node.id));
        write_node_file(&new_path, node)?;
        Self::regenerate_session_index(session_id)?;
        Ok(())
    }

    /// Delete a session node by id.
    pub fn delete_session(session_id: &str, id: &str) -> Result<(), String> {
        let base_dir = Config::wiki_dir().map_err(|e| e.to_string())?;
        let session_dir = base_dir.join("sessions").join(session_id);
        match Self::find_file_in(&session_dir, id) {
            Some(path) => {
                fs::remove_file(&path)
                    .map_err(|e| format!("Failed to delete {:?}: {}", path, e))?;
                Self::regenerate_session_index(session_id)?;
                Ok(())
            }
            None => Err(format!("Node {} not found in session {}", id, session_id)),
        }
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
        let session_dir = self.base_dir.join("sessions").join(session_id);
        if session_dir.exists() {
            if let Ok(nodes) = list_nodes_in(&session_dir) {
                for node in search_nodes(&nodes, query) {
                    if results.len() >= limit {
                        break;
                    }
                    seen.insert(node.id.clone());
                    results.push(node);
                }
            }
        }

        // Global layer (skip duplicates).
        if results.len() < limit {
            if let Ok(nodes) = self.list_global() {
                for node in search_nodes(&nodes, query) {
                    if results.len() >= limit {
                        break;
                    }
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

    // ── Internal helpers ─────────────────────────────────────────────────

    fn global_dir(&self) -> PathBuf {
        self.base_dir.join("global")
    }

    fn node_path_global(&self, node: &KnowledgeNode) -> PathBuf {
        self.global_dir()
            .join(node.node_type.plural_dir())
            .join(format!("{}.md", node.id))
    }

    /// Find a `.md` file in the global type directories by node id.
    fn find_file_global(&self, id: &str) -> Option<PathBuf> {
        Self::find_file_in(&self.global_dir(), id)
    }

    /// Find a `.md` file whose stem matches `id` anywhere inside `root`.
    fn find_file_in(root: &Path, id: &str) -> Option<PathBuf> {
        for d in TYPE_DIRS {
            let candidate = root.join(d).join(format!("{}.md", id));
            if candidate.exists() {
                return Some(candidate);
            }
        }
        None
    }

    /// Regenerate global `index.md` from all nodes.
    fn regenerate_index(&self) -> Result<(), String> {
        let nodes = self.list_global()?;
        let index = build_index_md(&nodes, "Global Knowledge Wiki");
        let path = self.global_dir().join("index.md");
        fs::write(&path, &index)
            .map_err(|e| format!("Failed to write index.md: {}", e))
    }

    /// Regenerate session `index.md`.
    fn regenerate_session_index(session_id: &str) -> Result<(), String> {
        let base_dir = Config::wiki_dir().map_err(|e| e.to_string())?;
        let session_dir = base_dir.join("sessions").join(session_id);
        if !session_dir.exists() {
            return Ok(());
        }
        let nodes = list_nodes_in(&session_dir)?;
        let index = build_index_md(&nodes, &format!("Session {}", session_id));
        let path = session_dir.join("index.md");
        fs::write(&path, &index)
            .map_err(|e| format!("Failed to write session index.md: {}", e))
    }

    /// Append a timestamped entry to `log.md`.
    fn append_log(&self, message: &str) -> Result<(), String> {
        let log_path = self.global_dir().join("log.md");
        let mut existing = String::new();
        if log_path.exists() {
            existing = fs::read_to_string(&log_path)
                .unwrap_or_default();
        }
        let entry = format!(
            "- [{}] {}\n",
            timestamp_iso(Utc::now().timestamp()),
            message
        );
        // Insert after the header block (first `---` or first blank line after
        // the title). For simplicity, append to the end.
        let new_content = existing + &entry;
        fs::write(&log_path, &new_content)
            .map_err(|e| format!("Failed to write log.md: {}", e))
    }

    /// Generate the `schema.md` content.
    fn build_schema() -> String {
        r#"# Wiki Schema

## Directory Structure

```
~/.dscode/wiki/
├── global/
│   ├── index.md          # Content catalog — all pages grouped by type
│   ├── log.md            # Append-only chronological operation log
│   ├── schema.md         # This file — directory structure definition
│   ├── facts/            # Verifiable facts (one .md file per node)
│   ├── concepts/         # Abstract concepts and topics
│   ├── patterns/         # Recurring coding patterns / idioms
│   ├── decisions/        # Design decisions and rationale
│   └── rules/            # Agent rules and constraints
├── sessions/
│   └── <session_id>/
│       ├── index.md      # Session-scoped content catalog
│       ├── facts/
│       ├── concepts/
│       ├── patterns/
│       ├── decisions/
│       └── rules/
└── quartz/               # Quartz-compatible export (generated on demand)
```

## Markdown File Format

Every `.md` file uses YAML frontmatter followed by markdown content:

```markdown
---
id: "uuid-v4"
title: "Page Title"
type: "fact"
tags: ["tag1", "tag2"]
created_at: 1700000000
session_id: "optional-session-id"
links:
  - "[[other-page-title]]"
  - "[[another-page]]"
---

Content here. Can reference [[other-page-title]] with wikilinks.
```

## Rules

1. Files are named `{uuid}.md` — the UUID in the filename always matches the
   `id` field in the frontmatter.
2. [[wikilinks]] in the content body are resolved to node titles at read time.
3. The `index.md` file is rebuilt automatically after every insert / update /
   delete operation.
4. The `log.md` file receives a timestamped entry after every mutation.
"#
        .to_string()
    }
}

// ── File I/O helpers ───────────────────────────────────────────────────────

/// Serialise a [`KnowledgeNode`] as a markdown file with YAML frontmatter.
fn write_node_file(path: &Path, node: &KnowledgeNode) -> Result<(), String> {
    // Ensure the parent directory exists.
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create dir {:?}: {}", parent, e))?;
    }

    let wikilinks = extract_wikilinks_from_text(&node.content);
    let fm = Frontmatter {
        id: node.id.clone(),
        title: node.title.clone(),
        node_type: node.node_type.as_str().to_string(),
        tags: node.tags.clone(),
        created_at: node.created_at,
        session_id: node.session_id.clone(),
        links: wikilinks,
    };

    let yaml_str = serde_yaml::to_string(&fm)
        .map_err(|e| format!("YAML serialize: {}", e))?;

    let md = format!("---\n{}---\n\n{}", yaml_str, node.content);
    fs::write(path, &md)
        .map_err(|e| format!("Failed to write {:?}: {}", path, e))
}

/// Parse a `.md` file back into a [`KnowledgeNode`].
fn read_node_file(path: &Path) -> Result<KnowledgeNode, String> {
    let raw = fs::read_to_string(path)
        .map_err(|e| format!("Failed to read {:?}: {}", path, e))?;

    let (fm_str, body) = split_frontmatter(&raw)?;
    let fm: Frontmatter =
        serde_yaml::from_str(&fm_str).map_err(|e| format!("YAML parse {:?}: {}", path, e))?;

    let node_type = NodeType::from_str(&fm.node_type).unwrap_or(NodeType::Concept);
    Ok(KnowledgeNode {
        id: fm.id,
        title: fm.title,
        content: body.trim().to_string(),
        node_type,
        tags: fm.tags,
        created_at: fm.created_at,
        session_id: fm.session_id,
    })
}

/// Split raw string into `(frontmatter_yaml, body)` delimited by `---`.
fn split_frontmatter(raw: &str) -> Result<(String, String), String> {
    // Frontmatter must start with "---" on its own line.
    let trimmed = raw.trim_start();
    if !trimmed.starts_with("---\n") && !trimmed.starts_with("---\r\n") {
        return Err("Missing opening --- frontmatter delimiter".to_string());
    }
    // Skip the opening `---`.
    let after_open = &trimmed[3..].trim_start(); // strip leading `---\n`
    // Find the closing `---`.
    let end_idx = after_open
        .find("\n---")
        .or_else(|| after_open.find("\r\n---"))
        .ok_or_else(|| "Missing closing --- frontmatter delimiter".to_string())?;

    let fm_str = after_open[..end_idx].to_string();
    let body_start = end_idx + 4; // skip "\n---"
    // In case body starts with a newline after the closing ---
    let body = after_open[body_start..]
        .trim_start_matches(|c: char| c == '\r' || c == '\n')
        .to_string();

    Ok((fm_str, body))
}

/// Extract [[wikilinks]] from text content using the regex `\[\[([^\]]+)\]\]`.
fn extract_wikilinks_from_text(text: &str) -> Vec<String> {
    let re = Regex::new(r"\[\[([^\]]+)\]\]").unwrap();
    re.captures_iter(text)
        .map(|cap| {
            let full = cap.get(0).unwrap().as_str().to_string();
            full
        })
        .collect()
}

// ── Search helpers ────────────────────────────────────────────────────────

/// Full-text search (grep-like) across a set of in-memory nodes:
/// matches `query` case-insensitively against title, content, and tags.
fn search_nodes(nodes: &[KnowledgeNode], query: &str) -> Vec<KnowledgeNode> {
    let query_lower = query.to_lowercase();
    let mut results: Vec<KnowledgeNode> = Vec::new();
    for node in nodes {
        let haystack = format!(
            "{} {} {}",
            node.title.to_lowercase(),
            node.content.to_lowercase(),
            node.tags.join(" ").to_lowercase()
        );
        if haystack.contains(&query_lower) {
            results.push(node.clone());
        }
    }
    results
}

/// List all `.md` nodes under a root directory (flat scan of type dirs).
fn list_nodes_in(root: &Path) -> Result<Vec<KnowledgeNode>, String> {
    let mut nodes: Vec<KnowledgeNode> = Vec::new();
    for d in TYPE_DIRS {
        let dir_path = root.join(d);
        if !dir_path.exists() {
            continue;
        }
        for entry in
            fs::read_dir(&dir_path).map_err(|e| format!("read_dir {:?}: {}", dir_path, e))?
        {
            let entry = entry.map_err(|e| format!("read_dir entry: {}", e))?;
            let path = entry.path();
            if path.extension().map_or(false, |e| e == "md") {
                if let Ok(node) = read_node_file(&path) {
                    nodes.push(node);
                }
            }
        }
    }
    nodes.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(nodes)
}

// ── Index.md builder ──────────────────────────────────────────────────────

/// Build the content of an `index.md` file from a list of nodes.
fn build_index_md(nodes: &[KnowledgeNode], title: &str) -> String {
    let mut out = format!("# {}\n\n", title);
    if nodes.is_empty() {
        out.push_str("_No nodes yet._\n");
        return out;
    }

    out.push_str(&format!("**{} nodes** total.\n\n", nodes.len()));

    let type_order: &[(&str, &str)] = &[
        ("pattern", "## Patterns"),
        ("rule", "## Rules"),
        ("decision", "## Decisions"),
        ("fact", "## Facts"),
        ("concept", "## Concepts"),
    ];

    let mut by_type: HashMap<&str, Vec<&KnowledgeNode>> = HashMap::new();
    for node in nodes {
        by_type
            .entry(node.node_type.as_str())
            .or_default()
            .push(node);
    }

    for (type_key, heading) in type_order {
        if let Some(group) = by_type.get(type_key) {
            out.push_str(&format!("\n{} ({})\n\n", heading, group.len()));
            for node in group.iter() {
                let summary = if node.content.len() > 80 {
                    format!("{}...", &node.content[..80])
                } else {
                    node.content.clone()
                };
                out.push_str(&format!(
                    "- **[[{}]]** — {}\n",
                    node.title, summary
                ));
            }
        }
    }

    out
}

// ── Misc helpers ──────────────────────────────────────────────────────────

/// Return an ISO-8601 UTC timestamp string for the given unix epoch.
fn timestamp_iso(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Create a throwaway Engine backed by a temp directory.
    fn temp_engine() -> (TempDir, Engine) {
        let dir = TempDir::new().unwrap();
        let wiki_root = dir.path().join("wiki");
        // Override by setting up the dirs manually (Engine::new uses Config::wiki_dir,
        // so we have to build the state ourselves).
        let global_dir = wiki_root.join("global");
        fs::create_dir_all(&global_dir).unwrap();
        for d in TYPE_DIRS {
            fs::create_dir_all(global_dir.join(d)).unwrap();
        }
        // Write minimal bootstrap files so the engine doesn't panic.
        fs::write(global_dir.join("schema.md"), "# Schema\n").unwrap();
        fs::write(global_dir.join("index.md"), "# Index\n").unwrap();
        fs::write(global_dir.join("log.md"), "# Log\n").unwrap();
        let engine = Engine {
            base_dir: wiki_root,
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
    fn test_search_unified() {
        let (_dir, engine) = temp_engine();
        let node = KnowledgeNode::new(
            "Rust Async".into(),
            "Understanding async await in Rust using Tokio.".into(),
            NodeType::Concept,
            vec!["rust".into(), "async".into()],
        );
        engine.insert_global(&node).unwrap();

        let results = engine.search_unified("test-session", "async", 10).unwrap();
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
        let results = engine.search_unified("test-session", "zzzzzz_nonexistent", 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_frontmatter_roundtrip() {
        let (_dir, engine) = temp_engine();
        let node = KnowledgeNode::new(
            "Frontmatter Test".into(),
            "Content with [[Page A]] and [[Page B]] wikilinks.".into(),
            NodeType::Pattern,
            vec!["test".into()],
        );
        engine.insert_global(&node).unwrap();
        let fetched = engine.get_global(&node.id).unwrap().unwrap();
        assert_eq!(fetched.title, "Frontmatter Test");
        assert_eq!(fetched.node_type, NodeType::Pattern);
        // Wikilinks are preserved in content.
        assert!(fetched.content.contains("[[Page A]]"));
        assert!(fetched.content.contains("[[Page B]]"));
    }

    #[test]
    fn test_extract_wikilinks() {
        let node = KnowledgeNode {
            id: "test".into(),
            title: "T".into(),
            content: "See [[Rust Async]] and also [[Tokio Runtime]] for details.".into(),
            node_type: NodeType::Concept,
            tags: vec![],
            created_at: 0,
            session_id: String::new(),
        };
        let links = node.extract_wikilinks();
        assert_eq!(links.len(), 2);
        assert!(links.contains(&"[[Rust Async]]".to_string()));
        assert!(links.contains(&"[[Tokio Runtime]]".to_string()));
    }

    #[test]
    fn test_index_regeneration() {
        let (_dir, engine) = temp_engine();
        engine.insert_global(&KnowledgeNode::new(
            "Fact Node".into(),
            "A fact about something important.".into(),
            NodeType::Fact,
            vec![],
        )).unwrap();
        engine.insert_global(&KnowledgeNode::new(
            "Rule Node".into(),
            "Always use proper error handling.".into(),
            NodeType::Rule,
            vec![],
        )).unwrap();

        let index_path = engine.global_dir().join("index.md");
        let index_content = fs::read_to_string(&index_path).unwrap();
        // Should mention both nodes.
        assert!(index_content.contains("Fact Node"));
        assert!(index_content.contains("Rule Node"));
        // Rules should come before Facts.
        let rule_pos = index_content.find("Rule Node").unwrap();
        let fact_pos = index_content.find("Fact Node").unwrap();
        assert!(
            rule_pos < fact_pos,
            "Rules section should appear before Facts"
        );
    }

    #[test]
    fn test_log_append() {
        let (_dir, engine) = temp_engine();
        let node = KnowledgeNode::new("LogTest".into(), "Content".into(), NodeType::Fact, vec![]);
        engine.insert_global(&node).unwrap();

        let log_path = engine.global_dir().join("log.md");
        let log_content = fs::read_to_string(&log_path).unwrap();
        assert!(log_content.contains("INSERT"));
        assert!(log_content.contains(&node.id));
    }
}
