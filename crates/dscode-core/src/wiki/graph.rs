//! Graph construction — weighted edges between knowledge nodes.
//!
//! The [`Graph`] represents relationships between [`KnowledgeNode`]s.  Edge
//! weights are computed from:
//!
//! - **Co-occurrence** — how often two nodes appear in the same context window.
//! - **Semantic similarity** — a lightweight approximation using shared tokens
//!   (Jaccard index over tokenized content), suitable for use without an
//!   external embedding service.
//!
//! The graph can be serialized to JSON for sigma.js visualization (see
//! [`crate::wiki::export`]).

use serde::Serialize;
use std::collections::{HashMap, HashSet};

use super::engine::KnowledgeNode;

// ── Edge ────────────────────────────────────────────────────────────────────

/// A weighted directed (or undirected) edge between two knowledge nodes.
#[derive(Debug, Clone, Serialize)]
pub struct Edge {
    /// Source node ID.
    pub source: String,
    /// Target node ID.
    pub target: String,
    /// Weight in range [0, 1].
    pub weight: f64,
    /// The primary basis for the edge weight.
    #[serde(rename = "weight_type")]
    pub weight_type: WeightType,
}

/// Describes how the edge weight was computed.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WeightType {
    /// Simple co-occurrence count normalised to [0, 1].
    CoOccurrence,
    /// Jaccard index over tokenised content.
    Semantic,
    /// Average of co-occurrence and semantic.
    Hybrid,
}

impl WeightType {
    /// Return the snake_case string representation.
    pub fn as_str(&self) -> &'static str {
        match self {
            WeightType::CoOccurrence => "co_occurrence",
            WeightType::Semantic => "semantic",
            WeightType::Hybrid => "hybrid",
        }
    }
}

// ── Graph ───────────────────────────────────────────────────────────────────

/// A weighted knowledge graph over a set of nodes.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Graph {
    /// All nodes in this graph.
    pub nodes: Vec<KnowledgeNode>,
    /// All edges with weights.
    pub edges: Vec<Edge>,
    /// Adjacency list: node_id -> [(neighbour_id, weight)].
    adjacency: HashMap<String, Vec<(String, f64)>>,
}

impl Graph {
    /// Create an empty graph.
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a graph from a set of nodes, computing edges via semantic similarity.
    pub fn from_nodes(nodes: Vec<KnowledgeNode>) -> Self {
        let mut graph = Self::new();
        graph.add_nodes(nodes);
        graph.compute_semantic_edges(0.1);
        graph
    }

    /// Add nodes to the graph.
    pub fn add_nodes(&mut self, nodes: Vec<KnowledgeNode>) {
        for node in nodes {
            self.add_node(node);
        }
    }

    /// Add a single node.
    pub fn add_node(&mut self, node: KnowledgeNode) {
        self.nodes.push(node);
    }

    /// Remove a node by id, along with all its incident edges.
    pub fn remove_node(&mut self, id: &str) {
        self.nodes.retain(|n| n.id != id);
        self.edges.retain(|e| e.source != id && e.target != id);
        self.adjacency.remove(id);
        for (_, neighbours) in self.adjacency.iter_mut() {
            neighbours.retain(|(nid, _)| nid != id);
        }
    }

    /// Add or update an edge (upsert).  If an edge between the same
    /// source→target already exists, its weight is replaced.
    pub fn set_edge(&mut self, edge: Edge) {
        // Remove existing edge for the same (source, target) pair.
        self.edges
            .retain(|e| !(e.source == edge.source && e.target == edge.target));

        self.adjacency
            .entry(edge.source.clone())
            .or_default()
            .push((edge.target.clone(), edge.weight));

        self.edges.push(edge);
    }

    /// Upsert a co-occurrence edge: creates or increments the weight.
    pub fn bump_co_occurrence(&mut self, source: &str, target: &str, increment: f64) {
        let existing_weight = self
            .edges
            .iter()
            .find(|e| e.source == source && e.target == target)
            .map(|e| e.weight)
            .unwrap_or(0.0);

        let new_weight = (existing_weight + increment).min(1.0);

        self.set_edge(Edge {
            source: source.to_string(),
            target: target.to_string(),
            weight: new_weight,
            weight_type: WeightType::CoOccurrence,
        });
    }

    /// Compute edges based on semantic (Jaccard) similarity.
    ///
    /// Only edges with weight >= `min_weight` are kept (default 0.1).
    pub fn compute_semantic_edges(&mut self, min_weight: f64) {
        for i in 0..self.nodes.len() {
            for j in (i + 1)..self.nodes.len() {
                let weight = jaccard_similarity(
                    &self.nodes[i].content,
                    &self.nodes[j].content,
                );
                if weight >= min_weight {
                    let a = self.nodes[i].id.clone();
                    let b = self.nodes[j].id.clone();
                    self.set_edge(Edge {
                        source: a.clone(),
                        target: b.clone(),
                        weight,
                        weight_type: WeightType::Semantic,
                    });
                    self.set_edge(Edge {
                        source: b,
                        target: a,
                        weight,
                        weight_type: WeightType::Semantic,
                    });
                }
            }
        }
    }

    /// Merge co-occurrence and semantic edges to produce hybrid weights.
    ///
    /// For every pair (u, v) that appears in either list, the hybrid weight is
    /// the average of the two (or just the one that exists).
    pub fn merge_hybrid(&mut self, co_occurrence: &Graph, semantic: &Graph) {
        // Build lookup maps.
        let co_map: HashMap<(&str, &str), f64> = co_occurrence
            .edges
            .iter()
            .map(|e| ((e.source.as_str(), e.target.as_str()), e.weight))
            .collect();
        let sem_map: HashMap<(&str, &str), f64> = semantic
            .edges
            .iter()
            .map(|e| ((e.source.as_str(), e.target.as_str()), e.weight))
            .collect();

        let mut seen_pairs: HashSet<(&str, &str)> = HashSet::new();

        for co in &co_occurrence.edges {
            let key = (co.source.as_str(), co.target.as_str());
            if seen_pairs.contains(&key) {
                continue;
            }
            seen_pairs.insert(key);
            let sem_w = sem_map.get(&key).copied().unwrap_or(0.0);
            let hybrid_w = (co.weight + sem_w) / 2.0;
            self.set_edge(Edge {
                source: co.source.clone(),
                target: co.target.clone(),
                weight: hybrid_w,
                weight_type: WeightType::Hybrid,
            });
        }

        for sem in &semantic.edges {
            let key = (sem.source.as_str(), sem.target.as_str());
            if seen_pairs.contains(&key) {
                continue;
            }
            seen_pairs.insert(key);
            let co_w = co_map.get(&key).copied().unwrap_or(0.0);
            let hybrid_w = (co_w + sem.weight) / 2.0;
            self.set_edge(Edge {
                source: sem.source.clone(),
                target: sem.target.clone(),
                weight: hybrid_w,
                weight_type: WeightType::Hybrid,
            });
        }
    }

    // ── Traversal ────────────────────────────────────────────────────────

    /// Get all neighbours of a node, sorted by descending weight.
    pub fn neighbours(&self, node_id: &str) -> Vec<(String, f64)> {
        match self.adjacency.get(node_id) {
            Some(neighs) => {
                let mut v = neighs.clone();
                v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                v
            }
            None => vec![],
        }
    }

    /// Breadth-first traversal from `start_id`, returning nodes sorted by
    /// weighted BFS depth (higher weight = closer).
    pub fn related_nodes(&self, start_id: &str, max_depth: usize) -> Vec<(String, f64)> {
        let mut visited: HashSet<String> = HashSet::new();
        let mut results: Vec<(String, f64)> = Vec::new();

        // (node_id, accumulated_weight)
        let mut frontier: Vec<(String, f64)> = vec![(start_id.to_string(), 1.0)];
        visited.insert(start_id.to_string());

        for _depth in 0..max_depth {
            if frontier.is_empty() {
                break;
            }
            let mut next_frontier: Vec<(String, f64)> = Vec::new();
            for (nid, acc) in frontier {
                for (neigh_id, w) in self.neighbours(&nid) {
                    if visited.contains(&neigh_id) {
                        continue;
                    }
                    let new_acc = acc * w;
                    results.push((neigh_id.clone(), new_acc));
                    next_frontier.push((neigh_id.clone(), new_acc));
                }
            }
            // Mark newly discovered nodes as visited.
            for (nid, _) in &next_frontier {
                visited.insert(nid.clone());
            }
            frontier = next_frontier;
        }

        // Sort descending by accumulated weight.
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        results
    }

    /// Serialize the graph to JSON (compatible with sigma.js / Quartz).
    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self)
            .map_err(|e| format!("Graph serialization error: {}", e))
    }
}

// ── Jaccard similarity ──────────────────────────────────────────────────────

/// Compute the Jaccard index between two strings using whitespace-delimited
/// token sets.  Returns a value in [0, 1].
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

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_node(id: &str, content: &str) -> KnowledgeNode {
        KnowledgeNode {
            id: id.to_string(),
            title: format!("Node {}", id),
            content: content.to_string(),
            node_type: super::super::engine::NodeType::Concept,
            tags: vec![],
            created_at: 0,
            session_id: String::new(),
        }
    }

    #[test]
    fn test_jaccard_identical() {
        assert!((jaccard_similarity("a b c", "a b c") - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_jaccard_disjoint() {
        assert!((jaccard_similarity("a b c", "d e f") - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_jaccard_partial() {
        let sim = jaccard_similarity("rust async tokio", "rust sync tokio");
        // tokens: {rust,async,tokio} vs {rust,sync,tokio}
        // intersection: {rust,tokio}=2, union: {rust,async,tokio,sync}=4 => 0.5
        assert!((sim - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_graph_build_and_neighbours() {
        let nodes = vec![
            make_node("1", "rust async tokio runtime"),
            make_node("2", "rust async tokio spawn"),
            make_node("3", "python django flask"),
        ];
        let graph = Graph::from_nodes(nodes);

        // Node 1 should have a semantic edge to node 2 (high overlap).
        let neighs = graph.neighbours("1");
        assert!(!neighs.is_empty());
        assert_eq!(neighs[0].0, "2");
        // Node 3 should be isolated from 1/2 (disjoint token sets).
    }

    #[test]
    fn test_related_nodes() {
        let nodes = vec![
            make_node("1", "rust async tokio runtime"),
            make_node("2", "rust async tokio spawn"),
            make_node("3", "rust sync mutex lock"),
        ];
        let graph = Graph::from_nodes(nodes);

        let related = graph.related_nodes("1", 2);
        // Should find node 2 and node 3.
        let ids: Vec<&str> = related.iter().map(|(id, _)| id.as_str()).collect();
        assert!(ids.contains(&"2"));
        assert!(ids.contains(&"3"));
    }

    #[test]
    fn test_co_occurrence_bump() {
        let nodes = vec![
            make_node("A", "content A"),
            make_node("B", "content B"),
        ];
        let mut graph = Graph::new();
        graph.add_nodes(nodes);
        graph.bump_co_occurrence("A", "B", 0.3);
        graph.bump_co_occurrence("A", "B", 0.3);

        let edge = graph.edges.iter().find(|e| e.source == "A" && e.target == "B");
        assert!(edge.is_some());
        assert!((edge.unwrap().weight - 0.6).abs() < 0.001);
    }

    #[test]
    fn test_to_json() {
        let nodes = vec![make_node("1", "hello world")];
        let graph = Graph::from_nodes(nodes);
        let json = graph.to_json().unwrap();
        assert!(json.contains("\"nodes\""));
        assert!(json.contains("\"edges\""));
    }

    #[test]
    fn test_remove_node() {
        let nodes = vec![
            make_node("X", "content X"),
            make_node("Y", "content Y"),
        ];
        let mut graph = Graph::from_nodes(nodes);
        graph.bump_co_occurrence("X", "Y", 0.5);
        assert_eq!(graph.nodes.len(), 2);
        // from_nodes adds semantic edges, so we check edges > 0
        assert!(graph.edges.len() >= 1);

        graph.remove_node("X");
        assert_eq!(graph.nodes.len(), 1);
        // After removing X, all edges involving X should be gone
        assert!(graph.edges.iter().all(|e| e.source != "X" && e.target != "X"),
            "edge involving X remains: {:?}", graph.edges);
    }
}
