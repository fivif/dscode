//! Task split + sub-agent spawn.
//!
//! The [`Dispatcher`] takes a task description, optionally uses an LLM to
//! decompose it into independent [`SubTask`]s, and spawns each sub-task as a
//! Tokio task with its own [`Forge`] instance.
//!
//! Results are collected via a [`tokio::sync::mpsc`] channel and can be
//! monitored through [`super::monitor::Monitor`] and merged with
//! [`super::orchestrator::Orchestrator`].

use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::agent::forge::Forge;
use crate::agent::stream::StreamEvent;
use crate::providers::trait_def::LlmProvider;
use crate::tools::registry::ToolRegistry;

/// A single sub-task assigned to one agent.
#[derive(Debug, Clone)]
pub struct SubTask {
    /// Human-readable task identifier.
    pub id: String,
    /// Task description / prompt for the sub-agent.
    pub prompt: String,
    /// Additional context injected into the sub-agent's system prompt.
    pub context: String,
}

/// A complete set of task assignments returned by the dispatcher.
#[derive(Debug, Clone)]
pub struct TaskAssignments {
    /// The original high-level task.
    pub task: String,
    /// Decomposed sub-tasks.
    pub sub_tasks: Vec<SubTask>,
    /// Instructions for merging results.
    pub merge_instructions: String,
}

/// Summary result from a single sub-agent run.
#[derive(Debug, Clone)]
pub struct SubAgentResult {
    /// The sub-task that was executed.
    pub sub_task: SubTask,
    /// The final text output from the sub-agent.
    pub output: String,
    /// Whether the sub-agent completed successfully.
    pub success: bool,
    /// Error message if the sub-agent failed.
    pub error: Option<String>,
}

/// The dispatcher that manages multi-agent task execution.
pub struct Dispatcher {
    /// Shared tool registry (cloned for each sub-agent).
    tools: Arc<ToolRegistry>,
    /// Working directory for all sub-agents.
    working_dir: PathBuf,
    /// Maximum iterations per sub-agent.
    max_iterations: u32,
}

impl Dispatcher {
    /// Create a new dispatcher.
    pub fn new(tools: Arc<ToolRegistry>, working_dir: PathBuf) -> Self {
        Self {
            tools,
            working_dir,
            max_iterations: 25,
        }
    }

    /// Set the maximum ReAct iterations per sub-agent (default 25).
    pub fn with_max_iterations(mut self, n: u32) -> Self {
        self.max_iterations = n;
        self
    }

    /// Decompose a task into subtasks using a lightweight heuristic.
    ///
    /// In a production system, this would call the LLM router.  Here we use a
    /// simple rule-based splitter that recognises common task structures.
    pub fn decompose_task(&self, task: &str) -> TaskAssignments {
        let sub_tasks = heuristic_split(task);

        let merge_instructions = if sub_tasks.len() > 1 {
            "Merge the sub-agent outputs by resolving conflicts: prefer the most \
             concrete answer, reconcile differences, and produce a unified final response."
                .to_string()
        } else {
            "No merging needed — single sub-task.".to_string()
        };

        TaskAssignments {
            task: task.to_string(),
            sub_tasks,
            merge_instructions,
        }
    }

    /// Spawn sub-agents for a set of task assignments and collect results.
    ///
    /// Each sub-agent gets its own `Forge` instance and runs in a separate
    /// Tokio task.  Results are collected via the returned channel receiver.
    pub async fn dispatch(
        &self,
        assignments: &TaskAssignments,
        session_id: &str,
        provider_factory: impl Fn() -> Box<dyn LlmProvider> + Send + Sync + 'static,
    ) -> Vec<SubAgentResult> {
        if assignments.sub_tasks.is_empty() {
            warn!("Dispatcher: no sub-tasks to dispatch");
            return vec![];
        }

        let task_count = assignments.sub_tasks.len();
        info!(count = task_count, "Dispatcher: spawning sub-agents");

        let (tx, mut rx) = mpsc::channel::<SubAgentResult>(task_count);

        for sub_task in &assignments.sub_tasks {
            let sub_task = sub_task.clone();
            let tools = self.tools.clone();
            let working_dir = self.working_dir.clone();
            let max_iterations = self.max_iterations;
            let session_id = session_id.to_string();
            let tx = tx.clone();
            let provider = provider_factory();

            tokio::spawn(async move {
                let result = run_sub_agent(
                    provider,
                    tools,
                    working_dir,
                    &session_id,
                    &sub_task,
                    max_iterations,
                )
                .await;

                if tx.send(result).await.is_err() {
                    debug!("Dispatcher: result channel closed");
                }
            });
        }

        // Drop the sender so the receiver closes when all tasks finish.
        drop(tx);

        let mut results = Vec::with_capacity(task_count);
        while let Some(result) = rx.recv().await {
            results.push(result);
        }

        info!(received = results.len(), expected = task_count, "Dispatcher: all sub-agents finished");
        results
    }
}

// ── Heuristic task splitting ────────────────────────────────────────────────

/// Split a task string into subtasks using simple heuristics.
///
/// Recognizes:
/// - Numbered lists ("1. ... 2. ...")
/// - Bullet points ("- ..." or "* ...")
/// - Semicolons / "and" as separators
/// - Double newlines as paragraph breaks
fn heuristic_split(task: &str) -> Vec<SubTask> {
    // Try numbered list first.
    let numbered = split_numbered(task);
    if numbered.len() > 1 {
        return numbered;
    }

    // Try bullet points.
    let bullets = split_bullets(task);
    if bullets.len() > 1 {
        return bullets;
    }

    // Try splitting by semicolons.
    let parts: Vec<&str> = task.split(';').map(str::trim).filter(|s| !s.is_empty()).collect();
    if parts.len() > 1 {
        return parts
            .into_iter()
            .enumerate()
            .map(|(i, p)| SubTask {
                id: format!("subtask-{}", i),
                prompt: p.to_string(),
                context: String::new(),
            })
            .collect();
    }

    // Try splitting by double newlines (paragraphs).
    let paras: Vec<&str> = task.split("\n\n").map(str::trim).filter(|s| !s.is_empty()).collect();
    if paras.len() > 1 {
        return paras
            .into_iter()
            .enumerate()
            .map(|(i, p)| SubTask {
                id: format!("subtask-{}", i),
                prompt: p.to_string(),
                context: String::new(),
            })
            .collect();
    }

    // Fallback: single subtask.
    vec![SubTask {
        id: "subtask-0".to_string(),
        prompt: task.to_string(),
        context: String::new(),
    }]
}

fn split_numbered(task: &str) -> Vec<SubTask> {
    let re = regex::Regex::new(r"(?m)^\d+[\.\)]\s+").unwrap();
    let splits: Vec<&str> = re.split(task).collect();

    if splits.len() <= 2 {
        // First element before any number is empty or preamble.
        return vec![];
    }

    splits
        .into_iter()
        .skip(1) // skip preamble
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .enumerate()
        .map(|(i, s)| SubTask {
            id: format!("subtask-{}", i),
            prompt: s.to_string(),
            context: String::new(),
        })
        .collect()
}

fn split_bullets(task: &str) -> Vec<SubTask> {
    let re = regex::Regex::new(r"(?m)^[-*]\s+").unwrap();
    let splits: Vec<&str> = re.split(task).collect();

    if splits.len() <= 2 {
        return vec![];
    }

    splits
        .into_iter()
        .skip(1)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .enumerate()
        .map(|(i, s)| SubTask {
            id: format!("subtask-{}", i),
            prompt: s.to_string(),
            context: String::new(),
        })
        .collect()
}

// ── Sub-agent execution ─────────────────────────────────────────────────────

/// Run a single sub-agent with its own Forge instance.
async fn run_sub_agent(
    provider: Box<dyn LlmProvider>,
    tools: Arc<ToolRegistry>,
    working_dir: PathBuf,
    session_id: &str,
    sub_task: &SubTask,
    max_iterations: u32,
) -> SubAgentResult {
    let system_prompt = format!(
        "You are a specialized sub-agent working on a specific task.\n\
         Context: {}\n\
         Complete your assigned task and return a clear, concise answer.",
        sub_task.context
    );

    let forge = Forge::new(provider, tools, working_dir)
        .with_system_prompt(system_prompt)
        .with_max_iterations(max_iterations);

    let (event_tx, mut event_rx) = mpsc::unbounded_channel();

    let result = forge
        .execute(&sub_task.prompt, session_id, vec![], event_tx)
        .await;

    // Wait briefly to allow any in-flight events to land in the channel
    // before draining. `try_recv()` is non-blocking and may miss events
    // that were sent just before the `execute()` future resolved but
    // haven't been buffered yet.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Collect text output from events.
    let mut output = String::new();
    while let Ok(event) = event_rx.try_recv() {
        if let StreamEvent::Token { content } = event {
            output.push_str(&content);
        }
    }

    match result {
        Ok(()) => SubAgentResult {
            sub_task: sub_task.clone(),
            output,
            success: true,
            error: None,
        },
        Err(e) => {
            error!(
                sub_task = %sub_task.id,
                error = %e,
                "Sub-agent failed"
            );
            SubAgentResult {
                sub_task: sub_task.clone(),
                output,
                success: false,
                error: Some(e.to_string()),
            }
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::forge::Forge;
    use crate::tools::registry::ToolRegistry;

    #[test]
    fn test_heuristic_split_numbered() {
        let task = "1. Write function A\n2. Write function B\n3. Test everything";
        let subs = heuristic_split(task);
        assert_eq!(subs.len(), 3);
        assert_eq!(subs[0].prompt, "Write function A");
        assert_eq!(subs[1].prompt, "Write function B");
        assert_eq!(subs[2].prompt, "Test everything");
    }

    #[test]
    fn test_heuristic_split_bullets() {
        let task = "- Install dependencies\n- Build the project\n- Run tests";
        let subs = heuristic_split(task);
        assert_eq!(subs.len(), 3);
    }

    #[test]
    fn test_heuristic_split_semicolons() {
        let task = "Implement login; add error handling; write unit tests";
        let subs = heuristic_split(task);
        assert_eq!(subs.len(), 3);
    }

    #[test]
    fn test_heuristic_split_paragraphs() {
        let task = "First paragraph.\n\nSecond paragraph.\n\nThird paragraph.";
        let subs = heuristic_split(task);
        assert_eq!(subs.len(), 3);
    }

    #[test]
    fn test_heuristic_split_single() {
        let task = "Write a single function to compute the GCD.";
        let subs = heuristic_split(task);
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].prompt, task);
    }

    #[test]
    fn test_dispatcher_decompose() {
        let tools = Arc::new(ToolRegistry::new());
        let dispatcher = Dispatcher::new(tools, PathBuf::from("/tmp"));

        let assignments = dispatcher.decompose_task(
            "1. Implement user model\n2. Create database migration\n3. Add API endpoints",
        );
        assert_eq!(assignments.sub_tasks.len(), 3);
        assert!(!assignments.merge_instructions.is_empty());
    }
}
