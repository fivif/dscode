//! Forge — the ReAct agent loop.
//!
//! The Forge is the heart of DS Code. It takes a user message, builds the
//! LLM context (system prompt + history + tool definitions), and enters a
//! reasoning-and-acting loop: call the model, parse its response, execute
//! any requested tools, feed the results back, and repeat until the
//! assistant produces a final answer or the iteration budget is exhausted.
//!
//! All progress is reported as a stream of [`StreamEvent`] values via a
//! Tokio unbounded channel, so UIs can render tokens, tool status, and
//! thinking content in real time.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use super::context::{build_context, compression_prompt, count_message_tokens, ContextPacket};
use super::error_withholding::ErrorWithholder;
use super::stream::{StreamEvent, ToolStatus};
use crate::auto::runner::AutoRunner;
use crate::config::settings::ContextConfig;
use crate::extensions::skills::SkillLoader;
use crate::plan::active::{format_question, plan_question_event, ActivePlanSession, PlanTurnResult};
use crate::providers::trait_def::{
    ChatResponse, LlmProvider, Message, MessageContent, ProviderError, Role, ToolCall, ToolDef,
};
use futures::StreamExt;
use crate::tools::registry::ToolRegistry;
use crate::safety::guard::SafetyGuard;
use crate::safety::permission::PermissionHub;
use crate::tools::trait_def::{ToolContext, ToolError};

/// The default system prompt injected at the start of every conversation.
pub const DEFAULT_SYSTEM_PROMPT: &str = r#"You are a coding agent with tools: shell, files, background tasks, web fetch/search, skills, and optional MCP tools.

Background vs foreground shell:
- do_bash — short commands that must finish before you continue (ls, git, tests, builds that exit).
- do_background — long-running processes that must NOT block: vite, npm run dev, next dev, cargo watch, docker compose up, servers.
  Returns a task_id immediately. Use do_task_status(task_id) for logs, do_task_kill(task_id) to stop.
  NEVER run dev servers with do_bash (it will hang "running" forever). NEVER use `cmd &` inside do_bash for servers.

Web (built-in, no API key):
- do_web_search — public web search (title / URL / snippet). Use when you need links and have no URL yet.
- do_web_fetch — GET public page(s) as text. Pass `url` or concurrent `urls` (up to 4).
  HTML results include **Candidate links** ranked for same-site/docs — for the user task, deep-fetch
  the best related links (official docs, API ref, next chapter). Prefer better known official URLs
  over random mirrors when you know them. Optional use_proxy. Never fetch localhost / private IPs.

Skills (Agent Skills / skills.sh ecosystem):
- Local packages live under ~/.dscode/skills (also reads ~/.claude/skills, ~/.agents/skills, project .claude/skills).
- Matching skills auto-activate from the user message (triggers / name).
- do_skill_list — see installed skills + scripts.
- do_skill_install — install third-party packages from GitHub (e.g. vercel-labs/agent-skills, mattpocock/skills/grill-me). Catalog: https://www.skills.sh/
- Only install when the user asks, or when a missing capability clearly blocks the task — then state what you will install and why.
- Bundled scripts under a skill should be run via do_bash with the absolute path shown when the skill activates.

MCP (Model Context Protocol):
- Tools named `mcp_<server>_<tool>` come from configured MCP servers (Settings → MCP).
- Prefer MCP tools when they match the task (docs lookup, browser, external APIs, etc.).
- If an MCP tool is listed in your available tools, you can and should call it — it is already connected.

Think step by step, use tools when needed, write clean code."#;

/// Maximum number of ReAct iterations before the agent stops (configurable).
const DEFAULT_MAX_ITERATIONS: u32 = 120;

/// Maximum number of historical messages to include in the context window.
const DEFAULT_MAX_HISTORY_MESSAGES: usize = 100;

/// Cap tool result text injected back into the conversation (chars).
const MAX_TOOL_RESULT_CHARS: usize = 24_000;

/// Start tool-loop detection after this many ReAct turns.
const LOOP_DETECT_FROM_ITERATION: u32 = 5;

/// Errors that can occur during the agent loop.
#[derive(Debug, thiserror::Error)]
pub enum ForgeError {
    /// The underlying LLM provider returned an error.
    #[error("provider error: {0}")]
    Provider(#[from] ProviderError),

    /// A tool failed during execution.
    #[error("tool error: {0}")]
    Tool(#[from] ToolError),

    /// The agent reached the iteration limit without producing a final answer.
    #[error("max iterations ({0}) reached without a final response")]
    MaxIterations(u32),

    /// The model returned no content and no tool calls (empty response).
    #[error("model returned an empty response (no content, no tool calls)")]
    EmptyResponse,

    /// Cancelled by user / team control plane.
    #[error("cancelled")]
    Cancelled,
}

/// The ReAct agent loop — the central execution engine of DS Code.
///
/// # Example
///
/// ```no_run
/// use std::sync::Arc;
/// use dscode_core::agent::forge::Forge;
/// use dscode_core::providers::openai::OpenAiProvider;
/// use dscode_core::tools::registry::ToolRegistry;
///
/// # async fn example() {
/// let provider = Box::new(OpenAiProvider::new(
///     "sk-...".into(),
///     "https://api.deepseek.com/v1".into(),
///     "deepseek-v4-pro".into(),
/// ));
/// let mut tools = ToolRegistry::new();
/// tools.register_default_tools();
///
/// let forge = Forge::new(
///     provider,
///     Arc::new(tools),
///     "/home/user/project".into(),
/// );
/// # }
/// ```
pub struct Forge {
    /// The LLM provider backend (OpenAI, Anthropic, DeepSeek, etc.).
    provider: Box<dyn LlmProvider>,

    /// The shared tool registry.
    tools: Arc<ToolRegistry>,

    /// Maximum number of ReAct iterations per execution.
    max_iterations: u32,

    /// The working directory for relative path resolution.
    working_dir: PathBuf,

    /// The system prompt injected at the start of every conversation.
    system_prompt: String,

    /// Maximum number of history messages to include in each context window.
    max_history_messages: usize,

    /// Context window configuration.
    context_config: ContextConfig,

    /// Whether compression has been applied in this session.
    compressed: AtomicBool,

    /// Whether /teams multi-agent mode is active.
    teams_mode: AtomicBool,

    /// Safety policy for tools (from config).
    safety_guard: Arc<SafetyGuard>,

    /// Optional GUI permission hub for Confirm-level commands.
    permission_hub: Option<Arc<PermissionHub>>,

    /// Permission prompt timeout seconds.
    permission_timeout_secs: u64,

    /// Teams v2 configuration.
    teams_config: crate::teams::config::TeamsConfig,

    /// Optional cancel token (checked each ReAct iteration).
    cancel_token: Option<CancellationToken>,

    /// Optional nudge queue — drained as user messages mid-loop.
    nudge_queue: Option<Arc<AsyncMutex<Vec<String>>>>,
}

impl Forge {
    /// Create a new Forge with the given provider, tool registry, and working
    /// directory. Uses the default system prompt and iteration limit.
    pub fn new(
        provider: Box<dyn LlmProvider>,
        tools: Arc<ToolRegistry>,
        working_dir: PathBuf,
    ) -> Self {
        Self {
            provider,
            tools,
            max_iterations: DEFAULT_MAX_ITERATIONS,
            working_dir,
            system_prompt: DEFAULT_SYSTEM_PROMPT.to_string(),
            max_history_messages: DEFAULT_MAX_HISTORY_MESSAGES,
            context_config: ContextConfig::default(),
            compressed: AtomicBool::new(false),
            teams_mode: AtomicBool::new(false),
            // Safe defaults: no write-outside, no absolute trust
            safety_guard: Arc::new(SafetyGuard::new(&[], false)),
            permission_hub: None,
            permission_timeout_secs: 120,
            teams_config: crate::teams::config::TeamsConfig::default(),
            cancel_token: None,
            nudge_queue: None,
        }
    }

    pub fn with_safety_guard(mut self, guard: Arc<SafetyGuard>) -> Self {
        self.safety_guard = guard;
        self
    }

    pub fn with_teams_config(mut self, cfg: crate::teams::config::TeamsConfig) -> Self {
        self.teams_config = cfg;
        self
    }

    /// Cooperative cancel for sub-agents / session abort.
    pub fn with_cancel_token(mut self, token: CancellationToken) -> Self {
        self.cancel_token = Some(token);
        self
    }

    /// Mid-run instruction queue (teams nudge).
    pub fn with_nudge_queue(mut self, q: Arc<AsyncMutex<Vec<String>>>) -> Self {
        self.nudge_queue = Some(q);
        self
    }

    pub fn with_permission_hub(mut self, hub: Arc<PermissionHub>) -> Self {
        self.permission_hub = Some(hub);
        self
    }

    pub fn with_permission_timeout(mut self, secs: u64) -> Self {
        self.permission_timeout_secs = secs.max(10);
        self
    }

    /// Override the system prompt (default: [`DEFAULT_SYSTEM_PROMPT`]).
    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = prompt.into();
        self
    }

    /// Override the maximum number of ReAct iterations.
    pub fn with_max_iterations(mut self, n: u32) -> Self {
        self.max_iterations = n;
        self
    }

    /// Override the maximum number of history messages to include in context.
    pub fn with_max_history_messages(mut self, n: usize) -> Self {
        self.max_history_messages = n;
        self
    }

    /// Set whether /teams multi-agent mode is active.
    pub fn with_teams_mode(self, on: bool) -> Self {
        self.teams_mode.store(on, Ordering::Relaxed);
        self
    }

    /// Override the context window configuration (also applies max_agent_iterations).
    pub fn with_context_config(mut self, cfg: ContextConfig) -> Self {
        if cfg.max_agent_iterations > 0 {
            self.max_iterations = cfg.max_agent_iterations;
        }
        self.context_config = cfg;
        self
    }

    /// Execute a user message and emit streaming events.
    ///
    /// # Arguments
    /// * `user_message` — the current user input to process.
    /// * `session_id`   — the active session identifier (passed into tool context).
    /// * `history`      — previous conversation messages (before this turn).
    /// * `event_tx`     — channel on which to emit [`StreamEvent`] values for the UI.
    ///
    /// # Flow
    /// 1. Builds the initial context (system prompt + history + tool defs).
    /// 2. Appends the user message.
    /// 3. Enters the ReAct loop (up to `max_iterations` times):
    ///    a. Calls the LLM provider.
    ///    b. Emits thinking content (DeepSeek reasoning) if present.
    ///    c. Emits token content as markdown text.
    ///    d. If the assistant requested tool calls, executes each and
    ///       appends the results to the conversation, then loops.
    ///    e. If the assistant produced a final answer, emits `Complete` and
    ///       returns.
    pub async fn execute(
        &self,
        user_message: &str,
        session_id: &str,
        history: Vec<Message>,
        event_tx: tokio::sync::mpsc::UnboundedSender<StreamEvent>,
    ) -> Result<(), ForgeError> {
        // F4: Reset compression flag for each new execution so re-use works.
        self.compressed.store(false, Ordering::Relaxed);

        let trimmed = user_message.trim();

        // ── Plan cancel ──
        if trimmed.eq_ignore_ascii_case("/plan cancel")
            || trimmed.eq_ignore_ascii_case("/cancel")
        {
            if ActivePlanSession::is_active(session_id) {
                ActivePlanSession::clear(session_id);
                let _ = event_tx.send(StreamEvent::Token {
                    content: "Plan interview cancelled.\n".into(),
                });
            } else {
                let _ = event_tx.send(StreamEvent::Token {
                    content: "No active plan interview.\n".into(),
                });
            }
            let _ = event_tx.send(StreamEvent::Complete { usage: None });
            return Ok(());
        }

        // ── Active multi-turn /plan interview (user answers) ──
        if ActivePlanSession::is_active(session_id)
            && !trimmed.starts_with("/plan")
            && !trimmed.starts_with("/auto")
            && !trimmed.starts_with("/teams")
        {
            return self
                .continue_plan_interview(session_id, trimmed, &event_tx)
                .await;
        }

        // ── /plan start ──
        if trimmed.starts_with("/plan") {
            let goal = trimmed
                .trim_start_matches("/plan")
                .trim()
                .trim_start_matches(':')
                .trim();
            return self
                .start_plan_interview(session_id, goal, &event_tx)
                .await;
        }

        // ── /auto MAGI spiral ──
        if trimmed.starts_with("/auto") {
            let task = trimmed
                .trim_start_matches("/auto")
                .trim()
                .trim_start_matches(':')
                .trim();
            let task = if task.is_empty() {
                // Fall back to last user message in history or require explicit task
                history
                    .iter()
                    .rev()
                    .find(|m| m.role == Role::User)
                    .and_then(|m| m.content.as_text().map(|s| s.to_string()))
                    .unwrap_or_default()
            } else {
                task.to_string()
            };
            if task.is_empty() {
                let _ = event_tx.send(StreamEvent::Token {
                    content: "Usage: `/auto <task or PRD>` — runs auto spiral until done.\n".into(),
                });
                let _ = event_tx.send(StreamEvent::Complete { usage: None });
                return Ok(());
            }
            return self.run_auto_mode(session_id, &task, &event_tx).await;
        }

        // --- Detect teams toggle ---
        let mode_prompt = if trimmed.eq_ignore_ascii_case("/teams")
            || trimmed.eq_ignore_ascii_case("/teams on")
        {
            self.teams_mode.store(true, Ordering::Relaxed);
            "\n\nTeams mode ON. Every message will be executed by concurrent sub-agents.\nType /teams off to disable."
        } else if trimmed.eq_ignore_ascii_case("/teams off")
            || trimmed.eq_ignore_ascii_case("/teams stop")
        {
            self.teams_mode.store(false, Ordering::Relaxed);
            "\n\nTeams mode OFF. Back to single-agent operation."
        } else {
            ""
        };

        // --- Build the enriched system prompt ---
        let enriched_system = format!(
            "{}{}\n\nCurrent working directory: {}",
            self.system_prompt,
            mode_prompt,
            self.working_dir.display()
        );

        // --- Prepare tool definitions once (immutable across iterations) ---
        let tool_defs = self.tools.to_openai_tools();

        // --- Check for matching skills (multi-path: dscode + claude + agents + project) ---
        let mut skill_prompt = String::new();
        let mut allowed_tool_patterns: Vec<String> = vec![];
        let mut loader = SkillLoader::new();
        let extra_dirs: Vec<std::path::PathBuf> = crate::config::settings::Config::load()
            .ok()
            .map(|c| {
                c.extensions
                    .skills_dirs
                    .iter()
                    .map(std::path::PathBuf::from)
                    .collect()
            })
            .unwrap_or_default();
        if let Ok(count) = loader.load_all(&extra_dirs, Some(&self.working_dir)) {
            if count > 0 {
                let matches = loader.find_matching(user_message);
                for skill in matches {
                    skill_prompt.push_str(&skill.to_agent_prompt());
                    skill_prompt.push('\n');
                    allowed_tool_patterns.extend(skill.allowed_tools.clone());
                    info!(
                        session = %session_id,
                        skill = %skill.name,
                        scripts = skill.resources.iter().filter(|r| matches!(r.kind, crate::extensions::skills::SkillResourceKind::Script)).count(),
                        "skill activated"
                    );
                }
            }
        }

        // --- Build initial context ---
        let enriched_with_skill = if skill_prompt.is_empty() {
            enriched_system
        } else {
            format!("{}\n{}\n---\nFollow the above skill instructions when applicable.", enriched_system, skill_prompt)
        };
        let ContextPacket { mut messages, tools } = build_context(
            &history,
            &enriched_with_skill,
            &tool_defs,
            self.max_history_messages,
        );

        // --- Append the current user message ---
        messages.push(Message {
            role: Role::User,
            content: MessageContent::Text(user_message.to_string()),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None, created_at: 0 });

        info!(
            session = %session_id,
            history_msgs = history.len(),
            prompt_len = user_message.len(),
            tool_count = tools.len(),
            "Forge: starting ReAct loop"
        );

        // F5: Track message count at start of this execute() so stall
        // detection only examines messages added during the current run.
        let initial_msg_count = messages.len();

        // F2: Sliding window of tool-call sets (order-independent) from the
        // last 5 iterations for alternating-pattern stall detection.
        let mut recent_tool_sets: std::collections::VecDeque<std::collections::BTreeSet<String>> =
            std::collections::VecDeque::new();

        // Transient provider errors + empty model responses: retry with backoff.
        let mut withholder = ErrorWithholder::new();

        // =================================================================
        // Teams Mode — if enabled, dispatch via run_teams_task
        // =================================================================
        let is_toggle = user_message.trim().eq_ignore_ascii_case("/teams")
            || user_message.trim().eq_ignore_ascii_case("/teams on")
            || user_message.trim().eq_ignore_ascii_case("/teams off")
            || user_message.trim().eq_ignore_ascii_case("/teams stop");
        if self.teams_mode.load(Ordering::Relaxed) && !is_toggle {
            return self.run_teams_task(user_message.trim(), session_id, history, event_tx).await;
        }

        // =================================================================
        // ReAct Loop
        // =================================================================
        for iteration in 1..=self.max_iterations {
            // Cooperative cancel (teams stop / session abort)
            if self
                .cancel_token
                .as_ref()
                .map(|t| t.is_cancelled())
                .unwrap_or(false)
            {
                info!(session = %session_id, iteration, "Forge: cancelled");
                let _ = event_tx.send(StreamEvent::Error {
                    content: "Agent cancelled.".into(),
                });
                return Err(ForgeError::Cancelled);
            }

            // Drain team nudges into the conversation
            if let Some(ref q) = self.nudge_queue {
                let mut g = q.lock().await;
                if !g.is_empty() {
                    let notes: Vec<String> = g.drain(..).collect();
                    drop(g);
                    let joined = notes.join("\n");
                    let note = format!(
                        "(Coordinator nudge — follow this additional instruction now):\n{joined}"
                    );
                    let _ = event_tx.send(StreamEvent::Token {
                        content: format!("\n_📩 Nudge:_ {joined}\n"),
                    });
                    messages.push(Message {
                        role: Role::User,
                        content: MessageContent::Text(note),
                        name: None,
                        tool_calls: None,
                        tool_call_id: None,
                        reasoning_content: None,
                        created_at: 0,
                    });
                }
            }

            debug!(
                session = %session_id,
                iteration,
                msg_count = messages.len(),
                "Forge: calling provider"
            );

            // Stall detection — sliding window of tool-call sets (from early on).
            if iteration >= LOOP_DETECT_FROM_ITERATION {
                // Only scan messages added during this execute() call (F5).
                let run_messages = &messages[initial_msg_count..];
                // Last assistant tool-call set (most recent turn)
                let current_set: std::collections::BTreeSet<String> = run_messages
                    .iter()
                    .rev()
                    .find_map(|m| m.tool_calls.as_ref())
                    .map(|tc| tc.iter().map(|t| t.function.name.clone()).collect())
                    .unwrap_or_default();
                if !current_set.is_empty() {
                    if recent_tool_sets.len() >= 5 {
                        recent_tool_sets.pop_front();
                    }
                    recent_tool_sets.push_back(current_set.clone());
                    let mut counts: std::collections::HashMap<
                        &std::collections::BTreeSet<String>,
                        usize,
                    > = std::collections::HashMap::new();
                    for s in recent_tool_sets.iter() {
                        *counts.entry(s).or_insert(0) += 1;
                    }
                    // ≥3 appearances of the same tool-set in the last 5 turns
                    if counts.values().any(|&c| c >= 3) {
                        let repeated: Vec<String> = current_set.iter().cloned().collect();
                        let _ = event_tx.send(StreamEvent::Token {
                            content: format!(
                                "\n\n**Tool loop detected** (iteration {iteration}): \
                                 repeated tools `{}`. Stop re-running the same tools; \
                                 consolidate results and give a final answer now.\n",
                                repeated.join(", ")
                            ),
                        });
                        messages.push(Message {
                            role: Role::User,
                            content: MessageContent::Text(
                                "(System: tool loop detected. Do NOT call the same tools again. \
                                 Summarize what you have and finish with a concrete answer.)"
                                    .into(),
                            ),
                            name: None,
                            tool_calls: None,
                            tool_call_id: None,
                            reasoning_content: None,
                            created_at: 0,
                        });
                        // Clear window so we don't re-nudge every turn
                        recent_tool_sets.clear();
                    }
                }
            }

            // (a.0) Check if context compression is needed
            if !self.compressed.load(Ordering::Relaxed) {
                // F7: Pass reference slices instead of cloning.
                let sys_refs: Vec<&Message> = messages.iter().filter(|m| m.role == Role::System).collect();
                let sys_tok = count_message_tokens(&sys_refs);
                let hist_refs: Vec<&Message> = messages.iter().filter(|m| m.role != Role::System).collect();
                let hist_tok = count_message_tokens(&hist_refs);
                let threshold = (self.context_config.window_tokens as f64 * self.context_config.compress_threshold) as u64;
                if sys_tok + hist_tok > threshold {
                    info!(session = %session_id, iteration, sys_tok, hist_tok, threshold, "compression");
                    // Build compression prompt — ensure we don't split tool chains
                    let non_sys: Vec<_> = messages.iter().filter(|m| m.role != Role::System).enumerate().collect::<Vec<_>>();
                    let mut compress_count = (non_sys.len() as f64 * 0.7) as usize;
                    // Align: skip forward past incomplete tool_call→tool_result pairs
                    while compress_count < non_sys.len() {
                        let (_, msg) = &non_sys[compress_count];
                        if msg.role == Role::Tool { compress_count += 1; }
                        else { break; }
                    }
                    if compress_count > 0 {
                        let old: Vec<_> = non_sys.iter().take(compress_count).map(|(_, m)| (*m).clone()).collect();
                        // C3: Limit compression prompt to half the context window.
                        let half_window = (self.context_config.window_tokens / 2) as u64;
                        let prompt = compression_prompt(&old, half_window);
                        let summary = self.provider.chat(
                            vec![Message { role: Role::User, content: MessageContent::Text(prompt), ..Default::default() }],
                            vec![],
                        ).await.map(|r| r.content).unwrap_or_default();
                        if !summary.is_empty() {
                            let sys = messages.iter().find(|m| m.role == Role::System).and_then(|m| m.content.as_text()).unwrap_or("").to_string();
                            let rest: Vec<_> = messages.iter().filter(|m| m.role != Role::System).skip(compress_count).cloned().collect();
                            // Re-anchor critical constraints after compression (anti dilution)
                            let anchors = format!(
                                "\n\n## Active constraints (re-asserted after compression)\n\
                                 - Working directory: {}\n\
                                 - Prefer tools over speculation; do not invent file paths.\n\
                                 - Follow user global instructions and language preferences from the system prompt.\n\
                                 - Do not re-run failed tool loops; consolidate and conclude when stuck.\n",
                                self.working_dir.display()
                            );
                            messages = vec![Message {
                                role: Role::System,
                                content: MessageContent::Text(format!(
                                    "{sys}\n\n## Conversation Summary\n{summary}{anchors}"
                                )),
                                ..Default::default()
                            }];
                            messages.extend(rest);
                            self.compressed.store(true, Ordering::Relaxed);
                        }
                    }
                }
            }

            // F10: Clean orphaned tool_calls on the original vec so the
            // fix persists across iterations (new messages appended to original).
            clean_orphaned_tool_calls(&mut messages);

            // (a) Call the LLM provider — SSE stream first, fall back to chat()
            let snapshot = messages.clone();
            let validated = validate_tool_chain_for_provider(snapshot);

            let response = match stream_provider_turn(
                &*self.provider,
                validated,
                tools.clone(),
                &event_tx,
                iteration,
            )
            .await
            {
                Ok(r) => r,
                Err(e) => {
                    match withholder.tolerate(e) {
                        Ok(()) => {
                            let attempt = withholder.attempts_used();
                            let max = withholder.max_attempts;
                            let delay = withholder.current_backoff();
                            warn!(
                                session = %session_id,
                                iteration,
                                attempt,
                                max,
                                delay_ms = delay.as_millis() as u64,
                                "Forge: transient provider error — retrying"
                            );
                            let _ = event_tx.send(StreamEvent::Token {
                                content: format!(
                                    "\n_Provider transient error, retrying ({attempt}/{max})…_\n"
                                ),
                            });
                            withholder.sleep_backoff().await;
                            continue;
                        }
                        Err(e) => {
                            error!(session = %session_id, iteration, %e, "provider error");
                            let _ = event_tx.send(StreamEvent::Error {
                                content: format!("Provider error: {}", e),
                            });
                            return Err(ForgeError::Provider(e));
                        }
                    }
                }
            };

            let has_tool_calls = !response.tool_calls.is_empty();
            let has_content = !response.content.trim().is_empty();
            // Reasoning-only turns with no text/tools still count as empty for completion.
            let has_reasoning_only = !has_content
                && !has_tool_calls
                && response
                    .reasoning_content
                    .as_ref()
                    .map(|s| !s.trim().is_empty())
                    .unwrap_or(false);

            let assistant_msg = Message {
                role: Role::Assistant,
                content: MessageContent::Text(response.content.clone()),
                name: None,
                tool_calls: if has_tool_calls { Some(response.tool_calls.clone()) } else { None },
                tool_call_id: None,
                reasoning_content: response.reasoning_content.clone(),
                created_at: 0,
            };

            // (d) Execute tool calls if present
            if has_tool_calls {
                withholder.reset();
                debug!(
                    session = %session_id,
                    iteration,
                    tool_count = response.tool_calls.len(),
                    "Forge: executing tool calls"
                );

                // F8: Push assistant message only when tool execution proceeds,
                // and before tool results so the API sees Assistant→Tool ordering.
                messages.push(assistant_msg);

                // Same-turn tool calls run concurrently (e.g. multiple do_web_fetch).
                // Results are appended in the original tool_call order for the LLM.
                let tool_results = {
                    let futs: Vec<_> = response
                        .tool_calls
                        .iter()
                        .map(|tc| {
                            execute_one_tool(
                                &self.tools,
                                tc,
                                &self.working_dir,
                                session_id,
                                &event_tx,
                                self.safety_guard.clone(),
                                self.permission_hub.clone(),
                                self.permission_timeout_secs,
                            )
                        })
                        .collect();
                    futures::future::join_all(futs).await
                };
                for msg in tool_results {
                    messages.push(msg);
                }

                // Loop again so the model can process tool results.
                continue;
            }

            // (e) No tool calls — this is the final answer.
            if has_content {
                withholder.reset();
                info!(
                    session = %session_id,
                    iteration,
                    "Forge: agent finished with final answer"
                );
                let _ = event_tx.send(StreamEvent::Complete { usage: response.usage });
                return Ok(());
            }

            // (f) No content and no tool calls — empty (or reasoning-only) response.
            // Retry with backoff; optional nudge after the first miss.
            match withholder.tolerate_empty() {
                Ok(attempt) => {
                    let max = withholder.max_attempts;
                    let delay = withholder.current_backoff();
                    warn!(
                        session = %session_id,
                        iteration,
                        attempt,
                        max,
                        delay_ms = delay.as_millis() as u64,
                        reasoning_only = has_reasoning_only,
                        "Forge: empty model response — retrying"
                    );
                    let _ = event_tx.send(StreamEvent::Token {
                        content: format!(
                            "\n_Empty model response{}, retrying ({attempt}/{max})…_\n",
                            if has_reasoning_only {
                                " (reasoning only)"
                            } else {
                                ""
                            }
                        ),
                    });
                    // Nudge the model once so a pure re-call of the same messages
                    // is more likely to produce content/tool calls.
                    if attempt == 1 {
                        messages.push(Message {
                            role: Role::User,
                            content: MessageContent::Text(
                                "(Your previous reply was empty. Continue the task: \
                                 either call a tool or write a concrete answer. \
                                 Do not reply with an empty message.)"
                                    .into(),
                            ),
                            name: None,
                            tool_calls: None,
                            tool_call_id: None,
                            reasoning_content: None,
                            created_at: 0,
                        });
                    }
                    withholder.sleep_backoff().await;
                    continue;
                }
                Err(()) => {
                    warn!(
                        session = %session_id,
                        iteration,
                        "Forge: model returned empty response after retries"
                    );
                    let _ = event_tx.send(StreamEvent::Error {
                        content: format!(
                            "Model returned an empty response (no content, no tool calls) \
                             after {} retries.",
                            withholder.max_attempts
                        ),
                    });
                    return Err(ForgeError::EmptyResponse);
                }
            }
        }

        // =================================================================
        // Max iterations exhausted
        // =================================================================
        error!(
            session = %session_id,
            iterations = self.max_iterations,
            "Forge: max iterations reached"
        );
        let _ = event_tx.send(StreamEvent::Error {
            content: format!(
                "Agent stopped after {} iterations without a final answer.",
                self.max_iterations
            ),
        });
        Err(ForgeError::MaxIterations(self.max_iterations))
    }

    /// Execute in /teams mode via TeamRuntime (v2 only — v1 path retired).
    async fn run_teams_task(
        &self,
        task: &str,
        session_id: &str,
        history: Vec<Message>,
        event_tx: tokio::sync::mpsc::UnboundedSender<StreamEvent>,
    ) -> Result<(), ForgeError> {
        if !self.teams_config.v2_enabled {
            warn!(
                "teams.v2_enabled=false is ignored; v1 path retired — using TeamRuntime"
            );
        }
        let runtime = crate::teams::runtime::TeamRuntime::new(
            self.provider.clone_box(),
            self.tools.clone(),
            self.working_dir.clone(),
            self.safety_guard.clone(),
            self.permission_hub.clone(),
            self.permission_timeout_secs,
            self.teams_config.clone(),
            event_tx,
        );
        runtime.run(task, session_id, history).await
    }

    /// Start a multi-turn /plan interview (grill-me style).
    async fn start_plan_interview(
        &self,
        session_id: &str,
        goal: &str,
        event_tx: &tokio::sync::mpsc::UnboundedSender<StreamEvent>,
    ) -> Result<(), ForgeError> {
        if goal.is_empty() {
            let _ = event_tx.send(StreamEvent::Token {
                content: "Usage: `/plan <what you want to build>`\n\n\
                    Starts an **LLM-driven** 5-phase grill-me interview \
                    (Scope → Requirements → Design → Risks → Quality).\n\
                    Each turn asks one high-leverage clarifying question (dynamic, not a fixed bank), \
                    using your project snapshot for context.\n\
                    Click an option button, enter a custom answer, or `yes`/`推荐` for the recommendation. \
                    `/plan cancel` aborts.\n".into(),
            });
            let _ = event_tx.send(StreamEvent::Complete { usage: None });
            return Ok(());
        }

        match ActivePlanSession::start_with_llm(
            &*self.provider,
            session_id,
            goal,
            self.working_dir.clone(),
        )
        .await
        {
            Ok((_session, result)) => {
                emit_plan_turn(event_tx, &result);
                Ok(())
            }
            Err(e) => {
                let _ = event_tx.send(StreamEvent::Error {
                    content: format!("Failed to start plan: {e}"),
                });
                Err(ForgeError::EmptyResponse)
            }
        }
    }

    /// Continue an active /plan interview with the user's answer.
    async fn continue_plan_interview(
        &self,
        session_id: &str,
        answer: &str,
        event_tx: &tokio::sync::mpsc::UnboundedSender<StreamEvent>,
    ) -> Result<(), ForgeError> {
        let mut session = match ActivePlanSession::load(session_id) {
            Some(s) => s,
            None => {
                let _ = event_tx.send(StreamEvent::Token {
                    content: "No active plan. Start with `/plan <goal>`.\n".into(),
                });
                let _ = event_tx.send(StreamEvent::Complete { usage: None });
                return Ok(());
            }
        };

        match session.answer_with_llm(&*self.provider, answer).await {
            Ok(result) => {
                emit_plan_turn(event_tx, &result);
                Ok(())
            }
            Err(e) => {
                let _ = event_tx.send(StreamEvent::Error {
                    content: format!("Plan error: {e}"),
                });
                Err(ForgeError::EmptyResponse)
            }
        }
    }

    /// Run the real MAGI three-brain /auto spiral via AutoRunner.
    ///
    /// When **Teams mode is ON**, independent ready subtasks run as concurrent
    /// MAGI spirals (auto + teams). When OFF, subtasks run sequentially.
    async fn run_auto_mode(
        &self,
        session_id: &str,
        task: &str,
        event_tx: &tokio::sync::mpsc::UnboundedSender<StreamEvent>,
    ) -> Result<(), ForgeError> {
        let prd = task.to_string();
        let teams_on = self.teams_mode.load(Ordering::Relaxed);

        // Interactive budgets: enough for real work, not silent multi-hour spirals.
        // Progress heartbeats stream Casper/Balthasar/Melchior + tool steps to the UI.
        let runner = AutoRunner::new(
            self.provider.clone_box(),
            self.provider.clone_box(),
            self.tools.clone(),
            self.working_dir.clone(),
        )
        .with_safety_guard(self.safety_guard.clone())
        .with_permission_hub(self.permission_hub.clone())
        .with_permission_timeout(self.permission_timeout_secs)
        .with_magi_max_rounds(3)
        .with_magi_max_steps(12)
        .with_teams_parallel(teams_on)
        .with_max_parallel(4)
        .with_progress(event_tx.clone());

        if teams_on {
            let _ = event_tx.send(StreamEvent::Token {
                content: "_Teams + /auto: independent subtasks will run auto spirals in parallel._\n\n"
                    .into(),
            });
        }

        match runner.run(&prd, session_id).await {
            Ok(result) => {
                let done = result
                    .subtasks
                    .iter()
                    .filter(|s| s.status == crate::auto::runner::SubtaskStatus::Done)
                    .count();
                let mode = if teams_on { "Auto+Teams" } else { "Auto" };
                let _ = event_tx.send(StreamEvent::Token {
                    content: format!(
                        "\n**{mode} finished.** {done}/{} subtasks done, avg quality {:.1}/100.\n",
                        result.subtasks.len(),
                        result.total_quality
                    ),
                });
                let _ = event_tx.send(StreamEvent::Complete { usage: None });
                Ok(())
            }
            Err(e) => {
                let _ = event_tx.send(StreamEvent::Error {
                    content: format!("/auto failed: {e}"),
                });
                Err(ForgeError::EmptyResponse)
            }
        }
    }
}

/// Emit plan turn markdown + structured PlanQuestion for button UI.
fn emit_plan_turn(
    event_tx: &tokio::sync::mpsc::UnboundedSender<StreamEvent>,
    result: &PlanTurnResult,
) {
    let md = format_question(result);
    let _ = event_tx.send(StreamEvent::Token { content: md });
    if let Some(pq) = plan_question_event(result) {
        let _ = event_tx.send(pq);
    }
    let _ = event_tx.send(StreamEvent::Complete { usage: None });
}

/// Call the provider with **SSE streaming**, emit Thinking/Token deltas live,
/// and assemble a final [`ChatResponse`]. Falls back to non-stream `chat()` if
/// the stream cannot be opened or yields no usable content before error.
async fn stream_provider_turn(
    provider: &dyn LlmProvider,
    messages: Vec<Message>,
    tools: Vec<ToolDef>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<StreamEvent>,
    iteration: u32,
) -> Result<ChatResponse, ProviderError> {
    use std::collections::BTreeMap;

    let stream_result = provider.chat_stream(messages.clone(), tools.clone()).await;

    let mut stream = match stream_result {
        Ok(s) => s,
        Err(e) => {
            warn!(%e, "SSE stream open failed — falling back to chat()");
            let r = provider.chat(messages, tools).await?;
            if let Some(ref reasoning) = r.reasoning_content {
                if !reasoning.is_empty() {
                    let _ = event_tx.send(StreamEvent::Thinking {
                        content: reasoning.clone(),
                        step: iteration,
                    });
                }
            }
            if !r.content.is_empty() {
                let _ = event_tx.send(StreamEvent::Token {
                    content: r.content.clone(),
                });
            }
            return Ok(r);
        }
    };

    let mut content = String::new();
    let mut reasoning = String::new();
    // index → (id, name, arguments)
    let mut tool_acc: BTreeMap<u32, (String, String, String)> = BTreeMap::new();
    let mut usage = None;
    let mut finish_reason: Option<String> = None;
    let mut got_any = false;

    // Overall stream budget: prevent infinite hang
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(300);

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            warn!("SSE stream overall timeout — using partial response");
            break;
        }

        let next = tokio::time::timeout(std::time::Duration::from_secs(90), stream.next()).await;
        match next {
            Ok(Some(Ok(chunk))) => {
                got_any = true;
                if let Some(rc) = chunk.reasoning_content {
                    if !rc.is_empty() {
                        reasoning.push_str(&rc);
                        let _ = event_tx.send(StreamEvent::Thinking {
                            content: rc,
                            step: iteration,
                        });
                    }
                }
                if let Some(c) = chunk.content {
                    if !c.is_empty() {
                        content.push_str(&c);
                        let _ = event_tx.send(StreamEvent::Token { content: c });
                    }
                }
                if let Some(deltas) = chunk.tool_calls {
                    for d in deltas {
                        let entry = tool_acc.entry(d.index).or_insert_with(|| {
                            (String::new(), String::new(), String::new())
                        });
                        if let Some(id) = d.id {
                            if !id.is_empty() {
                                entry.0 = id;
                            }
                        }
                        if let Some(f) = d.function {
                            if let Some(name) = f.name {
                                entry.1.push_str(&name);
                            }
                            if let Some(args) = f.arguments {
                                entry.2.push_str(&args);
                            }
                        }
                    }
                }
                if let Some(u) = chunk.usage {
                    usage = Some(u);
                }
                if let Some(fr) = chunk.finish_reason {
                    finish_reason = Some(fr);
                    // keep reading until stream ends for trailing usage frames
                }
            }
            Ok(Some(Err(e))) => {
                error!(%e, "SSE chunk error");
                if got_any && (!content.is_empty() || !tool_acc.is_empty()) {
                    warn!("using partial SSE response after chunk error");
                    break;
                }
                // Fall back to non-stream
                warn!(%e, "SSE failed with no content — falling back to chat()");
                let r = provider.chat(messages, tools).await?;
                if let Some(ref reasoning) = r.reasoning_content {
                    if !reasoning.is_empty() {
                        let _ = event_tx.send(StreamEvent::Thinking {
                            content: reasoning.clone(),
                            step: iteration,
                        });
                    }
                }
                if !r.content.is_empty() {
                    let _ = event_tx.send(StreamEvent::Token {
                        content: r.content.clone(),
                    });
                }
                return Ok(r);
            }
            Ok(None) => break,
            Err(_timeout) => {
                warn!("SSE idle timeout (90s)");
                if got_any {
                    break;
                }
                warn!("SSE idle with no data — falling back to chat()");
                let r = provider.chat(messages, tools).await?;
                if let Some(ref reasoning) = r.reasoning_content {
                    if !reasoning.is_empty() {
                        let _ = event_tx.send(StreamEvent::Thinking {
                            content: reasoning.clone(),
                            step: iteration,
                        });
                    }
                }
                if !r.content.is_empty() {
                    let _ = event_tx.send(StreamEvent::Token {
                        content: r.content.clone(),
                    });
                }
                return Ok(r);
            }
        }

        // If finish_reason is tool_calls or stop and we already have content/tools, we can end early
        // once the stream also closed — handled by Ok(None).
        let _ = finish_reason;
    }

    // If stream produced nothing useful, fall back
    if !got_any && content.is_empty() && tool_acc.is_empty() {
        warn!("SSE produced empty response — falling back to chat()");
        let r = provider.chat(messages, tools).await?;
        if let Some(ref reasoning) = r.reasoning_content {
            if !reasoning.is_empty() {
                let _ = event_tx.send(StreamEvent::Thinking {
                    content: reasoning.clone(),
                    step: iteration,
                });
            }
        }
        if !r.content.is_empty() {
            let _ = event_tx.send(StreamEvent::Token {
                content: r.content.clone(),
            });
        }
        return Ok(r);
    }

    let tool_calls: Vec<ToolCall> = tool_acc
        .into_iter()
        .map(|(_, (id, name, arguments))| ToolCall {
            id: if id.is_empty() {
                uuid::Uuid::new_v4().to_string()
            } else {
                id
            },
            call_type: "function".into(),
            function: crate::providers::trait_def::FunctionCall { name, arguments },
        })
        .filter(|tc| !tc.function.name.is_empty())
        .collect();

    Ok(ChatResponse {
        content,
        tool_calls,
        usage,
        reasoning_content: if reasoning.is_empty() {
            None
        } else {
            Some(reasoning)
        },
    })
}

/// Compress older messages using LLM summarization.
/// This is a free function (not a method) to avoid borrowing &self across await.
pub async fn compress_context(
    provider: &dyn LlmProvider,
    messages: &[Message],
    event_tx: &tokio::sync::mpsc::UnboundedSender<StreamEvent>,
    session_id: &str,
) -> Vec<Message> {
    let total = messages.iter().filter(|m| m.role != Role::System).count();
    let compress_count = (total as f64 * 0.7) as usize;
    if compress_count == 0 { return messages.to_vec(); }

    let old: Vec<_> = messages.iter().filter(|m| m.role != Role::System).take(compress_count).cloned().collect();
    let prompt = compression_prompt(&old, 65536);
    let summary = match provider.chat(
        vec![Message { role: Role::User, content: MessageContent::Text(prompt), ..Default::default() }],
        vec![],
    ).await {
        Ok(r) => r.content,
        Err(e) => { warn!(session = %session_id, %e, "compression failed"); return messages.to_vec(); }
    };

    if summary.is_empty() { return messages.to_vec(); }
    info!(session = %session_id, summary_len = summary.len(), "context compressed");

    let sys_content = messages.iter().find(|m| m.role == Role::System)
        .and_then(|m| m.content.as_text().map(|s| s.to_string())).unwrap_or_default();
    let remaining: Vec<_> = messages.iter().filter(|m| m.role != Role::System).skip(compress_count).cloned().collect();

    let mut result = Vec::new();
    result.push(Message {
        role: Role::System,
        content: MessageContent::Text(format!("{}\n\n## Conversation Summary\n{}", sys_content, summary)),
        name: None, tool_calls: None, tool_call_id: None, reasoning_content: None, created_at: 0 });
    result.extend(remaining);

    let _ = event_tx.send(StreamEvent::Fact {
        id: format!("compress_{}", session_id),
        subject: "context".into(),
        predicate: "compressed".into(),
        object: format!("compressed {} messages into summary", compress_count),
    });
    result
}

/// Remove tool_calls from messages that have no matching tool response.
/// Validate that messages sent to the provider have intact tool chains.
/// Returns cloned+fixed messages, logging any issues found.
fn validate_tool_chain_for_provider(mut messages: Vec<Message>) -> Vec<Message> {
    clean_orphaned_tool_calls(&mut messages);
    // Remove consecutive duplicates
    let mut i = 1;
    while i < messages.len() {
        let same_tc_ids = messages[i-1].tool_calls.as_ref().map(|tc| tc.iter().map(|t| &t.id).collect::<Vec<_>>())
            == messages[i].tool_calls.as_ref().map(|tc| tc.iter().map(|t| &t.id).collect::<Vec<_>>());
        if messages[i-1].role == messages[i].role
            && messages[i-1].content == messages[i].content
            && same_tc_ids
            && messages[i-1].tool_call_id == messages[i].tool_call_id
            && messages[i-1].reasoning_content == messages[i].reasoning_content
            && messages[i-1].name == messages[i].name
        {
            messages.remove(i);
        } else {
            i += 1;
        }
    }

    // Legacy/desktop bug: parallel ToolStart wrote one assistant per call.
    // OpenAI requires a single assistant(tool_calls=[…]) then matching tools.
    merge_consecutive_tool_call_assistants(&mut messages);

    // Sequential tool chain fix: scan left-to-right, track which tool_call_ids
    // have been declared by assistant(tc) messages. Only allow Tool messages
    // whose tool_call_id was declared by a PREVIOUS assistant. Remove tool_calls
    // from assistants that have no following tool messages.
    let mut pending_tc_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut responded_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

    // First pass: mark which IDs have responses
    for m in &messages {
        if m.role == Role::Tool {
            if let Some(ref id) = m.tool_call_id {
                responded_ids.insert(id.clone());
            }
        }
    }

    // Second pass: sequential validation
    let mut result = Vec::new();
    for mut m in messages {
        if m.role == Role::Assistant {
            // Track declared tool_call IDs from this assistant
            if let Some(ref tc) = m.tool_calls {
                for t in tc.iter() {
                    pending_tc_ids.insert(t.id.clone());
                }
            }
            // Only include tool_calls that will have responses
            if let Some(ref mut tc) = m.tool_calls {
                tc.retain(|t| responded_ids.contains(&t.id));
                if tc.is_empty() {
                    m.tool_calls = None;
                }
            }
            result.push(m);
        } else if m.role == Role::Tool {
            // Only keep Tool messages whose ID was declared by a previous assistant.
            // Consume the ID so it can't be reused (OpenAI requires uniqueness).
            if let Some(ref id) = m.tool_call_id {
                if pending_tc_ids.remove(id) {
                    result.push(m);
                }
            }
        } else {
            // User/System — clear pending tool calls (new conversation turn)
            pending_tc_ids.clear();
            result.push(m);
        }
    }

    // Remove ghost assistants
    result.retain(|m| {
        if m.role != Role::Assistant { return true; }
        if !m.content.is_empty() { return true; }
        if m.tool_calls.is_some() { return true; }
        if m.reasoning_content.as_ref().map_or(false, |r| !r.is_empty()) { return true; }
        false
    });

    result
}

/// Collapse consecutive assistant messages that each carry tool_calls into one.
fn merge_consecutive_tool_call_assistants(messages: &mut Vec<Message>) {
    let mut i = 0;
    while i < messages.len() {
        let has_tc = messages[i].role == Role::Assistant
            && messages[i]
                .tool_calls
                .as_ref()
                .map(|tc| !tc.is_empty())
                .unwrap_or(false);
        if !has_tc {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < messages.len()
            && messages[j].role == Role::Assistant
            && messages[j]
                .tool_calls
                .as_ref()
                .map(|tc| !tc.is_empty())
                .unwrap_or(false)
        {
            j += 1;
        }
        if j > i + 1 {
            let mut combined = messages[i].tool_calls.take().unwrap_or_default();
            for k in (i + 1)..j {
                if let Some(tcs) = messages[k].tool_calls.take() {
                    for tc in tcs {
                        if !combined.iter().any(|x| x.id == tc.id) {
                            combined.push(tc);
                        }
                    }
                }
                if messages[i].content.is_empty() && !messages[k].content.is_empty() {
                    messages[i].content = messages[k].content.clone();
                }
                if messages[i]
                    .reasoning_content
                    .as_ref()
                    .map_or(true, |r| r.is_empty())
                {
                    if let Some(ref rc) = messages[k].reasoning_content {
                        if !rc.is_empty() {
                            messages[i].reasoning_content = Some(rc.clone());
                        }
                    }
                }
            }
            messages[i].tool_calls = Some(combined);
            messages.drain((i + 1)..j);
        }
        i += 1;
    }
}

fn clean_orphaned_tool_calls(messages: &mut Vec<Message>) {
    let responded: std::collections::HashSet<String> = messages
        .iter().filter(|m| m.role == Role::Tool)
        .filter_map(|m| m.tool_call_id.clone()).collect();

    for msg in messages.iter_mut() {
        if let Some(ref mut tc) = msg.tool_calls {
            tc.retain(|t| responded.contains(&t.id));
            if tc.is_empty() {
                msg.tool_calls = None;
            }
        }
        // Always strip tool_call_id from Assistant messages — it belongs
        // only on Tool-role messages per OpenAI protocol.
        if msg.role == Role::Assistant && msg.tool_call_id.is_some() {
            msg.tool_call_id = None;
        }
    }

    let valid_ids: std::collections::HashSet<String> = messages.iter()
        .filter_map(|m| m.tool_calls.as_ref())
        .flat_map(|tc| tc.iter().map(|t| t.id.clone())).collect();

    messages.retain(|m| {
        if m.role != Role::Tool { return true; }
        m.tool_call_id.as_ref().map_or(false, |id| valid_ids.contains(id))
    });

    // Remove ghost Assistant messages: after cleaning orphaned tool_calls,
    // an assistant may have empty content, no tool_calls, and no reasoning.
    // These cause 400 errors: "messages with role 'assistant' must have
    // content or tool_calls".
    messages.retain(|m| {
        if m.role != Role::Assistant { return true; }
        if !m.content.is_empty() { return true; }
        if m.tool_calls.is_some() { return true; }
        if m.reasoning_content.as_ref().map_or(false, |r| !r.is_empty()) { return true; }
        false
    });
}

/// Execute a single tool call and emit lifecycle events (`ToolStart`,
/// `ToolProgress`, `ToolEnd`). Returns a `Role::Tool` message for the LLM.
async fn execute_one_tool(
    tools: &ToolRegistry,
    tc: &ToolCall,
    working_dir: &PathBuf,
    session_id: &str,
    event_tx: &tokio::sync::mpsc::UnboundedSender<StreamEvent>,
    safety_guard: Arc<SafetyGuard>,
    permission_hub: Option<Arc<PermissionHub>>,
    permission_timeout_secs: u64,
) -> Message {
    let tool_name = &tc.function.name;
    let tool_call_id = &tc.id;

    let description = tools
        .get(tool_name)
        .map(|t| t.description().to_string())
        .unwrap_or_default();

    let _ = event_tx.send(StreamEvent::ToolStart {
        id: tool_call_id.clone(),
        name: tool_name.clone(),
        description,
        arguments: tc.function.arguments.clone(),
    });

    let args: serde_json::Value = match serde_json::from_str(&tc.function.arguments) {
        Ok(v) => v,
        Err(e) => {
            let err_msg = format!(
                "Failed to parse arguments for tool '{}': {}",
                tool_name, e
            );
            warn!(%err_msg, "Forge: bad tool arguments");
            let _ = event_tx.send(StreamEvent::ToolEnd {
                id: tool_call_id.clone(),
                status: ToolStatus::Error,
                result: err_msg.clone(),
            });
            return Message {
                role: Role::Tool,
                content: MessageContent::Text(err_msg),
                name: None,
                tool_calls: None,
                tool_call_id: Some(tool_call_id.clone()),
                reasoning_content: None,
                created_at: 0,
            };
        }
    };

    let ctx = ToolContext {
        working_dir: working_dir.clone(),
        session_id: session_id.to_string(),
        tool_call_id: tool_call_id.clone(),
        sender: event_tx.clone(),
        safety_guard,
        permission_hub,
        permission_timeout_secs,
        team_agent_id: None,
        file_ownership: None,
        ownership_enforced: false,
        ownership_soft_log_only: true,
        read_paths: None,
        read_before_edit: false,
    };

    match tools.execute(tool_name, args, &ctx).await {
        Ok(result) => {
            let status = if result.success {
                ToolStatus::Success
            } else {
                ToolStatus::Error
            };
            let raw = if result.success {
                result.output.clone()
            } else {
                result
                    .error
                    .as_deref()
                    .unwrap_or(&result.output)
                    .to_string()
            };
            let output = truncate_tool_result(&raw, MAX_TOOL_RESULT_CHARS);

            debug!(
                tool = %tool_name,
                id = %tool_call_id,
                success = result.success,
                output_len = output.len(),
                "Forge: tool finished"
            );

            let _ = event_tx.send(StreamEvent::ToolEnd {
                id: tool_call_id.clone(),
                status,
                result: output.clone(),
            });

            Message {
                role: Role::Tool,
                content: MessageContent::Text(output),
                name: None,
                tool_calls: None,
                tool_call_id: Some(tool_call_id.clone()),
                reasoning_content: None,
                created_at: 0,
            }
        }
        Err(e) => {
            let err_str = e.to_string();
            error!(
                tool = %tool_name,
                id = %tool_call_id,
                %err_str,
                "Forge: tool execution error"
            );

            let _ = event_tx.send(StreamEvent::ToolEnd {
                id: tool_call_id.clone(),
                status: ToolStatus::Error,
                result: err_str.clone(),
            });

            Message {
                role: Role::Tool,
                content: MessageContent::Text(err_str),
                name: None,
                tool_calls: None,
                tool_call_id: Some(tool_call_id.clone()),
                reasoning_content: None,
                created_at: 0,
            }
        }
    }
}

/// Truncate tool output before injecting into LLM context (anti context bloat).
fn truncate_tool_result(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let head: String = s.chars().take(max_chars.saturating_sub(80)).collect();
    format!(
        "{head}\n\n…[tool output truncated at {max_chars} chars; re-read file/path if more is needed]…"
    )
}

#[cfg(test)]
mod tool_chain_tests {
    use super::*;
    use crate::providers::trait_def::{FunctionCall, MessageContent, ToolCall};

    fn tool_call(id: &str, name: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            call_type: "function".into(),
            function: FunctionCall {
                name: name.into(),
                arguments: "{}".into(),
            },
        }
    }

    fn assistant_tc(ids: &[&str]) -> Message {
        Message {
            role: Role::Assistant,
            content: MessageContent::Text(String::new()),
            name: None,
            tool_calls: Some(ids.iter().map(|id| tool_call(id, "do_web_fetch")).collect()),
            tool_call_id: None,
            reasoning_content: None,
            created_at: 0,
        }
    }

    fn tool_result(id: &str) -> Message {
        Message {
            role: Role::Tool,
            content: MessageContent::Text(format!("result-{id}")),
            name: None,
            tool_calls: None,
            tool_call_id: Some(id.into()),
            reasoning_content: None,
            created_at: 0,
        }
    }

    #[test]
    fn merge_parallel_tool_call_assistants() {
        let mut msgs = vec![
            Message {
                role: Role::User,
                content: MessageContent::Text("find proxies".into()),
                ..Default::default()
            },
            // Legacy split: one assistant per parallel ToolStart
            assistant_tc(&["a"]),
            assistant_tc(&["b"]),
            assistant_tc(&["c"]),
            tool_result("a"),
            tool_result("b"),
            tool_result("c"),
            Message {
                role: Role::Assistant,
                content: MessageContent::Text("here are proxies".into()),
                ..Default::default()
            },
        ];
        merge_consecutive_tool_call_assistants(&mut msgs);
        assert_eq!(msgs.len(), 6); // user + 1 assistant(tc) + 3 tools + final
        assert_eq!(msgs[1].role, Role::Assistant);
        let ids: Vec<_> = msgs[1]
            .tool_calls
            .as_ref()
            .unwrap()
            .iter()
            .map(|t| t.id.as_str())
            .collect();
        assert_eq!(ids, vec!["a", "b", "c"]);
        assert_eq!(msgs[2].tool_call_id.as_deref(), Some("a"));
        assert_eq!(msgs[3].tool_call_id.as_deref(), Some("b"));
        assert_eq!(msgs[4].tool_call_id.as_deref(), Some("c"));
    }

    #[test]
    fn validate_accepts_merged_parallel_chain() {
        let msgs = vec![
            Message {
                role: Role::System,
                content: MessageContent::Text("sys".into()),
                ..Default::default()
            },
            Message {
                role: Role::User,
                content: MessageContent::Text("q".into()),
                ..Default::default()
            },
            assistant_tc(&["a"]),
            assistant_tc(&["b"]),
            tool_result("a"),
            tool_result("b"),
            Message {
                role: Role::Assistant,
                content: MessageContent::Text("done".into()),
                ..Default::default()
            },
            Message {
                role: Role::User,
                content: MessageContent::Text("second msg".into()),
                ..Default::default()
            },
        ];
        let out = validate_tool_chain_for_provider(msgs);
        // One combined assistant with both tool_calls
        let tc_assistants: Vec<_> = out
            .iter()
            .filter(|m| m.role == Role::Assistant && m.tool_calls.is_some())
            .collect();
        assert_eq!(tc_assistants.len(), 1);
        assert_eq!(tc_assistants[0].tool_calls.as_ref().unwrap().len(), 2);
        // Both tool results retained immediately after
        let tools: Vec<_> = out.iter().filter(|m| m.role == Role::Tool).collect();
        assert_eq!(tools.len(), 2);
        // No assistant(tool_calls) without following tools — count positions
        let mut i = 0;
        while i < out.len() {
            if out[i].role == Role::Assistant {
                if let Some(ref tcs) = out[i].tool_calls {
                    for (k, tc) in tcs.iter().enumerate() {
                        let tool_msg = &out[i + 1 + k];
                        assert_eq!(tool_msg.role, Role::Tool, "tool must follow assistant");
                        assert_eq!(tool_msg.tool_call_id.as_deref(), Some(tc.id.as_str()));
                    }
                }
            }
            i += 1;
        }
    }

    #[test]
    fn does_not_merge_across_tool_results() {
        // Sequential tool turns (iteration 1 then 2) must stay separate.
        let mut msgs = vec![
            assistant_tc(&["a"]),
            tool_result("a"),
            assistant_tc(&["b"]),
            tool_result("b"),
        ];
        merge_consecutive_tool_call_assistants(&mut msgs);
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[0].tool_calls.as_ref().unwrap().len(), 1);
        assert_eq!(msgs[2].tool_calls.as_ref().unwrap().len(), 1);
    }
}
