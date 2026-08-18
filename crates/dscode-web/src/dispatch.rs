//! JSON-RPC style command dispatcher mirroring Tauri's `invoke(command, args)`.
//!
//! The web frontend reuses the exact same call names/arguments as the desktop
//! Tauri frontend, so `tauri.ts` only swaps `invoke()` for `fetch('/api/invoke')`.
//!
//! IMPORTANT: the JS side sends camelCase argument names (Tauri convention).
//! Tauri auto-converts camelCase → snake_case; we replicate that here with
//! `#[serde(rename_all = "camelCase")]` on every argument struct.

use std::sync::Arc;

use axum::extract::State;
use axum::Json;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};

use dscode_server::app_state::AppState;
use dscode_server::commands::{chat, config, mcp, session};

#[derive(Deserialize)]
pub struct InvokeRequest {
    pub command: String,
    #[serde(default)]
    pub args: Value,
}

pub async fn invoke_handler(
    State(state): State<Arc<AppState>>,
    Json(req): Json<InvokeRequest>,
) -> Result<Json<Value>, String> {
    let value = dispatch(state, req.command, req.args).await?;
    Ok(Json(value))
}

fn parse<T: DeserializeOwned>(args: Value) -> Result<T, String> {
    serde_json::from_value(args).map_err(|e| format!("bad args: {e}"))
}

fn to_value<T: serde::Serialize>(v: T) -> Result<Value, String> {
    serde_json::to_value(v).map_err(|e| format!("serialize: {e}"))
}

#[allow(dead_code)]
fn ok() -> Result<Value, String> {
    Ok(json!({ "ok": true }))
}

async fn dispatch(state: Arc<AppState>, command: String, args: Value) -> Result<Value, String> {
    match command.as_str() {
        // ── chat ──
        "send_message" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct A {
                session_id: String,
                message: String,
                #[serde(default)]
                teams_mode: bool,
                #[serde(default)]
                attachments: Option<Vec<String>>,
            }
            let a: A = parse(args)?;
            chat::send_message(state, a.session_id, a.message, a.teams_mode, a.attachments).await?;
            ok()
        }
        "stage_upload" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct A {
                session_id: String,
                name: String,
                base64_data: String,
            }
            let a: A = parse(args)?;
            let path = chat::stage_upload(state, a.session_id, a.name, a.base64_data).await?;
            to_value(path)
        }
        "approve_permission" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct A {
                request_id: String,
            }
            let a: A = parse(args)?;
            chat::approve_permission(state, a.request_id).await?;
            ok()
        }
        "deny_permission" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct A {
                request_id: String,
            }
            let a: A = parse(args)?;
            chat::deny_permission(state, a.request_id).await?;
            ok()
        }
        "abort" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct A {
                session_id: String,
            }
            let a: A = parse(args)?;
            chat::abort(state, a.session_id).await?;
            ok()
        }
        "stop_team_agent" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct A {
                session_id: String,
                agent_id: String,
            }
            let a: A = parse(args)?;
            let v = chat::stop_team_agent(a.session_id, a.agent_id).await?;
            to_value(v)
        }
        "nudge_team_agent" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct A {
                session_id: String,
                agent_id: String,
                message: String,
            }
            let a: A = parse(args)?;
            let v = chat::nudge_team_agent(a.session_id, a.agent_id, a.message).await?;
            to_value(v)
        }
        "list_tools" => {
            let v = chat::list_tools(state).await?;
            to_value(v)
        }
        "list_skills" => {
            let v = chat::list_skills().await?;
            to_value(v)
        }
        "save_skill" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct A {
                name: String,
                description: String,
                body: String,
                triggers: Option<String>,
                files: Option<Vec<chat::SkillFileInput>>,
            }
            let a: A = parse(args)?;
            let v = chat::save_skill(a.name, a.description, a.body, a.triggers, a.files).await?;
            to_value(v)
        }
        "write_skill_file" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct A {
                skill_name: String,
                relative_path: String,
                content: String,
            }
            let a: A = parse(args)?;
            let v = chat::write_skill_file(a.skill_name, a.relative_path, a.content).await?;
            to_value(v)
        }
        "skills_dir" => {
            let v = chat::skills_dir().await?;
            to_value(v)
        }
        "install_skill_package" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct A {
                package: String,
            }
            let a: A = parse(args)?;
            let v = chat::install_skill_package(a.package).await?;
            to_value(v)
        }
        "delete_skill" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct A {
                name: String,
                root: Option<String>,
            }
            let a: A = parse(args)?;
            let v = chat::delete_skill(a.name, a.root).await?;
            to_value(v)
        }
        "subscribe_task_events" => {
            chat::subscribe_task_events(state).await?;
            ok()
        }

        // ── mcp ──
        "list_mcp_servers" => {
            let v = mcp::list_mcp_servers(state).await?;
            to_value(v)
        }
        "add_mcp_server" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct A {
                name: String,
                command: String,
                args: String,
            }
            let a: A = parse(args)?;
            let v = mcp::add_mcp_server(state, a.name, a.command, a.args).await?;
            to_value(v)
        }
        "update_mcp_server" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct A {
                original_name: String,
                name: String,
                command: String,
                args: String,
            }
            let a: A = parse(args)?;
            let v = mcp::update_mcp_server(state, a.original_name, a.name, a.command, a.args).await?;
            to_value(v)
        }
        "remove_mcp_server" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct A {
                name: String,
            }
            let a: A = parse(args)?;
            let v = mcp::remove_mcp_server(state, a.name).await?;
            to_value(v)
        }
        "reload_mcp" => {
            let v = mcp::reload_mcp(state).await?;
            to_value(v)
        }

        // ── session ──
        "list_sessions" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct A {
                limit: Option<usize>,
                offset: Option<usize>,
            }
            let a: A = parse(args)?;
            let v = session::list_sessions(state, a.limit, a.offset).await?;
            to_value(v)
        }
        "get_session" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct A {
                id: String,
            }
            let a: A = parse(args)?;
            let v = session::get_session(state, a.id).await?;
            to_value(v)
        }
        "create_session" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct A {
                title: String,
                workspace: String,
            }
            let a: A = parse(args)?;
            let v = session::create_session(state, a.title, a.workspace).await?;
            to_value(v)
        }
        "get_last_session" => {
            let v = session::get_last_session(state).await?;
            to_value(v)
        }
        "update_session_workspace" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct A {
                session_id: String,
                workspace: String,
            }
            let a: A = parse(args)?;
            session::update_session_workspace(state, a.session_id, a.workspace).await?;
            ok()
        }
        "update_session_title" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct A {
                session_id: String,
                title: String,
            }
            let a: A = parse(args)?;
            session::update_session_title(state, a.session_id, a.title).await?;
            ok()
        }
        "update_session_model" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct A {
                session_id: String,
                model: String,
            }
            let a: A = parse(args)?;
            session::update_session_model(state, a.session_id, a.model).await?;
            ok()
        }
        "delete_session" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct A {
                id: String,
            }
            let a: A = parse(args)?;
            session::delete_session(state, a.id).await?;
            ok()
        }

        // ── config ──
        "get_config" => {
            let v = config::get_config(state).await?;
            to_value(v)
        }
        "update_config" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct A {
                config: dscode_core::config::settings::Config,
            }
            let a: A = parse(args)?;
            config::update_config(state, a.config).await?;
            ok()
        }
        "get_global_prompt" => {
            let v = config::get_global_prompt(state).await?;
            to_value(v)
        }
        "set_global_prompt" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct A {
                global_prompt: String,
                replace_system_prompt: bool,
            }
            let a: A = parse(args)?;
            let v = config::set_global_prompt(state, a.global_prompt, a.replace_system_prompt).await?;
            to_value(v)
        }
        "fetch_models" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct A {
                provider_key: String,
            }
            let a: A = parse(args)?;
            let v = config::fetch_models(state, a.provider_key).await?;
            to_value(v)
        }

        _ => Err(format!("unknown command: {command}")),
    }
}
