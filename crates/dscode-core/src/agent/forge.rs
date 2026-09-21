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

use super::compression::{CompressionAction, CompressionPipeline};
use super::context::{build_context, count_message_tokens, count_tool_def_tokens, ContextPacket};
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
const DEFAULT_MAX_HISTORY_MESSAGES: usize = 1000;

/// Cap tool result text injected back into the conversation (chars).
const MAX_TOOL_RESULT_CHARS: usize = 24_000;

/// Start tool-loop detection after this many ReAct turns.
const LOOP_DETECT_FROM_ITERATION: u32 = 5;

/// How many times one turn may answer a provider "over context window"
/// rejection by force-compressing and re-sending the same round.
///
/// One. [`CompressionPipeline::force_apply`] already applies the deepest level
/// that can run, so a second rejection means the irreducible core (system
/// prompt + live instruction + tool definitions) is itself larger than the
/// model's real window: compressing again cannot help, and every re-send of a
/// body the provider just rejected bills another generation. The counter is
/// per `execute()` call — i.e. per user turn — and a retry also requires that
/// the compression actually changed something, so this loop cannot spin.
const MAX_CONTEXT_OVERFLOW_RETRIES: u32 = 1;

/// Tool-call fingerprint: (tool name, hash of name + arguments).
///
/// Comparing only tool *names* caused false positives on legitimate
/// single-tool workflows (e.g. many `do_bash` calls with different commands,
/// or a compile → fix → recompile cycle). Hashing the arguments means a
/// repeated call only counts when both the tool AND its parameters are the
/// same — which is what an actual tool loop looks like.
fn tool_fingerprint(tc: &ToolCall) -> (String, u64) {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    tc.function.name.hash(&mut h);
    tc.function.arguments.hash(&mut h);
    (tc.function.name.clone(), h.finish())
}

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
    ///
    /// # Terminal events
    ///
    /// Every exit from `execute()` — success **and** every error path — emits
    /// exactly one terminal event. Failing paths emit an
    /// [`StreamEvent::Error`] with the reason, followed by
    /// [`StreamEvent::Complete`] so UIs that key their streaming state off
    /// `Complete` (rather than off `Error`) are not left spinning forever.
    ///
    /// The one exception is a **truncated stream** (`StreamedTurn::truncated`):
    /// it emits `Error` and deliberately **no** `Complete`, because "the reply
    /// is a prefix" must never be reported as a finished turn.
    pub async fn execute(
        &self,
        user_message: &str,
        session_id: &str,
        history: Vec<Message>,
        event_tx: tokio::sync::mpsc::UnboundedSender<StreamEvent>,
    ) -> Result<(), ForgeError> {
        // F4: Compression is tracked per execution (a local, not Forge state),
        // so two concurrent `execute()` calls on the same Forge instance can
        // never reset each other's compression bookkeeping.
        let mut compression_passes: u32 = 0;

        // Per-turn budget for "the provider says we are over its context
        // window" → force-compress → re-send. See
        // `MAX_CONTEXT_OVERFLOW_RETRIES` for why it is one.
        let mut overflow_retries: u32 = 0;

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

        // ── /compact — manual context compression ──
        //
        // Handled *before* the plan-interview guard below, so an active
        // interview cannot swallow it as an answer (that guard treats every
        // non-command input as the user's answer). Matched on a command
        // boundary, not with `starts_with`: `/compacter` must not hit it.
        // The `/plan`, `/auto` and `/teams` checks below keep their historical
        // `starts_with` — deliberately untouched.
        if invokes_command(trimmed, "/compact") {
            return self.compact_now(session_id, history, &event_tx).await;
        }

        // ── Active multi-turn /plan interview (user answers) ──
        if ActivePlanSession::is_active(session_id)
            && !trimmed.starts_with("/plan")
            && !trimmed.starts_with("/auto")
            && !trimmed.starts_with("/teams")
            // `/compact` is a command, never an interview answer. Belt and
            // braces: the short-circuit above already returns before this
            // guard, but the exclusion keeps the guard honest if that order
            // ever changes.
            && !invokes_command(trimmed, "/compact")
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
            reasoning_content: None,
            created_at: 0,
            ..Default::default()
        });

        info!(
            session = %session_id,
            history_msgs = history.len(),
            prompt_len = user_message.len(),
            tool_count = tools.len(),
            "Forge: starting ReAct loop"
        );

        // F5: Track message count at start of this execute() so stall
        // detection only examines messages added during the current run.
        // NOTE: compression (`*messages = work`) and validation can both shrink
        // the vector afterwards, so every slice built from this must be clamped
        // (see the `base` clamp in the loop).
        let initial_msg_count = messages.len();

        // F2: Sliding window of tool-call fingerprints (name + args hash) from
        // the last 5 iterations for alternating-pattern stall detection. Args
        // are hashed so different parameters are not conflated as a loop.
        let mut recent_tool_sets:
            std::collections::VecDeque<std::collections::BTreeSet<(String, u64)>> =
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
                // Terminal-event contract: every exit from `execute()` emits
                // exactly one `Complete`, so a UI that keys off it always
                // leaves the streaming state — `Error` alone is informational.
                let _ = event_tx.send(StreamEvent::Complete { usage: None });
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
                        ..Default::default()
                    });
                }
            }

            // =============================================================
            // (a.0) Context compression (L0-L4)
            // =============================================================
            //
            // Runs *before* the provider call and before stall detection, on
            // `provider_view` — the validated vector that is actually sent to
            // the provider (orphan tool rows dropped, unpaired tool_calls
            // stripped, duplicate rows merged). Metering, thresholds and the
            // stall detector therefore all describe the request the model
            // really receives, not the raw history.
            //
            // This is deliberately not a one-pass-per-turn latch: a long tool
            // loop refills the window between iterations (up to 24k chars of
            // tool output each time), and a turn that stopped compressing
            // after its first pass used to die on the provider's non-retryable
            // HTTP 400 around iteration 40 — after dozens of successful tool
            // calls. Repeat passes run with `cheap_only` so they never pay for
            // another LLM summarisation.
            let passes = compression_passes;
            let tool_tok = count_tool_def_tokens(&tools);
            let token_count = |msgs: &Vec<Message>| -> u64 {
                let sys: Vec<&Message> = msgs.iter().filter(|m| m.role == Role::System).collect();
                let hist: Vec<&Message> = msgs.iter().filter(|m| m.role != Role::System).collect();
                count_message_tokens(&sys) + count_message_tokens(&hist) + tool_tok
            };
            let mut provider_view = validate_tool_chain_for_provider(messages.clone());
            {
                let mut pipeline = CompressionPipeline::new(self.context_config.clone())
                    .with_tools(&tools);
                pipeline.cheap_only = passes > 0;
                let before_tok = token_count(&provider_view);
                // Compress the validated view in place (no extra clone): the
                // pipeline may also apply L0, which mutates even when it
                // reports no level action.
                let action = pipeline
                    .apply(&mut provider_view, &*self.provider, None)
                    .await;
                let after_tok = token_count(&provider_view);
                // A level action, or L0's irreversible snip (which mutates
                // without reporting a level): either way the prompt changed.
                if !matches!(action, CompressionAction::None) || after_tok != before_tok {
                    // Keep the working history in sync with the compressed,
                    // validated view: the provider will see `provider_view`, so
                    // anything else would make the next iteration reason about
                    // a different history than the model did.
                    provider_view = validate_tool_chain_for_provider(provider_view);
                    messages = provider_view.clone();
                    let _ = event_tx.send(StreamEvent::ContextCompressed {
                        before_tokens: before_tok,
                        after_tokens: after_tok,
                        window: self.context_config.window_tokens,
                    });
                    info!(
                        session = %session_id,
                        iteration,
                        action = ?action,
                        before_tok,
                        after_tok,
                        "context compression applied"
                    );
                    compression_passes += 1;
                }
            }

            // F10: Clean orphaned tool_calls on the original vec so the
            // fix persists across iterations (new messages appended to original).
            clean_orphaned_tool_calls(&mut messages);

            debug!(
                session = %session_id,
                iteration,
                msg_count = messages.len(),
                "Forge: calling provider"
            );

            // Stall detection — sliding window of tool-call sets (from early on).
            if iteration >= LOOP_DETECT_FROM_ITERATION {
                // Only scan messages added during this execute() call (F5).
                //
                // `initial_msg_count` was measured on `messages`, but
                // compression rewrites the vector and validation drops rows,
                // so the provider view can be much shorter. Clamp before
                // slicing: `&messages[initial_msg_count..]` used to panic with
                // "range start index N out of range" on iteration 5 whenever
                // compression had shrunk the history.
                let base = initial_msg_count.min(provider_view.len());
                let run_messages = &provider_view[base..];
                // Last assistant tool-call set (most recent turn)
                let current_set: std::collections::BTreeSet<(String, u64)> = run_messages
                    .iter()
                    .rev()
                    .find_map(|m| m.tool_calls.as_ref())
                    .map(|tc| tc.iter().map(tool_fingerprint).collect())
                    .unwrap_or_default();
                if !current_set.is_empty() {
                    if recent_tool_sets.len() >= 5 {
                        recent_tool_sets.pop_front();
                    }
                    recent_tool_sets.push_back(current_set.clone());
                    let mut counts: std::collections::HashMap<
                        &std::collections::BTreeSet<(String, u64)>,
                        usize,
                    > = std::collections::HashMap::new();
                    for s in recent_tool_sets.iter() {
                        *counts.entry(s).or_insert(0) += 1;
                    }
                    // ≥3 appearances of the same tool-set in the last 5 turns
                    if counts.values().any(|&c| c >= 3) {
                        let repeated: Vec<String> =
                            current_set.iter().map(|(name, _)| name.clone()).collect();
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
                            ..Default::default()
                        });
                        // Clear window so we don't re-nudge every turn
                        recent_tool_sets.clear();
                        // The nudge must reach the provider this iteration,
                        // so the authoritative view is rebuilt after it.
                        provider_view = validate_tool_chain_for_provider(messages.clone());
                    }
                }
            }

            // (a) Call the LLM provider — SSE stream first, fall back to chat()
            //
            // `provider_view` (built above, before compression) is the
            // authoritative history for this iteration: it is what the
            // provider actually receives.
            let turn = match stream_provider_turn(
                &*self.provider,
                provider_view,
                tools.clone(),
                &event_tx,
                iteration,
            )
            .await
            {
                Ok(r) => r,
                Err(e) => {
                    // ── Context overflow: the one provider 400 worth
                    // re-sending ─────────────────────────────────────────
                    //
                    // `error_withholding` classifies every 400 as permanent
                    // (correctly: re-sending the *same body* cannot succeed),
                    // so this has to be intercepted before it. But the body is
                    // not immutable — shrink it and the same call goes
                    // through. This is also the only signal that reflects the
                    // real model's window instead of the user-typed
                    // `window_tokens`, which is why it beats the estimate.
                    if e.is_context_overflow() {
                        if overflow_retries < MAX_CONTEXT_OVERFLOW_RETRIES {
                            // `provider_view` was moved into the failed call;
                            // rebuild it from the working history (the same
                            // derivation the loop uses to build it).
                            let mut view =
                                validate_tool_chain_for_provider(messages.clone());
                            let before_tok = token_count(&view);
                            let mut pipeline = CompressionPipeline::new(
                                self.context_config.clone(),
                            )
                            .with_tools(&tools);
                            // Bypasses `cheap_only` on purpose: the provider
                            // has already rejected this prompt, so a dead turn
                            // is the expensive outcome, not the summary call.
                            let action =
                                pipeline.force_apply(&mut view, &*self.provider).await;

                            if !matches!(action, CompressionAction::None) {
                                overflow_retries += 1;
                                compression_passes += 1;
                                let after_tok = token_count(&view);
                                view = validate_tool_chain_for_provider(view);
                                // Keep the working history in sync with what
                                // will be sent, exactly like the proactive
                                // pass above.
                                messages = view;
                                let _ = event_tx.send(StreamEvent::ContextCompressed {
                                    before_tokens: before_tok,
                                    after_tokens: after_tok,
                                    window: self.context_config.window_tokens,
                                });
                                let _ = event_tx.send(StreamEvent::Token {
                                    content: format!(
                                        "\n_⚠️ 模型报告上下文超过其上限,已强制压缩\
                                         ({}:{before_tok} → {after_tok} tokens)并重试本回合。\
                                         要根治请把设置里的上下文窗口改成该模型的真实大小。_\n",
                                        action_label(&action)
                                    ),
                                });
                                warn!(
                                    session = %session_id,
                                    iteration,
                                    action = ?action,
                                    before_tok,
                                    after_tok,
                                    "provider rejected the prompt as over-window — force-compressed, retrying the round"
                                );
                                continue;
                            }
                            warn!(
                                session = %session_id,
                                iteration,
                                "provider rejected the prompt as over-window but no level could compress further"
                            );
                        }
                        // Retry budget spent (or nothing left to cut): this is
                        // fatal, and the generic "Provider error: …" text
                        // would hide the one thing the user can act on.
                        error!(
                            session = %session_id,
                            iteration,
                            %e,
                            overflow_retries,
                            "context overflow persists after compression — failing the turn"
                        );
                        let what = if overflow_retries > 0 {
                            "压缩后仍然超过模型的上下文上限"
                        } else {
                            "上下文超过模型的上限,且已无可安全压缩的内容"
                        };
                        let _ = event_tx.send(StreamEvent::Error {
                            content: format!(
                                "{what},provider 原文:{e}\n\
                                 把设置里的上下文窗口(context.window_tokens)改成该模型的真实大小,\
                                 或开一个新会话后重试。"
                            ),
                        });
                        let _ = event_tx.send(StreamEvent::Complete { usage: None });
                        return Err(ForgeError::Provider(e));
                    }

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
                            let _ = event_tx.send(StreamEvent::Complete { usage: None });
                            return Err(ForgeError::Provider(e));
                        }
                    }
                }
            };

            // A stream that ended without the provider's own completion marker
            // is not a success: the accumulated text and tool-call arguments
            // are a prefix. Executing a half-parsed tool call — or reporting
            // `Complete` on half a reply — is worse than failing the turn, so
            // this terminates here with the ONE deliberate exception to the
            // terminal-event contract: an `Error` and no `Complete`, because a
            // `Complete` is exactly the "report an incomplete reply as a
            // finished one" bug this path exists to stop. (`ChatResponse`
            // cannot carry the flag, so it travels in `StreamedTurn`.)
            if let Some(reason) = turn.truncated.as_ref() {
                let msg = format!(
                    "Response stream was interrupted ({reason}); the reply is incomplete \
                     and was not executed. Please retry the request."
                );
                error!(session = %session_id, iteration, %reason, "Forge: truncated stream");
                let _ = event_tx.send(StreamEvent::Error {
                    content: msg.clone(),
                });
                return Err(ForgeError::Provider(ProviderError::StreamInterrupted(msg)));
            }
            let response = turn.response;

            // `finish_reason` is the provider's own account of *why* it stopped.
            // `length` (max_tokens) and `content_filter` both used to be
            // swallowed and reported as a normal, successful answer.
            let finish_note = finish_reason_note(turn.finish_reason.as_deref());
            if let Some(note) = finish_note {
                warn!(
                    session = %session_id,
                    iteration,
                    finish_reason = ?turn.finish_reason,
                    "Forge: provider ended the turn abnormally"
                );
                let _ = event_tx.send(StreamEvent::Token {
                    content: format!("\n\n_⚠️ {note}_\n"),
                });
            }

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
                ..Default::default()
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
                //
                // The batch races the cancel token: the token was previously
                // only checked at the top of each iteration, so one tool that
                // never returns (a hung `do_bash`) pinned the whole turn
                // forever. We only race here — `ToolContext` deliberately has
                // no cancel field, so tools stay cancellable by dropping the
                // future, not by threading a token through every call site.
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
                    let batch = futures::future::join_all(futs);
                    match self.cancel_token.as_ref() {
                        Some(token) => {
                            tokio::select! {
                                results = batch => results,
                                _ = token.cancelled() => {
                                    info!(
                                        session = %session_id,
                                        iteration,
                                        "Forge: cancelled while tools were running"
                                    );
                                    let _ = event_tx.send(StreamEvent::Error {
                                        content: "Agent cancelled.".into(),
                                    });
                                    let _ = event_tx.send(StreamEvent::Complete { usage: None });
                                    return Err(ForgeError::Cancelled);
                                }
                            }
                        }
                        None => batch.await,
                    }
                };
                for mut msg in tool_results {
                    if let Some(note) = finish_note {
                        // Attach the truncation warning to the tool result the
                        // model reads back, so it knows to split the call next
                        // time instead of silently trusting a cut-off payload.
                        let text = msg.content.as_text().unwrap_or("").to_string();
                        msg.content = MessageContent::Text(format!("{text}\n\n[note: {note}]"));
                    }
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
                            ..Default::default()
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
                    let _ = event_tx.send(StreamEvent::Complete { usage: None });
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
        let _ = event_tx.send(StreamEvent::Complete { usage: None });
        Err(ForgeError::MaxIterations(self.max_iterations))
    }

    /// `/compact` — compress the current context on demand and report the
    /// result.
    ///
    /// Short-circuits the ReAct loop entirely: the user asked to shrink the
    /// context, not to run a turn.
    /// [`CompressionPipeline::force_apply`] ignores the configured thresholds,
    /// so this works even when `window_tokens` is much larger than the model's
    /// real window — the situation the automatic ladder cannot see.
    ///
    /// Persistence caveat: `execute()` receives `history` by value and Forge
    /// owns no storage handle, so the compressed vector cannot be written back
    /// from here. The report below therefore describes what a request would
    /// cost now; making the compression stick across turns needs the
    /// session/desktop layer.
    async fn compact_now(
        &self,
        session_id: &str,
        history: Vec<Message>,
        event_tx: &tokio::sync::mpsc::UnboundedSender<StreamEvent>,
    ) -> Result<(), ForgeError> {
        // Same metering as the ReAct loop, tool definitions included: they are
        // sent on every request and can be a large part of the prompt.
        let tool_defs = self.tools.to_openai_tools();
        let tool_tok = count_tool_def_tokens(&tool_defs);
        let count = |msgs: &Vec<Message>| -> u64 {
            let sys: Vec<&Message> = msgs.iter().filter(|m| m.role == Role::System).collect();
            let hist: Vec<&Message> = msgs.iter().filter(|m| m.role != Role::System).collect();
            count_message_tokens(&sys) + count_message_tokens(&hist) + tool_tok
        };

        let mut view = validate_tool_chain_for_provider(history);
        let before_tok = count(&view);
        let mut pipeline =
            CompressionPipeline::new(self.context_config.clone()).with_tools(&tool_defs);
        let action = pipeline.force_apply(&mut view, &*self.provider).await;
        let after_tok = count(&view);
        let window = self.context_config.window_tokens;

        // `force_apply` reports L0 with its own action (see `CompressionAction`),
        // but the token delta is the belt-and-braces check: a pass that reports
        // nothing yet changed the prompt must not be called a no-op.
        let changed = !matches!(action, CompressionAction::None) || after_tok < before_tok;
        info!(
            session = %session_id,
            action = ?action,
            before_tok,
            after_tok,
            "Forge: /compact"
        );

        // Emit the usage event whether or not anything changed, so the UI can
        // refresh its context meter and the user always gets an answer.
        let _ = event_tx.send(StreamEvent::ContextCompressed {
            before_tokens: before_tok,
            after_tokens: after_tok,
            window,
        });
        let text = if changed {
            let pct = if window > 0 {
                after_tok as f64 / window as f64 * 100.0
            } else {
                0.0
            };
            format!(
                "已压缩({}):{} → {} tokens(窗口 {},约占 {:.1}%)。\n",
                action_label(&action),
                before_tok,
                after_tok,
                window,
                pct
            )
        } else {
            format!(
                "当前上下文无需压缩:{before_tok} tokens(窗口 {window}),已无可安全压缩的内容。\n"
            )
        };
        let _ = event_tx.send(StreamEvent::Token { content: text });
        let _ = event_tx.send(StreamEvent::Complete { usage: None });
        Ok(())
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
            Some(event_tx),
        )
        .await
        {
            Ok((_session, result)) => {
                emit_plan_turn(event_tx, &result).await;
                Ok(())
            }
            Err(e) => {
                let _ = event_tx.send(StreamEvent::Error {
                    content: format!("Failed to start plan: {e}"),
                });
                let _ = event_tx.send(StreamEvent::Complete { usage: None });
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

        match session
            .answer_with_llm(&*self.provider, answer, Some(event_tx))
            .await
        {
            Ok(result) => {
                emit_plan_turn(event_tx, &result).await;
                Ok(())
            }
            Err(e) => {
                let _ = event_tx.send(StreamEvent::Error {
                    content: format!("Plan error: {e}"),
                });
                let _ = event_tx.send(StreamEvent::Complete { usage: None });
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
                let _ = event_tx.send(StreamEvent::Complete { usage: None });
                Err(ForgeError::EmptyResponse)
            }
        }
    }
}

/// Emit plan turn markdown (chunked for progressive UI) + PlanQuestion buttons.
async fn emit_plan_turn(
    event_tx: &tokio::sync::mpsc::UnboundedSender<StreamEvent>,
    result: &PlanTurnResult,
) {
    let md = format_question(result);
    // Chunk markdown so the question/PRD appears progressively after status lines.
    let chars: Vec<char> = md.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let end = (i + 48).min(chars.len());
        let chunk: String = chars[i..end].iter().collect();
        let _ = event_tx.send(StreamEvent::Token { content: chunk });
        i = end;
        // Yield so the desktop event loop can paint between chunks.
        if i < chars.len() {
            tokio::task::yield_now().await;
        }
    }
    if let Some(pq) = plan_question_event(result) {
        let _ = event_tx.send(pq);
    }
    let _ = event_tx.send(StreamEvent::Complete { usage: None });
}

/// Overall SSE stream budget (seconds): a hard ceiling on one provider turn.
const STREAM_TOTAL_BUDGET_SECS: u64 = 300;
/// SSE idle timeout (seconds): no chunk at all for this long means the
/// connection is dead even if it was never closed.
const STREAM_IDLE_TIMEOUT_SECS: u64 = 90;

/// One provider turn plus the facts the ReAct loop needs that
/// [`ChatResponse`] cannot carry.
///
/// `ChatResponse` is shared with `providers/**` and its shape is not ours to
/// change, so the stream-level signals travel alongside it.
struct StreamedTurn {
    response: ChatResponse,
    /// `Some(reason)` when the stream was cut **before** the provider
    /// signalled completion: `response` holds a prefix, and any tool-call
    /// arguments in it may be half a JSON document. The caller must not
    /// execute those tools or report `Complete`.
    truncated: Option<String>,
    /// The provider's `finish_reason` ("stop", "length", "content_filter", …).
    /// Previously computed and then discarded with `let _ = finish_reason;`,
    /// which made a max_tokens cut look like a normal answer.
    finish_reason: Option<String>,
}

/// Emit a non-streamed response's thinking/text and wrap it as a complete
/// turn. Every `chat()` fallback path in [`stream_provider_turn`] ends here.
fn completed_turn(
    response: ChatResponse,
    event_tx: &tokio::sync::mpsc::UnboundedSender<StreamEvent>,
    iteration: u32,
) -> StreamedTurn {
    if let Some(ref reasoning) = response.reasoning_content {
        if !reasoning.is_empty() {
            let _ = event_tx.send(StreamEvent::Thinking {
                content: reasoning.clone(),
                step: iteration,
            });
        }
    }
    if !response.content.is_empty() {
        let _ = event_tx.send(StreamEvent::Token {
            content: response.content.clone(),
        });
    }
    StreamedTurn {
        response,
        truncated: None,
        finish_reason: None,
    }
}

/// Whether `input` invokes the built-in command `cmd`.
///
/// Matches `cmd` exactly, or `cmd` followed by a separator (whitespace or
/// `:`), so `/compact` matches while `/compacter` does not. Used for
/// `/compact` only: the `/plan`, `/auto` and `/teams` checks keep their
/// historical `starts_with`, where `/planet` still hits `/plan`.
fn invokes_command(input: &str, cmd: &str) -> bool {
    match input.strip_prefix(cmd) {
        Some(rest) => {
            rest.is_empty() || rest.starts_with(char::is_whitespace) || rest.starts_with(':')
        }
        None => false,
    }
}

/// Short user-facing name for the level a compression action came from.
fn action_label(action: &CompressionAction) -> &'static str {
    match action {
        CompressionAction::None => "无",
        CompressionAction::Snipped { .. } => "L0 工具输出剪裁",
        CompressionAction::Truncated { .. } => "L1 截断",
        CompressionAction::TagCompressed { .. } => "L2 工具输出压缩",
        CompressionAction::LlmSummarized { .. } => "L3 LLM 摘要",
        CompressionAction::Chunked { .. } => "L4 分块",
    }
}

/// Turn an abnormal `finish_reason` into a user-visible note.
///
/// Returns `None` for a normal stop (and for providers that omit the field).
fn finish_reason_note(finish_reason: Option<&str>) -> Option<&'static str> {
    match finish_reason.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("length") | Some("max_tokens") => Some(
            "the model hit its max_tokens limit — the reply was cut off and any tool \
             arguments may be incomplete; re-issue the call split into smaller pieces",
        ),
        Some("content_filter") => Some(
            "the provider filtered part of the reply (content_filter); treat the \
             result as incomplete",
        ),
        _ => None,
    }
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
) -> Result<StreamedTurn, ProviderError> {
    use std::collections::BTreeMap;

    let stream_result = provider.chat_stream(messages.clone(), tools.clone()).await;

    let mut stream = match stream_result {
        Ok(s) => s,
        Err(e) => {
            // An over-window rejection answers a question about the *body*:
            // the unary call would carry the same history and be rejected the
            // same way, for another billed generation. Surface it so the
            // caller can compress and re-send instead of paying for a
            // guaranteed failure.
            if e.is_context_overflow() {
                warn!(%e, "SSE stream open rejected as over-window — not falling back to chat()");
                return Err(e);
            }
            warn!(%e, "SSE stream open failed — falling back to chat()");
            let r = provider.chat(messages, tools).await?;
            return Ok(completed_turn(r, event_tx, iteration));
        }
    };

    let mut content = String::new();
    let mut reasoning = String::new();
    // index → (id, name, arguments)
    let mut tool_acc: BTreeMap<u32, (String, String, String)> = BTreeMap::new();
    let mut usage = None;
    let mut finish_reason: Option<String> = None;
    // True only when a chunk carried an actual payload. A provider that sends
    // heartbeat/keep-alive frames (`data: ` with all-None fields) must not
    // count as "we got something", or a stream that then stalls returns an
    // empty response instead of falling back to `chat()`.
    let mut got_payload = false;
    // Why the stream ended early, when it did.
    let mut truncated: Option<String> = None;

    // Overall stream budget: prevent infinite hang
    let deadline = tokio::time::Instant::now()
        + std::time::Duration::from_secs(STREAM_TOTAL_BUDGET_SECS);

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            warn!("SSE stream overall timeout — using partial response");
            truncated = Some(format!(
                "no completion after {STREAM_TOTAL_BUDGET_SECS}s"
            ));
            break;
        }

        let next = tokio::time::timeout(
            std::time::Duration::from_secs(STREAM_IDLE_TIMEOUT_SECS),
            stream.next(),
        )
        .await;
        match next {
            Ok(Some(Ok(chunk))) => {
                let had_payload = chunk
                    .reasoning_content
                    .as_deref()
                    .map_or(false, |s| !s.is_empty())
                    || chunk.content.as_deref().map_or(false, |s| !s.is_empty())
                    || chunk.tool_calls.as_ref().map_or(false, |d| !d.is_empty());
                if had_payload {
                    got_payload = true;
                }
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
                                // Set once, never append: gateways repeat the
                                // full function name on every delta, and
                                // `push_str` turned that into `do_readdo_bash`.
                                if entry.1.is_empty() {
                                    entry.1 = name;
                                }
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
                if got_payload && (!content.is_empty() || !tool_acc.is_empty()) {
                    warn!("using partial SSE response after chunk error");
                    truncated = Some(format!("stream error mid-response: {e}"));
                    break;
                }
                // Some gateways report an over-window prompt as an error frame
                // on a 200 stream; the unary retry would send the same body.
                if e.is_context_overflow() {
                    warn!(%e, "SSE error frame is an over-window rejection — not falling back to chat()");
                    return Err(e);
                }
                // Fall back to non-stream
                warn!(%e, "SSE failed with no content — falling back to chat()");
                let r = provider.chat(messages, tools).await?;
                return Ok(completed_turn(r, event_tx, iteration));
            }
            Ok(None) => break,
            Err(_timeout) => {
                warn!("SSE idle timeout ({}s)", STREAM_IDLE_TIMEOUT_SECS);
                if got_payload {
                    truncated =
                        Some(format!("no data for {STREAM_IDLE_TIMEOUT_SECS}s"));
                    break;
                }
                warn!("SSE idle with no data — falling back to chat()");
                let r = provider.chat(messages, tools).await?;
                return Ok(completed_turn(r, event_tx, iteration));
            }
        }
    }

    // If the provider told us *why* it stopped, the message itself is
    // complete — a stream that ends right after `finish_reason` is only
    // missing trailing usage frames, not content.
    if finish_reason.is_some() {
        truncated = None;
    }

    // If stream produced nothing useful, fall back
    if !got_payload && content.is_empty() && tool_acc.is_empty() {
        warn!("SSE produced empty response — falling back to chat()");
        let r = provider.chat(messages, tools).await?;
        return Ok(completed_turn(r, event_tx, iteration));
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

    Ok(StreamedTurn {
        response: ChatResponse {
            content,
            tool_calls,
            usage,
            reasoning_content: if reasoning.is_empty() {
                None
            } else {
                Some(reasoning)
            },
        },
        truncated,
        finish_reason,
    })
}

/// Validate that messages sent to the provider have intact tool chains.
///
/// Returns the **authoritative provider view**: cloned messages with orphaned
/// tool rows dropped, unpaired `tool_calls` stripped, duplicated rows merged
/// and ghost assistants removed, plus any issues logged. Callers should meter
/// tokens, apply compression and run stall detection on this vector rather
/// than on the raw history — a history that still contains orphan tool rows
/// describes a request the model will never see.
pub fn validate_tool_chain_for_provider(mut messages: Vec<Message>) -> Vec<Message> {
    clean_orphaned_tool_calls(&mut messages);
    // Remove consecutive duplicates — Tool/Assistant rows only.
    //
    // This exists to clean up rows the storage layer wrote twice (the same
    // assistant persisted more than once). Applying it to `User` messages is
    // wrong: a user who sends the same text twice really did ask twice, and
    // `[User(X), User(X)]` (which is exactly what a failed turn that persisted
    // no assistant reply produces when the user resends) was collapsed into a
    // single request, so two explicit instructions produced one action.
    let mut i = 1;
    while i < messages.len() {
        let dedupable_role = matches!(messages[i].role, Role::Tool | Role::Assistant)
            && messages[i - 1].role == messages[i].role;
        let same_tc_ids = messages[i-1].tool_calls.as_ref().map(|tc| tc.iter().map(|t| &t.id).collect::<Vec<_>>())
            == messages[i].tool_calls.as_ref().map(|tc| tc.iter().map(|t| &t.id).collect::<Vec<_>>());
        if dedupable_role
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

    // Position-aware tool chain pairing.
    //
    // Old logic collected ALL tool-response ids up front, then dropped Tool
    // messages whose id wasn't "declared" by an earlier assistant. When history
    // was out of order (Tool row before its assistant — SQLite same-second
    // ordering, compression rebuilds, desktop re-inserts), the Tool message was
    // dropped while the assistant KEPT its tool_calls → provider 400
    // "assistant message with 'tool_calls' must be followed by tool messages".
    //
    // New logic pairs each declared tool_call_id with the FIRST unused response
    // that appears AFTER the declaring assistant, without crossing a User/System
    // turn boundary. Only paired responses are kept, and assistants only keep
    // tool_calls that were actually paired.
    let mut resp_pos: std::collections::HashMap<String, Vec<usize>> = std::collections::HashMap::new();
    for (i, m) in messages.iter().enumerate() {
        if m.role == Role::Tool {
            if let Some(ref id) = m.tool_call_id {
                resp_pos.entry(id.clone()).or_default().push(i);
            }
        }
    }

    // Pair declarations → responses (index-aware, order-preserving).
    let mut used_resp: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut kept: std::collections::HashSet<(usize, String)> = std::collections::HashSet::new();
    for (i, m) in messages.iter().enumerate() {
        if m.role == Role::Assistant {
            if let Some(ref tc) = m.tool_calls {
                for t in tc.iter() {
                    if let Some(poss) = resp_pos.get(&t.id) {
                        if let Some(&p) = poss
                            .iter()
                            .find(|&&p| p > i && !used_resp.contains(&p))
                        {
                            // Do not pair across anything that is not a Tool
                            // message: a User/System turn boundary, or another
                            // Assistant (whose own tool_calls and results sit
                            // in between). `[A(tc=[a,b]), T(a), A(tc=[c]), T(b)]`
                            // used to pair `b` with a response that belongs to
                            // a later assistant, which the provider rejects.
                            let crossing = messages[i + 1..p]
                                .iter()
                                .any(|mm| mm.role != Role::Tool);
                            if !crossing {
                                used_resp.insert(p);
                                kept.insert((i, t.id.clone()));
                            }
                        }
                    }
                }
            }
        }
    }

    // Rebuild with only the paired tool chain.
    let mut result = Vec::new();
    for (i, m) in messages.into_iter().enumerate() {
        if m.role == Role::Assistant {
            let mut m = m;
            if let Some(ref mut tc) = m.tool_calls {
                // `kept` is keyed by `(index, id)`, so a duplicated tool_call
                // id inside one assistant would survive it while only a single
                // response was paired — the provider then sees two identical
                // tool_call ids for one result. De-duplicate by id as well.
                let mut seen: std::collections::HashSet<String> =
                    std::collections::HashSet::new();
                tc.retain(|t| kept.contains(&(i, t.id.clone())) && seen.insert(t.id.clone()));
                if tc.is_empty() {
                    m.tool_calls = None;
                }
            }
            result.push(m);
        } else if m.role == Role::Tool {
            // Keep only responses that were actually paired with a declaration.
            if used_resp.contains(&i) {
                result.push(m);
            }
        } else {
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
                ..Default::default()
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
                ..Default::default()
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
                ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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

    #[test]
    fn validate_drops_out_of_order_tool_chain() {
        // Regression: Tool response BEFORE its assistant (bad SQLite ordering /
        // compression rebuild / desktop re-insert) previously produced a 400
        // "assistant message with 'tool_calls' must be followed by tool messages"
        // because the Tool msg was dropped while the assistant kept its tool_calls.
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
            tool_result("call_x"), // out-of-order: no assistant declared it yet
            assistant_tc(&["call_x"]),
            Message {
                role: Role::Assistant,
                content: MessageContent::Text("done".into()),
                ..Default::default()
            },
        ];
        let out = validate_tool_chain_for_provider(msgs);
        let tc_assts: Vec<_> = out
            .iter()
            .filter(|m| m.role == Role::Assistant && m.tool_calls.is_some())
            .collect();
        assert!(
            tc_assts.is_empty(),
            "assistant must not keep unpaired tool_calls"
        );
        assert_eq!(
            out.iter().filter(|m| m.role == Role::Tool).count(),
            0,
            "orphan tool dropped"
        );
        assert!(out.iter().any(|m| {
            m.role == Role::Assistant
                && m.content.as_text().map_or(false, |c| c == "done")
        }));
    }

    #[test]
    fn validate_keeps_in_order_tool_chain() {
        let msgs = vec![
            assistant_tc(&["a", "b"]),
            tool_result("a"),
            tool_result("b"),
            Message {
                role: Role::Assistant,
                content: MessageContent::Text("answer".into()),
                ..Default::default()
            },
        ];
        let out = validate_tool_chain_for_provider(msgs);
        let tc_assts: Vec<_> = out
            .iter()
            .filter(|m| m.role == Role::Assistant && m.tool_calls.is_some())
            .collect();
        assert_eq!(tc_assts.len(), 1);
        assert_eq!(tc_assts[0].tool_calls.as_ref().unwrap().len(), 2);
        assert_eq!(out.iter().filter(|m| m.role == Role::Tool).count(), 2);
    }

    #[test]
    fn validate_pairs_first_response_only() {
        // Duplicate responses for one id: only the first (after the assistant)
        // is paired; the extra Tool message must be dropped.
        let msgs = vec![
            assistant_tc(&["a"]),
            tool_result("a"),
            tool_result("a"),
            Message {
                role: Role::Assistant,
                content: MessageContent::Text("answer".into()),
                ..Default::default()
            },
        ];
        let out = validate_tool_chain_for_provider(msgs);
        let tc_assts: Vec<_> = out
            .iter()
            .filter(|m| m.role == Role::Assistant && m.tool_calls.is_some())
            .collect();
        assert_eq!(tc_assts.len(), 1);
        assert_eq!(tc_assts[0].tool_calls.as_ref().unwrap().len(), 1);
        assert_eq!(
            out.iter().filter(|m| m.role == Role::Tool).count(),
            1,
            "duplicate response dropped"
        );
    }

    #[test]
    fn validate_does_not_pair_across_another_assistant() {
        // `[A(tc=[a,b]), T(a), A(tc=[c]), T(c), T(b)]` — `b`'s response sits
        // after a second assistant, so it does not belong to the first one.
        // Pairing it anyway handed the first assistant a `b` whose matching tool
        // message was not the one immediately following it (provider 400).
        //
        // The second assistant must carry a response of its own: with a bare
        // `A(tc=[c])`, `clean_orphaned_tool_calls` drops it before pairing runs,
        // the barrier disappears, and the input collapses to
        // `[A(tc=[a,b]), T(a), T(b)]` — which is a valid chain.
        let msgs = vec![
            assistant_tc(&["a", "b"]),
            tool_result("a"),
            assistant_tc(&["c"]),
            tool_result("c"),
            tool_result("b"),
        ];
        let out = validate_tool_chain_for_provider(msgs);
        let tc_assts: Vec<_> = out
            .iter()
            .filter(|m| m.role == Role::Assistant && m.tool_calls.is_some())
            .collect();
        assert_eq!(tc_assts.len(), 2, "got {out:#?}");
        let ids: Vec<&str> = tc_assts[0]
            .tool_calls
            .as_ref()
            .unwrap()
            .iter()
            .map(|t| t.id.as_str())
            .collect();
        assert_eq!(
            ids,
            vec!["a"],
            "`b` must not be paired across another assistant"
        );
        // `c` still pairs with its own response; `b`'s orphaned one is dropped.
        assert_eq!(out.iter().filter(|m| m.role == Role::Tool).count(), 2);
    }

    #[test]
    fn validate_deduplicates_tool_call_ids_within_one_assistant() {
        // `kept` is keyed by (index, id): the same id declared twice in one
        // assistant survived while only one response was paired.
        let msgs = vec![assistant_tc(&["a", "a"]), tool_result("a")];
        let out = validate_tool_chain_for_provider(msgs);
        let tc_assts: Vec<_> = out
            .iter()
            .filter(|m| m.role == Role::Assistant && m.tool_calls.is_some())
            .collect();
        assert_eq!(tc_assts.len(), 1);
        assert_eq!(
            tc_assts[0].tool_calls.as_ref().unwrap().len(),
            1,
            "duplicate tool_call id must be collapsed"
        );
        assert_eq!(out.iter().filter(|m| m.role == Role::Tool).count(), 1);
    }

    #[test]
    fn validate_keeps_a_user_message_sent_twice() {
        // A user who resends the same instruction really did ask twice. The
        // row de-duplication is for duplicated storage rows, and must not touch
        // User messages — folding them collapsed two explicit requests into one.
        let msgs = vec![
            Message {
                role: Role::User,
                content: MessageContent::Text("run the tests".into()),
                ..Default::default()
            },
            Message {
                role: Role::User,
                content: MessageContent::Text("run the tests".into()),
                ..Default::default()
            },
        ];
        let out = validate_tool_chain_for_provider(msgs);
        assert_eq!(
            out.iter().filter(|m| m.role == Role::User).count(),
            2,
            "a repeated user instruction must survive"
        );
    }

    #[test]
    fn validate_deduplicates_repeated_assistant_rows() {
        // Two identical assistant rows (double-persisted turn) still collapse.
        let msgs = vec![
            assistant_tc(&["a"]),
            assistant_tc(&["a"]),
            tool_result("a"),
        ];
        let out = validate_tool_chain_for_provider(msgs);
        assert_eq!(
            out.iter().filter(|m| m.role == Role::Assistant).count(),
            1,
            "duplicate assistant row must be dropped: {out:#?}"
        );
        assert_eq!(out.iter().filter(|m| m.role == Role::Tool).count(), 1);
    }

    /// A command must match on a token boundary: `/compacter` is not
    /// `/compact`. The helper accepts an argument or `:` suffix because that
    /// is how a command with arguments would be written (`/plan: fix it`),
    /// but never a longer word — which is exactly what the historical
    /// `starts_with` checks get wrong (`/planet` still hits `/plan`).
    #[test]
    fn command_matching_stops_at_a_token_boundary() {
        assert!(invokes_command("/plan", "/plan"));
        assert!(invokes_command("/plan build it", "/plan"));
        assert!(invokes_command("/plan: build it", "/plan"));
        assert!(!invokes_command("/planet", "/plan"));
        assert!(!invokes_command("/plans", "/plan"));

        assert!(invokes_command("/auto", "/auto"));
        assert!(invokes_command("/auto do x", "/auto"));
        assert!(!invokes_command("/autopilot", "/auto"));

        assert!(invokes_command("/compact", "/compact"));
        assert!(invokes_command("/compact ", "/compact"));
        assert!(!invokes_command("/compaction", "/compact"));
        assert!(!invokes_command("/foo", "/compact"));
    }

    #[test]
    fn tool_fingerprint_distinguishes_args() {
        // Same tool + same args → same fingerprint (real loop).
        let a = tool_call("1", "do_bash");
        let b = tool_call("2", "do_bash");
        assert_eq!(tool_fingerprint(&a), tool_fingerprint(&b));

        // Same tool + DIFFERENT args → different fingerprint (NOT a loop).
        let mut c = tool_call("3", "do_bash");
        c.function.arguments = "{\"command\":\"ls\"}".into();
        let mut d = tool_call("4", "do_bash");
        d.function.arguments = "{\"command\":\"cargo check\"}".into();
        assert_ne!(tool_fingerprint(&c), tool_fingerprint(&d));

        // Different tool → different fingerprint.
        let e = tool_call("5", "do_web_search");
        assert_ne!(tool_fingerprint(&a), tool_fingerprint(&e));
    }
}
