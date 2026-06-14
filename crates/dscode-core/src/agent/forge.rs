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

use tracing::{debug, error, info, warn};

use super::context::{build_context, compression_prompt, count_message_tokens, ContextPacket};
use super::stream::{StreamEvent, ToolStatus};
use crate::extensions::skills::SkillLoader;
use crate::providers::trait_def::{
    LlmProvider, Message, MessageContent, ProviderError, Role, ToolCall,
};
use crate::tools::registry::ToolRegistry;
use crate::tools::trait_def::{ToolContext, ToolError};
use crate::config::settings::ContextConfig;

/// The default system prompt injected at the start of every conversation.
pub const DEFAULT_SYSTEM_PROMPT: &str =
    "You are a coding agent. You have access to tools. Think step by step, use tools when needed, and write clean code.";

/// Maximum number of ReAct iterations before the agent stops.
const DEFAULT_MAX_ITERATIONS: u32 = u32::MAX;

/// Maximum number of historical messages to include in the context window.
const DEFAULT_MAX_HISTORY_MESSAGES: usize = 100;

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
        }
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

    /// Override the context window configuration.
    pub fn with_context_config(mut self, cfg: ContextConfig) -> Self {
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

        // --- Detect slash commands and inject mode prompts ---
        let mode_prompt = if user_message.starts_with("/plan") {
            "\n\n## MODE: /plan — 5-Phase PRD Deep Interview\n\
            Conduct a structured product requirements discovery. Follow these phases:\n\
            1. SCOPE — Understand project boundaries and goals\n\
            2. REQUIREMENTS — Deep-dive into features, constraints, dependencies\n\
            3. DESIGN — Architecture, component design, data models\n\
            4. RISKS — Technical risks, mitigations, trade-offs\n\
            5. QUALITY — Success criteria, testing strategy, acceptance\n\
            Ask probing questions. Challenge assumptions. Output a comprehensive PRD."
        } else if user_message.starts_with("/auto") {
            "\n\n## MODE: /auto — Continuous Improvement Spiral\n\
            Operate in an endless spiral: ASSESS → EXECUTE → PROMOTE.\n\
            - ASSESS: Examine current state, identify gaps, prioritize improvements\n\
            - EXECUTE: Take concrete actions — write/fix code, run tests, make changes\n\
            - PROMOTE: Refine, optimize, document, and prepare for next spiral\n\
            DO NOT STOP until the task is genuinely complete. Each spiral must produce tangible progress."
        } else if user_message.starts_with("/teams") {
            // /teams: extract task description, decompose, execute subtasks in this loop
            let task_desc = user_message.strip_prefix("/teams").unwrap_or(user_message).trim();
            let task = if task_desc.is_empty() { "Analyze and improve the current project" } else { task_desc };
            return self.run_teams_task(task, session_id, history, event_tx).await;
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

        // --- Check for matching skills ---
        let mut skill_prompt = String::new();
        let mut allowed_tool_patterns: Vec<String> = vec![];
        let mut loader = SkillLoader::new();
        if let Ok(count) = loader.load_from_dir(&SkillLoader::default_skills_dir()) {
            if count > 0 {
                let matches = loader.find_matching(user_message);
                for skill in matches {
                    skill_prompt.push_str(&format!("\n## Active Skill: {}\n{}\n", skill.name, skill.body));
                    allowed_tool_patterns.extend(skill.allowed_tools.clone());
                    info!(session = %session_id, skill = %skill.name, "skill activated");
                }
            }
        }

        // --- Build initial context ---
        let enriched_with_skill = if skill_prompt.is_empty() {
            enriched_system
        } else {
            format!("{}\n{}\n---\nFollow the above skill instructions when applicable.", enriched_system, skill_prompt)
        };
        let wiki_nodes: &[String] = &[];
        let ContextPacket { mut messages, tools } = build_context(
            &history,
            &enriched_with_skill,
            &tool_defs,
            wiki_nodes,
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

        // =================================================================
        // ReAct Loop
        // =================================================================
        for iteration in 1..=self.max_iterations {
            debug!(
                session = %session_id,
                iteration,
                msg_count = messages.len(),
                "Forge: calling provider"
            );

            // Stall detection — sliding window of tool-call sets.
            // Check every iteration starting at 60.
            if iteration >= 60 {
                // Only scan messages added during this execute() call (F5).
                let run_messages = &messages[initial_msg_count..];
                let current_set: std::collections::BTreeSet<String> = run_messages
                    .iter()
                    .rev()
                    .filter_map(|m| m.tool_calls.as_ref())
                    .flat_map(|tc| tc.iter().map(|t| t.function.name.clone()))
                    .collect();
                if !current_set.is_empty() {
                    if recent_tool_sets.len() >= 5 {
                        recent_tool_sets.pop_front();
                    }
                    recent_tool_sets.push_back(current_set.clone());
                    // Detect if any set repeats 3+ times in the sliding window
                    let mut counts: std::collections::HashMap<&std::collections::BTreeSet<String>, usize> =
                        std::collections::HashMap::new();
                    for s in recent_tool_sets.iter() {
                        *counts.entry(s).or_insert(0) += 1;
                    }
                    if counts.values().any(|&c| c >= 10) {
                        let repeated: Vec<String> = current_set.iter().cloned().collect();
                        let _ = event_tx.send(StreamEvent::Token {
                            content: format!("\n\n⚠️ Tool loop detected ({} iterations). Consider consolidating results and concluding.\n", iteration),
                        });
                        // Don't stop — let the agent decide to wrap up
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
                            messages = vec![Message { role: Role::System, content: MessageContent::Text(format!("{}\n\n## Conversation Summary\n{}", sys, summary)), ..Default::default() }];
                            messages.extend(rest);
                            self.compressed.store(true, Ordering::Relaxed);
                        }
                    }
                }
            }

            // F10: Clean orphaned tool_calls on the original vec so the
            // fix persists across iterations (new messages appended to original).
            clean_orphaned_tool_calls(&mut messages);

            // (a) Call the LLM provider — reliable non-streaming
            let snapshot = messages.clone();
            let validated = validate_tool_chain_for_provider(snapshot);

            let response = match self.provider.chat(validated, tools.clone()).await {
                Ok(r) => r,
                Err(e) => {
                    error!(session = %session_id, iteration, %e, "provider error");
                    let _ = event_tx.send(StreamEvent::Error { content: format!("Provider error: {}", e) });
                    return Err(ForgeError::Provider(e));
                }
            };

            // Emit thinking
            if let Some(ref reasoning) = response.reasoning_content {
                if !reasoning.is_empty() {
                    let _ = event_tx.send(StreamEvent::Thinking { content: reasoning.clone(), step: iteration });
                }
            }
            // Emit text as one chunk (reliable, no hang)
            if !response.content.is_empty() {
                let _ = event_tx.send(StreamEvent::Token { content: response.content.clone() });
            }

            let has_tool_calls = !response.tool_calls.is_empty();
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
                debug!(
                    session = %session_id,
                    iteration,
                    tool_count = response.tool_calls.len(),
                    "Forge: executing tool calls"
                );

                // F8: Push assistant message only when tool execution proceeds,
                // and before tool results so the API sees Assistant→Tool ordering.
                messages.push(assistant_msg);

                for tc in &response.tool_calls {
                    execute_one_tool(
                        &self.tools,
                        tc,
                        &self.working_dir,
                        session_id,
                        &event_tx,
                        &mut messages,
                    )
                    .await;
                }

                // Loop again so the model can process tool results.
                continue;
            }

            // (e) No tool calls — this is the final answer.
            if !response.content.is_empty() {
                info!(
                    session = %session_id,
                    iteration,
                    "Forge: agent finished with final answer"
                );
                let _ = event_tx.send(StreamEvent::Complete { usage: response.usage });
                return Ok(());
            }

            // (f) No content and no tool calls — empty response.
            warn!(
                session = %session_id,
                iteration,
                "Forge: model returned empty response"
            );
            let _ = event_tx.send(StreamEvent::Error {
                content: "Model returned an empty response (no content, no tool calls)."
                    .to_string(),
            });
            return Err(ForgeError::EmptyResponse);
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

    /// Execute in /teams mode: decompose → execute subtasks → aggregate.
    async fn run_teams_task(
        &self,
        task: &str,
        session_id: &str,
        history: Vec<Message>,
        event_tx: tokio::sync::mpsc::UnboundedSender<StreamEvent>,
    ) -> Result<(), ForgeError> {
        let _ = event_tx.send(StreamEvent::Token {
            content: format!("## /teams mode activated\n\nTask: {}\n\nDecomposing...\n\n", task),
        });

        // Phase 1: Ask LLM to decompose into subtasks
        let decompose_prompt = format!(
            "You are a project manager. Break down this task into 3-5 independent subtasks.\n\
             For each subtask, give a 1-line description.\n\
             Format: `- [ ] description`\n\nTASK: {}",
            task
        );
        let _ = event_tx.send(StreamEvent::Token { content: "_Asking LLM to decompose..._\n\n".into() });

        // Run decomposition as a simple forge call
        let decompose_result = self.provider.chat(
            vec![Message {
                role: Role::User,
                content: MessageContent::Text(decompose_prompt),
                name: None, tool_calls: None, tool_call_id: None,
                reasoning_content: None, created_at: 0,
            }],
            vec![],
        ).await;

        let subtasks = match decompose_result {
            Ok(r) => {
                let lines: Vec<String> = r.content.lines()
                    .filter(|l| l.trim().starts_with("- [ ]") || l.trim().starts_with("- "))
                    .map(|l| l.trim().trim_start_matches("- [ ]").trim_start_matches("- ").trim().to_string())
                    .collect();
                if lines.is_empty() { vec![task.to_string()] } else { lines }
            }
            Err(_) => vec![task.to_string()],
        };

        let _ = event_tx.send(StreamEvent::Token {
            content: format!("Decomposed into {} subtasks:\n\n", subtasks.len()),
        });
        for (i, st) in subtasks.iter().enumerate() {
            let _ = event_tx.send(StreamEvent::Token {
                content: format!("{}. {}\n", i + 1, st),
            });
        }

        // Phase 2: Execute each subtask sequentially
        for (i, subtask) in subtasks.iter().enumerate() {
            let _ = event_tx.send(StreamEvent::Token {
                content: format!("\n---\n### Subtask {}/{}: {}\n\n", i + 1, subtasks.len(), subtask),
            });

            // Run subtask as a direct LLM call (no recursive forge)
            let sub_result = self.provider.chat(
                vec![Message {
                    role: Role::User,
                    content: MessageContent::Text(format!("Complete this subtask using available tools:\n\n{}", subtask)),
                    name: None, tool_calls: None, tool_call_id: None, reasoning_content: None, created_at: 0,
                }],
                self.tools.to_openai_tools(),
            ).await;
            match sub_result {
                Ok(r) => {
                    let _ = event_tx.send(StreamEvent::Token { content: format!("\n{}\n\n✅ Subtask {}/{} complete.\n", r.content, i+1, subtasks.len()) });
                }
                Err(e) => {
                    let _ = event_tx.send(StreamEvent::Token { content: format!("\n❌ Subtask {}/{} failed: {}\n", i+1, subtasks.len(), e) });
                }
            }
        }

        let _ = event_tx.send(StreamEvent::Complete { usage: None });
        Ok(())
    }
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
/// `ToolProgress`, `ToolEnd`). The resulting tool output (or error) is
/// appended to `messages` as a `Role::Tool` message so the LLM can see it
/// on the next turn.
async fn execute_one_tool(
    tools: &ToolRegistry,
    tc: &ToolCall,
    working_dir: &PathBuf,
    session_id: &str,
    event_tx: &tokio::sync::mpsc::UnboundedSender<StreamEvent>,
    messages: &mut Vec<Message>,
) {
    let tool_name = &tc.function.name;
    let tool_call_id = &tc.id;

    // --- Look up the tool for its description ---
    let description = tools
        .get(tool_name)
        .map(|t| t.description().to_string())
        .unwrap_or_default();

    // --- Emit ToolStart ---
    let _ = event_tx.send(StreamEvent::ToolStart {
        id: tool_call_id.clone(),
        name: tool_name.clone(),
        description,
        arguments: tc.function.arguments.clone(),
    });

    // --- Parse arguments ---
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
            messages.push(Message {
                role: Role::Tool,
                content: MessageContent::Text(err_msg),
                name: None,
                tool_calls: None,
                tool_call_id: Some(tool_call_id.clone()),
                reasoning_content: None, created_at: 0 });
            return;
        }
    };

    // --- Build tool context ---
    let ctx = ToolContext {
        working_dir: working_dir.clone(),
        session_id: session_id.to_string(),
        tool_call_id: tool_call_id.clone(),
        sender: event_tx.clone(),
    };

    // --- Execute the tool ---
    match tools.execute(tool_name, args, &ctx).await {
        Ok(result) => {
            let status = if result.success {
                ToolStatus::Success
            } else {
                ToolStatus::Error
            };
            let output = if result.success {
                result.output.clone()
            } else {
                result
                    .error
                    .as_deref()
                    .unwrap_or(&result.output)
                    .to_string()
            };

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

            messages.push(Message {
                role: Role::Tool,
                content: MessageContent::Text(output),
                name: None,
                tool_calls: None,
                tool_call_id: Some(tool_call_id.clone()),
                reasoning_content: None, created_at: 0 });
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

            messages.push(Message {
                role: Role::Tool,
                content: MessageContent::Text(err_str),
                name: None,
                tool_calls: None,
                tool_call_id: Some(tool_call_id.clone()),
                reasoning_content: None, created_at: 0 });
        }
    }
}
