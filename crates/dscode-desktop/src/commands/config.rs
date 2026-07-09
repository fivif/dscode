//! Config commands — read and update the application configuration.
//!
//! Configuration is persisted to `~/.dscode/config.toml`.

use dscode_core::agent::forge::DEFAULT_SYSTEM_PROMPT;
use dscode_core::config::settings::{AgentConfig, Config};
use serde::Serialize;
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

/// Payload for the global system-prompt editor UI.
#[derive(Debug, Clone, Serialize)]
pub struct GlobalPromptInfo {
    /// User custom text (may be empty).
    pub global_prompt: String,
    /// Replace built-in prompt instead of appending.
    pub replace_system_prompt: bool,
    /// Built-in default system prompt (read-only reference).
    pub default_prompt: String,
    /// Effective prompt that will be sent to the model (preview).
    pub effective_prompt: String,
}

/// Read global prompt settings + default text for the modal editor.
#[tauri::command]
pub async fn get_global_prompt(
    state: tauri::State<'_, AppState>,
) -> Result<GlobalPromptInfo, String> {
    let config = state.config.lock().await;
    let agent = config.agent.clone();
    Ok(GlobalPromptInfo {
        global_prompt: agent.global_prompt.clone(),
        replace_system_prompt: agent.replace_system_prompt,
        default_prompt: DEFAULT_SYSTEM_PROMPT.to_string(),
        effective_prompt: agent.resolve_system_prompt(DEFAULT_SYSTEM_PROMPT),
    })
}

/// Update only the global system prompt fields (does not touch other config).
#[tauri::command]
pub async fn set_global_prompt(
    state: tauri::State<'_, AppState>,
    global_prompt: String,
    replace_system_prompt: bool,
) -> Result<GlobalPromptInfo, String> {
    info!(
        replace = replace_system_prompt,
        len = global_prompt.len(),
        "config: set_global_prompt"
    );
    let mut config = state.config.lock().await.clone();
    config.agent = AgentConfig {
        global_prompt: global_prompt.clone(),
        replace_system_prompt,
        memory_enabled: config.agent.memory_enabled,
        read_before_edit: config.agent.read_before_edit,
        memory_auto_ingest: config.agent.memory_auto_ingest,
    };
    config
        .save()
        .map_err(|e| format!("Failed to save config: {e}"))?;
    {
        let mut guard = state.config.lock().await;
        *guard = config.clone();
    }
    Ok(GlobalPromptInfo {
        global_prompt: config.agent.global_prompt.clone(),
        replace_system_prompt: config.agent.replace_system_prompt,
        default_prompt: DEFAULT_SYSTEM_PROMPT.to_string(),
        effective_prompt: config.agent.resolve_system_prompt(DEFAULT_SYSTEM_PROMPT),
    })
}

/// Update the application configuration.
///
/// The config is first persisted to disk. If the save succeeds, the in-memory
/// state is updated. If the save fails, the in-memory state is NOT modified
/// (the error is returned with the original config intact).
#[tauri::command]
pub async fn update_config(
    state: tauri::State<'_, AppState>,
    config: Config,
) -> Result<(), String> {
    info!("config: updating");

    // BUG10: Only reset session_manager if retention_days actually changed.
    let retention_changed = {
        let guard = state.config.lock().await;
        guard.session.retention_days != config.session.retention_days
    };

    // Persist to disk first. If this fails, in-memory state is untouched.
    config.save().map_err(|e| format!("Failed to save config: {}", e))?;

    // Update in-memory state.
    {
        let mut guard = state.config.lock().await;
        *guard = config;
    }

    // BUG10: Re-initialize the session manager only if retention_days changed.
    if retention_changed {
        let mut guard = state.session_manager.lock().await;
        *guard = None;
    }

    info!("config: updated successfully");
    Ok(())
}

/// Fetch available models from a provider's API (OpenAI-compatible `/v1/models`).
#[tauri::command]
pub async fn fetch_models(
    state: tauri::State<'_, AppState>,
    provider_key: String,
) -> Result<Vec<String>, String> {
    let (api_key, base_url, proxy) = {
        let config = state.config.lock().await;
        let pc = match provider_key.as_str() {
            "deepseek" => &config.providers.deepseek,
            "openai" => &config.providers.openai,
            "anthropic" => &config.providers.anthropic,
            "ollama" => &config.providers.ollama,
            _ => return Err(format!("Unknown provider: {}", provider_key)),
        };
        if pc.base_url.trim().is_empty() {
            return Err("接口地址为空，请先填写 Base URL".into());
        }
        let proxy = config
            .proxy_for_provider(&provider_key)
            .map(|s| s.to_string());
        (pc.api_key.clone(), pc.base_url.clone(), proxy)
    };

    let mut base = base_url.trim().trim_end_matches('/').to_string();
    // Normalize to .../v1 for OpenAI-compatible endpoints
    if !base.ends_with("/v1") && !base.contains("/v1/") {
        base = format!("{base}/v1");
    }
    let url = format!("{base}/models");

    let client = dscode_core::config::settings::build_http_client(proxy.as_deref())
        .map_err(|e| format!("HTTP 客户端错误: {e}"))?;

    let mut req = client.get(&url);
    if !api_key.is_empty() {
        req = req.header("Authorization", format!("Bearer {api_key}"));
        // Some Anthropic-compatible gateways also accept this
        if provider_key == "anthropic" {
            req = req.header("x-api-key", &api_key);
            req = req.header("anthropic-version", "2023-06-01");
        }
    }

    let resp = req
        .send()
        .await
        .map_err(|e| format!("请求失败 ({url}): {e}"))?;
    let status = resp.status();
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("解析 JSON 失败: {e}"))?;

    if !status.is_success() {
        let msg = body
            .pointer("/error/message")
            .and_then(|v| v.as_str())
            .or_else(|| body.get("message").and_then(|v| v.as_str()))
            .unwrap_or("未知错误");
        return Err(format!("API 错误 ({}): {msg}", status.as_u16()));
    }

    // Support shapes:
    // { "data": [ { "id": "..." } ] }  OpenAI
    // { "models": [ "..." ] } / { "models": [ { "name": "..." } ] }  Ollama-ish
    // [ { "id": "..." } ]
    let mut models: Vec<String> = Vec::new();

    if let Some(arr) = body.get("data").and_then(|v| v.as_array()) {
        for m in arr {
            if let Some(id) = m.get("id").and_then(|v| v.as_str()) {
                models.push(id.to_string());
            } else if let Some(id) = m.as_str() {
                models.push(id.to_string());
            }
        }
    } else if let Some(arr) = body.get("models").and_then(|v| v.as_array()) {
        for m in arr {
            if let Some(id) = m.get("id").or_else(|| m.get("name")).and_then(|v| v.as_str()) {
                models.push(id.to_string());
            } else if let Some(id) = m.as_str() {
                models.push(id.to_string());
            }
        }
    } else if let Some(arr) = body.as_array() {
        for m in arr {
            if let Some(id) = m.get("id").and_then(|v| v.as_str()) {
                models.push(id.to_string());
            } else if let Some(id) = m.as_str() {
                models.push(id.to_string());
            }
        }
    }

    models.sort();
    models.dedup();

    if models.is_empty() {
        return Err(format!(
            "接口返回了成功状态，但未解析到模型列表。URL={url} body={}",
            body.to_string().chars().take(200).collect::<String>()
        ));
    }

    Ok(models)
}
