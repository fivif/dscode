//! AutoRunner — self-directed loop that decomposes tasks, runs MAGI spirals,
//! detects stalls, and re-decomposes until done or interrupted.

use std::path::PathBuf;
use std::sync::Arc;

use tracing::{info, warn};

#[allow(unused_imports)]
use crate::magi::scheduler::{MagiError, MagiRound, MagiScheduler, Promotion};
use crate::providers::trait_def::{LlmProvider, ProviderError};
use crate::tools::registry::ToolRegistry;

use super::decomposer::decompose_task;
use super::stall::StallDetector;

/// Default maximum number of re-decomposition cycles before giving up.
const DEFAULT_MAX_REDECOMPOSE_CYCLES: u32 = 5;

/// Default stall threshold — three consecutive rounds with no improvement.
const DEFAULT_STALL_ROUNDS: usize = 3;

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
            magi_max_rounds: 10,
            magi_max_steps: 30,
            max_recompose_cycles: DEFAULT_MAX_REDECOMPOSE_CYCLES,
            stall_rounds: DEFAULT_STALL_ROUNDS,
        }
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
        let mut subtasks = decompose_task(&**self.runtime_provider, prd).await?;

        if subtasks.is_empty() {
            warn!(session = %session_id, "AutoRunner: no subtasks produced");
            return Err(AutoError::NoSubtasks);
        }

        info!(
            session = %session_id,
            subtask_count = subtasks.len(),
            "AutoRunner: task decomposed into {} subtasks",
            subtasks.len()
        );

        let mut all_rounds: Vec<Vec<MagiRound>> = Vec::new();
        let mut stall_detector = StallDetector::new(self.stall_rounds);
        let mut recompose_count = 0u32;
        let mut consecutive_recompose_failures = 0u32;

        loop {
            // Find the next ready subtask
            let ready = find_next_ready(&subtasks);

            let Some(task_idx) = ready else {
                // Check if there's any work left
                let pending_count = subtasks
                    .iter()
                    .filter(|s| s.status == SubtaskStatus::Pending)
                    .count();

                if pending_count == 0 {
                    info!(session = %session_id, "AutoRunner: all subtasks resolved");
                    break;
                }

                // Remaining subtasks are blocked on failures
                warn!(
                    session = %session_id,
                    pending = pending_count,
                    "AutoRunner: all remaining subtasks are blocked on failed dependencies"
                );
                break;
            };

            // Extract task data before taking a mutable reference to subtasks.
            let (task_id, task_desc) = {
                let task = &subtasks[task_idx];
                (task.id, task.description.clone())
            };

            info!(
                session = %session_id,
                subtask_id = task_id,
                description = %task_desc,
                "AutoRunner: executing subtask"
            );

            subtasks[task_idx].status = SubtaskStatus::InProgress;

            // Create a MAGI scheduler for this subtask by cloning Arc'd providers
            let scheduler = MagiScheduler::from_arc_providers(
                Arc::clone(&self.provider),
                Arc::clone(&self.runtime_provider),
                Arc::clone(&self.tools),
                self.working_dir.clone(),
            )
            .with_max_rounds(self.magi_max_rounds)
            .with_max_steps_per_round(self.magi_max_steps);

            // Run the MAGI spiral
            let subtask_prd = format!(
                "Original PRD:\n{}\n\nSubtask:\n{}",
                prd, task_desc
            );

            let subtask_session = format!("{}-s{}", session_id, task_id);

            match scheduler.run_spiral(&subtask_prd, &subtask_session).await {
                Ok(rounds) => {
                    // Record the quality from the last round
                    let last_quality = rounds
                        .last()
                        .map(|r| r.promotion.quality_score)
                        .unwrap_or(0.0);

                    stall_detector.record(last_quality);

                    // Check if Melchior stopped naturally (should_stop)
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
                    } else {
                        // Low quality — mark as failed for now; re-decomposition may fix it
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
                    }

                    all_rounds.push(rounds);
                }
                Err(e) => {
                    subtasks[task_idx].status =
                        SubtaskStatus::Failed(format!("MAGI error: {}", e));
                    warn!(
                        session = %session_id,
                        subtask_id = task_id,
                        error = %e,
                        "AutoRunner: subtask failed"
                    );
                    // Push empty rounds for this failed subtask to maintain alignment
                    all_rounds.push(vec![]);
                    // Record a zero-quality data point for stall detection
                    stall_detector.record(0.0);
                }
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

        Ok(AutoRunResult {
            subtasks,
            rounds_per_subtask: all_rounds,
            total_quality,
            stalled,
        })
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

/// Find the next subtask that is ready to execute (all dependencies satisfied).
fn find_next_ready(subtasks: &[Subtask]) -> Option<usize> {
    subtasks.iter().position(|s| {
        if s.status != SubtaskStatus::Pending {
            return false;
        }
        s.dependencies.iter().all(|dep_id| {
            subtasks
                .iter()
                .any(|t| t.id == *dep_id && t.status == SubtaskStatus::Done)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
