//! Background ingestion agent — extracts entities and relations from
//! conversation turns after each message exchange.
//!
//! The ingestor is spawned via `tokio::spawn` after the agent finishes a turn.
//! It processes the latest assistant and user messages, uses a lightweight
//! heuristic (regex + keyword) extractor to identify potential knowledge nodes,
//! and writes them into the wiki engine.

use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use super::engine::{Engine, KnowledgeNode, NodeType};

use crate::providers::trait_def::Message;

/// The background ingestor that runs after each conversation turn.
pub struct Ingestor {
    /// Shared wiki engine reference.
    engine: Arc<Mutex<Engine>>,
    /// Minimum content length (characters) for a node to be ingested.
    min_content_length: usize,
}

impl Ingestor {
    /// Create a new ingestor wrapping the given wiki engine.
    pub fn new(engine: Arc<Mutex<Engine>>) -> Self {
        Self {
            engine,
            min_content_length: 20,
        }
    }

    /// Set the minimum content length for ingestion (default 20).
    pub fn with_min_content_length(mut self, n: usize) -> Self {
        self.min_content_length = n;
        self
    }

    /// Spawn a background task to ingest knowledge from the latest messages.
    ///
    /// Takes the last N messages from a conversation turn. The ingestor
    /// extracts entities, creates nodes, and updates graph edges without
    /// blocking the main agent loop.
    pub fn spawn_ingest(&self, session_id: String, messages: Vec<Message>) {
        let engine = self.engine.clone();
        let min_len = self.min_content_length;

        tokio::spawn(async move {
            let result = ingest_turn(&engine, &session_id, &messages, min_len).await;
            match result {
                Ok(count) => {
                    debug!(
                        session = %session_id,
                        nodes_created = count,
                        "Ingestor: turn processed"
                    );
                }
                Err(e) => {
                    warn!(
                        session = %session_id,
                        error = %e,
                        "Ingestor: turn processing error"
                    );
                }
            }
        });
    }
}

/// Core ingestion logic (runs inside the spawned task).
async fn ingest_turn(
    engine: &Arc<Mutex<Engine>>,
    session_id: &str,
    messages: &[Message],
    min_content_length: usize,
) -> Result<usize, String> {
    // Extract the text content from each message.
    let texts: Vec<String> = messages
        .iter()
        .filter_map(|m| m.content.as_text().map(|t| t.to_string()))
        .collect();

    if texts.is_empty() {
        return Ok(0);
    }

    let combined = texts.join("\n");

    // Extract entities using heuristics.
    let entities = extract_entities(&combined, min_content_length);
    if entities.is_empty() {
        return Ok(0);
    }

    // Lock the engine and write nodes.
    let _eng = engine.lock().await;
    let mut count = 0;

    for entity in &entities {
        let node = KnowledgeNode::new(
            entity.title.clone(),
            entity.content.clone(),
            entity.node_type.clone(),
            entity.tags.clone(),
        )
        .with_session(session_id.to_string());

        // Insert into the session layer.
        Engine::insert_session(session_id, &node)?;
        count += 1;
    }

    info!(
        session = %session_id,
        entities_found = entities.len(),
        "Ingestor: created {} knowledge nodes",
        count
    );

    Ok(count)
}

// ── Entity extraction (lightweight heuristics) ──────────────────────────────

/// A candidate knowledge node extracted from conversation text.
#[derive(Debug, Clone)]
struct ExtractedEntity {
    title: String,
    content: String,
    node_type: NodeType,
    tags: Vec<String>,
}

/// Recognize common coding patterns from text content.
fn classify_node_type(text: &str) -> NodeType {
    let lower = text.to_lowercase();

    // Pattern markers.
    if lower.contains("pattern")
        || lower.contains("idiom")
        || lower.contains("best practice")
        || lower.contains("anti-pattern")
    {
        return NodeType::Pattern;
    }

    // Decision markers.
    if lower.contains("decided")
        || lower.contains("chose")
        || lower.contains("because")
        || lower.contains("trade-off")
        || lower.contains("prefer ")
    {
        return NodeType::Decision;
    }

    // Rule markers.
    if lower.contains("always")
        || lower.contains("never")
        || lower.contains("must")
        || lower.contains("should")
        || lower.contains("rule")
        || lower.contains("convention")
    {
        return NodeType::Rule;
    }

    // Fact markers.
    if lower.contains("is a")
        || lower.contains("version")
        || lower.contains("deprecated")
        || lower.contains("introduced in")
    {
        return NodeType::Fact;
    }

    NodeType::Concept
}

/// Extract tags from text using keyword frequency.
fn extract_tags(text: &str) -> Vec<String> {
    let keywords = [
        "rust", "tokio", "async", "sqlite", "serde", "json",
        "react", "next.js", "typescript", "python", "docker",
        "kubernetes", "api", "database", "testing", "error",
        "performance", "security", "memory", "concurrency",
        "cli", "tui", "web", "wasm", "ffi",
    ];

    let lower = text.to_lowercase();
    let mut tags: Vec<String> = keywords
        .iter()
        .filter(|kw| lower.contains(&**kw))
        .map(|kw| kw.to_string())
        .collect();

    // Also capture capitalized identifiers (Rust types, function names).
    for word in text.split_whitespace() {
        let trimmed = word.trim_matches(|c: char| !c.is_alphanumeric());
        if trimmed.len() > 3
            && trimmed.chars().next().map_or(false, |c| c.is_uppercase())
            && trimmed.chars().all(|c| c.is_alphanumeric() || c == '_')
        {
            let lower_trimmed = trimmed.to_lowercase();
            if !tags.contains(&lower_trimmed) {
                tags.push(lower_trimmed);
            }
        }
    }

    tags.sort();
    tags.dedup();
    tags
}

/// Extract entities from a combined conversation text blob.
fn extract_entities(text: &str, min_content_length: usize) -> Vec<ExtractedEntity> {
    let mut entities: Vec<ExtractedEntity> = Vec::new();
    let mut seen_fingerprints: HashSet<String> = HashSet::new();

    // Split into sentences (roughly).
    let sentences: Vec<&str> = text
        .split(|c| c == '.' || c == '\n' || c == '!')
        .map(str::trim)
        .filter(|s| s.len() >= min_content_length)
        .collect();

    for sentence in &sentences {
        // Generate a title from the first ~8 words.
        let words: Vec<&str> = sentence.split_whitespace().collect();
        let title_words: Vec<&str> = words.iter().take(8).copied().collect();
        let title = title_words.join(" ");

        if title.len() < 10 {
            continue;
        }

        // Deduplicate by content fingerprint (first 50 chars).
        let fingerprint = sentence.chars().take(50).collect::<String>();
        if seen_fingerprints.contains(&fingerprint) {
            continue;
        }
        seen_fingerprints.insert(fingerprint);

        let node_type = classify_node_type(sentence);
        let tags = extract_tags(sentence);

        entities.push(ExtractedEntity {
            title,
            content: sentence.to_string(),
            node_type,
            tags,
        });
    }

    // Limit to at most 10 entities per turn to avoid flooding.
    entities.truncate(10);
    entities
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_pattern() {
        assert_eq!(
            classify_node_type("The visitor pattern is common in Rust."),
            NodeType::Pattern
        );
    }

    #[test]
    fn test_classify_decision() {
        assert_eq!(
            classify_node_type("We decided to use SQLite because of simplicity."),
            NodeType::Decision
        );
    }

    #[test]
    fn test_classify_rule() {
        assert_eq!(
            classify_node_type("Always use Result for fallible functions."),
            NodeType::Rule
        );
    }

    #[test]
    fn test_classify_fact() {
        // "is a" triggers fact classification
        assert_eq!(
            classify_node_type("Rust is a systems programming language."),
            NodeType::Fact
        );
    }

    #[test]
    fn test_classify_concept_default() {
        // No pattern/decision/rule/fact keywords → defaults to Concept
        assert_eq!(
            classify_node_type("Rust 1.70 introduced OnceCell."),
            NodeType::Concept
        );
    }

    #[test]
    fn test_extract_tags() {
        let tags = extract_tags("We use async Rust with Tokio and Serde for JSON.");
        assert!(tags.contains(&"rust".to_string()));
        assert!(tags.contains(&"async".to_string()));
        assert!(tags.contains(&"tokio".to_string()));
        assert!(tags.contains(&"serde".to_string()));
        assert!(tags.contains(&"json".to_string()));
    }

    #[test]
    fn test_extract_entities() {
        let text = "The observer pattern is useful for event systems. We decided to use Rust async with Tokio for concurrency.";
        let entities = extract_entities(text, 20);
        assert_eq!(entities.len(), 2);
        assert!(entities[0].title.contains("observer"));
        assert!(entities[1].title.contains("decided"));
    }

    #[test]
    fn test_extract_entities_min_length() {
        let text = "Short. This is a longer sentence that should be extracted.";
        let entities = extract_entities(text, 20);
        assert_eq!(entities.len(), 1);
        assert!(entities[0].title.contains("This is"));
    }

    #[test]
    fn test_extract_entities_dedup() {
        let text = "Same content here. Same content here. Same content here.";
        let entities = extract_entities(text, 15);
        // Should deduplicate identical sentences.
        assert_eq!(entities.len(), 1);
    }
}
