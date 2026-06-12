//! JSON export for sigma.js and Quartz visualization.
//!
//! Serializes a knowledge graph into a format compatible with:
//! - **sigma.js** v2+ — a graph visualization library expecting `{ nodes: [...], edges: [...] }`.
//! - **Quartz** (Obsidian-compatible) — a Markdown-knowledge-graph tool that consumes
//!   node/edge JSON for rendering link graphs.

use serde::Serialize;

use super::graph::Graph;
#[allow(unused_imports)]
use super::engine::KnowledgeNode;

// ── sigma.js export ─────────────────────────────────────────────────────────

/// Sigma.js v2+ compatible graph representation.
#[derive(Debug, Clone, Serialize)]
pub struct SigmaGraph {
    pub nodes: Vec<SigmaNode>,
    pub edges: Vec<SigmaEdge>,
}

/// A node in the sigma.js format.
#[derive(Debug, Clone, Serialize)]
pub struct SigmaNode {
    /// Unique node identifier.
    pub key: String,
    /// A map of attributes (sigma.js uses `attributes` to drive rendering).
    pub attributes: SigmaNodeAttributes,
}

#[derive(Debug, Clone, Serialize)]
pub struct SigmaNodeAttributes {
    pub label: String,
    #[serde(rename = "type")]
    pub node_type: String,
    #[serde(rename = "size")]
    pub size: f64,
    #[serde(rename = "color")]
    pub color: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<i64>,
}

/// An edge in the sigma.js format.
#[derive(Debug, Clone, Serialize)]
pub struct SigmaEdge {
    pub key: String,
    pub source: String,
    pub target: String,
    pub attributes: SigmaEdgeAttributes,
}

#[derive(Debug, Clone, Serialize)]
pub struct SigmaEdgeAttributes {
    pub weight: f64,
    #[serde(rename = "type")]
    pub edge_type: String,
    #[serde(rename = "size")]
    pub size: f64,
}

// ── Quartz export ───────────────────────────────────────────────────────────

/// Quartz-compatible (Obsidian-alike) content node.
#[derive(Debug, Clone, Serialize)]
pub struct QuartzNode {
    /// The page slug (filename).
    pub slug: String,
    /// Page title.
    pub title: String,
    /// Markdown content.
    pub content: String,
    /// Frontmatter tags.
    pub tags: Vec<String>,
    /// Outgoing links (node IDs).
    pub links: Vec<String>,
}

/// Export a graph to sigma.js v2+ JSON format.
pub fn to_sigma_json(graph: &Graph) -> Result<String, String> {
    let sigma = graph_to_sigma(graph);
    serde_json::to_string_pretty(&sigma)
        .map_err(|e| format!("Sigma export serialization error: {}", e))
}

/// Build a SigmaGraph from our internal Graph.
fn graph_to_sigma(graph: &Graph) -> SigmaGraph {
    // Collect node sizes (degree).
    let mut degree: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for edge in &graph.edges {
        *degree.entry(edge.source.clone()).or_default() += 1;
        *degree.entry(edge.target.clone()).or_default() += 1;
    }

    let nodes: Vec<SigmaNode> = graph
        .nodes
        .iter()
        .map(|n| {
            let deg = degree.get(&n.id).copied().unwrap_or(0) as f64;
            let size = 5.0 + deg.ln_1p() * 3.0; // logarithmic scaling
            let color = node_type_color(&n.node_type);
            SigmaNode {
                key: n.id.clone(),
                attributes: SigmaNodeAttributes {
                    label: n.title.clone(),
                    node_type: n.node_type.as_str().to_string(),
                    size,
                    color: color.to_string(),
                    tags: if n.tags.is_empty() {
                        None
                    } else {
                        Some(n.tags.clone())
                    },
                    created_at: Some(n.created_at),
                },
            }
        })
        .collect();

    let edges: Vec<SigmaEdge> = graph
        .edges
        .iter()
        .map(|e| {
            let size = (e.weight * 5.0).max(0.5);
            SigmaEdge {
                key: format!("{}->{}", e.source, e.target),
                source: e.source.clone(),
                target: e.target.clone(),
                attributes: SigmaEdgeAttributes {
                    weight: e.weight,
                    edge_type: e.weight_type.as_str().to_string(),
                    size,
                },
            }
        })
        .collect();

    SigmaGraph { nodes, edges }
}

/// Export a graph to Quartz-compatible JSON (array of content nodes).
pub fn to_quartz_json(graph: &Graph) -> Result<String, String> {
    let quartz_nodes = graph_to_quartz(graph);
    serde_json::to_string_pretty(&quartz_nodes)
        .map_err(|e| format!("Quartz export serialization error: {}", e))
}

/// Build QuartzNodes from our internal Graph.
fn graph_to_quartz(graph: &Graph) -> Vec<QuartzNode> {
    // Build outgoing links map from edges.
    let mut links: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for edge in &graph.edges {
        links
            .entry(edge.source.clone())
            .or_default()
            .push(edge.target.clone());
    }

    graph
        .nodes
        .iter()
        .map(|n| {
            let outgoing = links.get(&n.id).cloned().unwrap_or_default();
            let content = format!(
                "---\ntitle: {}\ntype: {}\ntags: [{}]\n---\n\n{}",
                n.title,
                n.node_type.as_str(),
                n.tags.join(", "),
                n.content,
            );
            QuartzNode {
                slug: slugify(&n.title),
                title: n.title.clone(),
                content,
                tags: n.tags.clone(),
                links: outgoing,
            }
        })
        .collect()
}

/// Export all nodes from the engine as a sigma.js graph (no edge computation —
/// edges are optional and can be computed separately).
pub fn export_nodes_to_sigma(engine: &super::engine::Engine) -> Result<String, String> {
    let nodes = engine.list_global()?;
    let graph = Graph::from_nodes(nodes);
    to_sigma_json(&graph)
}

/// Export all nodes from the engine as Quartz pages.
pub fn export_quartz_pages(engine: &super::engine::Engine) -> Result<String, String> {
    let nodes = engine.list_global()?;
    let graph = Graph::from_nodes(nodes);
    to_quartz_json(&graph)
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Map node type to a hex colour for sigma.js rendering.
fn node_type_color(node_type: &super::engine::NodeType) -> &'static str {
    match node_type {
        super::engine::NodeType::Concept => "#4A90D9",  // blue
        super::engine::NodeType::Fact => "#50C878",      // green
        super::engine::NodeType::Pattern => "#F5A623",   // amber
        super::engine::NodeType::Decision => "#D0021B",  // red
        super::engine::NodeType::Rule => "#9013FE",       // purple
    }
}

/// Turn a title into a URL-safe slug.
fn slugify(title: &str) -> String {
    title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<&str>>()
        .join("-")
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_node(id: &str, title: &str, content: &str) -> KnowledgeNode {
        KnowledgeNode {
            id: id.to_string(),
            title: title.to_string(),
            content: content.to_string(),
            node_type: super::super::engine::NodeType::Concept,
            tags: vec!["rust".into()],
            created_at: 0,
            session_id: String::new(),
        }
    }

    #[test]
    fn test_sigma_export() {
        let nodes = vec![
            make_node("1", "Node One", "content one"),
            make_node("2", "Node Two", "content two"),
        ];
        let mut graph = Graph::from_nodes(nodes);
        graph.bump_co_occurrence("1", "2", 0.5);

        let json = to_sigma_json(&graph).unwrap();
        assert!(json.contains("\"nodes\""));
        assert!(json.contains("\"edges\""));
        assert!(json.contains("\"Node One\""));
        // Edge weight should be present in serialized form
        assert!(json.contains("\"weight\""));
    }

    #[test]
    fn test_quartz_export() {
        let nodes = vec![make_node("1", "Rust Async", "Learning async Rust.")];
        let graph = Graph::from_nodes(nodes);

        let json = to_quartz_json(&graph).unwrap();
        assert!(json.contains("\"slug\""));
        assert!(json.contains("rust-async"));
        assert!(json.contains("Learning async Rust."));
    }

    #[test]
    fn test_slugify() {
        assert_eq!(slugify("Hello World!"), "hello-world");
        assert_eq!(slugify("Rust Async/Await"), "rust-async-await");
        assert_eq!(slugify("  Multiple   Spaces  "), "multiple-spaces");
    }

    #[test]
    fn test_node_type_color() {
        assert_eq!(node_type_color(&super::super::engine::NodeType::Rule), "#9013FE");
        assert_eq!(node_type_color(&super::super::engine::NodeType::Concept), "#4A90D9");
    }
}
