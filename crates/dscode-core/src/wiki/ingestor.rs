//! Background ingestion agent — extracts entities and relations from
//! conversation turns after each message exchange.
//!
//! The ingestor is spawned via `tokio::spawn` after the agent finishes a turn.
//! It processes the latest assistant and user messages, uses a lightweight
//! heuristic (regex + keyword) extractor to identify potential knowledge nodes,
//! and writes them into the wiki engine.

use regex::Regex;
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

// ── Public auto-ingest entry point ───────────────────────────────────────────

/// Automatically ingest knowledge from a full conversation message set.
///
/// Runs in a background `tokio::spawn` task — non-blocking for the caller.
/// Creates an [`Engine`] internally, extracts key facts, file edits, decisions,
/// and errors from every message, and persists them as [`KnowledgeNode`]s into
/// the global and session wiki layers.
pub fn auto_ingest(session_id: String, messages: Vec<Message>) {
    tokio::spawn(async move {
        let engine = match Engine::new() {
            Ok(eng) => eng,
            Err(e) => {
                warn!(session = %session_id, error = %e, "auto_ingest: engine creation failed");
                return;
            }
        };

        match auto_ingest_impl(&engine, &session_id, &messages) {
            Ok(count) => {
                info!(
                    session = %session_id,
                    nodes_created = count,
                    "auto_ingest: knowledge nodes created"
                );
            }
            Err(e) => {
                warn!(
                    session = %session_id,
                    error = %e,
                    "auto_ingest: processing failed"
                );
            }
        }
    });
}

/// Core auto-ingest logic — extracts entities from all messages and persists them.
fn auto_ingest_impl(
    engine: &Engine,
    session_id: &str,
    messages: &[Message],
) -> Result<usize, String> {
    // Collect text from all non-empty messages.
    let texts: Vec<String> = messages
        .iter()
        .filter_map(|m| m.content.as_text().map(|t| t.to_string()))
        .collect();

    if texts.is_empty() {
        return Ok(0);
    }

    // Use enhanced heuristics: file paths, errors, decisions, edits.
    let entities = extract_entities_enhanced(&texts);
    if entities.is_empty() {
        return Ok(0);
    }

    let mut count = 0;
    for entity in &entities {
        let node = KnowledgeNode::new(
            entity.title.clone(),
            entity.content.clone(),
            entity.node_type.clone(),
            entity.tags.clone(),
        )
        .with_session(session_id.to_string());

        // Insert into global layer first, then session.
        engine.insert_global(&node)?;
        Engine::insert_session(session_id, &node)?;
        count += 1;
    }

    info!(
        session = %session_id,
        entities_found = entities.len(),
        "auto_ingest: created {} knowledge nodes",
        count
    );

    Ok(count)
}

// ── Enhanced entity extraction (regex-based heuristics) ──────────────────────

/// Extract entities from a list of message texts using enhanced heuristics.
fn extract_entities_enhanced(texts: &[String]) -> Vec<ExtractedEntity> {
    let mut entities: Vec<ExtractedEntity> = Vec::new();
    let mut seen_fingerprints: HashSet<String> = HashSet::new();

    // Compile regex patterns (lazily).
    let file_path_re = Regex::new(
        r#"(?:[\w\-./\\]+\.(?:rs|py|tsx?|jsx?|go|java|toml|ya?ml|json|md|css|html|sql))"#,
    )
    .unwrap();
    let error_re = Regex::new(
        r"(?i)\b(error|panic|crash|fail(?:ed|ure)?|cannot|unable|timeout|refused|denied|not found|invalid)\b",
    )
    .unwrap();
    let decision_re = Regex::new(
        r"(?i)\b(fix:|feat:|refactor:|chore:|decided|chose|we should|we will|let's|i'll)\b",
    )
    .unwrap();

    for text in texts {
        // ── Extract file-path entities ──
        for caps in file_path_re.captures_iter(text) {
            let path = caps.get(0).unwrap().as_str().to_string();
            let fp = path.chars().take(50).collect::<String>();
            if seen_fingerprints.contains(&fp) || path.len() < 4 {
                continue;
            }
            seen_fingerprints.insert(fp);

            entities.push(ExtractedEntity {
                title: path.clone(),
                content: format!("File referenced in conversation: {}", path),
                node_type: NodeType::Fact,
                tags: extract_tags(text),
            });
        }

        // ── Extract error entities ──
        for caps in error_re.captures_iter(text) {
            let err_word = caps.get(1).unwrap().as_str().to_string();
            // Capture the surrounding context (~100 chars around the match).
            let match_start = caps.get(0).unwrap().start();
            let ctx_start = match_start.saturating_sub(60);
            let ctx_end = (match_start + 100).min(text.len());
            let context = text[ctx_start..ctx_end].trim().to_string();

            let fingerprint = context.chars().take(50).collect::<String>();
            if seen_fingerprints.contains(&fingerprint) || context.len() < 15 {
                continue;
            }
            seen_fingerprints.insert(fingerprint);

            let title = format!("Error: {}", err_word);
            entities.push(ExtractedEntity {
                title,
                content: context,
                node_type: NodeType::Fact,
                tags: {
                    let mut tags = extract_tags(text);
                    tags.push("error".to_string());
                    tags
                },
            });
        }

        // ── Extract decision entities ──
        for caps in decision_re.captures_iter(text) {
            let marker = caps.get(1).unwrap().as_str().to_string();
            // Capture the sentence around the decision marker.
            let match_start = caps.get(0).unwrap().start();
            let ctx_start = match_start.saturating_sub(40);
            let ctx_end = (match_start + 160).min(text.len());
            let context = text[ctx_start..ctx_end].trim().to_string();

            let fingerprint = context.chars().take(50).collect::<String>();
            if seen_fingerprints.contains(&fingerprint) || context.len() < 15 {
                continue;
            }
            seen_fingerprints.insert(fingerprint);

            let title = format!("Decision: {}", marker);
            entities.push(ExtractedEntity {
                title,
                content: context,
                node_type: NodeType::Decision,
                tags: extract_tags(text),
            });
        }
    }

    // Also run the standard sentence-based extraction for pattern/rule/concept.
    let combined = texts.join("\n");
    let standard_entities = extract_entities(&combined, 30);
    for se in standard_entities {
        let fingerprint = se.content.chars().take(50).collect::<String>();
        if !seen_fingerprints.contains(&fingerprint) {
            seen_fingerprints.insert(fingerprint);
            entities.push(se);
        }
    }

    // Limit to at most 20 entities to avoid flooding.
    entities.truncate(20);
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

    #[test]
    fn test_extract_enhanced_file_paths() {
        let texts = vec![
            "I edited the file src/main.rs to add the new feature.".to_string(),
            "The config lives in crates/dscode-core/src/config/settings.toml.".to_string(),
        ];
        let entities = extract_entities_enhanced(&texts);
        // Should find file path entities.
        assert!(entities.iter().any(|e| e.content.contains("src/main.rs")));
        assert!(entities.iter().any(|e| e.content.contains("settings.toml")));
    }

    #[test]
    fn test_extract_enhanced_errors() {
        let texts = vec![
            "Build failed with error: cannot find module 'serde'".to_string(),
        ];
        let entities = extract_entities_enhanced(&texts);
        // Should find error entities.
        assert!(entities.iter().any(|e| e.title.contains("Error:")));
        assert!(entities.iter().any(|e| e.tags.contains(&"error".to_string())));
    }

    #[test]
    fn test_extract_enhanced_decisions() {
        let texts = vec![
            "fix: resolve race condition in the wiki ingestor".to_string(),
            "We decided to use regex for entity extraction.".to_string(),
        ];
        let entities = extract_entities_enhanced(&texts);
        // Should find decision entities (title starts with "Decision:").
        assert!(entities.iter().any(|e| e.node_type == NodeType::Decision));
    }

    #[test]
    fn test_extract_enhanced_empty() {
        let texts: Vec<String> = vec![];
        let entities = extract_entities_enhanced(&texts);
        assert!(entities.is_empty());
    }
}
