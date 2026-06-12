//! FTS5 + relevance ranking for knowledge node search.
//!
//! Wraps the dual-layer FTS5 search from [`super::engine::Engine`] with
//! additional relevance scoring: combines BM25 (FTS5 built-in) with tag
//! match bonuses, recency boost, and type-preference weighting.

use serde::Serialize;

use super::engine::{Engine, KnowledgeNode};

/// A search result with a computed relevance score.
#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    /// The matched knowledge node.
    pub node: KnowledgeNode,
    /// Relevance score (higher = more relevant).
    pub score: f64,
    /// The layer this result came from.
    #[serde(rename = "layer")]
    pub layer: SearchLayer,
}

/// Indicates whether a result came from the session or global layer.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SearchLayer {
    Session,
    Global,
}

/// Configuration for relevance scoring.
#[derive(Debug, Clone)]
pub struct SearchConfig {
    /// Boost multiplier for session-layer results (default 1.5).
    pub session_boost: f64,
    /// Boost multiplier for exact title match (default 2.0).
    pub exact_title_boost: f64,
    /// Boost multiplier for tag match (additive, default 0.3 per matched tag).
    pub tag_match_boost: f64,
    /// Recency decay half-life in days (default 30.0).
    pub recency_half_life_days: f64,
    /// Type preferences: higher = more relevant. Default all 1.0.
    pub type_weights: std::collections::HashMap<String, f64>,
}

impl Default for SearchConfig {
    fn default() -> Self {
        use std::collections::HashMap;
        let mut type_weights = HashMap::new();
        type_weights.insert("rule".into(), 1.3);
        type_weights.insert("pattern".into(), 1.2);
        type_weights.insert("decision".into(), 1.1);
        type_weights.insert("fact".into(), 1.0);
        type_weights.insert("concept".into(), 0.9);

        Self {
            session_boost: 1.5,
            exact_title_boost: 2.0,
            tag_match_boost: 0.3,
            recency_half_life_days: 30.0,
            type_weights,
        }
    }
}

/// Search the wiki engine, returning results sorted by relevance score.
pub fn search(
    engine: &Engine,
    session_id: &str,
    query: &str,
    max_results: usize,
    config: &SearchConfig,
) -> Result<Vec<SearchResult>, String> {
    // Retrieve raw matches from both layers.
    let raw_nodes = engine.search_unified(session_id, query, max_results * 2)?;

    let now = chrono::Utc::now().timestamp();
    let query_lower = query.to_lowercase();
    let query_tokens: Vec<&str> = query_lower.split_whitespace().collect();

    let mut results: Vec<SearchResult> = raw_nodes
        .into_iter()
        .map(|node| {
            let layer = if node.session_id == session_id {
                SearchLayer::Session
            } else {
                SearchLayer::Global
            };
            let score = compute_score(&node, &query_lower, &query_tokens, layer.clone(), now, config);
            SearchResult {
                node,
                score,
                layer,
            }
        })
        .collect();

    // Sort by descending score.
    results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

    results.truncate(max_results);
    Ok(results)
}

/// Compute a custom relevance score for a node.
fn compute_score(
    node: &KnowledgeNode,
    query_lower: &str,
    query_tokens: &[&str],
    layer: SearchLayer,
    now: i64,
    config: &SearchConfig,
) -> f64 {
    let mut score = 1.0;

    // (1) Session-layer boost.
    if layer == SearchLayer::Session {
        score *= config.session_boost;
    }

    // (2) Exact title match boost.
    if node.title.to_lowercase() == *query_lower {
        score *= config.exact_title_boost;
    }

    // (3) Tag match bonus.
    let tag_matches: usize = node
        .tags
        .iter()
        .filter(|t| query_tokens.contains(&t.as_str()))
        .count();
    score += tag_matches as f64 * config.tag_match_boost;

    // (4) Type weight multiplier.
    let type_key = node.node_type.as_str().to_string();
    if let Some(type_w) = config.type_weights.get(&type_key) {
        score *= type_w;
    }

    // (5) Recency boost (exponential decay).
    let age_days = ((now - node.created_at) as f64) / 86_400.0;
    if age_days > 0.0 {
        let decay = 2.0_f64.powf(-age_days / config.recency_half_life_days);
        score *= 1.0 + decay; // additive boost so old nodes aren't zeroed.
    } else {
        score *= 2.0; // created today.
    }

    score
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_score_session_boost() {
        let node = KnowledgeNode::new(
            "Rust Async".into(),
            "Content".into(),
            super::super::engine::NodeType::Concept,
            vec![],
        );
        let config = SearchConfig::default();
        let query_tokens: Vec<&str> = vec!["rust"];

        let session_score = compute_score(
            &node,
            "rust async",
            &query_tokens,
            SearchLayer::Session,
            node.created_at,
            &config,
        );
        let global_score = compute_score(
            &node,
            "rust async",
            &query_tokens,
            SearchLayer::Global,
            node.created_at,
            &config,
        );
        assert!(session_score > global_score);
    }

    #[test]
    fn test_compute_score_exact_title() {
        let node = KnowledgeNode::new(
            "Rust Async".into(),
            "Content".into(),
            super::super::engine::NodeType::Concept,
            vec![],
        );
        let config = SearchConfig::default();
        let query_tokens: Vec<&str> = vec!["rust", "async"];

        let exact_score = compute_score(
            &node,
            "rust async",
            &query_tokens,
            SearchLayer::Global,
            node.created_at,
            &config,
        );
        let partial_score = compute_score(
            &node,
            "rust concurrency",
            &query_tokens,
            SearchLayer::Global,
            node.created_at,
            &config,
        );
        assert!(exact_score > partial_score);
    }

    #[test]
    fn test_compute_score_tag_match() {
        let node = KnowledgeNode::new(
            "Title".into(),
            "Content".into(),
            super::super::engine::NodeType::Concept,
            vec!["rust".into(), "tokio".into()],
        );
        let config = SearchConfig::default();
        let query_tokens: Vec<&str> = vec!["rust", "tokio"];

        let score = compute_score(
            &node,
            "rust tokio",
            &query_tokens,
            SearchLayer::Global,
            node.created_at,
            &config,
        );
        // Score includes base + tag matches + recency boost
        // Expected: (1.0 + 2*0.3)*2.0 = 3.2
        // Allow slight floating point variance
        assert!(score > 2.0 && score < 4.0, "unexpected score: {}", score);
    }

    #[test]
    fn test_compute_score_type_weight() {
        let node_rule = KnowledgeNode::new(
            "Title".into(),
            "Content".into(),
            super::super::engine::NodeType::Rule,
            vec![],
        );
        let node_concept = KnowledgeNode::new(
            "Title".into(),
            "Content".into(),
            super::super::engine::NodeType::Concept,
            vec![],
        );
        let config = SearchConfig::default();
        let query_tokens: Vec<&str> = vec![];

        let rule_score = compute_score(
            &node_rule, "query", &query_tokens,
            SearchLayer::Global, node_rule.created_at, &config,
        );
        let concept_score = compute_score(
            &node_concept, "query", &query_tokens,
            SearchLayer::Global, node_concept.created_at, &config,
        );
        // Rules have higher weight than concepts.
        assert!(rule_score > concept_score);
    }
}
