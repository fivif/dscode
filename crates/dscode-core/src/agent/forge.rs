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

use super::context::{build_context, compression_prompt, count_message_tokens, count_tokens, ContextPacket};
use super::stream::{StreamEvent, ToolStatus};
use crate::extensions::skills::SkillLoader;
use futures::StreamExt;
use crate::providers::trait_def::{
    FunctionCall, LlmProvider, Message, MessageContent, ProviderError, Role, ToolCall, ToolDef,
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

        // --- Build the enriched system prompt ---
        let enriched_system = format!(
            "{}\n\nCurrent working directory: {}",
            self.system_prompt,
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

            // F1+F2: Stall detection — sliding window of tool-call sets.
            // Check every 20 iterations starting at 60, including beyond 180.
            if iteration >= 60 && (iteration <= 180 || iteration % 20 == 0) {
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
                    if counts.values().any(|&c| c >= 3) {
                        let repeated: Vec<String> = current_set.iter().cloned().collect();
                        let _ = event_tx.send(StreamEvent::Error {
                            content: format!("检测到循环调用，已停止。重复的工具: {}", repeated.join(", ")),
                        });
                        return Ok(());
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

            // (a) Call the LLM provider with a snapshot of the cleaned messages,
            // using chat_stream() for SSE streaming instead of blocking chat().
            let snapshot = messages.clone();
            let validated = validate_tool_chain_for_provider(snapshot);
            let mut stream = match self.provider.chat_stream(validated, tools.clone()).await {
                Ok(s) => s,
                Err(e) => {
                    error!(session = %session_id, iteration, %e, "provider error");
                    let _ = event_tx.send(StreamEvent::Error {
                        content: format!("Provider error: {}", e),
                    });
                    return Err(ForgeError::Provider(e));
                }
            };

            // (b) Iterate the SSE stream, emitting Token/Thinking per-chunk
            // and accumulating the full response for tool_calls processing.
            let mut full_content = String::new();
            let mut full_reasoning = String::new();
            let mut tool_calls: Vec<ToolCall> = Vec::new();
            let mut usage = None;

            while let Some(chunk_result) = stream.next().await {
                match chunk_result {
                    Ok(chunk) => {
                        // Emit content delta — each chunk carries pure deltas.
                        if let Some(ref text) = chunk.content {
                            if !text.is_empty() {
                                let _ = event_tx.send(StreamEvent::Token {
                                    content: text.clone(),
                                });
                                full_content.push_str(text);
                            }
                        }

                        // Emit reasoning delta
                        if let Some(ref rc) = chunk.reasoning_content {
                            if !rc.is_empty() {
                                let _ = event_tx.send(StreamEvent::Thinking {
                                    content: rc.clone(),
                                    step: iteration,
                                });
                                full_reasoning.push_str(rc);
                            }
                        }

                        // Accumulate tool call deltas by index
                        if let Some(ref tc_deltas) = chunk.tool_calls {
                            for delta in tc_deltas {
                                let idx = delta.index as usize;
                                while tool_calls.len() <= idx {
                                    tool_calls.push(ToolCall {
                                        id: String::new(),
                                        call_type: "function".to_string(),
                                        function: FunctionCall {
                                            name: String::new(),
                                            arguments: String::new(),
                                        },
                                    });
                                }
                                if let Some(ref id) = delta.id {
                                    tool_calls[idx].id = id.clone();
                                }
                                if let Some(ref func) = delta.function {
                                    if let Some(ref name) = func.name {
                                        tool_calls[idx].function.name = name.clone();
                                    }
                                    if let Some(ref args) = func.arguments {
                                        tool_calls[idx].function.arguments.push_str(args);
                                    }
                                }
                            }
                        }

                        // Capture usage from the final chunk
                        if chunk.usage.is_some() {
                            usage = chunk.usage;
                        }
                    }
                    Err(e) => {
                        warn!(session = %session_id, iteration, %e, "stream chunk parse error");
                    }
                }
            }

            // (c) Build the assistant message from accumulated stream data
            let has_tool_calls = !tool_calls.is_empty();

            let assistant_msg = Message {
                role: Role::Assistant,
                content: MessageContent::Text(full_content.clone()),
                name: None,
                tool_calls: if has_tool_calls {
                    Some(tool_calls.clone())
                } else {
                    None
                },
                tool_call_id: None,
                reasoning_content: if full_reasoning.is_empty() {
                    None
                } else {
                    Some(full_reasoning)
                },
                created_at: 0,
            };

            // (d) Execute tool calls if present
            if has_tool_calls {
                debug!(
                    session = %session_id,
                    iteration,
                    tool_count = tool_calls.len(),
                    "Forge: executing tool calls"
                );

                // F8: Push assistant message only when tool execution proceeds,
                // and before tool results so the API sees Assistant→Tool ordering.
                messages.push(assistant_msg);

                for tc in &tool_calls {
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
            if !full_content.is_empty() {
                info!(
                    session = %session_id,
                    iteration,
                    "Forge: agent finished with final answer"
                );
                let _ = event_tx.send(StreamEvent::Complete { usage });
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

    /// Mark compression as done for this session.
    fn mark_compressed(&self) { self.compressed.store(true, Ordering::Relaxed); }
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
    // Remove consecutive duplicates (ignoring created_at)
    let mut i = 1;
    while i < messages.len() {
        if messages[i-1].role == messages[i].role
            && messages[i-1].content == messages[i].content
            && messages[i-1].tool_calls == messages[i].tool_calls
            && messages[i-1].tool_call_id == messages[i].tool_call_id
            && messages[i-1].reasoning_content == messages[i].reasoning_content
            && messages[i-1].name == messages[i].name
        {
            messages.remove(i);
        } else {
            i += 1;
        }
    }

    // Second pass: ensure no assistant message with tool_calls lacks follow-up tool messages
    let responded: std::collections::HashSet<String> = messages
        .iter().filter(|m| m.role == Role::Tool)
        .filter_map(|m| m.tool_call_id.clone()).collect();

    // Track tool_call_ids from assistant messages that have tool_calls
    let mut expected_responses: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    for m in &messages {
        if let Some(ref tc) = m.tool_calls {
            for t in tc.iter() {
                expected_responses.insert(t.id.clone());
            }
        }
    }

    // Find orphaned: tool_calls without responses
    let orphaned: std::collections::HashSet<_> = expected_responses
        .difference(&responded).cloned().collect();

    if !orphaned.is_empty() {
        warn!("Forge: found {} orphaned tool_calls, removing from messages", orphaned.len());
        for m in &mut messages {
            if let Some(ref mut tc) = m.tool_calls {
                tc.retain(|t| !orphaned.contains(&t.id));
                if tc.is_empty() {
                    m.tool_calls = None;
                    m.tool_call_id = None;
                }
            }
        }
    }

    // Strip tool_call_id on ALL Assistant messages (belongs only on Tool messages)
    for m in &mut messages {
        if m.role == Role::Assistant && m.tool_call_id.is_some() {
            m.tool_call_id = None;
        }
    }

    // Final: remove any remaining Tool messages without matching tool_calls
    let valid_ids: std::collections::HashSet<String> = messages.iter()
        .filter_map(|m| m.tool_calls.as_ref())
        .flat_map(|tc| tc.iter().map(|t| t.id.clone())).collect();

    messages.retain(|m| {
        if m.role != Role::Tool { return true; }
        m.tool_call_id.as_ref().map_or(false, |id| valid_ids.contains(id))
    });

    messages
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
