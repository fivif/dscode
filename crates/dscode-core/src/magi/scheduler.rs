//! MagiScheduler — the spiral orchestrator.
//!
//! Runs Casper → Balthasar → Melchior rounds in sequence until the task
//! is complete or the round budget is exhausted.

use std::path::PathBuf;
use std::sync::Arc;

use crate::safety::guard::SafetyGuard;
use crate::safety::permission::PermissionHub;

use tracing::{info, warn};

use crate::providers::trait_def::{LlmProvider, ProviderError};
use crate::tools::registry::ToolRegistry;

use super::execute::{execute_subtask, MagiProgress};
use super::promote::promote;
use super::scrutinize::scrutinize;
use crate::agent::stream::StreamEvent;

/// Default maximum number of MAGI spiral rounds.
const DEFAULT_MAX_ROUNDS: u32 = 3;

/// Default maximum ReAct steps per Balthasar execution.
const DEFAULT_MAX_STEPS_PER_ROUND: u32 = 12;

/// Casper/Melchior single LLM call timeout.
const PHASE_LLM_TIMEOUT_SECS: u64 = 120;

/// Errors that can occur during a MAGI spiral.
#[derive(Debug, thiserror::Error)]
pub enum MagiError {
    /// The underlying LLM provider returned an error.
    #[error("provider error: {0}")]
    Provider(#[from] ProviderError),

    /// Failed to parse a structured response from the LLM.
    #[error("parse error: {0}")]
    Parse(String),

    /// The spiral exhausted its round budget without Melchior stopping.
    /// Wraps the max rounds reached and the completed rounds so callers
    /// can access partial results.
    #[error("max rounds ({0}) reached without completion")]
    MaxRounds(u32, Vec<MagiRound>),
}

/// A single round of the MAGI spiral.
#[derive(Debug, Clone)]
pub struct MagiRound {
    /// Monotonically increasing round number starting at 1.
    pub round_number: u32,
    /// Casper's scrutiny report — what was reviewed, what gaps were found.
    pub scrutiny: String,
    /// Balthasar's execution output — the combined text result from the agent loop.
    pub execution: String,
    /// Melchior's quality evaluation and continuation decision.
    pub promotion: Promotion,
}

/// Melchior's quality evaluation after a round completes.
#[derive(Debug, Clone)]
pub struct Promotion {
    /// Quality score from 0 to 100.
    pub quality_score: f64,
    /// Whether the spiral should stop (task is done).
    pub should_stop: bool,
    /// Human-readable reason for the stop/continue decision.
    pub stop_reason: String,
    /// What the next round should focus on.
    pub next_round_focus: String,
}

/// The MAGI spiral orchestrator.
///
/// Holds two LLM providers — the primary (powerful) model for Casper/Balthasar
/// reasoning and a cheaper runtime model for Melchior's evaluation pass.
///
/// Providers are stored behind `Arc<Box<...>>` so they can be shared with
/// callers like [`super::super::auto::runner::AutoRunner`] that need to
/// create multiple scheduler instances.
pub struct MagiScheduler {
    /// Primary LLM provider used by Casper and Balthasar.
    provider: Arc<Box<dyn LlmProvider>>,
    /// Cheaper LLM provider used by Melchior for evaluation.
    runtime_provider: Arc<Box<dyn LlmProvider>>,
    /// Shared tool registry for Balthasar's execution phase.
    tools: Arc<ToolRegistry>,
    /// Working directory for file-relative path resolution.
    working_dir: PathBuf,
    /// Maximum number of spiral rounds before forced stop.
    max_rounds: u32,
    /// Maximum ReAct steps per Balthasar execution round.
    max_steps_per_round: u32,
    /// Optional UI progress (Token + TeamAgentOutput + tools).
    progress: Option<MagiProgress>,
    /// Safety policy for Balthasar tools.
    safety_guard: Arc<SafetyGuard>,
    /// Optional permission hub (usually None in headless /auto — confirm → deny).
    permission_hub: Option<Arc<PermissionHub>>,
    permission_timeout_secs: u64,
}

impl MagiScheduler {
    /// Create a new MAGI scheduler.
    ///
    /// The providers are wrapped in `Arc` so they can be shared.
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
            max_rounds: DEFAULT_MAX_ROUNDS,
            max_steps_per_round: DEFAULT_MAX_STEPS_PER_ROUND,
            progress: None,
            safety_guard: Arc::new(SafetyGuard::new(&[], false)),
            permission_hub: None,
            permission_timeout_secs: 120,
        }
    }

    /// Create a new MAGI scheduler from already-Arc'd providers
    /// (allows sharing providers with other components like AutoRunner).
    pub fn from_arc_providers(
        provider: Arc<Box<dyn LlmProvider>>,
        runtime_provider: Arc<Box<dyn LlmProvider>>,
        tools: Arc<ToolRegistry>,
        working_dir: PathBuf,
    ) -> Self {
        Self {
            provider,
            runtime_provider,
            tools,
            working_dir,
            max_rounds: DEFAULT_MAX_ROUNDS,
            max_steps_per_round: DEFAULT_MAX_STEPS_PER_ROUND,
            progress: None,
            safety_guard: Arc::new(SafetyGuard::new(&[], false)),
            permission_hub: None,
            permission_timeout_secs: 120,
        }
    }

    pub fn with_safety_guard(mut self, guard: Arc<SafetyGuard>) -> Self {
        self.safety_guard = guard;
        self
    }

    pub fn with_permission_hub(mut self, hub: Option<Arc<PermissionHub>>) -> Self {
        self.permission_hub = hub;
        self
    }

    pub fn with_permission_timeout(mut self, secs: u64) -> Self {
        self.permission_timeout_secs = secs.max(10);
        self
    }

    /// Override the maximum number of spiral rounds.
    pub fn with_max_rounds(mut self, n: u32) -> Self {
        self.max_rounds = n;
        self
    }

    /// Override the maximum ReAct steps per execution round.
    pub fn with_max_steps_per_round(mut self, n: u32) -> Self {
        self.max_steps_per_round = n;
        self
    }

    /// Attach UI progress (same channel as AutoRunner / Forge).
    pub fn with_progress(mut self, progress: MagiProgress) -> Self {
        self.progress = Some(progress);
        self
    }

    fn emit_phase(&self, line: impl Into<String>) {
        let line = line.into();
        if let Some(ref p) = self.progress {
            let _ = p.tx.send(StreamEvent::TeamAgentOutput {
                agent_id: p.agent_id.clone(),
                content: format!("{line}\n"),
            });
            let _ = p.tx.send(StreamEvent::Token {
                content: format!("_{line}_\n"),
            });
            let _ = p.tx.send(StreamEvent::Thinking {
                content: format!("{line}\n"),
                step: 0,
            });
        }
    }

    /// Run the full MAGI spiral on a PRD (Product Requirements Document).
    ///
    /// Each round runs Casper → Balthasar → Melchior. The spiral stops when
    /// Melchior signals `should_stop` or when `max_rounds` is reached.
    ///
    /// # Returns
    /// A `Vec<MagiRound>` containing every completed round in order.
    pub async fn run_spiral(
        &self,
        prd: &str,
        session_id: &str,
    ) -> Result<Vec<MagiRound>, MagiError> {
        let mut rounds: Vec<MagiRound> = Vec::new();

        for round_num in 1..=self.max_rounds {
            info!(
                session = %session_id,
                round = round_num,
                max_rounds = self.max_rounds,
                "MAGI: starting round"
            );
            self.emit_phase(format!(
                "Auto round {round_num}/{} — reviewing…",
                self.max_rounds
            ));

            // ---- 1. Casper: scrutinize ----
            let previous_scrutiny = rounds.last().map(|r| r.scrutiny.as_str()).unwrap_or("");
            let scrutiny = retry_with_backoff(
                || async {
                    tokio::time::timeout(
                        std::time::Duration::from_secs(PHASE_LLM_TIMEOUT_SECS),
                        scrutinize(&**self.provider, prd, &rounds, previous_scrutiny),
                    )
                    .await
                    .map_err(|_| {
                        MagiError::Parse(format!(
                            "Casper timed out after {PHASE_LLM_TIMEOUT_SECS}s (round {round_num})"
                        ))
                    })?
                },
                2,
            )
            .await?;

            let scr_preview: String = scrutiny.chars().take(280).collect();
            self.emit_phase(format!("Review done — {scr_preview}…"));

            // ---- 2. Balthasar: execute ----
            self.emit_phase(format!(
                "Round {round_num} — executing (max {} steps)…",
                self.max_steps_per_round
            ));
            let progress = self.progress.clone();
            let safety = self.safety_guard.clone();
            let hub = self.permission_hub.clone();
            let pto = self.permission_timeout_secs;
            let execution = retry_with_backoff(
                || {
                    execute_subtask(
                        &**self.provider,
                        &self.tools,
                        &self.working_dir,
                        session_id,
                        prd,
                        &scrutiny,
                        self.max_steps_per_round,
                        progress.as_ref(),
                        safety.clone(),
                        hub.clone(),
                        pto,
                    )
                },
                2,
            )
            .await?;

            // ---- 3. Melchior: promote ----
            self.emit_phase(format!("Round {round_num} — evaluating quality…"));
            let promotion = retry_with_backoff(
                || async {
                    tokio::time::timeout(
                        std::time::Duration::from_secs(PHASE_LLM_TIMEOUT_SECS),
                        promote(&**self.runtime_provider, prd, &scrutiny, &execution),
                    )
                    .await
                    .map_err(|_| {
                        MagiError::Parse(format!(
                            "Melchior timed out after {PHASE_LLM_TIMEOUT_SECS}s (round {round_num})"
                        ))
                    })?
                },
                2,
            )
            .await?;

            info!(
                session = %session_id,
                round = round_num,
                quality = promotion.quality_score,
                should_stop = promotion.should_stop,
                "MAGI: round complete"
            );
            self.emit_phase(format!(
                "Round {round_num} complete — quality {:.0}/100, stop={} ({})",
                promotion.quality_score, promotion.should_stop, promotion.stop_reason
            ));

            rounds.push(MagiRound {
                round_number: round_num,
                scrutiny,
                execution,
                promotion: promotion.clone(),
            });

            if promotion.should_stop {
                info!(
                    session = %session_id,
                    total_rounds = round_num,
                    reason = %promotion.stop_reason,
                    "MAGI: spiral complete"
                );
                return Ok(rounds);
            }
        }

        warn!(
            session = %session_id,
            rounds = self.max_rounds,
            "MAGI: max rounds reached without completion"
        );
        Err(MagiError::MaxRounds(self.max_rounds, rounds))
    }
}

/// Retry an async operation with exponential backoff for transient errors.
async fn retry_with_backoff<T, E: std::fmt::Display, Fut: std::future::Future<Output = Result<T, E>>>(
    mut f: impl FnMut() -> Fut,
    max_retries: u32,
) -> Result<T, E> {
    let mut attempt = 0;
    loop {
        match f().await {
            Ok(val) => return Ok(val),
            Err(e) => {
                attempt += 1;
                if attempt > max_retries {
                    return Err(e);
                }
                let delay = std::time::Duration::from_secs(2u64.pow(attempt - 1));
                tracing::warn!("Retry {attempt}/{max_retries} after error: {e}. Waiting {delay:?}");
                tokio::time::sleep(delay).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use futures::stream::Stream;
    use std::pin::Pin;

    use crate::providers::trait_def::{ChatResponse, Message, ToolDef};
    use crate::tools::trait_def::{Tool, ToolContext, ToolResult};

    // ------------------------------------------------------------------
    // Stub providers
    // ------------------------------------------------------------------

    /// A provider that returns canned responses from a queue.
    struct StubProvider {
        responses: std::sync::Mutex<Vec<ChatResponse>>,
    }

    impl StubProvider {
        fn new(responses: Vec<ChatResponse>) -> Self {
            Self {
                responses: std::sync::Mutex::new(responses),
            }
        }
    }

    #[async_trait]
    impl LlmProvider for StubProvider {
        async fn chat(
            &self,
            _messages: Vec<Message>,
            _tools: Vec<ToolDef>,
        ) -> Result<ChatResponse, ProviderError> {
            let mut guard = self.responses.lock().unwrap();
            if guard.is_empty() {
                // Return a completion signal if queue is exhausted
                Ok(ChatResponse {
                    content: "STOP: true\nREASON: Done\nQUALITY: 95\nFOCUS: None".into(),
                    tool_calls: vec![],
                    usage: None,
                    reasoning_content: None, })
            } else {
                Ok(guard.remove(0))
            }
        }

        async fn chat_stream(
            &self,
            _messages: Vec<Message>,
            _tools: Vec<ToolDef>,
        ) -> Result<
            Pin<
                Box<
                    dyn Stream<Item = Result<crate::providers::trait_def::StreamChunk, ProviderError>>
                        + Send,
                >,
            >,
            ProviderError,
        > {
            unimplemented!("stub does not support streaming")
        }
    
    fn clone_box(&self) -> Box<dyn LlmProvider> { panic!("clone_box not used in tests") }
}

    // ------------------------------------------------------------------
    // Stub tool
    // ------------------------------------------------------------------

    struct EchoTool;

    #[async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str {
            "do_echo"
        }
        fn description(&self) -> &str {
            "Echoes input"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "message": { "type": "string" }
                },
                "required": ["message"]
            })
        }
        async fn execute(
            &self,
            args: serde_json::Value,
            _ctx: &ToolContext,
        ) -> Result<ToolResult, crate::tools::trait_def::ToolError> {
            let msg = args["message"].as_str().unwrap_or("no message");
            Ok(ToolResult::ok(format!("echo: {}", msg)))
        }
    }

    // ------------------------------------------------------------------
    // Tests
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn test_spiral_single_round_stop() {
        // Casper: scrutiny text (just returns some review)
        // Balthasar: execution (just returns a final answer)
        // Melchior: promotion with should_stop=true
        let responses = vec![
            // 1. Casper
            ChatResponse {
                content: "Casper review: looks good, proceed.".into(),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None, },
            // 2. Balthasar
            ChatResponse {
                content: "Balthasar executed: implemented main.rs".into(),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None, },
            // 3. Melchior (runtime provider)
            ChatResponse {
                content: "STOP: true\nREASON: All requirements met\nQUALITY: 95\nFOCUS: None".into(),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None, },
        ];

        let primary = Box::new(StubProvider::new(responses));
        let runtime = Box::new(StubProvider::new(vec![ChatResponse {
            content: "STOP: true\nREASON: All requirements met\nQUALITY: 95\nFOCUS: None".into(),
            tool_calls: vec![],
            usage: None,
            reasoning_content: None, }]));

        let mut tools = ToolRegistry::new();
        tools.register(EchoTool);

        let scheduler = MagiScheduler::new(
            primary,
            runtime,
            Arc::new(tools),
            PathBuf::from("/tmp"),
        )
        .with_max_rounds(3);

        let result = scheduler
            .run_spiral("Build a CLI tool", "test-session")
            .await;

        assert!(result.is_ok());
        let rounds = result.unwrap();
        assert_eq!(rounds.len(), 1);
        assert_eq!(rounds[0].round_number, 1);
        assert!(rounds[0].promotion.should_stop);
        assert!(rounds[0].promotion.quality_score > 90.0);
    }

    #[tokio::test]
    async fn test_spiral_max_rounds_exhausted() {
        // Provider always returns content that causes the spiral to continue
        // (Melchior never says stop)
        let responses: Vec<ChatResponse> = (0..30)
            .map(|_| ChatResponse {
                content: "CONTINUE".into(),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None, })
            .collect();

        let primary = Box::new(StubProvider::new(responses.clone()));
        let runtime = Box::new(StubProvider::new(responses));

        let mut tools = ToolRegistry::new();
        tools.register(EchoTool);

        let scheduler = MagiScheduler::new(
            primary,
            runtime,
            Arc::new(tools),
            PathBuf::from("/tmp"),
        )
        .with_max_rounds(2);

        let result = scheduler
            .run_spiral("Build everything", "test-session")
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            MagiError::MaxRounds(max, completed_rounds) => {
                assert_eq!(max, 2);
                assert!(!completed_rounds.is_empty()); // rounds should be preserved
            }
            e => panic!("expected MaxRounds(2, rounds), got {:?}", e),
        }
    }
}
