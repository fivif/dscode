//! Chat commands — the main agent interaction loop.

use std::path::PathBuf;

use dscode_core::agent::forge::Forge;
use dscode_core::agent::stream::StreamEvent;
use dscode_core::providers::create_provider;
use dscode_core::providers::trait_def::{FunctionCall, Message, MessageContent, Role, ToolCall};
use tauri::{Emitter, Manager};
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use crate::app_state::{ActiveForge, AppState};
use crate::events;

/// Send a user message to the agent and stream the response back to the
/// frontend via `stream-event` Tauri events.
///
/// `attachments` — optional absolute file paths (from dialog / staged uploads).
/// Files are copied into the workspace `.dscode/uploads/` and described in the
/// prompt so the agent can open them with tools.
#[tauri::command]
pub async fn send_message(
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
    session_id: String,
    message: String,
    teams_mode: bool,
    attachments: Option<Vec<String>>,
) -> Result<(), String> {
    let att_n = attachments.as_ref().map(|a| a.len()).unwrap_or(0);
    info!(%session_id, msg_len = message.len(), teams_mode, att_n, "chat: send_message");

    // Toggle teams_mode in persistent state based on the message.
    if message.trim().eq_ignore_ascii_case("/teams") || message.trim().eq_ignore_ascii_case("/teams on") {
        state.teams_mode.store(true, std::sync::atomic::Ordering::Relaxed);
    } else if message.trim().eq_ignore_ascii_case("/teams off") || message.trim().eq_ignore_ascii_case("/teams stop") {
        state.teams_mode.store(false, std::sync::atomic::Ordering::Relaxed);
    }

    // DB5: Per-session mutex — prevent concurrent sends to the same session.
    let _session_guard = state.acquire_session_lock(&session_id).await;

    // 1. Read config and create the correct provider (OpenAI-compat / Anthropic).
    let (
        provider,
        context_cfg,
        system_prompt,
        safety_guard,
        perm_timeout,
        teams_cfg,
        memory_auto_ingest,
        memory_enabled,
        read_before_edit,
    ) = {
        let config = state.config.lock().await;
        let model = config.default_model.clone();
        let provider = create_provider(&model, &config)
            .map_err(|e| format!("Failed to create provider for '{model}': {e}"))?;
        let mut system_prompt = config
            .agent
            .resolve_system_prompt(dscode_core::agent::forge::DEFAULT_SYSTEM_PROMPT);
        // Optional memory recall into system prompt
        if config.agent.memory_enabled {
            if let Ok(scribe) = dscode_core::memory::scribe::Scribe::new() {
                let hits = scribe.recall(&message, 6);
                if !hits.is_empty() {
                    system_prompt.push_str("\n\n## Memory recall (optional context)\n");
                    for h in hits {
                        system_prompt.push_str("- ");
                        system_prompt.push_str(&h);
                        system_prompt.push('\n');
                    }
                }
            }
        }
        let safety_guard = std::sync::Arc::new(
            dscode_core::safety::guard::SafetyGuard::from_config(&config),
        );
        let perm_timeout = config.safety.permission_timeout_secs.max(10);
        (
            provider,
            config.context.clone(),
            system_prompt,
            safety_guard,
            perm_timeout,
            config.teams.clone(),
            config.agent.memory_auto_ingest,
            config.agent.memory_enabled,
            config.agent.read_before_edit,
        )
    };
    let _ = read_before_edit; // reserved: wire into ToolContext when main-agent RBE enabled

    // DB4: Hold session_manager lock once for all session DB operations.
    state.ensure_session_manager().await?;

    let (history, working_dir, full_message) = {
        let sm_guard = state.session_manager.lock().await;
        let sm = sm_guard
            .as_ref()
            .ok_or_else(|| "Session manager not initialized".to_string())?;

        let session = sm
            .get_session(&session_id)?
            .ok_or_else(|| format!("Session '{}' not found", session_id))?;
        let history = session.messages;

        let wd = if session.workspace.is_empty() {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
        } else {
            PathBuf::from(&session.workspace)
        };

        let paths = attachments.unwrap_or_default();
        let full_message = crate::attachments::build_message_with_attachments(
            &message,
            &paths,
            &wd,
            &session_id,
        )?;

        // 3. Persist the user message (with attachment context for history continuity).
        sm.add_message(
            &session_id,
            &Message {
                role: Role::User,
                content: MessageContent::Text(full_message.clone()),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
                created_at: 0,
            },
        )?;

        // Auto-name from user-visible text when possible
        let title_src = if message.trim().is_empty() && !paths.is_empty() {
            format!(
                "附件: {}",
                paths
                    .iter()
                    .filter_map(|p| PathBuf::from(p).file_name()?.to_str().map(|s| s.to_string()))
                    .take(3)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        } else {
            message.clone()
        };
        if let Ok(Some(new_title)) = sm.maybe_auto_title(&session_id, &title_src) {
            info!(%session_id, %new_title, "session: auto-titled");
            let _ = app_handle.emit(
                "session-title-updated",
                serde_json::json!({ "session_id": session_id, "title": new_title }),
            );
        }

        (history, wd, full_message)
    };

    // 4. Build the Forge using the session's workspace + global system prompt.
    let forge = Forge::new(
        provider,
        state.tool_registry.clone(),
        working_dir,
    )
    .with_system_prompt(system_prompt)
    .with_context_config(context_cfg)
    .with_teams_mode(teams_mode)
    .with_teams_config(teams_cfg)
    .with_safety_guard(safety_guard)
    .with_permission_hub(state.permission_hub.clone())
    .with_permission_timeout(perm_timeout);

    // 5. Spawn the agent loop in a background task with cancellation support.
    let app_handle_clone = app_handle.clone();
    let session_id_clone = session_id.clone();
    let persist_sid = session_id.clone();

    // DB2+DB3: Shared CancellationToken so abort cancels both event loop and forge.
    let cancel = CancellationToken::new();
    let event_loop_cancel = cancel.clone();
    let forge_cancel = cancel.clone();

    let user_for_memory = full_message.clone();
    let handle = tokio::spawn(async move {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<StreamEvent>();

        // DB3: Forge task uses select! so cancellation aborts in-progress LLM calls.
        let forge_task = tokio::spawn(async move {
            tokio::select! {
                biased;
                _ = forge_cancel.cancelled() => Ok(()),
                result = forge.execute(&full_message, &session_id_clone, history, tx) => result,
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
        let forge_ok = match forge_task.await {
            Ok(Ok(())) => {
                info!(session = %persist_sid, "forge completed");
                true
            }
            Ok(Err(e)) => {
                error!(session = %persist_sid, %e, "forge failed");
                false
            }
            Err(e) => {
                error!(session = %persist_sid, ?e, "forge panicked");
                false
            }
        };

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
                    content: MessageContent::Text(assistant_content.clone()),
                    name: None, tool_calls: None, tool_call_id: None,
                    reasoning_content: None, created_at: now,
                }).ok();
            }
        }
        drop(sm_guard);

        // Memory closed-loop: ingest user + assistant when auto_ingest on
        if forge_ok && memory_auto_ingest {
            if let Ok(scribe) = dscode_core::memory::scribe::Scribe::new() {
                let _ = scribe.ingest_turn(&persist_sid, "user", &user_for_memory);
                if !assistant_content.is_empty() {
                    let excerpt: String = assistant_content.chars().take(8000).collect();
                    let _ = scribe.ingest_turn(&persist_sid, "assistant", &excerpt);
                }
                info!(session = %persist_sid, "memory auto-ingest complete");
            }
        }
        let _ = memory_enabled; // recall already applied at turn start
    });

    // Per-session forge — does NOT cancel other sessions' runs.
    state.prune_finished_forges().await;
    state
        .set_active_forge(session_id.clone(), ActiveForge { cancel, handle })
        .await;

    Ok(())
}

/// Abort the forge task for a specific session.
///
/// Cancels only that session's agent turn; other concurrent sessions continue.
#[tauri::command]
pub async fn abort(
    state: tauri::State<'_, AppState>,
    session_id: String,
) -> Result<(), String> {
    info!(%session_id, "chat: abort requested");
    // Stop all teams sub-agents for this session (control plane)
    if let Some(cp) = dscode_core::teams::global_control_planes()
        .get(&session_id)
        .await
    {
        cp.stop_all().await;
    }
    if state.abort_forge(&session_id).await {
        // Brief yield so cancel handlers can persist partial content.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        info!(%session_id, "chat: forge task aborted");
    } else {
        info!(%session_id, "chat: no active forge for session");
    }
    Ok(())
}

/// List all registered tools (built-in + MCP).
#[tauri::command]
pub async fn list_tools(state: tauri::State<'_, AppState>) -> Result<Vec<ToolInfo>, String> {
    Ok(state
        .tool_registry
        .list_tools_detailed()
        .into_iter()
        .map(|(name, description)| ToolInfo { name, description })
        .collect())
}

#[derive(serde::Serialize, Clone)]
pub struct ToolInfo { pub name: String, pub description: String }

/// List all loaded skills (including bundled scripts/references/assets).
/// Scans DS Code + Claude + agents.sh + project skill directories.
/// Shows every package path (same name under different roots appears multiple times)
/// so each copy can be deleted independently.
#[tauri::command]
pub async fn list_skills() -> Result<Vec<SkillInfo>, String> {
    let mut loader = dscode_core::extensions::skills::SkillLoader::new();
    let count = loader
        .load_all_packages(&[], None)
        .map_err(|e| format!("加载 Skills 失败: {e}"))?;
    info!(%count, "skills: listed (multi-path, all packages)");
    Ok(loader
        .list_all()
        .iter()
        .map(|s| SkillInfo {
            name: s.name.clone(),
            description: s.description.clone(),
            triggers: s.triggers.clone(),
            hidden: s.hidden,
            body: s.body.clone(),
            root: s.root.display().to_string(),
            resources: s
                .resources
                .iter()
                .map(|r| SkillResourceInfo {
                    relative_path: r.relative_path.clone(),
                    absolute_path: r.absolute_path.clone(),
                    kind: match r.kind {
                        dscode_core::extensions::skills::SkillResourceKind::Script => {
                            "script".into()
                        }
                        dscode_core::extensions::skills::SkillResourceKind::Reference => {
                            "reference".into()
                        }
                        dscode_core::extensions::skills::SkillResourceKind::Asset => {
                            "asset".into()
                        }
                        dscode_core::extensions::skills::SkillResourceKind::Other => {
                            "other".into()
                        }
                    },
                    size_bytes: r.size_bytes,
                    executable: r.executable,
                })
                .collect(),
        })
        .collect())
}

#[derive(serde::Serialize, Clone)]
pub struct SkillResourceInfo {
    pub relative_path: String,
    pub absolute_path: String,
    pub kind: String,
    pub size_bytes: u64,
    pub executable: bool,
}

#[derive(serde::Serialize, Clone)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    pub triggers: Vec<String>,
    pub hidden: bool,
    pub body: String,
    pub root: String,
    pub resources: Vec<SkillResourceInfo>,
}

/// A file bundled into the skill package (scripts/references/assets).
#[derive(serde::Deserialize, Clone)]
pub struct SkillFileInput {
    /// Relative path under skill root, e.g. `scripts/review.sh`
    pub path: String,
    pub content: String,
}

/// Create or update a skill package.
/// `triggers` optional; `files` optional bundled scripts/docs.
#[tauri::command]
pub async fn save_skill(
    name: String,
    description: String,
    body: String,
    triggers: Option<String>,
    files: Option<Vec<SkillFileInput>>,
) -> Result<String, String> {
    let trigger_list: Vec<String> = triggers
        .unwrap_or_default()
        .split(|c| c == ',' || c == ';' || c == '\n' || c == '|' || c == '，' || c == '；')
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect();

    let file_pairs: Vec<(String, String)> = files
        .unwrap_or_default()
        .into_iter()
        .map(|f| (f.path, f.content))
        .collect();

    let path = dscode_core::extensions::skills::SkillLoader::save_skill(
        &name,
        &description,
        &body,
        &trigger_list,
        &file_pairs,
    )?;
    info!(path = %path.display(), files = file_pairs.len(), "skills: saved package");
    Ok(path
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| path.display().to_string()))
}

/// Write a single file into an existing skill package (scripts/references/assets).
#[tauri::command]
pub async fn write_skill_file(
    skill_name: String,
    relative_path: String,
    content: String,
) -> Result<String, String> {
    let path = dscode_core::extensions::skills::SkillLoader::write_skill_file(
        &skill_name,
        &relative_path,
        &content,
    )?;
    Ok(path.display().to_string())
}

/// Reveal the skills root directory path (for Finder / file manager).
#[tauri::command]
pub async fn skills_dir() -> Result<String, String> {
    Ok(dscode_core::extensions::skills::SkillLoader::default_skills_dir()
        .display()
        .to_string())
}

/// Install a third-party skill package from GitHub / skills.sh.
/// Spec: `owner/repo` or `owner/repo/skill-path`.
#[tauri::command]
pub async fn install_skill_package(package: String) -> Result<String, String> {
    info!(%package, "skills: install package");
    let report =
        dscode_core::extensions::skills::SkillLoader::install_from_spec(package.trim())?;
    Ok(report.message)
}

/// Approve a pending dangerous-command permission request (Safe mode).
#[tauri::command]
pub async fn approve_permission(
    state: tauri::State<'_, AppState>,
    request_id: String,
) -> Result<(), String> {
    info!(%request_id, "permission: approve");
    state.permission_hub.resolve(&request_id, true).await
}

/// Deny a pending dangerous-command permission request.
#[tauri::command]
pub async fn deny_permission(
    state: tauri::State<'_, AppState>,
    request_id: String,
) -> Result<(), String> {
    info!(%request_id, "permission: deny");
    state.permission_hub.resolve(&request_id, false).await
}

/// Stage raw file bytes (paste / drag from webview) into session uploads.
/// Returns absolute path for use in `send_message` attachments.
#[tauri::command]
pub async fn stage_upload(
    state: tauri::State<'_, AppState>,
    session_id: String,
    name: String,
    base64_data: String,
) -> Result<String, String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(base64_data.trim())
        .map_err(|e| format!("invalid base64: {e}"))?;
    if bytes.len() > crate::attachments::MAX_UPLOAD_BYTES {
        return Err(format!(
            "File too large ({} bytes)",
            bytes.len()
        ));
    }
    let workspace = {
        state.ensure_session_manager().await?;
        let sm_guard = state.session_manager.lock().await;
        let sm = sm_guard
            .as_ref()
            .ok_or_else(|| "Session manager not initialized".to_string())?;
        sm.get_session(&session_id)?
            .and_then(|s| {
                if s.workspace.is_empty() {
                    None
                } else {
                    Some(PathBuf::from(s.workspace))
                }
            })
    };
    crate::attachments::stage_bytes(
        &name,
        &bytes,
        workspace.as_deref(),
        &session_id,
    )
}

/// Delete a skill package.
///
/// Prefer `root` (absolute package path from list_skills) so multi-path skills
/// under ~/.claude/skills etc. can be removed — not only ~/.dscode/skills.
#[tauri::command]
pub async fn delete_skill(name: String, root: Option<String>) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() || name.contains("..") {
        return Err("非法 Skill 名称".into());
    }
    // Slash only illegal when used as bare name (root path is separate)
    if root.as_ref().map(|r| r.trim().is_empty()).unwrap_or(true)
        && (name.contains('/') || name.contains('\\'))
    {
        return Err("非法 Skill 名称".into());
    }
    info!(%name, root = ?root, "skills: delete");
    let msg = dscode_core::extensions::skills::SkillLoader::delete_skill_package(
        name,
        root.as_deref(),
        None,
    )?;
    info!(%msg, "skills: deleted");
    Ok(msg)
}

/// Subscribe to real-time background task notifications.
///
/// Spawns a background Tokio task that listens on the [`TaskManager`] broadcast
/// channel and emits `task-notification` Tauri events to the frontend for each
/// task start, progress, and completion. The frontend should call this once at
/// startup to enable push-based task monitoring.
#[tauri::command]
pub async fn subscribe_task_events(
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let mut rx = state.task_manager.subscribe();

    tokio::spawn(async move {
        while let Ok(notification) = rx.recv().await {
            let _ = app_handle.emit("task-notification", &notification);
        }
    });

    Ok(())
}

/// Stop a running teams sub-agent by id (Teams v2 control plane).
#[tauri::command]
pub async fn stop_team_agent(session_id: String, agent_id: String) -> Result<bool, String> {
    if let Some(cp) = dscode_core::teams::global_control_planes()
        .get(&session_id)
        .await
    {
        Ok(cp.stop_agent(&agent_id).await)
    } else {
        Ok(false)
    }
}

/// Nudge a running teams sub-agent with extra instruction text.
#[tauri::command]
pub async fn nudge_team_agent(
    session_id: String,
    agent_id: String,
    message: String,
) -> Result<bool, String> {
    if let Some(cp) = dscode_core::teams::global_control_planes()
        .get(&session_id)
        .await
    {
        Ok(cp.nudge_agent(&agent_id, message).await)
    } else {
        Ok(false)
    }
}
