//! Config commands — read and update the application configuration.
//!
//! Configuration is persisted to `~/.dscode/config.toml`.

use dscode_core::config::settings::Config;
use tracing::info;

use crate::app_state::AppState;

/// Get the current application configuration.
#[tauri::command]
pub async fn get_config(
    state: tauri::State<'_, AppState>,
) -> Result<Config, String> {
    let config = state.config.lock().await;
    Ok(config.clone())
}

/// Update the application configuration.
///
/// This replaces the in-memory config and persists it to disk immediately.
/// If the save fails, the in-memory state is NOT rolled back (the error is
/// returned, but the new config remains in memory until the next successful
/// save).
#[tauri::command]
pub async fn update_config(
    state: tauri::State<'_, AppState>,
    config: Config,
) -> Result<(), String> {
    info!("config: updating");

    // Persist to disk first.
    config.save().map_err(|e| format!("Failed to save config: {}", e))?;

    // Update in-memory state.
    {
        let mut guard = state.config.lock().await;
        *guard = config;
    }

    // Re-initialize the session manager with the new retention setting
    // (the old manager will be dropped and recreated on next use).
    {
        let mut guard = state.session_manager.lock().await;
        *guard = None;
    }

    info!("config: updated successfully");
    Ok(())
}

/// Fetch available models from a provider's API.
#[tauri::command]
pub async fn fetch_models(
    state: tauri::State<'_, AppState>,
    provider_key: String,
) -> Result<Vec<String>, String> {
    let config = state.config.lock().await;
    let pc = match provider_key.as_str() {
        "deepseek" => &config.providers.deepseek,
        "openai" => &config.providers.openai,
        "anthropic" => &config.providers.anthropic,
        _ => return Err(format!("Unknown provider: {}", provider_key)),
    };

    let mut base = pc.base_url.trim_end_matches('/').to_string();
    if !base.ends_with("/v1") && !base.contains("/v1/") {
        base = format!("{}/v1", base);
    }

    let client = reqwest::Client::new();
    let mut req = client.get(format!("{}/models", base));
    if !pc.api_key.is_empty() {
        req = req.header("Authorization", format!("Bearer {}", pc.api_key));
    }

    let resp = req.send().await.map_err(|e| format!("请求失败: {}", e))?;
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.map_err(|e| format!("解析失败: {}", e))?;

    if !status.is_success() {
        return Err(format!("API 错误 ({}): {}", status.as_u16(),
            body["error"]["message"].as_str().unwrap_or("未知错误")));
    }

    let models: Vec<String> = body["data"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|m| m["id"].as_str().map(|s| s.to_string()))
        .collect();

    Ok(models)
}
