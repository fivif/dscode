//! Wiki commands — knowledge graph search and visualization.
//!
//! The wiki is a two-layer SQLite-backed knowledge base (global + per-session)
//! that persists agent-learned facts, patterns, and decisions across sessions.

use dscode_core::wiki::graph::Graph;
use dscode_core::wiki::search::{search, SearchConfig, SearchResult};
use tracing::info;

use crate::app_state::AppState;

/// Search the wiki for knowledge nodes matching the query.
///
/// Searches both the global wiki and the current session's wiki, returning
/// results ranked by relevance score (BM25 + tag matching + recency boost).
#[tauri::command]
pub async fn wiki_search(
    state: tauri::State<'_, AppState>,
    query: String,
) -> Result<Vec<SearchResult>, String> {
    info!(%query, "wiki: search");

    state.ensure_wiki_engine().await?;

    let wiki_guard = state.wiki_engine.lock().await;
    let engine = wiki_guard
        .as_ref()
        .ok_or_else(|| "Wiki engine not initialized".to_string())?;

    // Use the default search config with a max of 20 results.
    let config = SearchConfig::default();
    search(engine, "global", &query, 20, &config)
}

/// Build and return the full knowledge graph (all nodes + semantic edges).
///
/// The returned graph contains nodes from both the global and current session
/// layers. Edges are computed via Jaccard semantic similarity. The graph can
/// be visualized by the frontend using sigma.js or similar.
#[tauri::command]
pub async fn wiki_graph(
    state: tauri::State<'_, AppState>,
) -> Result<Graph, String> {
    info!("wiki: building graph");

    state.ensure_wiki_engine().await?;

    let wiki_guard = state.wiki_engine.lock().await;
    let engine = wiki_guard
        .as_ref()
        .ok_or_else(|| "Wiki engine not initialized".to_string())?;

    // Gather nodes from the global layer.
    let global_nodes = engine.list_global()?;

    // Build the graph with semantic edges.
    let graph = Graph::from_nodes(global_nodes);

    Ok(graph)
}
