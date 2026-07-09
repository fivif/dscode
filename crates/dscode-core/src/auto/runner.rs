//! AutoRunner — self-directed loop that decomposes tasks, runs MAGI spirals,
//! detects stalls, and re-decomposes until done or interrupted.

use std::path::PathBuf;
use std::sync::Arc;

use tracing::{info, warn};

#[allow(unused_imports)]
use crate::agent::stream::StreamEvent;
use crate::magi::scheduler::{MagiError, MagiRound, MagiScheduler};
use crate::providers::trait_def::{LlmProvider, ProviderError};
use crate::tools::registry::ToolRegistry;

use super::decomposer::decompose_task;
use super::stall::StallDetector;

/// Optional progress sink for UI streaming.
pub type ProgressTx = tokio::sync::mpsc::UnboundedSender<StreamEvent>;

/// Default maximum number of re-decomposition cycles before giving up.
const DEFAULT_MAX_REDECOMPOSE_CYCLES: u32 = 3;

/// Default stall threshold — three consecutive rounds with no improvement.
const DEFAULT_STALL_ROUNDS: usize = 3;

/// Practical defaults for interactive /auto (was 10×30 — felt hung forever).
const DEFAULT_MAGI_MAX_ROUNDS: u32 = 3;
const DEFAULT_MAGI_MAX_STEPS: u32 = 12;

/// Max concurrent MAGI spirals when Teams is combined with /auto.
const DEFAULT_MAX_PARALLEL: usize = 4;

/// Errors that can occur during the auto-runner loop.
#[derive(Debug, thiserror::Error)]
pub enum AutoError {
    /// The underlying MAGI spiral returned an error.
    #[error("MAGI error: {0}")]
    Magi(#[from] MagiError),

    /// The LLM provider returned an error.
    #[error("provider error: {0}")]
    Provider(#[from] ProviderError),

    /// Failed to parse a structured LLM response.
    #[error("parse error: {0}")]
    Parse(String),

    /// Maximum re-decomposition cycles reached without completion.
    #[error("max re-decompose cycles ({0}) reached")]
    MaxRecomposeCycles(u32),

    /// No subtasks were produced by the decomposer.
    #[error("decomposer produced no subtasks")]
    NoSubtasks,
}

/// A single subtask within the auto-runner's work plan.
#[derive(Debug, Clone)]
pub struct Subtask {
    /// Unique identifier within this decomposition.
    pub id: usize,
    /// Human-readable description of what the subtask entails.
    pub description: String,
    /// IDs of subtasks that must complete before this one can start.
    pub dependencies: Vec<usize>,
    /// Current execution status.
    pub status: SubtaskStatus,
}

/// Execution status of a subtask.
#[derive(Debug, Clone, PartialEq)]
pub enum SubtaskStatus {
    /// Not yet started.
    Pending,
    /// Currently executing via MAGI spiral.
    InProgress,
    /// Completed successfully.
    Done,
    /// Failed with an error message.
    Failed(String),
}

/// The result of a complete auto-runner execution.
#[derive(Debug, Clone)]
pub struct AutoRunResult {
    /// The final set of subtasks (with their statuses).
    pub subtasks: Vec<Subtask>,
    /// MAGI rounds executed for each subtask (in execution order).
    pub rounds_per_subtask: Vec<Vec<MagiRound>>,
    /// Average quality score across all completed subtasks.
    pub total_quality: f64,
    /// Whether the run was interrupted by an unresolvable stall.
    pub stalled: bool,
}

/// The auto-runner orchestrator.
///
/// Takes a high-level task description, decomposes it into subtasks, runs
/// MAGI spirals on each, detects stalls, and re-decomposes when necessary.
///
/// Providers are stored behind `Arc` so they can be cloned into
/// [`MagiScheduler`] instances without giving up ownership.
pub struct AutoRunner {
    /// Primary LLM provider for MAGI spirals (Casper + Balthasar).
    provider: Arc<Box<dyn LlmProvider>>,
    /// Cheaper LLM provider for Melchior evaluation and decomposition.
    runtime_provider: Arc<Box<dyn LlmProvider>>,
    /// Shared tool registry for all execution rounds.
    tools: Arc<ToolRegistry>,
    /// Working directory for file-relative path resolution.
    working_dir: PathBuf,
    /// Maximum MAGI spiral rounds per subtask.
    magi_max_rounds: u32,
    /// Maximum MAGI ReAct steps per execution round.
    magi_max_steps: u32,
    /// Maximum re-decomposition cycles before forced stop.
    max_recompose_cycles: u32,
    /// Number of consecutive no-progress rounds that trigger a stall.
    stall_rounds: usize,
    /// Optional UI progress channel.
    progress: Option<ProgressTx>,
    /// When true (Teams mode ON + /auto), run ready subtasks concurrently.
    teams_parallel: bool,
    /// Cap on concurrent MAGI spirals in teams_parallel mode.
    max_parallel: usize,
    safety_guard: Arc<crate::safety::guard::SafetyGuard>,
    permission_hub: Option<Arc<crate::safety::permission::PermissionHub>>,
    permission_timeout_secs: u64,
}

impl AutoRunner {
    /// Create a new auto-runner.
    pub fn new(
        provider: Box<dyn LlmProvider>,
        runtime_provider: Box<dyn LlmProvider>,
        tools: Arc<ToolRegistry>,
        working_dir: PathBuf,
    ) -> Self {
        Self {
            provider: Arc::new(provider),
            runtime_provider: Arc::new(runtime_provider),
            tools,
            working_dir,
            magi_max_rounds: DEFAULT_MAGI_MAX_ROUNDS,
            magi_max_steps: DEFAULT_MAGI_MAX_STEPS,
            max_recompose_cycles: DEFAULT_MAX_REDECOMPOSE_CYCLES,
            stall_rounds: DEFAULT_STALL_ROUNDS,
            progress: None,
            teams_parallel: false,
            max_parallel: DEFAULT_MAX_PARALLEL,
            safety_guard: Arc::new(crate::safety::guard::SafetyGuard::new(&[], false)),
            permission_hub: None,
            permission_timeout_secs: 120,
        }
    }

    pub fn with_safety_guard(mut self, guard: Arc<crate::safety::guard::SafetyGuard>) -> Self {
        self.safety_guard = guard;
        self
    }

    pub fn with_permission_hub(
        mut self,
        hub: Option<Arc<crate::safety::permission::PermissionHub>>,
    ) -> Self {
        self.permission_hub = hub;
        self
    }

    pub fn with_permission_timeout(mut self, secs: u64) -> Self {
        self.permission_timeout_secs = secs.max(10);
        self
    }

    /// Attach a progress event channel for UI streaming.
    pub fn with_progress(mut self, tx: ProgressTx) -> Self {
        self.progress = Some(tx);
        self
    }

    /// Enable Teams-style parallel MAGI for independent ready subtasks.
    pub fn with_teams_parallel(mut self, on: bool) -> Self {
        self.teams_parallel = on;
        self
    }

    /// Override max concurrent MAGI spirals when teams_parallel is on.
    pub fn with_max_parallel(mut self, n: usize) -> Self {
        self.max_parallel = n.max(1);
        self
    }

    /// UI agent id — `at-N` when auto+teams so the frontend labels "Auto · Teams".
    fn agent_id_for(&self, task_id: usize) -> String {
        if self.teams_parallel {
            format!("at-{task_id}")
        } else {
            format!("subtask-{task_id}")
        }
    }

    fn emit(&self, event: StreamEvent) {
        if let Some(ref tx) = self.progress {
            let _ = tx.send(event);
        }
    }

    fn emit_token(&self, content: impl Into<String>) {
        self.emit(StreamEvent::Token {
            content: content.into(),
        });
    }

    fn emit_thinking(&self, content: impl Into<String>, step: u32) {
        self.emit(StreamEvent::Thinking {
            content: content.into(),
            step,
        });
    }

    /// Override the max MAGI rounds per subtask.
    pub fn with_magi_max_rounds(mut self, n: u32) -> Self {
        self.magi_max_rounds = n;
        self
    }

    /// Override the max MAGI steps per execution round.
    pub fn with_magi_max_steps(mut self, n: u32) -> Self {
        self.magi_max_steps = n;
        self
    }

    /// Override the max re-decomposition cycles.
    pub fn with_max_recompose_cycles(mut self, n: u32) -> Self {
        self.max_recompose_cycles = n;
        self
    }

    /// Override the number of stall rounds before re-decomposition.
    pub fn with_stall_rounds(mut self, n: usize) -> Self {
        self.stall_rounds = n;
        self
    }

    /// Run the full auto-loop on a task description.
    ///
    /// # Flow
    /// 1. Decompose the PRD into subtasks.
    /// 2. For each ready subtask (respecting dependencies), run a MAGI spiral.
    /// 3. Track quality scores across subtasks.
    /// 4. If progress stalls for `stall_rounds` consecutive completions,
    ///    re-decompose the remaining work.
    /// 5. Repeat until all subtasks are resolved or re-decomposition cycles
    ///    are exhausted.
    pub async fn run(&self, prd: &str, session_id: &str) -> Result<AutoRunResult, AutoError> {
        let mode_line = if self.teams_parallel {
            format!(
                "## /auto + Teams — parallel auto spirals\n\n\
                 Teams is ON: ready subtasks run concurrently (max {} at a time).\n\n\
                 Decomposing task into subtasks…\n\n",
                self.max_parallel
            )
        } else {
            "## /auto — auto spiral\n\n\
             Decomposing task into subtasks…\n\n\
             _Tip: turn on **TEAM** to run independent subtasks in parallel._\n\n"
                .to_string()
        };
        self.emit_token(mode_line);
        let mut subtasks = decompose_task(&**self.runtime_provider, prd).await?;

        if subtasks.is_empty() {
            warn!(session = %session_id, "AutoRunner: no subtasks produced");
            return Err(AutoError::NoSubtasks);
        }

        info!(
            session = %session_id,
            subtask_count = subtasks.len(),
            teams_parallel = self.teams_parallel,
            "AutoRunner: task decomposed into {} subtasks",
            subtasks.len()
        );

        self.emit_token(format!(
            "### Plan ({} subtasks){}\n\n{}\n\n",
            subtasks.len(),
            if self.teams_parallel {
                " · **parallel auto**"
            } else {
                " · sequential auto"
            },
            subtasks
                .iter()
                .map(|s| format!("- [ ] **#{}** {}", s.id, s.description))
                .collect::<Vec<_>>()
                .join("\n")
        ));

        let mut all_rounds: Vec<Vec<MagiRound>> = Vec::new();
        let mut stall_detector = StallDetector::new(self.stall_rounds);
        let mut recompose_count = 0u32;
        let mut consecutive_recompose_failures = 0u32;

        loop {
            let ready_indices = find_all_ready(&subtasks);

            if ready_indices.is_empty() {
                let pending_count = subtasks
                    .iter()
                    .filter(|s| s.status == SubtaskStatus::Pending)
                    .count();

                if pending_count == 0 {
                    info!(session = %session_id, "AutoRunner: all subtasks resolved");
                    break;
                }

                warn!(
                    session = %session_id,
                    pending = pending_count,
                    "AutoRunner: all remaining subtasks are blocked on failed dependencies"
                );
                break;
            };

            // Sequential: one; Teams parallel: up to max_parallel ready tasks
            let batch: Vec<usize> = if self.teams_parallel {
                ready_indices
                    .into_iter()
                    .take(self.max_parallel)
                    .collect()
            } else {
                vec![ready_indices[0]]
            };

            if self.teams_parallel && batch.len() > 1 {
                self.emit_token(format!(
                    "\n---\n### Parallel wave — {} auto spirals\n\n",
                    batch.len()
                ));
            }

            // Mark + announce all in batch
            let mut jobs: Vec<(usize, usize, String, String)> = Vec::new();
            for &task_idx in &batch {
                let task_id = subtasks[task_idx].id;
                let task_desc = subtasks[task_idx].description.clone();
                let agent_id = self.agent_id_for(task_id);
                subtasks[task_idx].status = SubtaskStatus::InProgress;
                self.emit(StreamEvent::TeamAgentStart {
                    agent_id: agent_id.clone(),
                    task: task_desc.clone(),
                });
                self.emit_token(format!(
                    "\n### Subtask #{task_id}: {task_desc}\n\n\
                     Running auto spiral (review → execute → evaluate)…\n\n"
                ));
                jobs.push((task_idx, task_id, task_desc, agent_id));
            }

            // Run batch (parallel when >1)
            let outcomes = self
                .run_magi_batch(prd, session_id, &jobs)
                .await;

            for (task_idx, task_id, agent_id, spiral) in outcomes {
                self.apply_spiral_result(
                    &mut subtasks,
                    &mut all_rounds,
                    &mut stall_detector,
                    session_id,
                    task_idx,
                    task_id,
                    &agent_id,
                    spiral,
                );
            }

            // Check for stall
            if stall_detector.is_stalled() {
                warn!(
                    session = %session_id,
                    stalled_rounds = stall_detector.stalled_rounds(),
                    "AutoRunner: progress stalled, re-decomposing"
                );

                recompose_count += 1;
                if recompose_count > self.max_recompose_cycles {
                    warn!(
                        session = %session_id,
                        cycles = recompose_count,
                        "AutoRunner: max re-decompose cycles exhausted"
                    );
                    break;
                }

                // Re-decompose remaining subtasks
                let remaining_descriptions: Vec<String> = subtasks
                    .iter()
                    .filter(|s| {
                        s.status == SubtaskStatus::Pending
                            || matches!(&s.status, SubtaskStatus::Failed(_))
                    })
                    .map(|s| s.description.clone())
                    .collect();

                if remaining_descriptions.is_empty() {
                    break;
                }

                let remaining_prd = format!(
                    "Original PRD:\n{}\n\nRemaining unfinished work:\n{}",
                    prd,
                    remaining_descriptions
                        .iter()
                        .enumerate()
                        .map(|(i, d)| format!("{}. {}", i + 1, d))
                        .collect::<Vec<_>>()
                        .join("\n")
                );

                match decompose_task(&**self.runtime_provider, &remaining_prd).await {
                    Ok(new_subtasks) => {
                        // Remove all non-Completed subtasks before appending new ones.
                        // Only keep subtasks that are already Done.
                        subtasks.retain(|s| s.status == SubtaskStatus::Done);
                        let next_id = subtasks.iter().map(|s| s.id).max().unwrap_or(0) + 1;
                        for (i, mut st) in new_subtasks.into_iter().enumerate() {
                            st.id = next_id + i;
                            st.status = SubtaskStatus::Pending;
                            subtasks.push(st);
                        }
                        stall_detector.reset();
                        consecutive_recompose_failures = 0;
                        info!(
                            session = %session_id,
                            pending_count = subtasks.iter().filter(|s| s.status == SubtaskStatus::Pending).count(),
                            "AutoRunner: re-decomposed remaining work"
                        );
                    }
                    Err(e) => {
                        consecutive_recompose_failures += 1;
                        warn!(
                            session = %session_id,
                            error = %e,
                            consecutive_failures = consecutive_recompose_failures,
                            "AutoRunner: re-decomposition failed"
                        );
                        if consecutive_recompose_failures >= 3 {
                            warn!(
                                session = %session_id,
                                "AutoRunner: 3 consecutive re-decomposition failures, aborting"
                            );
                            return Err(AutoError::Parse(format!(
                                "Re-decomposition failed {} times consecutively: {}",
                                consecutive_recompose_failures, e
                            )));
                        }
                        stall_detector.reset();
                    }
                }
            }
        }

        let all_done = subtasks
            .iter()
            .all(|s| s.status == SubtaskStatus::Done);

        let stalled = recompose_count > self.max_recompose_cycles;

        let total_quality = compute_average_quality(&all_rounds);

        info!(
            session = %session_id,
            all_done,
            stalled,
            total_quality,
            subtask_count = subtasks.len(),
            "AutoRunner: run complete"
        );

        let done_n = subtasks.iter().filter(|s| s.status == SubtaskStatus::Done).count();
        let fail_n = subtasks.iter().filter(|s| matches!(s.status, SubtaskStatus::Failed(_))).count();
        self.emit(StreamEvent::TeamComplete {
            completed: done_n,
            failed: fail_n,
        });
        self.emit_token(format!(
            "\n---\n### /auto complete\n\n- Done: {done_n}\n- Failed: {fail_n}\n- Avg quality: {total_quality:.1}/100\n- Stalled: {stalled}\n\n"
        ));

        Ok(AutoRunResult {
            subtasks,
            rounds_per_subtask: all_rounds,
            total_quality,
            stalled,
        })
    }

    /// Run one or more MAGI spirals (concurrent when teams_parallel and batch > 1).
    async fn run_magi_batch(
        &self,
        prd: &str,
        session_id: &str,
        jobs: &[(usize, usize, String, String)], // (idx, id, desc, agent_id)
    ) -> Vec<(usize, usize, String, Result<Vec<MagiRound>, MagiError>)> {
        if jobs.len() <= 1 {
            let mut out = Vec::new();
            for (task_idx, task_id, task_desc, agent_id) in jobs {
                let spiral = self
                    .run_one_magi(prd, session_id, *task_id, task_desc, agent_id)
                    .await;
                out.push((*task_idx, *task_id, agent_id.clone(), spiral));
            }
            return out;
        }

        use futures::stream::{FuturesUnordered, StreamExt};

        let mut futs = FuturesUnordered::new();
        for (task_idx, task_id, task_desc, agent_id) in jobs {
            let provider = Arc::clone(&self.provider);
            let runtime = Arc::clone(&self.runtime_provider);
            let tools = Arc::clone(&self.tools);
            let wd = self.working_dir.clone();
            let max_rounds = self.magi_max_rounds;
            let max_steps = self.magi_max_steps;
            let progress = self.progress.clone();
            let safety = Arc::clone(&self.safety_guard);
            let hub = self.permission_hub.clone();
            let pto = self.permission_timeout_secs;
            let prd = prd.to_string();
            let session_id = session_id.to_string();
            let task_idx = *task_idx;
            let task_id = *task_id;
            let task_desc = task_desc.clone();
            let agent_id = agent_id.clone();

            futs.push(async move {
                let mut scheduler = MagiScheduler::from_arc_providers(
                    provider, runtime, tools, wd,
                )
                .with_max_rounds(max_rounds)
                .with_max_steps_per_round(max_steps)
                .with_safety_guard(safety)
                .with_permission_hub(hub)
                .with_permission_timeout(pto);
                if let Some(tx) = progress {
                    scheduler = scheduler.with_progress(crate::magi::execute::MagiProgress {
                        tx,
                        agent_id: agent_id.clone(),
                    });
                }
                let subtask_prd = format!("Original PRD:\n{prd}\n\nSubtask:\n{task_desc}");
                let subtask_session = format!("{session_id}-s{task_id}");
                let spiral = scheduler.run_spiral(&subtask_prd, &subtask_session).await;
                (task_idx, task_id, agent_id, spiral)
            });
        }

        let mut out = Vec::new();
        while let Some(item) = futs.next().await {
            out.push(item);
        }
        out
    }

    async fn run_one_magi(
        &self,
        prd: &str,
        session_id: &str,
        task_id: usize,
        task_desc: &str,
        agent_id: &str,
    ) -> Result<Vec<MagiRound>, MagiError> {
        let mut scheduler = MagiScheduler::from_arc_providers(
            Arc::clone(&self.provider),
            Arc::clone(&self.runtime_provider),
            Arc::clone(&self.tools),
            self.working_dir.clone(),
        )
        .with_max_rounds(self.magi_max_rounds)
        .with_max_steps_per_round(self.magi_max_steps)
        .with_safety_guard(Arc::clone(&self.safety_guard))
        .with_permission_hub(self.permission_hub.clone())
        .with_permission_timeout(self.permission_timeout_secs);

        if let Some(ref tx) = self.progress {
            scheduler = scheduler.with_progress(crate::magi::execute::MagiProgress {
                tx: tx.clone(),
                agent_id: agent_id.to_string(),
            });
        }

        let subtask_prd = format!("Original PRD:\n{prd}\n\nSubtask:\n{task_desc}");
        let subtask_session = format!("{session_id}-s{task_id}");
        scheduler.run_spiral(&subtask_prd, &subtask_session).await
    }

    fn apply_spiral_result(
        &self,
        subtasks: &mut [Subtask],
        all_rounds: &mut Vec<Vec<MagiRound>>,
        stall_detector: &mut StallDetector,
        session_id: &str,
        task_idx: usize,
        task_id: usize,
        agent_id: &str,
        spiral: Result<Vec<MagiRound>, MagiError>,
    ) {
        match spiral {
            Ok(rounds) => {
                for r in &rounds {
                    self.emit_thinking(
                        format!(
                            "Subtask #{task_id} round {}: quality={:.0}/100 stop={} — {}\n",
                            r.round_number,
                            r.promotion.quality_score,
                            r.promotion.should_stop,
                            r.promotion.stop_reason,
                        ),
                        r.round_number,
                    );
                    let exec_preview: String = r.execution.chars().take(400).collect();
                    if !exec_preview.is_empty() {
                        self.emit_token(format!(
                            "**#{task_id} Round {}**\n\n{exec_preview}\n\n",
                            r.round_number
                        ));
                    }
                }

                let last_quality = rounds
                    .last()
                    .map(|r| r.promotion.quality_score)
                    .unwrap_or(0.0);
                stall_detector.record(last_quality);
                let naturally_done = rounds
                    .last()
                    .map(|r| r.promotion.should_stop)
                    .unwrap_or(false);

                if naturally_done || last_quality >= 70.0 {
                    subtasks[task_idx].status = SubtaskStatus::Done;
                    info!(
                        session = %session_id,
                        subtask_id = task_id,
                        quality = last_quality,
                        rounds = rounds.len(),
                        "AutoRunner: subtask completed"
                    );
                    self.emit(StreamEvent::TeamAgentEnd {
                        agent_id: agent_id.to_string(),
                        success: true,
                        summary: format!(
                            "quality={last_quality:.0}/100, rounds={}",
                            rounds.len()
                        ),
                    });
                    self.emit_token(format!(
                        "✅ Subtask #{task_id} done (quality {last_quality:.0}/100, {} rounds)\n\n",
                        rounds.len()
                    ));
                } else {
                    subtasks[task_idx].status = SubtaskStatus::Failed(format!(
                        "Low quality score {:.1}/100 after {} rounds",
                        last_quality,
                        rounds.len()
                    ));
                    warn!(
                        session = %session_id,
                        subtask_id = task_id,
                        quality = last_quality,
                        "AutoRunner: subtask quality too low"
                    );
                    self.emit(StreamEvent::TeamAgentEnd {
                        agent_id: agent_id.to_string(),
                        success: false,
                        summary: format!("low quality {last_quality:.0}/100"),
                    });
                }
                all_rounds.push(rounds);
            }
            Err(MagiError::MaxRounds(max, rounds)) => {
                let last_quality = rounds
                    .last()
                    .map(|r| r.promotion.quality_score)
                    .unwrap_or(0.0);
                stall_detector.record(last_quality);
                if last_quality >= 60.0 {
                    subtasks[task_idx].status = SubtaskStatus::Done;
                    self.emit(StreamEvent::TeamAgentEnd {
                        agent_id: agent_id.to_string(),
                        success: true,
                        summary: format!("max rounds {max}, quality={last_quality:.0}"),
                    });
                    self.emit_token(format!(
                        "⚠️ Subtask #{task_id} reached max auto rounds ({max}) with quality {last_quality:.0}/100 — accepting partial result.\n\n"
                    ));
                } else {
                    subtasks[task_idx].status = SubtaskStatus::Failed(format!(
                        "Max auto rounds ({max}) with quality {last_quality:.0}/100"
                    ));
                    self.emit(StreamEvent::TeamAgentEnd {
                        agent_id: agent_id.to_string(),
                        success: false,
                        summary: format!("max rounds, quality={last_quality:.0}"),
                    });
                }
                all_rounds.push(rounds);
            }
            Err(e) => {
                subtasks[task_idx].status = SubtaskStatus::Failed(format!("auto error: {e}"));
                warn!(
                    session = %session_id,
                    subtask_id = task_id,
                    error = %e,
                    "AutoRunner: subtask failed"
                );
                self.emit(StreamEvent::TeamAgentEnd {
                    agent_id: agent_id.to_string(),
                    success: false,
                    summary: e.to_string(),
                });
                all_rounds.push(vec![]);
                stall_detector.record(0.0);
            }
        }
    }
}

/// Compute the average quality score across all completed subtask spirals.
fn compute_average_quality(all_rounds: &[Vec<MagiRound>]) -> f64 {
    let qualities: Vec<f64> = all_rounds
        .iter()
        .filter_map(|rounds| rounds.last())
        .map(|r| r.promotion.quality_score)
        .collect();

    if qualities.is_empty() {
        0.0
    } else {
        qualities.iter().sum::<f64>() / qualities.len() as f64
    }
}

/// All subtasks ready to execute (dependencies satisfied).
fn find_all_ready(subtasks: &[Subtask]) -> Vec<usize> {
    subtasks
        .iter()
        .enumerate()
        .filter(|(_, s)| {
            s.status == SubtaskStatus::Pending
                && s.dependencies.iter().all(|dep_id| {
                    subtasks
                        .iter()
                        .find(|t| t.id == *dep_id)
                        .map(|t| t.status == SubtaskStatus::Done)
                        .unwrap_or(false)
                })
        })
        .map(|(i, _)| i)
        .collect()
}

/// Find the next subtask that is ready to execute (all dependencies satisfied).
fn find_next_ready(subtasks: &[Subtask]) -> Option<usize> {
    find_all_ready(subtasks).into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magi::scheduler::Promotion;

    #[test]
    fn test_find_next_ready_no_dependencies() {
        let subtasks = vec![
            Subtask {
                id: 1,
                description: "Task A".into(),
                dependencies: vec![],
                status: SubtaskStatus::Pending,
            },
            Subtask {
                id: 2,
                description: "Task B".into(),
                dependencies: vec![],
                status: SubtaskStatus::Pending,
            },
        ];
        let ready = find_next_ready(&subtasks);
        assert!(ready.is_some());
        assert_eq!(ready.unwrap(), 0); // first pending
    }

    #[test]
    fn test_find_next_ready_with_dependencies() {
        let subtasks = vec![
            Subtask {
                id: 1,
                description: "Task A".into(),
                dependencies: vec![],
                status: SubtaskStatus::Done,
            },
            Subtask {
                id: 2,
                description: "Task B".into(),
                dependencies: vec![1],
                status: SubtaskStatus::Pending,
            },
            Subtask {
                id: 3,
                description: "Task C".into(),
                dependencies: vec![2],
                status: SubtaskStatus::Pending,
            },
        ];
        let ready = find_next_ready(&subtasks);
        assert!(ready.is_some());
        assert_eq!(subtasks[ready.unwrap()].id, 2);
    }

    #[test]
    fn test_find_next_ready_blocked() {
        let subtasks = vec![
            Subtask {
                id: 1,
                description: "Task A".into(),
                dependencies: vec![],
                status: SubtaskStatus::Failed("error".into()),
            },
            Subtask {
                id: 2,
                description: "Task B".into(),
                dependencies: vec![1],
                status: SubtaskStatus::Pending,
            },
        ];
        let ready = find_next_ready(&subtasks);
        assert!(ready.is_none());
    }

    #[test]
    fn test_find_next_ready_in_progress_skipped() {
        let subtasks = vec![
            Subtask {
                id: 1,
                description: "Task A".into(),
                dependencies: vec![],
                status: SubtaskStatus::InProgress,
            },
            Subtask {
                id: 2,
                description: "Task B".into(),
                dependencies: vec![],
                status: SubtaskStatus::Pending,
            },
        ];
        let ready = find_next_ready(&subtasks);
        assert!(ready.is_some());
        assert_eq!(subtasks[ready.unwrap()].id, 2);
    }

    #[test]
    fn test_compute_average_quality() {
        let rounds = vec![
            vec![MagiRound {
                round_number: 1,
                scrutiny: "x".into(),
                execution: "x".into(),
                promotion: Promotion {
                    quality_score: 80.0,
                    should_stop: true,
                    stop_reason: "done".into(),
                    next_round_focus: "None".into(),
                },
            }],
            vec![MagiRound {
                round_number: 1,
                scrutiny: "y".into(),
                execution: "y".into(),
                promotion: Promotion {
                    quality_score: 90.0,
                    should_stop: true,
                    stop_reason: "done".into(),
                    next_round_focus: "None".into(),
                },
            }],
        ];
        let avg = compute_average_quality(&rounds);
        assert!((avg - 85.0).abs() < 0.01);
    }

    #[test]
    fn test_compute_average_quality_empty() {
        let avg = compute_average_quality(&[]);
        assert!((avg - 0.0).abs() < f64::EPSILON);
    }
}
