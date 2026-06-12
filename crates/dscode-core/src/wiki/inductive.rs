//! Cross-session pattern detection via inductive reasoning.
//!
//! Scans the global wiki for recurring patterns across multiple sessions.
//! Produces [`PatternReport`]s that highlight clusters of related nodes,
//! emerging themes, and potential rules that can be promoted from
//! session-local to global knowledge.

use std::collections::{HashMap, HashSet};

use super::engine::{Engine, KnowledgeNode, NodeType};

/// A detected cross-session pattern.
#[derive(Debug, Clone)]
pub struct PatternReport {
    /// Descriptive name for the pattern.
    pub name: String,
    /// The shared theme or topic.
    pub theme: String,
    /// IDs of the nodes that form the cluster.
    pub node_ids: Vec<String>,
    /// Suggested node type for the extracted pattern.
    pub suggested_type: NodeType,
    /// Confidence score [0, 1].
    pub confidence: f64,
    /// Number of distinct sessions contributing to this pattern.
    pub session_count: usize,
}

/// Configuration for inductive analysis.
#[derive(Debug, Clone)]
pub struct InductiveConfig {
    /// Minimum number of nodes required to form a cluster.
    pub min_cluster_size: usize,
    /// Minimum number of distinct sessions for a cross-session pattern.
    pub min_sessions: usize,
    /// Jaccard similarity threshold for considering two nodes related.
    pub similarity_threshold: f64,
    /// Maximum number of patterns to return.
    pub max_patterns: usize,
}

impl Default for InductiveConfig {
    fn default() -> Self {
        Self {
            min_cluster_size: 3,
            min_sessions: 2,
            similarity_threshold: 0.25,
            max_patterns: 10,
        }
    }
}

/// Run inductive pattern detection on the global wiki.
pub fn detect_patterns(
    engine: &Engine,
    config: &InductiveConfig,
) -> Result<Vec<PatternReport>, String> {
    let all_nodes = engine.list_global()?;

    if all_nodes.len() < config.min_cluster_size {
        return Ok(vec![]);
    }

    // Build similarity matrix (undirected, weighted by Jaccard).
    let mut clusters: Vec<Vec<usize>> = Vec::new();
    let mut visited: HashSet<usize> = HashSet::new();

    for i in 0..all_nodes.len() {
        if visited.contains(&i) {
            continue;
        }
        let mut cluster: Vec<usize> = vec![i];
        visited.insert(i);

        for j in (i + 1)..all_nodes.len() {
            if visited.contains(&j) {
                continue;
            }
            let sim = jaccard_similarity(&all_nodes[i].content, &all_nodes[j].content);
            if sim >= config.similarity_threshold {
                cluster.push(j);
                visited.insert(j);
            }
        }

        if cluster.len() >= config.min_cluster_size {
            clusters.push(cluster);
        }
    }

    // Build reports from clusters.
    let mut reports: Vec<PatternReport> = Vec::new();

    for cluster in &clusters {
        // Collect node IDs and sessions.
        let node_ids: Vec<String> = cluster.iter().map(|&idx| all_nodes[idx].id.clone()).collect();
        let sessions: HashSet<&str> = cluster
            .iter()
            .filter_map(|&idx| {
                let s = all_nodes[idx].session_id.as_str();
                if s.is_empty() { None } else { Some(s) }
            })
            .collect();

        if sessions.len() < config.min_sessions {
            continue;
        }

        // Determine the theme from the most frequent title words.
        let theme = extract_theme(cluster.iter().map(|&idx| &all_nodes[idx]));

        // Determine suggested type from the majority node_type.
        let suggested_type = majority_type(cluster.iter().map(|&idx| &all_nodes[idx].node_type));

        // Confidence based on cluster cohesion and session count.
        let avg_sim: f64 = compute_cluster_cohesion(cluster, &all_nodes);
        let session_penalty = (sessions.len() as f64 / config.min_sessions as f64).min(1.0);
        let confidence = (avg_sim * 0.7 + session_penalty * 0.3).min(1.0);

        reports.push(PatternReport {
            name: format!("Pattern: {}", theme),
            theme,
            node_ids,
            suggested_type,
            confidence,
            session_count: sessions.len(),
        });
    }

    // Sort by confidence descending.
    reports.sort_by(|a, b| b.confidence.partial_cmp(&a.confidence).unwrap_or(std::cmp::Ordering::Equal));
    reports.truncate(config.max_patterns);

    Ok(reports)
}

/// Compute Jaccard similarity between two strings.
fn jaccard_similarity(a: &str, b: &str) -> f64 {
    let tokens_a: HashSet<&str> = a.split_whitespace().collect();
    let tokens_b: HashSet<&str> = b.split_whitespace().collect();

    if tokens_a.is_empty() && tokens_b.is_empty() {
        return 1.0;
    }

    let intersection = tokens_a.intersection(&tokens_b).count();
    let union = tokens_a.union(&tokens_b).count();
    intersection as f64 / union as f64
}

/// Extract a theme from a set of nodes by finding the most frequent word
/// (excluding stop words).
fn extract_theme<'a>(nodes: impl Iterator<Item = &'a KnowledgeNode>) -> String {
    let stop_words: HashSet<&str> = [
        "the", "a", "an", "is", "are", "was", "were", "be", "been",
        "being", "have", "has", "had", "do", "does", "did", "will",
        "would", "could", "should", "may", "might", "can", "shall",
        "to", "of", "in", "for", "on", "with", "at", "by", "from",
        "this", "that", "these", "those", "it", "its", "and", "or",
        "but", "not", "no", "we", "you", "he", "she", "they", "i",
    ]
    .iter()
    .copied()
    .collect();

    let mut word_counts: HashMap<&str, usize> = HashMap::new();

    for node in nodes {
        for word in node.title.split_whitespace() {
            let lower = word.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
            if lower.len() < 3 || stop_words.contains(lower.as_str()) {
                continue;
            }
            // Leak the word for the HashMap key lifetime.
            *word_counts.entry(word).or_default() += 1;
        }
    }

    // Return the most frequent word.
    word_counts
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(word, _)| word.to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Determine the majority `NodeType` from an iterator.
fn majority_type<'a>(types: impl Iterator<Item = &'a NodeType>) -> NodeType {
    let mut counts: HashMap<&NodeType, usize> = HashMap::new();
    for t in types {
        *counts.entry(t).or_default() += 1;
    }
    counts
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(t, _)| t.clone())
        .unwrap_or(NodeType::Concept)
}

/// Compute the average Jaccard similarity within a cluster.
fn compute_cluster_cohesion(cluster: &[usize], nodes: &[KnowledgeNode]) -> f64 {
    if cluster.len() < 2 {
        return 1.0;
    }
    let mut total = 0.0;
    let mut count = 0;
    for i in 0..cluster.len() {
        for j in (i + 1)..cluster.len() {
            total += jaccard_similarity(&nodes[cluster[i]].content, &nodes[cluster[j]].content);
            count += 1;
        }
    }
    total / count as f64
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_node(id: &str, title: &str, content: &str, session: &str) -> KnowledgeNode {
        KnowledgeNode {
            id: id.to_string(),
            title: title.to_string(),
            content: content.to_string(),
            node_type: NodeType::Concept,
            tags: vec![],
            created_at: chrono::Utc::now().timestamp(),
            session_id: session.to_string(),
        }
    }

    #[test]
    fn test_majority_type() {
        let types = vec![
            NodeType::Pattern,
            NodeType::Pattern,
            NodeType::Rule,
            NodeType::Pattern,
        ];
        assert_eq!(majority_type(types.iter()), NodeType::Pattern);
    }

    #[test]
    fn test_extract_theme() {
        let nodes = vec![
            make_node("1", "Rust async functions", "", "s1"),
            make_node("2", "Async Rust patterns", "", "s2"),
            make_node("3", "Rust concurrency model", "", "s3"),
        ];
        let theme = extract_theme(nodes.iter());
        // "Rust" should be the most frequent non-stop word.
        assert!(theme.contains("Rust") || theme.contains("async") || theme.contains("Async"));
    }

    #[test]
    fn test_cluster_cohesion() {
        let nodes = vec![
            make_node("1", "t1", "a b c d e", "s1"),
            make_node("2", "t2", "a b c x y", "s1"),
            make_node("3", "t3", "a b z w", "s1"),
        ];
        let cluster = vec![0, 1, 2];
        let cohesion = compute_cluster_cohesion(&cluster, &nodes);
        assert!(cohesion > 0.0);
        assert!(cohesion <= 1.0);
    }
}
