//! Chat commands — the main agent interaction loop.

use std::path::PathBuf;

use dscode_core::agent::forge::Forge;
use dscode_core::agent::stream::StreamEvent;
use dscode_core::providers::openai::OpenAiProvider;
use dscode_core::providers::trait_def::{FunctionCall, Message, MessageContent, Role, ToolCall};
use tauri::Manager;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use crate::app_state::{ActiveForge, AppState};
use crate::events;

/// Send a user message to the agent and stream the response back to the
/// frontend via `stream-event` Tauri events.
///
/// # Flow
/// 1. Acquires a per-session mutex to prevent concurrent sends to the same session.
/// 2. Reads the current config and creates an [`OpenAiProvider`].
/// 3. Loads conversation history from the session manager.
/// 4. Persists the user message to the session.
/// 5. Builds a [`Forge`] with registered tools and the current working dir.
/// 6. Spawns a background Tokio task that runs `forge.execute()` and relays
///    every [`StreamEvent`] to the frontend.
/// 7. Stores the task handle and [`CancellationToken`] so the frontend can abort it.
#[tauri::command]
pub async fn send_message(
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
    session_id: String,
    message: String,
) -> Result<(), String> {
    info!(%session_id, msg_len = message.len(), "chat: send_message");

    // DB5: Per-session mutex — prevent concurrent sends to the same session.
    let _session_guard = state.acquire_session_lock(&session_id).await;

    // 1. Read config and create provider.
    let (api_key, base_url, model, max_tokens, temperature, reasoning_effort, context_cfg) = {
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
            config.context.clone(),
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

    // DB4: Hold session_manager lock once for all session DB operations.
    state.ensure_session_manager().await?;

    let (history, working_dir) = {
        let sm_guard = state.session_manager.lock().await;
        let sm = sm_guard
            .as_ref()
            .ok_or_else(|| "Session manager not initialized".to_string())?;

        let session = sm
            .get_session(&session_id)?
            .ok_or_else(|| format!("Session '{}' not found", session_id))?;
        let history = session.messages;

        // 3. Persist the user message.
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

        let wd = if session.workspace.is_empty() {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
        } else {
            PathBuf::from(&session.workspace)
        };

        (history, wd)
    };

    // 4. Build the Forge using the session's workspace.
    let forge = Forge::new(
        Box::new(provider),
        state.tool_registry.clone(),
        working_dir,
    )
    .with_context_config(context_cfg);

    // 5. Spawn the agent loop in a background task with cancellation support.
    let app_handle_clone = app_handle.clone();
    let session_id_clone = session_id.clone();
    let persist_sid = session_id.clone();

    // DB2+DB3: Shared CancellationToken so abort cancels both event loop and forge.
    let cancel = CancellationToken::new();
    let event_loop_cancel = cancel.clone();
    let forge_cancel = cancel.clone();

    // Clone conversation history for background wiki ingestion after forge completes.
    let ingest_messages = history.clone();
    let ingest_sid = session_id.clone();

    let handle = tokio::spawn(async move {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<StreamEvent>();

        // DB3: Forge task uses select! so cancellation aborts in-progress LLM calls.
        let forge_task = tokio::spawn(async move {
            tokio::select! {
                biased;
                _ = forge_cancel.cancelled() => Ok(()),
                result = forge.execute(&message, &session_id_clone, history, tx) => result,
            }
        });

        let mut assistant_content = String::new();
        // DB6: Accumulate thinking content across events, persist as one message.
        let mut thinking_buffer = String::new();

        loop {
            tokio::select! {
                biased;
                // DB2: On cancellation, persist accumulated content before exiting.
                _ = event_loop_cancel.cancelled() => {
                    let app_state = app_handle_clone.state::<AppState>();
                    let sm_guard = app_state.session_manager.lock().await;
                    if let Some(ref sm) = *sm_guard {
                        let now = chrono::Utc::now().timestamp();
                        // DB6: Persist accumulated thinking as one message.
                        if !thinking_buffer.is_empty() {
                            sm.add_message(&persist_sid, &Message {
                                role: Role::Assistant,
                                content: MessageContent::Text(String::new()),
                                name: None, tool_calls: None, tool_call_id: None,
                                reasoning_content: Some(std::mem::take(&mut thinking_buffer)),
                                created_at: now,
                            }).ok();
                        }
                        if !assistant_content.is_empty() {
                            sm.add_message(&persist_sid, &Message {
                                role: Role::Assistant,
                                content: MessageContent::Text(std::mem::take(&mut assistant_content)),
                                name: None, tool_calls: None, tool_call_id: None,
                                reasoning_content: None, created_at: now,
                            }).ok();
                        }
                    }
                    info!(session = %persist_sid, "forge cancelled, partial content persisted");
                    break;
                }
                event = rx.recv() => {
                    match event {
                        Some(ref ev) => {
                            let app_state = app_handle_clone.state::<AppState>();
                            let sm_guard = app_state.session_manager.lock().await;
                            if let Some(ref sm) = *sm_guard {
                                let now = chrono::Utc::now().timestamp();
                                match ev {
                                    StreamEvent::Token { content: ref t_content } => {
                                        // DB6: Flush accumulated thinking before text.
                                        if !thinking_buffer.is_empty() {
                                            sm.add_message(&persist_sid, &Message {
                                                role: Role::Assistant,
                                                content: MessageContent::Text(String::new()),
                                                name: None, tool_calls: None, tool_call_id: None,
                                                reasoning_content: Some(std::mem::take(&mut thinking_buffer)),
                                                created_at: now,
                                            }).ok();
                                        }
                                        assistant_content.push_str(t_content);
                                    }
                                    StreamEvent::ToolStart { id, name, arguments, .. } => {
                                        // DB6: Flush accumulated thinking before tool.
                                        if !thinking_buffer.is_empty() {
                                            sm.add_message(&persist_sid, &Message {
                                                role: Role::Assistant,
                                                content: MessageContent::Text(String::new()),
                                                name: None, tool_calls: None, tool_call_id: None,
                                                reasoning_content: Some(std::mem::take(&mut thinking_buffer)),
                                                created_at: now,
                                            }).ok();
                                        }
                                        // Save accumulated text as a message before the tool.
                                        if !assistant_content.is_empty() {
                                            sm.add_message(&persist_sid, &Message {
                                                role: Role::Assistant,
                                                content: MessageContent::Text(assistant_content.clone()),
                                                name: None, tool_calls: None, tool_call_id: None,
                                                reasoning_content: None, created_at: now,
                                            }).ok();
                                            assistant_content.clear();
                                        }
                                        // DB1: Persist tool call WITH arguments from StreamEvent.
                                        let tool_msg = Message {
                                            role: Role::Assistant,
                                            content: MessageContent::Text(String::new()),
                                            name: None,
                                            tool_call_id: None,
                                            tool_calls: Some(vec![ToolCall {
                                                id: id.clone(),
                                                call_type: "function".into(),
                                                function: FunctionCall {
                                                    name: name.clone(),
                                                    arguments: arguments.clone(),
                                                },
                                            }]),
                                            reasoning_content: None,
                                            created_at: now,
                                        };
                                        sm.add_message(&persist_sid, &tool_msg).ok();
                                    }
                                    StreamEvent::ToolEnd { id, result, .. } => {
                                        // DB6: Flush accumulated thinking before tool result.
                                        if !thinking_buffer.is_empty() {
                                            sm.add_message(&persist_sid, &Message {
                                                role: Role::Assistant,
                                                content: MessageContent::Text(String::new()),
                                                name: None, tool_calls: None, tool_call_id: None,
                                                reasoning_content: Some(std::mem::take(&mut thinking_buffer)),
                                                created_at: now,
                                            }).ok();
                                        }
                                        let tool_end = Message {
                                            role: Role::Tool,
                                            content: MessageContent::Text(result.clone()),
                                            name: None, tool_calls: None,
                                            tool_call_id: Some(id.clone()),
                                            reasoning_content: None, created_at: now,
                                        };
                                        sm.add_message(&persist_sid, &tool_end).ok();
                                    }
                                    StreamEvent::Thinking { content: ref t_content, .. } => {
                                        // DB6: Accumulate thinking instead of persisting each event.
                                        thinking_buffer.push_str(t_content);
                                    }
                                    _ => {}
                                }
                            }
                            // Release sm_guard before emitting event.
                            drop(sm_guard);
                            events::emit_event(&app_handle_clone, ev, &persist_sid);
                        }
                        None => break, // Channel closed, forge finished.
                    }
                }
            }
        }

        // If the loop ended normally (channel closed), check forge result.
        match forge_task.await {
            Ok(Ok(())) => info!(session = %persist_sid, "forge completed"),
            Ok(Err(e)) => error!(session = %persist_sid, %e, "forge failed"),
            Err(e) => error!(session = %persist_sid, ?e, "forge panicked"),
        }

        // DB2: Persist final accumulated content after normal completion.
        let app_state = app_handle_clone.state::<AppState>();
        let sm_guard = app_state.session_manager.lock().await;
        if let Some(ref sm) = *sm_guard {
            let now = chrono::Utc::now().timestamp();
            // DB6: Persist any remaining accumulated thinking.
            if !thinking_buffer.is_empty() {
                sm.add_message(&persist_sid, &Message {
                    role: Role::Assistant,
                    content: MessageContent::Text(String::new()),
                    name: None, tool_calls: None, tool_call_id: None,
                    reasoning_content: Some(thinking_buffer),
                    created_at: now,
                }).ok();
            }
            if !assistant_content.is_empty() {
                sm.add_message(&persist_sid, &Message {
                    role: Role::Assistant,
                    content: MessageContent::Text(assistant_content),
                    name: None, tool_calls: None, tool_call_id: None,
                    reasoning_content: None, created_at: now,
                }).ok();
            }
        }
        drop(sm_guard);

        // After forge completes, spawn background wiki ingestion.
        tokio::spawn(async move {
            dscode_core::wiki::ingestor::auto_ingest(ingest_sid, ingest_messages);
        });
    });

    // Store the handle and cancellation token so `abort` can cancel it.
    {
        let mut guard = state.active_forge_handle.lock().await;
        if let Some(old) = guard.take() {
            old.cancel.cancel();
            old.handle.abort();
        }
        *guard = Some(ActiveForge { cancel, handle });
    }

    Ok(())
}

/// Abort the currently running forge task.
///
/// This cancels the [`CancellationToken`] shared with the forge and event-loop
/// tasks, causing both to stop. Accumulated assistant text and thinking content
/// are persisted before the tasks exit. The frontend should treat this as an
/// interrupted turn.
#[tauri::command]
pub async fn abort(state: tauri::State<'_, AppState>) -> Result<(), String> {
    info!("chat: abort requested");

    let mut guard = state.active_forge_handle.lock().await;
    if let Some(active) = guard.take() {
        // DB3: Cancel the token — this stops both the forge (LLM call) and
        // the event loop. The handle.abort() is a safety net.
        active.cancel.cancel();
        // Give tasks a moment to respond to cancellation.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        if !active.handle.is_finished() {
            active.handle.abort();
        }
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

/// Manually trigger wiki knowledge ingestion from a session's messages.
///
/// Reads the latest messages from the given session, extracts key facts,
/// decisions, file edits, and errors, and persists them as knowledge nodes
/// in the wiki engine. Runs in a background task — non-blocking.
#[tauri::command]
pub async fn wiki_ingest(
    state: tauri::State<'_, AppState>,
    session_id: String,
) -> Result<(), String> {
    info!(%session_id, "wiki_ingest: manual trigger");

    state.ensure_session_manager().await?;

    let messages = {
        let sm_guard = state.session_manager.lock().await;
        let sm = sm_guard
            .as_ref()
            .ok_or_else(|| "Session manager not initialized".to_string())?;

        let session = sm
            .get_session(&session_id)?
            .ok_or_else(|| format!("Session '{}' not found", session_id))?;
        session.messages
    };

    if messages.is_empty() {
        return Err("No messages in session to ingest".to_string());
    }

    info!(
        session = %session_id,
        msg_count = messages.len(),
        "wiki_ingest: spawning background task"
    );

    dscode_core::wiki::ingestor::auto_ingest(session_id, messages);

    Ok(())
}
