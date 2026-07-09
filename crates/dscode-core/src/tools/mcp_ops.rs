//! MCP tools exposed to the agent as normal registry tools (`mcp_<server>_<tool>`).

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Mutex;
use tracing::{info, warn};

use super::trait_def::{Tool, ToolContext, ToolError, ToolResult};
use crate::config::settings::{Config, McpServerConfig};
use crate::extensions::mcp::{McpClient, McpConnection, McpToolDef};
use crate::tools::registry::ToolRegistry;

/// Sanitize server/tool name segments for registry keys.
fn sanitize_segment(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Build the registered tool name: `mcp_<server>_<tool>`.
pub fn mcp_tool_name(server: &str, tool: &str) -> String {
    format!(
        "mcp_{}_{}",
        sanitize_segment(server).to_lowercase(),
        sanitize_segment(tool)
    )
}

/// Proxy that forwards `execute` to a live MCP server connection.
pub struct McpProxyTool {
    registered_name: String,
    remote_name: String,
    description: String,
    parameters: serde_json::Value,
    server_name: String,
    conn: Arc<Mutex<McpConnection>>,
}

impl McpProxyTool {
    pub fn new(
        server_name: &str,
        def: &McpToolDef,
        conn: Arc<Mutex<McpConnection>>,
    ) -> Self {
        let schema = if def.input_schema.is_null() {
            serde_json::json!({
                "type": "object",
                "properties": {},
            })
        } else {
            def.input_schema.clone()
        };
        let desc = if def.description.is_empty() {
            format!("MCP tool `{}` from server `{}`", def.name, server_name)
        } else {
            format!("[MCP:{}] {}", server_name, def.description)
        };
        Self {
            registered_name: mcp_tool_name(server_name, &def.name),
            remote_name: def.name.clone(),
            description: desc,
            parameters: schema,
            server_name: server_name.to_string(),
            conn,
        }
    }
}

#[async_trait]
impl Tool for McpProxyTool {
    fn name(&self) -> &str {
        &self.registered_name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> serde_json::Value {
        self.parameters.clone()
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let mut guard = self.conn.lock().await;
        if !guard.is_alive() {
            return Err(ToolError::Internal(format!(
                "MCP server '{}' is not running",
                self.server_name
            )));
        }
        match guard.call_tool(&self.remote_name, args).await {
            Ok(result) => {
                // MCP tools/call returns { content: [...], isError?: bool }
                let text = format_mcp_result(&result);
                let is_err = result
                    .get("isError")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if is_err {
                    Ok(ToolResult::err(text.clone(), text))
                } else {
                    Ok(ToolResult::ok(text))
                }
            }
            Err(e) => Err(ToolError::Internal(format!(
                "MCP {}.{} failed: {e}",
                self.server_name, self.remote_name
            ))),
        }
    }
}

fn format_mcp_result(result: &serde_json::Value) -> String {
    if let Some(arr) = result.get("content").and_then(|c| c.as_array()) {
        let mut parts = Vec::new();
        for item in arr {
            let typ = item.get("type").and_then(|t| t.as_str()).unwrap_or("text");
            if typ == "text" {
                if let Some(t) = item.get("text").and_then(|t| t.as_str()) {
                    parts.push(t.to_string());
                }
            } else {
                parts.push(item.to_string());
            }
        }
        if !parts.is_empty() {
            return parts.join("\n");
        }
    }
    result.to_string()
}

/// Load MCP server configs from config.toml + ~/.dscode/mcp_servers.json.
pub fn load_mcp_server_configs() -> Vec<McpServerConfig> {
    let mut by_name: std::collections::BTreeMap<String, McpServerConfig> =
        std::collections::BTreeMap::new();

    // 1) config.toml extensions.mcp_servers
    if let Ok(cfg) = Config::load() {
        for s in cfg.extensions.mcp_servers {
            by_name.insert(s.name.clone(), s);
        }
    }

    // 2) Legacy / UI file mcp_servers.json (fills gaps or overrides empty toml)
    if let Ok(path) = Config::data_dir().map(|d| d.join("mcp_servers.json")) {
        if path.exists() {
            if let Ok(raw) = std::fs::read_to_string(&path) {
                if let Ok(file) = serde_json::from_str::<McpServersFile>(&raw) {
                    for s in file.servers {
                        // Prefer explicit config.toml entry if both exist
                        by_name.entry(s.name.clone()).or_insert(s);
                    }
                }
            }
        }
    }

    by_name.into_values().collect()
}

#[derive(serde::Deserialize, serde::Serialize)]
struct McpServersFile {
    servers: Vec<McpServerConfig>,
}

/// Persist server list to mcp_servers.json (UI source of truth for list UI).
pub fn save_mcp_servers_file(servers: &[McpServerConfig]) -> Result<(), String> {
    let path = Config::data_dir()
        .map_err(|e| e.to_string())?
        .join("mcp_servers.json");
    let file = McpServersFile {
        servers: servers.to_vec(),
    };
    let raw = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
    std::fs::write(path, raw).map_err(|e| e.to_string())
}

/// Also mirror into config.toml extensions.mcp_servers.
pub fn sync_mcp_to_config(servers: &[McpServerConfig]) -> Result<(), String> {
    let mut cfg = Config::load().map_err(|e| e.to_string())?;
    cfg.extensions.mcp_servers = servers.to_vec();
    cfg.save().map_err(|e| e.to_string())
}

/// Connect all configured MCP servers and register their tools.
/// Returns (registered_tool_count, human status lines).
pub async fn register_mcp_tools(registry: &ToolRegistry) -> (usize, Vec<String>) {
    // Drop previous MCP tools before re-register
    registry.unregister_where(|name| name.starts_with("mcp_"));

    let servers = load_mcp_server_configs();
    if servers.is_empty() {
        return (
            0,
            vec!["No MCP servers configured (config.toml or ~/.dscode/mcp_servers.json)".into()],
        );
    }

    let mut total = 0usize;
    let mut status = Vec::new();

    for srv in servers {
        let client = McpClient::new(&srv.name, &srv.command)
            .with_args(srv.args.clone())
            .with_env_map(srv.env.clone());

        let cfg = Config::load().ok();
        let proxy = cfg
            .as_ref()
            .and_then(|c| c.proxy_for_mcp().map(|s| s.to_string()));
        let proxy_cfg_note = match (&cfg, &proxy) {
            (Some(c), Some(_)) => format!("proxy={}", c.proxy.url.trim()),
            (Some(c), None) if c.proxy.is_configured() => {
                "proxy configured but MCP toggle off (and not global)".into()
            }
            (Some(_), None) => "no proxy".into(),
            (None, _) => "config load failed".into(),
        };

        match client.connect_with_proxy(proxy.as_deref()).await {
            Ok(conn) => {
                let conn = Arc::new(Mutex::new(conn));
                let tools = {
                    let mut g = conn.lock().await;
                    match g.list_tools().await {
                        Ok(t) => t,
                        Err(e) => {
                            warn!(server = %srv.name, %e, "MCP tools/list failed");
                            status.push(format!(
                                "[err] {} tools/list failed: {e} ({proxy_cfg_note})",
                                srv.name
                            ));
                            continue;
                        }
                    }
                };
                let n = tools.len();
                for def in &tools {
                    let proxy_tool = McpProxyTool::new(&srv.name, def, Arc::clone(&conn));
                    let name = proxy_tool.name().to_string();
                    registry.register_or_replace(proxy_tool);
                    info!(server = %srv.name, tool = %name, "MCP tool registered");
                }
                total += n;
                let via = if proxy.is_some() { " via proxy" } else { " direct" };
                status.push(format!("[ok] {} — {n} tools{via}", srv.name));
            }
            Err(e) => {
                warn!(server = %srv.name, %e, "MCP connect failed");
                // Truncate very long errors for UI
                let msg = e.to_string();
                let short: String = msg.chars().take(500).collect();
                status.push(format!(
                    "[err] {} connect failed ({proxy_cfg_note}): {short}",
                    srv.name
                ));
            }
        }
    }

    (total, status)
}
