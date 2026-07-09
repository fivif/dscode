//! MCP server management commands for the desktop UI.

use dscode_core::config::settings::McpServerConfig;
use dscode_core::tools::mcp_ops::{
    load_mcp_server_configs, register_mcp_tools, save_mcp_servers_file, sync_mcp_to_config,
};
use serde::Serialize;
use tracing::info;

use crate::app_state::AppState;

#[derive(Debug, Clone, Serialize)]
pub struct McpServerInfo {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub connected: bool,
    pub tool_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct McpReloadResult {
    pub registered: usize,
    pub status: Vec<String>,
}

/// List configured MCP servers and how many tools they contributed.
#[tauri::command]
pub async fn list_mcp_servers(state: tauri::State<'_, AppState>) -> Result<Vec<McpServerInfo>, String> {
    let configs = load_mcp_server_configs();
    let tools = state.tool_registry.list_tools();
    let mut out = Vec::new();
    for c in configs {
        let prefix = format!(
            "mcp_{}_",
            c.name
                .chars()
                .map(|ch| {
                    if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                        ch
                    } else {
                        '_'
                    }
                })
                .collect::<String>()
                .to_lowercase()
        );
        let tool_count = tools.iter().filter(|t| t.starts_with(&prefix)).count();
        out.push(McpServerInfo {
            name: c.name,
            command: c.command,
            args: c.args,
            connected: tool_count > 0,
            tool_count,
        });
    }
    Ok(out)
}

/// Add an MCP server, persist, and reload tools.
#[tauri::command]
pub async fn add_mcp_server(
    state: tauri::State<'_, AppState>,
    name: String,
    command: String,
    args: String,
) -> Result<McpReloadResult, String> {
    let name = name.trim().to_string();
    let command = command.trim().to_string();
    if name.is_empty() || command.is_empty() {
        return Err("name and command required".into());
    }
    let arg_vec: Vec<String> = args
        .split_whitespace()
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let mut servers = load_mcp_server_configs();
    if servers.iter().any(|s| s.name.eq_ignore_ascii_case(&name)) {
        return Err(format!("MCP server '{name}' already exists"));
    }
    servers.push(McpServerConfig {
        name: name.clone(),
        command,
        args: arg_vec,
        env: Default::default(),
    });
    save_mcp_servers_file(&servers)?;
    sync_mcp_to_config(&servers)?;
    info!(%name, "MCP server added");
    reload_mcp_inner(&state).await
}

/// Update an existing MCP server (matched by `original_name`).
/// `name` may rename the server; command/args are replaced.
#[tauri::command]
pub async fn update_mcp_server(
    state: tauri::State<'_, AppState>,
    original_name: String,
    name: String,
    command: String,
    args: String,
) -> Result<McpReloadResult, String> {
    let original = original_name.trim().to_string();
    let name = name.trim().to_string();
    let command = command.trim().to_string();
    if original.is_empty() || name.is_empty() || command.is_empty() {
        return Err("name and command required".into());
    }
    let arg_vec: Vec<String> = args
        .split_whitespace()
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let mut servers = load_mcp_server_configs();
    let idx = servers
        .iter()
        .position(|s| s.name.eq_ignore_ascii_case(&original))
        .ok_or_else(|| format!("MCP server '{original}' not found"))?;

    // Rename conflict check
    if !name.eq_ignore_ascii_case(&original)
        && servers
            .iter()
            .enumerate()
            .any(|(i, s)| i != idx && s.name.eq_ignore_ascii_case(&name))
    {
        return Err(format!("MCP server '{name}' already exists"));
    }

    let prev_env = servers[idx].env.clone();
    servers[idx] = McpServerConfig {
        name: name.clone(),
        command,
        args: arg_vec,
        env: prev_env,
    };
    save_mcp_servers_file(&servers)?;
    sync_mcp_to_config(&servers)?;
    info!(from = %original, to = %name, "MCP server updated");
    reload_mcp_inner(&state).await
}

/// Remove an MCP server and reload.
#[tauri::command]
pub async fn remove_mcp_server(
    state: tauri::State<'_, AppState>,
    name: String,
) -> Result<McpReloadResult, String> {
    let mut servers = load_mcp_server_configs();
    let before = servers.len();
    servers.retain(|s| !s.name.eq_ignore_ascii_case(name.trim()));
    if servers.len() == before {
        return Err(format!("MCP server '{name}' not found"));
    }
    save_mcp_servers_file(&servers)?;
    sync_mcp_to_config(&servers)?;
    info!(%name, "MCP server removed");
    reload_mcp_inner(&state).await
}

/// Reconnect all MCP servers and re-register tools.
#[tauri::command]
pub async fn reload_mcp(state: tauri::State<'_, AppState>) -> Result<McpReloadResult, String> {
    reload_mcp_inner(&state).await
}

async fn reload_mcp_inner(state: &AppState) -> Result<McpReloadResult, String> {
    let (registered, status) = register_mcp_tools(&state.tool_registry).await;
    info!(registered, ?status, "MCP tools reloaded");
    Ok(McpReloadResult { registered, status })
}
