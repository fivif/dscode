//! TeamRuntime — sole production orchestrator for pure `/teams` mode (v2).

use std::path::PathBuf;
use std::sync::Arc;

use futures::stream::{FuturesUnordered, StreamExt};
use tokio::sync::{mpsc, Mutex};
use tracing::{info, warn};

use crate::agent::forge::{Forge, ForgeError};
use crate::agent::stream::StreamEvent;
use crate::providers::trait_def::{LlmProvider, Message, MessageContent, Role};
use crate::safety::guard::SafetyGuard;
use crate::safety::permission::PermissionHub;
use crate::tools::registry::ToolRegistry;

use super::board::{TaskBoard, TaskSpec, TaskStatus};
use super::config::TeamsConfig;
use super::control::global_control_planes;
use super::merge::{
    build_results_block, decompose_prompt, merge_prompt, raw_concat_report, synthesize_prompt,
};
use super::ownership::FileOwnership;
use super::role::{tool_names_for_role, AgentRole, RoleToolPolicy};
use super::schema::{
    fallback_task, parse_decompose, parse_synthesize, prefer_skip_research, SchemaError,
};

/// Runtime dependencies for a teams session.
pub struct TeamRuntime {
    provider: Box<dyn LlmProvider>,
    tools: Arc<ToolRegistry>,
    working_dir: PathBuf,
    safety_guard: Arc<SafetyGuard>,
    permission_hub: Option<Arc<PermissionHub>>,
    permission_timeout_secs: u64,
    config: TeamsConfig,
    event_tx: mpsc::UnboundedSender<StreamEvent>,
}

impl TeamRuntime {
    pub fn new(
        provider: Box<dyn LlmProvider>,
        tools: Arc<ToolRegistry>,
        working_dir: PathBuf,
        safety_guard: Arc<SafetyGuard>,
        permission_hub: Option<Arc<PermissionHub>>,
        permission_timeout_secs: u64,
        config: TeamsConfig,
        event_tx: mpsc::UnboundedSender<StreamEvent>,
    ) -> Self {
        Self {
            provider,
            tools,
            working_dir,
            safety_guard,
            permission_hub,
            permission_timeout_secs,
            config,
            event_tx,
        }
    }

    fn emit(&self, ev: StreamEvent) {
        let _ = self.event_tx.send(ev);
    }

    fn emit_token(&self, s: impl Into<String>) {
        self.emit(StreamEvent::Token {
            content: s.into(),
        });
    }

    /// Run full teams v2 pipeline for a user task.
    pub async fn run(
        &self,
        task: &str,
        session_id: &str,
        history: Vec<Message>,
    ) -> Result<(), ForgeError> {
        let cfg = &self.config;
        let max_agents = cfg.max_agents_capped();
        let max_parallel = cfg.max_parallel_capped();
        let waves = cfg.effective_waves();
        if cfg.waves_enabled && !super::config::MULTI_WAVE_IMPLEMENTED {
            warn!("teams.waves_enabled ignored — multi-wave not implemented");
        }

        let cp = global_control_planes().get_or_create(session_id).await;
        let session_token = tokio_util::sync::CancellationToken::new();
        cp.begin_session(session_token.child_token()).await;

        let mut board = TaskBoard::new(session_id);
        if cfg.persist_board {
            if let Ok(dir) = crate::config::settings::Config::tasks_dir() {
                board = board.with_persist_path(dir.join(format!("{session_id}-board.json")));
            }
        }
        let ownership = Arc::new(Mutex::new(FileOwnership::new()));

        let context_summary = history_summary(&history);
        self.emit_token(format!(
            "## Teams v2 — coordinator\n\n\
             **Task:** {task}\n\n\
             **Workspace:** `{}`\n\n\
             waves={} · max_parallel={max_parallel} · max_agents={max_agents}\n\n",
            self.working_dir.display(),
            waves
        ));

        // ── Decompose ──
        let skip = prefer_skip_research(task, waves);
        let decomp_prompt = decompose_prompt(
            task,
            &context_summary,
            &self.working_dir.display().to_string(),
            max_agents,
            waves && !skip,
            skip || !waves,
        );

        let mut plan_text = "Dispatch implement tasks.".to_string();
        let mut initial_tasks: Vec<TaskSpec> = match self.llm_text(&decomp_prompt).await {
            Ok(raw) => match parse_decompose(&raw, max_agents, waves && !skip) {
                Ok((plan, tasks, _skip_flag)) => {
                    plan_text = if plan.is_empty() {
                        plan_text
                    } else {
                        plan
                    };
                    tasks
                }
                Err(e) => {
                    warn!(error = %e, "decompose parse failed — fallback");
                    self.emit_token(format!(
                        "⚠️ Decompose parse failed ({e}); single-agent fallback.\n\n"
                    ));
                    vec![fallback_task(task)]
                }
            },
            Err(e) => {
                warn!(error = %e, "decompose LLM failed — fallback");
                self.emit_token(format!(
                    "⚠️ Decompose LLM failed ({e}); single-agent fallback.\n\n"
                ));
                vec![fallback_task(task)]
            }
        };

        if initial_tasks.is_empty() {
            initial_tasks.push(fallback_task(task));
        }
        if initial_tasks.len() > max_agents {
            initial_tasks.truncate(max_agents);
        }

        board.upsert_many(initial_tasks).map_err(|e| {
            ForgeError::Tool(crate::tools::trait_def::ToolError::Internal(e.to_string()))
        })?;

        self.emit_token(format!("### Plan\n\n{plan_text}\n\n"));
        for t in board.tasks() {
            self.emit_token(format!(
                "- **{}** ({:?}): {}\n",
                t.id, t.role, t.title
            ));
            self.emit(StreamEvent::TeamAgentStart {
                agent_id: t.id.clone(),
                task: t.prompt.clone(),
            });
        }
        self.emit_token("\n---\n\n");

        let has_explore = board.tasks().any(|t| t.role == AgentRole::Explore);
        info!(
            has_explore,
            tasks = board.len(),
            "TeamRuntime: board ready"
        );

        // ── Schedule loop ──
        let mut synthesized = false;
        let mut verify_seeded = false;
        loop {
            if session_token.is_cancelled() {
                for t in board.tasks().map(|t| t.id.clone()).collect::<Vec<_>>() {
                    let st = board.get(&t).map(|x| x.status);
                    if matches!(st, Some(TaskStatus::Running) | Some(TaskStatus::Pending)) {
                        let _ = board.mark_cancelled(&t);
                    }
                }
                break;
            }

            // Multi-wave: after all explore terminal, synthesize once
            if waves
                && !synthesized
                && has_explore
                && !board.any_running()
                && board
                    .tasks()
                    .filter(|t| t.role == AgentRole::Explore)
                    .all(|t| {
                        matches!(
                            t.status,
                            TaskStatus::Done
                                | TaskStatus::Failed
                                | TaskStatus::Cancelled
                                | TaskStatus::Blocked
                        )
                    })
            {
                self.emit_token("### Synthesizing implement plan…\n\n");
                let research: Vec<&TaskSpec> = board
                    .tasks()
                    .filter(|t| t.role == AgentRole::Explore)
                    .collect();
                let block = build_results_block(&research);
                let existing: Vec<String> = board.tasks().map(|t| t.id.clone()).collect();
                match self.llm_text(&synthesize_prompt(task, &block)).await {
                    Ok(raw) => match parse_synthesize(&raw, max_agents, &existing) {
                        Ok((plan, impl_tasks)) => {
                            if !plan.is_empty() {
                                plan_text = plan;
                            }
                            for t in &impl_tasks {
                                self.emit(StreamEvent::TeamAgentStart {
                                    agent_id: t.id.clone(),
                                    task: t.prompt.clone(),
                                });
                                self.emit_token(format!(
                                    "- **{}** (implement): {}\n",
                                    t.id, t.title
                                ));
                            }
                            let _ = board.upsert_many(impl_tasks);
                        }
                        Err(e) => {
                            warn!(%e, "synthesize parse failed");
                            let mut fb = fallback_task(&format!(
                                "{task}\n\n## Research excerpts\n{block}"
                            ));
                            fb.id = "impl-fallback".into();
                            let _ = board.upsert(fb.clone());
                            self.emit(StreamEvent::TeamAgentStart {
                                agent_id: fb.id.clone(),
                                task: fb.prompt.clone(),
                            });
                        }
                    },
                    Err(e) => {
                        warn!(%e, "synthesize LLM failed");
                        let mut fb =
                            fallback_task(&format!("{task}\n\n## Research excerpts\n{block}"));
                        fb.id = "impl-fallback".into();
                        let _ = board.upsert(fb.clone());
                        self.emit(StreamEvent::TeamAgentStart {
                            agent_id: fb.id.clone(),
                            task: fb.prompt.clone(),
                        });
                    }
                }
                synthesized = true;
            }

            // After implement tasks terminal, seed one Verify task (waves only)
            if waves
                && !verify_seeded
                && !board.any_running()
                && board.tasks().any(|t| t.role == AgentRole::Implement)
                && board
                    .tasks()
                    .filter(|t| t.role == AgentRole::Implement)
                    .all(|t| {
                        matches!(
                            t.status,
                            TaskStatus::Done
                                | TaskStatus::Failed
                                | TaskStatus::Cancelled
                                | TaskStatus::Blocked
                        )
                    })
                && !board.tasks().any(|t| t.role == AgentRole::Verify)
            {
                let done_impl: Vec<String> = board
                    .tasks()
                    .filter(|t| t.role == AgentRole::Implement && t.status == TaskStatus::Done)
                    .map(|t| t.id.clone())
                    .collect();
                if !done_impl.is_empty() {
                    let mut v = TaskSpec::new(
                        "verify-1",
                        "Verify changes",
                        format!(
                            "Verify the implement work for: {task}\n\
                             Run available tests (cargo test / npm test / etc). \
                             Report pass/fail with evidence. Do not modify product code."
                        ),
                        AgentRole::Verify,
                    );
                    v.dependencies = done_impl;
                    self.emit_token("- **verify-1** (verify): Verify changes\n");
                    self.emit(StreamEvent::TeamAgentStart {
                        agent_id: v.id.clone(),
                        task: v.prompt.clone(),
                    });
                    let _ = board.upsert(v);
                }
                verify_seeded = true;
            }

            let ready_ids: Vec<String> = board
                .schedulable_tasks()
                .into_iter()
                .map(|t| t.id.clone())
                .take(max_parallel)
                .collect();

            if ready_ids.is_empty() {
                if board.any_running() {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    continue;
                }
                break;
            }

            let mut futs = FuturesUnordered::new();
            for tid in ready_ids {
                let agent_id = tid.clone();
                if let Err(e) = board.claim(&tid, &agent_id) {
                    warn!(%tid, error = %e, "claim failed");
                    continue;
                }
                let task_spec = board.get(&tid).cloned().unwrap();

                if task_spec.role == AgentRole::Implement {
                    let mut fo = ownership.lock().await;
                    if let Err(acc) = fo.reserve(&agent_id, &task_spec.owned_paths) {
                        warn!(?acc, "ownership reserve failed");
                        let _ = board.mark_failed(&tid, format!("ownership: {acc:?}"));
                        self.emit(StreamEvent::TeamAgentEnd {
                            agent_id: agent_id.clone(),
                            success: false,
                            summary: "ownership conflict".into(),
                        });
                        continue;
                    }
                }

                let tools = self.tools_for_role(task_spec.role);
                let prov = self.provider.clone_box();
                let wd = self.working_dir.clone();
                let sg = self.safety_guard.clone();
                let ph = self.permission_hub.clone();
                let pto = self.permission_timeout_secs;
                let tx = self.event_tx.clone();
                let sid = session_id.to_string();
                let max_iter = task_spec.role.max_iterations();
                let prompt = task_spec.prompt.clone();
                let sys = format!(
                    "{}\nWorking directory: {}.\nTask id: {}.",
                    task_spec.role.system_fragment(),
                    wd.display(),
                    task_spec.id
                );
                let own = ownership.clone();
                let cp_agent = cp.clone();
                let (cancel, nudge, _notify) =
                    cp.register(agent_id.clone(), tid.clone()).await;

                futs.push(async move {
                    let result = run_sub_agent(
                        prov,
                        tools,
                        wd,
                        sid,
                        agent_id.clone(),
                        prompt,
                        sys,
                        max_iter,
                        sg,
                        ph,
                        pto,
                        tx.clone(),
                        cancel,
                        nudge,
                    )
                    .await;
                    own.lock().await.release(&agent_id);
                    cp_agent.unregister(&agent_id).await;
                    (agent_id, result)
                });
            }

            while let Some((agent_id, result)) = futs.next().await {
                match result {
                    Ok(output) => {
                        let summary: String = output.chars().take(200).collect();
                        let _ = board.mark_done(&agent_id, summary.clone(), output);
                        self.emit(StreamEvent::TeamAgentEnd {
                            agent_id: agent_id.clone(),
                            success: true,
                            summary,
                        });
                    }
                    Err(e) if e == "cancelled" || e.starts_with("cancelled") => {
                        let _ = board.mark_cancelled(&agent_id);
                        self.emit(StreamEvent::TeamAgentEnd {
                            agent_id: agent_id.clone(),
                            success: false,
                            summary: e,
                        });
                    }
                    Err(e) => {
                        let _ = board.mark_failed(&agent_id, e.clone());
                        self.emit(StreamEvent::TeamAgentEnd {
                            agent_id: agent_id.clone(),
                            success: false,
                            summary: e,
                        });
                    }
                }
            }

            let _ = board.persist();
        }

        // ── Merge ──
        let counts = board.counts();
        self.emit(StreamEvent::TeamComplete {
            completed: counts.done,
            failed: counts.failed + counts.cancelled + counts.blocked,
        });
        self.emit_token(format!(
            "\n---\n\n### Main agent summary\n\n\
             Done **{}** · failed/blocked **{}** · total **{}**\n\n",
            counts.done,
            counts.failed + counts.blocked + counts.cancelled,
            counts.total
        ));

        let all: Vec<TaskSpec> = board.tasks().cloned().collect();
        let refs: Vec<&TaskSpec> = all.iter().collect();
        let block = build_results_block(&refs);
        let mp = merge_prompt(task, &plan_text, &block);
        match self.llm_text(&mp).await {
            Ok(report) if !report.trim().is_empty() => {
                self.emit_token(report);
            }
            Ok(_) | Err(_) => {
                self.emit_token(raw_concat_report(task, &plan_text, &refs));
            }
        }

        cp.end_session().await;
        global_control_planes().remove(session_id).await;

        self.emit(StreamEvent::Complete { usage: None });
        Ok(())
    }

    fn tools_for_role(&self, role: AgentRole) -> Arc<ToolRegistry> {
        let policy = tool_names_for_role(role, self.config.explore_bash);
        match policy {
            RoleToolPolicy::Allowlist(names) => {
                let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
                self.tools.with_allowlist(&refs)
            }
            RoleToolPolicy::Denylist(names) => {
                let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
                // Also denylist mcp for non-implement already handled by allowlist.
                // For implement: denylist skill_install only; keep mcp_*
                self.tools.with_denylist(&refs)
            }
        }
    }

    async fn llm_text(&self, prompt: &str) -> Result<String, ForgeError> {
        let resp = self
            .provider
            .chat(
                vec![Message {
                    role: Role::User,
                    content: MessageContent::Text(prompt.to_string()),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                    created_at: 0,
                }],
                vec![],
            )
            .await?;
        Ok(resp.content)
    }
}

async fn run_sub_agent(
    provider: Box<dyn LlmProvider>,
    tools: Arc<ToolRegistry>,
    working_dir: PathBuf,
    session_id: String,
    agent_id: String,
    prompt: String,
    system: String,
    max_iterations: u32,
    safety_guard: Arc<SafetyGuard>,
    permission_hub: Option<Arc<PermissionHub>>,
    permission_timeout_secs: u64,
    event_tx: mpsc::UnboundedSender<StreamEvent>,
    cancel: tokio_util::sync::CancellationToken,
    nudge: Arc<tokio::sync::Mutex<Vec<String>>>,
) -> Result<String, String> {
    let mut forge = Forge::new(provider, tools, working_dir)
        .with_system_prompt(system)
        .with_teams_mode(false)
        .with_max_iterations(max_iterations)
        .with_safety_guard(safety_guard)
        .with_cancel_token(cancel.clone())
        .with_nudge_queue(nudge);
    if let Some(hub) = permission_hub {
        forge = forge.with_permission_hub(hub);
    }
    forge = forge.with_permission_timeout(permission_timeout_secs);

    let (stx, mut srx) = mpsc::unbounded_channel();
    let out_buf = Arc::new(tokio::sync::Mutex::new(String::new()));
    let out_for_drain = out_buf.clone();
    let aid = agent_id.clone();
    let tx_d = event_tx.clone();

    let drain = async move {
        while let Some(ev) = srx.recv().await {
            match ev {
                StreamEvent::Token { content } => {
                    out_for_drain.lock().await.push_str(&content);
                    let _ = tx_d.send(StreamEvent::TeamAgentOutput {
                        agent_id: aid.clone(),
                        content,
                    });
                }
                StreamEvent::ToolStart { name, .. } => {
                    let note = format!("\n🔧 {name}\n");
                    out_for_drain.lock().await.push_str(&note);
                    let _ = tx_d.send(StreamEvent::TeamAgentOutput {
                        agent_id: aid.clone(),
                        content: note,
                    });
                }
                StreamEvent::Error { content } => {
                    out_for_drain.lock().await.push_str(&content);
                }
                _ => {}
            }
        }
    };

    // Cooperative cancel on Forge + select abort drops the execute future (stops LLM wait).
    tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            let partial = out_buf.lock().await.clone();
            if partial.is_empty() {
                Err("cancelled".into())
            } else {
                Err(format!(
                    "cancelled (partial): {}",
                    partial.chars().take(200).collect::<String>()
                ))
            }
        }
        joined = async {
            let exec = forge.execute(&prompt, &session_id, vec![], stx);
            let (res, ()) = tokio::join!(exec, drain);
            res
        } => {
            let out = out_buf.lock().await.clone();
            match joined {
                Ok(()) => Ok(out),
                Err(crate::agent::forge::ForgeError::Cancelled) => Err("cancelled".into()),
                Err(e) => {
                    if out.is_empty() {
                        Err(e.to_string())
                    } else {
                        Ok(out)
                    }
                }
            }
        }
    }
}

fn history_summary(history: &[Message]) -> String {
    let mut s = String::new();
    for msg in history.iter().rev().take(10).rev() {
        if let Some(t) = msg.content.as_text() {
            let trunc: String = t.chars().take(200).collect();
            s.push_str(&format!("[{:?}]: {trunc}\n", msg.role));
        }
    }
    if s.is_empty() {
        "(no context)".into()
    } else {
        s
    }
}

// silence unused import warning for SchemaError in some builds
#[allow(dead_code)]
fn _schema_err(_: SchemaError) {}
