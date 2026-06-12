//! Chat commands — the main agent interaction loop.

use std::path::PathBuf;

use dscode_core::agent::forge::Forge;
use dscode_core::agent::stream::StreamEvent;
use dscode_core::providers::openai::OpenAiProvider;
use dscode_core::providers::trait_def::{Message, MessageContent, Role};
use tauri::Manager;
use tracing::{error, info};

use crate::app_state::AppState;
use crate::events;

/// Send a user message to the agent and stream the response back to the
/// frontend via `stream-event` Tauri events.
///
/// # Flow
/// 1. Reads the current config and creates an [`OpenAiProvider`].
/// 2. Loads conversation history from the session manager.
/// 3. Persists the user message to the session.
/// 4. Builds a [`Forge`] with registered tools and the current working dir.
/// 5. Spawns a background Tokio task that runs `forge.execute()` and relays
///    every [`StreamEvent`] to the frontend.
/// 6. Stores the task handle so the frontend can abort it.
#[tauri::command]
pub async fn send_message(
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
    session_id: String,
    message: String,
) -> Result<(), String> {
    info!(%session_id, msg_len = message.len(), "chat: send_message");

    // 1. Read config and create provider.
    let (api_key, base_url, model, max_tokens, temperature, reasoning_effort) = {
        let config = state.config.lock().await;
        let pc = config
            .provider_for_model(&config.default_model)
            .ok_or_else(|| format!("No provider config for model '{}'", config.default_model))?;
        (
            pc.api_key.clone(),
            pc.base_url.clone(),
            config.default_model.clone(),
            config.generation.max_tokens,
            config.generation.temperature,
            config.generation.reasoning_effort.clone(),
        )
    };

    // Strip provider prefix from model name (e.g. "openai/gpt-4o" -> "gpt-4o").
    let actual_model = match model.split_once('/') {
        Some((_, m)) => m.to_string(),
        None => model.clone(),
    };

    // Ensure /v1 path for OpenAI-compatible endpoints
    let mut base_url = base_url.trim_end_matches('/').to_string();
    if !base_url.ends_with("/v1") && !base_url.contains("/v1/") {
        base_url = format!("{}/v1", base_url);
    }

    let mut provider = OpenAiProvider::new(api_key, base_url, actual_model);
    provider.max_tokens = max_tokens;
    provider.temperature = temperature;
    provider.reasoning_effort = Some(reasoning_effort);

    // 2. Ensure session manager exists and load history.
    state.ensure_session_manager().await?;

    let history: Vec<Message> = {
        let sm_guard = state.session_manager.lock().await;
        let sm = sm_guard
            .as_ref()
            .ok_or_else(|| "Session manager not initialized".to_string())?;

        let session = sm
            .get_session(&session_id)?
            .ok_or_else(|| format!("Session '{}' not found", session_id))?;
        session.messages
    };

    // 3. Persist the user message.
    {
        let sm_guard = state.session_manager.lock().await;
        let sm = sm_guard
            .as_ref()
            .ok_or_else(|| "Session manager not initialized".to_string())?;

        sm.add_message(
            &session_id,
            &Message {
                role: Role::User,
                content: MessageContent::Text(message.clone()),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
                created_at: 0,
            },
        )?;
    }

    // 4. Build the Forge using the session's workspace.
    let working_dir = {
        let sm_guard = state.session_manager.lock().await;
        let sm = sm_guard.as_ref().ok_or_else(|| "Session manager not initialized".to_string())?;
        let session = sm.get_session(&session_id)?.ok_or_else(|| format!("Session '{}' not found", session_id))?;
        if session.workspace.is_empty() {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
        } else {
            PathBuf::from(&session.workspace)
        }
    };
    let context_cfg = {
        let config = state.config.lock().await;
        config.context.clone()
    };
    let forge = Forge::new(
        Box::new(provider),
        state.tool_registry.clone(),
        working_dir,
    )
    .with_context_config(context_cfg);

    // 5. Spawn the agent loop in a background task.
    let app_handle_clone = app_handle.clone();
    let session_id_clone = session_id.clone();
    let session_id_for_forge = session_id.clone();
    let message_for_forge = message.clone();
    let persist_sid = session_id.clone();
    let handle = tokio::spawn(async move {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<StreamEvent>();
        let forge_task = tokio::spawn(async move {
            forge.execute(&message_for_forge, &session_id_for_forge, history, tx).await
        });

        let mut assistant_content = String::new();
        while let Some(event) = rx.recv().await {
            // Persist tool calls and thinking as separate messages for history
            let app_state = app_handle_clone.state::<AppState>();
            let sm_guard = app_state.session_manager.lock().await;
            if let Some(ref sm) = *sm_guard {
                let now = chrono::Utc::now().timestamp();
                match &event {
                    StreamEvent::Token { content } => assistant_content.push_str(content),
                    StreamEvent::ToolStart { id, name, .. } => {
                        // Save accumulated text as a message before the tool
                        if !assistant_content.is_empty() {
                            sm.add_message(&persist_sid, &Message {
                                role: Role::Assistant, content: MessageContent::Text(assistant_content.clone()),
                                name: None, tool_calls: None, tool_call_id: None,
                                reasoning_content: None, created_at: now,
                            }).ok();
                            assistant_content.clear();
                        }
                        let tool_msg = Message {
                            role: Role::Assistant, content: MessageContent::Text(String::new()),
                            name: None, tool_call_id: Some(id.clone()),
                            tool_calls: Some(vec![dscode_core::providers::trait_def::ToolCall {
                                id: id.clone(), call_type: "function".into(),
                                function: dscode_core::providers::trait_def::FunctionCall { name: name.clone(), arguments: String::new() },
                            }]),
                            reasoning_content: None,
                            created_at: now,
                        };
                        sm.add_message(&persist_sid, &tool_msg).ok();
                    }
                    StreamEvent::ToolEnd { id, result, .. } => {
                        let tool_end = Message {
                            role: Role::Tool, content: MessageContent::Text(result.clone()),
                            name: None, tool_calls: None, tool_call_id: Some(id.clone()),
                            reasoning_content: None, created_at: now,
                        };
                        sm.add_message(&persist_sid, &tool_end).ok();
                    }
                    StreamEvent::Thinking { content, step: _ } => {
                        let think_msg = Message {
                            role: Role::Assistant, content: MessageContent::Text(String::new()),
                            name: None, tool_calls: None, tool_call_id: None,
                            reasoning_content: Some(content.clone()),
                            created_at: now,
                        };
                        sm.add_message(&persist_sid, &think_msg).ok();
                    }
                    _ => {}
                }
            }
            events::emit_event(&app_handle_clone, &event);
        }

        match forge_task.await {
            Ok(Ok(())) => info!(session = %session_id_clone, "forge completed"),
            Ok(Err(e)) => error!(session = %session_id_clone, %e, "forge failed"),
            Err(e) => error!(session = %session_id_clone, ?e, "forge panicked"),
        }

        // Persist final text response
        let app_state = app_handle_clone.state::<AppState>();
        let sm_guard = app_state.session_manager.lock().await;
        if let Some(ref sm) = *sm_guard {
            if !assistant_content.is_empty() {
                sm.add_message(&persist_sid, &Message {
                    role: Role::Assistant,
                    content: MessageContent::Text(assistant_content),
                    name: None, tool_calls: None, tool_call_id: None,
                    reasoning_content: None, created_at: chrono::Utc::now().timestamp(),
                }).ok();
            }
        }
    });

    // Store the handle so `abort` can cancel it.
    {
        let mut guard = state.active_forge_handle.lock().await;
        if let Some(old_handle) = guard.take() { old_handle.abort(); }
        *guard = Some(handle);
    }

    Ok(())
}

/// Abort the currently running forge task.
///
/// This sends an abort signal to the background Tokio task spawned by
/// [`send_message`]. Any in-progress LLM call or tool execution will be
/// cancelled. The frontend should treat this as an interrupted turn.
#[tauri::command]
pub async fn abort(state: tauri::State<'_, AppState>) -> Result<(), String> {
    info!("chat: abort requested");

    let mut guard = state.active_forge_handle.lock().await;
    if let Some(handle) = guard.take() {
        handle.abort();
        info!("chat: forge task aborted");
    } else {
        info!("chat: no active forge task to abort");
    }
    Ok(())
}

/// List all registered tools (built-in + MCP).
#[tauri::command]
pub async fn list_tools(state: tauri::State<'_, AppState>) -> Result<Vec<ToolInfo>, String> {
    Ok(state.tool_registry.list_tools().into_iter().map(|name| {
        let desc = state.tool_registry.get(&name)
            .map(|t| t.description().to_string())
            .unwrap_or_default();
        ToolInfo { name, description: desc }
    }).collect())
}

#[derive(serde::Serialize, Clone)]
pub struct ToolInfo { pub name: String, pub description: String }

/// List all loaded skills.
#[tauri::command]
pub async fn list_skills() -> Result<Vec<SkillInfo>, String> {
    let mut loader = dscode_core::extensions::skills::SkillLoader::new();
    loader.load_from_dir(&dscode_core::extensions::skills::SkillLoader::default_skills_dir()).ok();
    Ok(loader.list_all().iter().map(|s| SkillInfo {
        name: s.name.clone(),
        description: s.description.clone(),
        triggers: s.triggers.clone(),
        hidden: s.hidden,
        body: s.body.clone(),
    }).collect())
}

#[derive(serde::Serialize, Clone)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    pub triggers: Vec<String>,
    pub hidden: bool,
    pub body: String,
}

/// Create or update a skill file.
#[tauri::command]
pub async fn save_skill(name: String, description: String, body: String) -> Result<(), String> {
    let dir = dscode_core::extensions::skills::SkillLoader::default_skills_dir().join(&name);
    std::fs::create_dir_all(&dir).map_err(|e| format!("Cannot create dir: {}", e))?;
    let content = format!("---\nname: {}\ndescription: {}\n---\n\n{}", name, description, body);
    std::fs::write(dir.join("SKILL.md"), content).map_err(|e| format!("Cannot write: {}", e))?;
    Ok(())
}

/// Delete a skill directory.
#[tauri::command]
pub async fn delete_skill(name: String) -> Result<(), String> {
    let dir = dscode_core::extensions::skills::SkillLoader::default_skills_dir().join(&name);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|e| format!("Cannot delete: {}", e))?;
    }
    Ok(())
}
