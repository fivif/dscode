//! Graph construction — weighted edges between knowledge nodes.
//!
//! The [`Graph`] represents relationships between [`KnowledgeNode`]s.  Edge
//! weights are computed from a **4-signal relevance model**:
//!
//! | Signal        | Weight | Description                                      |
//! |---------------|--------|--------------------------------------------------|
//! | Direct wikilink   | 3.0    | `[[page-name]]` in content                       |
//! | Source overlap    | 4.0    | Pages sharing the same session_id (co-created)   |
//! | Adamic-Adar       | 1.5    | Common neighbor weighted by log degree           |
//! | Type affinity     | 1.0    | Same or related types get bonus                  |
//!
//! Signals are combined and normalized to [0, 1].  Edges are capped at 10 per
//! node.  A simple connected-components community detection is also provided.
//!
//! The graph can be serialized to JSON for sigma.js visualization (see
//! [`crate::wiki::export`]).

use serde::Serialize;
use std::collections::{HashMap, HashSet};

use super::engine::KnowledgeNode;

// ── Signal weights ──────────────────────────────────────────────────────────

/// Weight for direct wikilink edges.
const WIKILINK_WEIGHT: f64 = 3.0;
/// Weight for session-based (co-created) edges.
const SESSION_WEIGHT: f64 = 4.0;
/// Weight for Adamic-Adar edges.
const ADAMIC_ADAR_WEIGHT: f64 = 1.5;
/// Weight for type-affinity edges.
const TYPE_AFFINITY_WEIGHT: f64 = 1.0;
/// Max edges per node after combination.
const MAX_EDGES_PER_NODE: usize = 10;

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
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum WeightType {
    /// Simple co-occurrence count normalised to [0, 1].
    CoOccurrence,
    /// Jaccard index over tokenised content.
    Semantic,
    /// Average of co-occurrence and semantic.
    Hybrid,
    /// [[wikilink]] reference in content.
    Wikilink,
    /// Same session_id (co-created).
    Session,
    /// Adamic-Adar common-neighbor index.
    AdamicAdar,
    /// Same or related NodeType.
    TypeAffinity,
    /// Combined from multiple signals (weighted sum, normalized).
    Combined,
}

impl WeightType {
    /// Return the snake_case string representation.
    pub fn as_str(&self) -> &'static str {
        match self {
            WeightType::CoOccurrence => "co_occurrence",
            WeightType::Semantic => "semantic",
            WeightType::Hybrid => "hybrid",
            WeightType::Wikilink => "wikilink",
            WeightType::Session => "session",
            WeightType::AdamicAdar => "adamic_adar",
            WeightType::TypeAffinity => "type_affinity",
            WeightType::Combined => "combined",
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

    // ── 4-signal construction ─────────────────────────────────────────────

    /// Build a graph from a set of nodes using the 4-signal relevance model.
    ///
    /// This is the main entry point.  It computes wikilink, session,
    /// Adamic-Adar, and type-affinity edges, combines them with weighted
    /// summation, normalizes to [0, 1], and caps at `MAX_EDGES_PER_NODE`
    /// outgoing edges per node.
    pub fn from_nodes(nodes: Vec<KnowledgeNode>) -> Self {
        let mut graph = Self::new();
        graph.add_nodes(nodes);

        // 1. Compute each signal individually.
        let wikilink_edges = graph.compute_wikilink_edges();
        let session_edges = graph.compute_session_edges();
        let type_edges = graph.compute_type_affinity_edges();

        // 2. Build a preliminary adjacency (wikilink + session) for Adamic-Adar.
        let prelim_adj = graph.build_preliminary_adjacency(&wikilink_edges, &session_edges);
        let adamic_adar_edges = graph.compute_adamic_adar_edges(&prelim_adj);

        // 3. Combine all signals into combined_edges.
        graph.combine_signals(
            &wikilink_edges,
            &session_edges,
            &type_edges,
            &adamic_adar_edges,
        );

        // 4. Normalize to [0, 1].
        graph.normalize_weights();

        // 5. Cap edges per node.
        graph.cap_edges_per_node(MAX_EDGES_PER_NODE);

        graph
    }

    // ── Signal 1: Wikilink edges ──────────────────────────────────────────

    /// Parse `[[page-name]]` or `[[page-name|alias]]` from content and create
    /// edges where a node's content references another node's title.
    ///
    /// Returns a lookup: (source_id, target_id) -> raw contribution (always 1.0).
    fn compute_wikilink_edges(&self) -> HashMap<(String, String), f64> {
        let mut edges: HashMap<(String, String), f64> = HashMap::new();

        // Build a set of known titles for fast lookup.
        let title_to_ids: HashMap<String, Vec<String>> = self
            .nodes
            .iter()
            .map(|n| (n.title.to_lowercase(), n.id.clone()))
            .fold(
                HashMap::new(),
                |mut acc, (title, id)| {
                    acc.entry(title).or_default().push(id);
                    acc
                },
            );

        for source_node in &self.nodes {
            let refs = extract_wikilinks(&source_node.content);
            for target_title in refs {
                let key = target_title.to_lowercase();
                if let Some(target_ids) = title_to_ids.get(&key) {
                    for target_id in target_ids {
                        if *target_id == source_node.id {
                            continue;
                        }
                        edges
                            .entry((source_node.id.clone(), target_id.clone()))
                            .or_insert(1.0);
                    }
                }
            }
        }

        edges
    }

    // ── Signal 2: Session (source overlap) edges ──────────────────────────

    /// Nodes that share the same (non-empty) `session_id` are linked — they
    /// were co-created in the same conversation.
    ///
    /// Returns a lookup: (source_id, target_id) -> raw contribution (always 1.0).
    fn compute_session_edges(&self) -> HashMap<(String, String), f64> {
        let mut edges: HashMap<(String, String), f64> = HashMap::new();

        // Group node ids by session_id (skip empty/global sessions).
        let mut session_groups: HashMap<String, Vec<String>> = HashMap::new();
        for node in &self.nodes {
            if !node.session_id.is_empty() {
                session_groups
                    .entry(node.session_id.clone())
                    .or_default()
                    .push(node.id.clone());
            }
        }

        // Create edges between all pairs within the same session.
        for ids in session_groups.values() {
            for i in 0..ids.len() {
                for j in (i + 1)..ids.len() {
                    let a = &ids[i];
                    let b = &ids[j];
                    edges.insert((a.clone(), b.clone()), 1.0);
                    edges.insert((b.clone(), a.clone()), 1.0);
                }
            }
        }

        edges
    }

    // ── Signal 3: Type-affinity edges ─────────────────────────────────────

    /// Same-type and related-type pairs get a fixed bonus.
    ///
    /// Affinity table:
    ///   pattern↔pattern: 1.5   decision↔decision: 1.2
    ///   concept↔concept: 1.2   fact↔fact: 0.8   rule↔rule: 1.0
    ///   cross-type: 0.5
    ///
    /// Returns a lookup: (source_id, target_id) -> affinity score.
    fn compute_type_affinity_edges(&self) -> HashMap<(String, String), f64> {
        let mut edges: HashMap<(String, String), f64> = HashMap::new();

        for i in 0..self.nodes.len() {
            for j in (i + 1)..self.nodes.len() {
                let affinity = type_affinity(&self.nodes[i].node_type, &self.nodes[j].node_type);
                if affinity > 0.0 {
                    let a = self.nodes[i].id.clone();
                    let b = self.nodes[j].id.clone();
                    edges.insert((a.clone(), b.clone()), affinity);
                    edges.insert((b, a), affinity);
                }
            }
        }

        edges
    }

    /// Build a preliminary adjacency map from wikilink + session edges,
    /// used as input for Adamic-Adar computation.
    fn build_preliminary_adjacency(
        &self,
        wikilink_edges: &HashMap<(String, String), f64>,
        session_edges: &HashMap<(String, String), f64>,
    ) -> HashMap<String, HashSet<String>> {
        let mut adj: HashMap<String, HashSet<String>> = HashMap::new();
        // Initialize empty sets for all nodes.
        for node in &self.nodes {
            adj.entry(node.id.clone()).or_default();
        }
        for (src, tgt) in wikilink_edges.keys() {
            adj.entry(src.clone()).or_default().insert(tgt.clone());
        }
        for (src, tgt) in session_edges.keys() {
            adj.entry(src.clone()).or_default().insert(tgt.clone());
        }
        adj
    }

    // ── Signal 4: Adamic-Adar edges ───────────────────────────────────────

    /// Compute Adamic-Adar similarity for pairs that share at least one
    /// neighbor in the preliminary adjacency graph.
    ///
    /// AA(u, v) = sum_{z in common_neighbors(u, v)} 1 / log(deg(z) + 1)
    ///
    /// Returns a lookup: (source_id, target_id) -> raw AA score (capped at 1.0).
    fn compute_adamic_adar_edges(
        &self,
        prelim_adj: &HashMap<String, HashSet<String>>,
    ) -> HashMap<(String, String), f64> {
        let mut edges: HashMap<(String, String), f64> = HashMap::new();

        // Precompute degree for every node.
        let degree: HashMap<&String, f64> = prelim_adj
            .iter()
            .map(|(id, neighs)| (id, (neighs.len() as f64 + 1.0).ln()))
            .collect();

        let node_ids: Vec<&String> = self.nodes.iter().map(|n| &n.id).collect();

        for i in 0..node_ids.len() {
            let u = node_ids[i];
            let u_neighs = match prelim_adj.get(u) {
                Some(n) => n,
                None => continue,
            };
            let deg_u = degree.get(u).copied().unwrap_or(0.0);

            for j in (i + 1)..node_ids.len() {
                let v = node_ids[j];
                let v_neighs = match prelim_adj.get(v) {
                    Some(n) => n,
                    None => continue,
                };

                // Compute common neighbors with AA weighting.
                let aa: f64 = u_neighs
                    .intersection(v_neighs)
                    .map(|z| {
                        let deg_z = degree.get(z).copied().unwrap_or(deg_u);
                        1.0 / deg_z.max(1.0)
                    })
                    .sum();

                if aa > 0.0 {
                    // Cap per-edge AA contribution at 1.0.
                    let capped = aa.min(1.0);
                    edges.insert((u.clone(), v.clone()), capped);
                    edges.insert((v.clone(), u.clone()), capped);
                }
            }
        }

        edges
    }

    // ── Signal combination ────────────────────────────────────────────────

    /// Combine all four signals into a single set of combined edges.
    ///
    /// For each unique (source, target) pair that appears in any signal, the
    /// combined raw weight is:
    ///
    ///   wikilink_n * 3.0 + session_n * 4.0 + aa_score * 1.5 + affinity * 1.0
    ///
    /// where each signal contribution is its raw value (0–1 for wikilink/session,
    /// possibly fractional for AA and affinity).
    fn combine_signals(
        &mut self,
        wikilink: &HashMap<(String, String), f64>,
        session: &HashMap<(String, String), f64>,
        type_affinity: &HashMap<(String, String), f64>,
        adamic_adar: &HashMap<(String, String), f64>,
    ) {
        let mut combined: HashMap<(String, String), f64> = HashMap::new();

        // Collect all unique (src, tgt) pairs.
        let mut all_keys: HashSet<(String, String)> = HashSet::new();
        for k in wikilink.keys() {
            all_keys.insert(k.clone());
        }
        for k in session.keys() {
            all_keys.insert(k.clone());
        }
        for k in type_affinity.keys() {
            all_keys.insert(k.clone());
        }
        for k in adamic_adar.keys() {
            all_keys.insert(k.clone());
        }

        for key in &all_keys {
            let wl = wikilink.get(key).copied().unwrap_or(0.0) * WIKILINK_WEIGHT;
            let ss = session.get(key).copied().unwrap_or(0.0) * SESSION_WEIGHT;
            let aa = adamic_adar.get(key).copied().unwrap_or(0.0) * ADAMIC_ADAR_WEIGHT;
            let ta = type_affinity.get(key).copied().unwrap_or(0.0) * TYPE_AFFINITY_WEIGHT;
            combined.insert(key.clone(), wl + ss + aa + ta);
        }

        // Insert into graph edges.
        self.edges.clear();
        for ((src, tgt), weight) in &combined {
            self.edges.push(Edge {
                source: src.clone(),
                target: tgt.clone(),
                weight: *weight,
                weight_type: WeightType::Combined,
            });
        }
    }

    /// Normalize all edge weights into the [0, 1] range.
    ///
    /// Divides every weight by the maximum weight observed.  If max is 0,
    /// all weights stay at 0.
    fn normalize_weights(&mut self) {
        let max_w = self
            .edges
            .iter()
            .map(|e| e.weight)
            .fold(0.0_f64, f64::max);

        if max_w > 0.0 {
            for e in &mut self.edges {
                e.weight /= max_w;
            }
        }
    }

    // ── Capping / adjacency ───────────────────────────────────────────────

    /// Cap each node to at most `max_per_node` outgoing edges, keeping the
    /// strongest weights.  Rebuilds the adjacency list afterwards.
    pub fn cap_edges_per_node(&mut self, max_per_node: usize) {
        let mut node_edge_counts: HashMap<String, usize> = HashMap::new();
        // Sort by weight descending.
        self.edges
            .sort_by(|a, b| b.weight.partial_cmp(&a.weight).unwrap_or(std::cmp::Ordering::Equal));
        self.edges.retain(|e| {
            let count = node_edge_counts.entry(e.source.clone()).or_insert(0);
            if *count < max_per_node {
                *count += 1;
                true
            } else {
                false
            }
        });
        // Rebuild adjacency.
        self.adjacency.clear();
        for edge in &self.edges {
            self.adjacency
                .entry(edge.source.clone())
                .or_default()
                .push((edge.target.clone(), edge.weight));
        }
    }

    // ── Community detection ───────────────────────────────────────────────

    /// Simple connected-components community detection over the current edge
    /// set (treating edges as undirected).
    ///
    /// Returns a map: node_id -> component_id (0-based), and the total number
    /// of components.
    pub fn connected_components(&self) -> (HashMap<String, usize>, usize) {
        let mut comp: HashMap<String, usize> = HashMap::new();
        let mut comp_id: usize = 0;

        // Build an undirected adjacency set from current edges.
        let mut undirected: HashMap<String, Vec<String>> = HashMap::new();
        for edge in &self.edges {
            undirected
                .entry(edge.source.clone())
                .or_default()
                .push(edge.target.clone());
            undirected
                .entry(edge.target.clone())
                .or_default()
                .push(edge.source.clone());
        }

        // BFS from each unvisited node.
        for node in &self.nodes {
            if comp.contains_key(&node.id) {
                continue;
            }
            // BFS to label all reachable nodes with comp_id.
            let mut queue: Vec<String> = vec![node.id.clone()];
            comp.insert(node.id.clone(), comp_id);
            let mut front = 0usize;
            while front < queue.len() {
                let current = queue[front].clone();
                front += 1;
                if let Some(neighs) = undirected.get(&current) {
                    for nid in neighs {
                        if !comp.contains_key(nid) {
                            comp.insert(nid.clone(), comp_id);
                            queue.push(nid.clone());
                        }
                    }
                }
            }
            comp_id += 1;
        }

        (comp, comp_id)
    }

    // ── Graph insights ────────────────────────────────────────────────────

    /// Compute graph-level insights.
    ///
    /// Returns:
    /// - `surprising_connections`: cross-type edges with normalized weight >= 0.5.
    /// - `knowledge_gaps`: node IDs whose degree (outgoing edge count) is 0 or 1.
    pub fn compute_graph_insights(&self) -> GraphInsights {
        let mut surprising_connections: Vec<SurprisingConnection> = Vec::new();
        let mut knowledge_gaps: Vec<String> = Vec::new();

        // Build node lookup: id -> &KnowledgeNode
        let node_map: HashMap<&str, &KnowledgeNode> =
            self.nodes.iter().map(|n| (n.id.as_str(), n)).collect();

        // Compute degree per node.
        let mut degree: HashMap<&str, usize> = HashMap::new();
        for edge in &self.edges {
            *degree.entry(edge.source.as_str()).or_insert(0) += 1;
        }

        // Surprising connections: cross-type edges with high weight.
        for edge in &self.edges {
            if edge.weight < 0.5 {
                continue;
            }
            let src_node = node_map.get(edge.source.as_str());
            let tgt_node = node_map.get(edge.target.as_str());
            if let (Some(s), Some(t)) = (src_node, tgt_node) {
                if s.node_type != t.node_type {
                    surprising_connections.push(SurprisingConnection {
                        source: edge.source.clone(),
                        source_type: s.node_type.as_str().to_string(),
                        target: edge.target.clone(),
                        target_type: t.node_type.as_str().to_string(),
                        weight: edge.weight,
                    });
                }
            }
        }

        // Knowledge gaps: nodes with degree 0 or 1.
        for node in &self.nodes {
            let d = degree.get(node.id.as_str()).copied().unwrap_or(0);
            if d <= 1 {
                knowledge_gaps.push(node.id.clone());
            }
        }

        GraphInsights {
            surprising_connections,
            knowledge_gaps,
            num_components: self.connected_components().1,
        }
    }

    // ── CRUD ──────────────────────────────────────────────────────────────

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
    /// Retained for backward compatibility; new code should prefer
    /// [`Graph::from_nodes`] for the 4-signal model.
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

// ── Graph insights struct ─────────────────────────────────────────────────

/// Results from [`Graph::compute_graph_insights`].
#[derive(Debug, Clone, Serialize)]
pub struct GraphInsights {
    /// Cross-type edges with normalized weight >= 0.5.
    pub surprising_connections: Vec<SurprisingConnection>,
    /// Node IDs with degree 0 or 1.
    pub knowledge_gaps: Vec<String>,
    /// Number of connected components.
    pub num_components: usize,
}

/// A cross-type edge with high weight — potentially a "surprising" link.
#[derive(Debug, Clone, Serialize)]
pub struct SurprisingConnection {
    pub source: String,
    pub source_type: String,
    pub target: String,
    pub target_type: String,
    pub weight: f64,
}

// ── Helper: wikilink extraction ─────────────────────────────────────────────

/// Extract page names from `[[page-name]]` and `[[page-name|alias]]` syntax.
fn extract_wikilinks(text: &str) -> Vec<String> {
    let mut results = Vec::new();
    let len = text.len();

    // Simple state-machine parser to find [[ ... ]].
    let bytes = text.as_bytes();
    let mut pos = 0usize;
    while pos + 1 < len {
        // Look for [[
        if bytes[pos] == b'[' && bytes[pos + 1] == b'[' {
            pos += 2;
            let start = pos;
            while pos < len {
                if bytes[pos] == b']' && pos + 1 < len && bytes[pos + 1] == b']' {
                    // Found closing ]]
                    let raw = &text[start..pos];
                    // Handle alias: [[page|alias]] -> take "page"
                    let name = raw.split('|').next().unwrap_or(raw).trim();
                    if !name.is_empty() {
                        results.push(name.to_string());
                    }
                    pos += 2;
                    break;
                }
                pos += 1;
            }
        } else {
            pos += 1;
        }
    }

    results
}

// ── Helper: type affinity ──────────────────────────────────────────────────

/// Return the type-affinity score for a pair of [`NodeType`]s.
///
/// | Pair             | Score |
/// |------------------|-------|
/// | pattern↔pattern   | 1.5   |
/// | decision↔decision | 1.2   |
/// | concept↔concept   | 1.2   |
/// | fact↔fact         | 0.8   |
/// | rule↔rule         | 1.0   |
/// | cross-type        | 0.5   |
fn type_affinity(a: &super::engine::NodeType, b: &super::engine::NodeType) -> f64 {
    use super::engine::NodeType::*;
    match (a, b) {
        (Pattern, Pattern) => 1.5,
        (Decision, Decision) => 1.2,
        (Concept, Concept) => 1.2,
        (Fact, Fact) => 0.8,
        (Rule, Rule) => 1.0,
        _ => 0.5, // cross-type
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
    use super::super::engine::NodeType;

    fn make_node(id: &str, content: &str) -> KnowledgeNode {
        KnowledgeNode {
            id: id.to_string(),
            title: format!("Node {}", id),
            content: content.to_string(),
            node_type: NodeType::Concept,
            tags: vec![],
            created_at: 0,
            session_id: String::new(),
        }
    }

    fn make_typed_node(id: &str, title: &str, content: &str, nt: NodeType) -> KnowledgeNode {
        KnowledgeNode {
            id: id.to_string(),
            title: title.to_string(),
            content: content.to_string(),
            node_type: nt,
            tags: vec![],
            created_at: 0,
            session_id: String::new(),
        }
    }

    // ── Jaccard tests (unchanged) ───────────────────────────────────────────

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
        assert!((sim - 0.5).abs() < 0.001);
    }

    // ── Wikilink extraction ─────────────────────────────────────────────────

    #[test]
    fn test_extract_wikilinks_simple() {
        let links = extract_wikilinks("See [[Rust]] for details");
        assert_eq!(links, vec!["Rust"]);
    }

    #[test]
    fn test_extract_wikilinks_alias() {
        let links = extract_wikilinks("Use [[tokio|the Tokio runtime]] here");
        assert_eq!(links, vec!["tokio"]);
    }

    #[test]
    fn test_extract_wikilinks_multiple() {
        let links = extract_wikilinks("[[Rust]] and [[async]] and [[tokio]]");
        assert_eq!(links, vec!["Rust", "async", "tokio"]);
    }

    #[test]
    fn test_extract_wikilinks_none() {
        let links = extract_wikilinks("No wikilinks here");
        assert!(links.is_empty());
    }

    // ── Type affinity ──────────────────────────────────────────────────────

    #[test]
    fn test_type_affinity_pattern() {
        assert!((type_affinity(&NodeType::Pattern, &NodeType::Pattern) - 1.5).abs() < 0.001);
    }

    #[test]
    fn test_type_affinity_decision() {
        assert!((type_affinity(&NodeType::Decision, &NodeType::Decision) - 1.2).abs() < 0.001);
    }

    #[test]
    fn test_type_affinity_concept() {
        assert!((type_affinity(&NodeType::Concept, &NodeType::Concept) - 1.2).abs() < 0.001);
    }

    #[test]
    fn test_type_affinity_fact() {
        assert!((type_affinity(&NodeType::Fact, &NodeType::Fact) - 0.8).abs() < 0.001);
    }

    #[test]
    fn test_type_affinity_rule() {
        assert!((type_affinity(&NodeType::Rule, &NodeType::Rule) - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_type_affinity_cross() {
        assert!((type_affinity(&NodeType::Concept, &NodeType::Fact) - 0.5).abs() < 0.001);
        assert!((type_affinity(&NodeType::Pattern, &NodeType::Decision) - 0.5).abs() < 0.001);
    }

    // ── Wikilink edges ─────────────────────────────────────────────────────

    #[test]
    fn test_wikilink_edges() {
        let nodes = vec![
            make_typed_node("1", "Rust", "A language", NodeType::Concept),
            make_typed_node("2", "Tokio", "See [[Rust]] for the runtime", NodeType::Concept),
        ];
        let graph = Graph {
            nodes,
            edges: vec![],
            adjacency: HashMap::new(),
        };
        let edges = graph.compute_wikilink_edges();
        assert!(edges.contains_key(&("2".to_string(), "1".to_string())));
        // No self-link.
        assert!(!edges.contains_key(&("1".to_string(), "1".to_string())));
    }

    #[test]
    fn test_wikilink_edges_alias() {
        let nodes = vec![
            make_typed_node("1", "Async Runtime", "...", NodeType::Concept),
            make_typed_node("2", "Guide", "Use [[Async Runtime|the runtime]]", NodeType::Concept),
        ];
        let graph = Graph {
            nodes,
            edges: vec![],
            adjacency: HashMap::new(),
        };
        let edges = graph.compute_wikilink_edges();
        assert!(edges.contains_key(&("2".to_string(), "1".to_string())));
    }

    // ── Session edges ──────────────────────────────────────────────────────

    #[test]
    fn test_session_edges() {
        let mut n1 = make_node("A", "content");
        n1.session_id = "sess-1".into();
        let mut n2 = make_node("B", "content");
        n2.session_id = "sess-1".into();
        let mut n3 = make_node("C", "content");
        n3.session_id = String::new(); // global, no session
        let graph = Graph {
            nodes: vec![n1, n2, n3],
            edges: vec![],
            adjacency: HashMap::new(),
        };
        let edges = graph.compute_session_edges();
        // A <-> B from same session.
        assert!(edges.contains_key(&("A".to_string(), "B".to_string())));
        assert!(edges.contains_key(&("B".to_string(), "A".to_string())));
        // C should have no session edges.
        assert!(!edges.contains_key(&("C".to_string(), "A".to_string())));
        assert!(!edges.contains_key(&("C".to_string(), "B".to_string())));
    }

    // ── 4-signal graph construction ────────────────────────────────────────

    #[test]
    fn test_from_nodes_with_wikilinks() {
        let nodes = vec![
            make_typed_node("1", "Rust", "Systems programming language", NodeType::Concept),
            make_typed_node("2", "Tokio", "See [[Rust]] async runtime", NodeType::Pattern),
            make_typed_node("3", "Python", "Another language", NodeType::Concept),
        ];
        let graph = Graph::from_nodes(nodes);

        // Node 2 should have an edge to 1 via wikilink.
        let neighs = graph.neighbours("2");
        let ids: Vec<&str> = neighs.iter().map(|(id, _)| id.as_str()).collect();
        assert!(ids.contains(&"1"), "neighbours of 2: {:?}", ids);
    }

    #[test]
    fn test_weights_normalized() {
        let nodes = vec![
            make_typed_node("1", "A", "... [[B]]", NodeType::Concept),
            make_typed_node("2", "B", "...", NodeType::Concept),
        ];
        let graph = Graph::from_nodes(nodes);

        // All edges should have weight in [0, 1].
        for e in &graph.edges {
            assert!(e.weight >= 0.0 && e.weight <= 1.0,
                "weight {} out of range", e.weight);
        }
        // All edges should be Combined type.
        for e in &graph.edges {
            assert_eq!(e.weight_type, WeightType::Combined);
        }
    }

    #[test]
    fn test_related_nodes_with_new_model() {
        let nodes = vec![
            make_typed_node("1", "Rust", "... [[Tokio]]", NodeType::Concept),
            make_typed_node("2", "Tokio", "...", NodeType::Pattern),
            make_typed_node("3", "Python", "...", NodeType::Concept),
        ];
        let graph = Graph::from_nodes(nodes);

        let related = graph.related_nodes("1", 2);
        let ids: Vec<&str> = related.iter().map(|(id, _)| id.as_str()).collect();
        assert!(ids.contains(&"2"), "related should contain 2: {:?}", ids);
    }

    // ── Graph insights ────────────────────────────────────────────────────

    #[test]
    fn test_compute_graph_insights() {
        let mut n1 = make_typed_node("1", "Rust", "... [[Tokio]]", NodeType::Concept);
        let mut n2 = make_typed_node("2", "Tokio", "...", NodeType::Pattern);
        let mut n3 = make_typed_node("3", "AsyncDecision", "use tokio", NodeType::Decision);
        let n4 = make_typed_node("4", "IsolatedRule", "no refs", NodeType::Rule);
        n1.session_id = "s1".into();
        n2.session_id = "s1".into();
        n3.session_id = "s1".into();
        // n4 has no session, no wikilinks → only cross-type affinity edges.

        let graph = Graph::from_nodes(vec![n1, n2, n3, n4]);
        let insights = graph.compute_graph_insights();

        // n4 (Rule) only has cross-type affinity edges (0.5) to all 3 others.
        // After normalization these become weak but count.  With 3 peers, degree
        // will be >= 2, so it is NOT a knowledge gap.  Gaps require degree 0 or 1.
        // We mainly verify surprising connections exist (cross-type Concept↔Pattern).
        let sc_types: Vec<(&str, &str)> = insights
            .surprising_connections
            .iter()
            .map(|sc| (sc.source_type.as_str(), sc.target_type.as_str()))
            .collect();
        // Concept(1) <-> Pattern(2) should be surprising (cross-type, high weight).
        let has_cross = sc_types.contains(&("concept", "pattern"))
            || sc_types.contains(&("pattern", "concept"));
        assert!(has_cross, "expected cross-type surprising connection, got {:?}", sc_types);

        // With 4 nodes of varied types and session overlap, we likely have
        // at most one connected component.
        assert!(insights.num_components >= 1);
    }

    // ── Community detection ────────────────────────────────────────────────

    #[test]
    fn test_connected_components() {
        let nodes = vec![
            make_typed_node("A", "Alpha", "... [[Beta]]", NodeType::Concept),
            make_typed_node("B", "Beta", "...", NodeType::Concept),
            make_typed_node("C", "Gamma", "...", NodeType::Concept),
            make_typed_node("D", "Delta", "... [[Gamma]]", NodeType::Concept),
        ];
        let graph = Graph::from_nodes(nodes);

        let (_comp_map, num_components) = graph.connected_components();
        assert_eq!(num_components, 1, "should be one component");
    }

    #[test]
    fn test_connected_components_disjoint() {
        // Cross-type affinity still connects them into one component.
        let n1 = make_typed_node("A", "Alpha", "...", NodeType::Pattern);
        let n2 = make_typed_node("B", "Beta", "...", NodeType::Fact);
        let graph = Graph::from_nodes(vec![n1, n2]);

        let (_comp_map, num_components) = graph.connected_components();
        // Cross-type affinity (0.5 * 1.0 = 0.5) still creates edges after normalization
        // but capped at 10 per node; both get connected.
        assert_eq!(num_components, 1, "cross-type affinity connects them into one component");
    }

    // ── Backward-compatible tests ──────────────────────────────────────────

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
        assert!(graph.edges.len() >= 1);

        graph.remove_node("X");
        assert_eq!(graph.nodes.len(), 1);
        assert!(graph.edges.iter().all(|e| e.source != "X" && e.target != "X"),
            "edge involving X remains: {:?}", graph.edges);
    }
}
