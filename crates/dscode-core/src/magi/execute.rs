//! Balthasar brain — executes tasks based on scrutiny feedback.
//!
//! Balthasar wraps a tool-equipped ReAct agent loop. It receives the PRD
//! and Casper's scrutiny report, runs the agent with access to the full
//! tool registry, and returns the combined execution output.

use std::path::PathBuf;
use std::sync::Arc;

use futures::FutureExt;
use tracing::{debug, warn};

#[allow(unused_imports)]
use crate::providers::trait_def::{
    LlmProvider, Message, MessageContent, Role, ToolCall, ToolDef,
};
use crate::tools::registry::ToolRegistry;
#[allow(unused_imports)]
use crate::safety::guard::SafetyGuard;
use crate::safety::permission::PermissionHub;
use crate::tools::trait_def::ToolContext;

const MAX_TOOL_RESULT_CHARS: usize = 24_000;

use crate::agent::stream::{StreamEvent, ToolStatus};
use super::scheduler::MagiError;

/// Optional UI progress sink for MAGI /auto (prevents "silent hang" UX).
pub type MagiProgressTx = tokio::sync::mpsc::UnboundedSender<StreamEvent>;

/// Progress context for a Balthasar run (lives on the subtask agent card + main stream).
#[derive(Clone)]
pub struct MagiProgress {
    pub tx: MagiProgressTx,
    /// e.g. `subtask-1` — drives TeamAgentOutput on the UI card
    pub agent_id: String,
}

impl MagiProgress {
    fn emit(&self, event: StreamEvent) {
        let _ = self.tx.send(event);
    }

    fn heartbeat(&self, line: impl Into<String>) {
        let line = line.into();
        self.emit(StreamEvent::TeamAgentOutput {
            agent_id: self.agent_id.clone(),
            content: format!("{line}\n"),
        });
        self.emit(StreamEvent::Token {
            content: format!("_{line}_\n"),
        });
    }
}

/// Per-call LLM timeout — avoids infinite "排队/执行中" when the API stalls.
const CHAT_TIMEOUT_SECS: u64 = 120;
/// Soft cap on a single tool execution.
const TOOL_TIMEOUT_SECS: u64 = 90;

/// The system prompt that primes Balthasar for execution.
const BALTHASAR_SYSTEM_PROMPT: &str = r#"You are Balthasar, the execution brain of the MAGI system.
Your job is to implement code changes based on:

1. The PRD (Product Requirements Document)
2. Casper's scrutiny report with specific focus areas

Use the available tools to read, write, edit, and execute code.
Focus on the areas highlighted by Casper's review.
Produce clean, well-tested, working code.

Be efficient: prefer concrete file edits over long analysis. Do not re-scan the whole monorepo.

After completing your work, provide a summary of:
- What you changed or created
- What files were modified
- What still needs attention (if anything)"#;

/// Execute a subtask using the agent loop.
///
/// # Arguments
/// * `progress` — optional UI sink; when set, emits step/tool heartbeats so the UI is not stuck.
///
/// # Returns
/// The combined text output from the agent's execution rounds.
pub async fn execute_subtask(
    provider: &dyn LlmProvider,
    tools: &Arc<ToolRegistry>,
    working_dir: &PathBuf,
    session_id: &str,
    prd: &str,
    scrutiny: &str,
    max_steps: u32,
    progress: Option<&MagiProgress>,
    safety_guard: Arc<SafetyGuard>,
    permission_hub: Option<Arc<PermissionHub>>,
    permission_timeout_secs: u64,
    cancel: Option<&tokio_util::sync::CancellationToken>,
    write_lock: Option<&Arc<tokio::sync::Mutex<()>>>,
) -> Result<String, MagiError> {
    let tool_defs = tools.to_openai_tools();
    // A zero step budget would silently do no work at all.
    let max_steps = max_steps.max(1);

    // Build conversation messages
    let mut messages = Vec::new();

    // System prompt
    messages.push(Message {
        role: Role::System,
        content: MessageContent::Text(format!(
            "{}\n\nCurrent working directory: {}",
            BALTHASAR_SYSTEM_PROMPT,
            working_dir.display()
        )),
        name: None,
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None, created_at: 0, });

    // User prompt with PRD + scrutiny
    let user_prompt = build_execution_prompt(prd, scrutiny);
    messages.push(Message {
        role: Role::User,
        content: MessageContent::Text(user_prompt),
        name: None,
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None, created_at: 0, });

    let mut final_output = String::new();
    let mut empty_responses = 0u32;

    // ── ReAct loop ──
    for step in 1..=max_steps {
        // Cooperative cancel — checked before every LLM call and tool batch.
        if cancel.map(|t| t.is_cancelled()).unwrap_or(false) {
            return Err(MagiError::cancelled());
        }

        // Shared position-aware validator (same fix as forge.rs) so the provider
        // never receives an assistant(tool_calls) without its Tool responses.
        messages = crate::agent::forge::validate_tool_chain_for_provider(messages);

        debug!(
            session = %session_id,
            step,
            max_steps,
            msg_count = messages.len(),
            "Balthasar: calling provider"
        );

        if let Some(p) = progress {
            p.heartbeat(format!("Auto step {step}/{max_steps} — thinking…"));
        }

        let response = match tokio::time::timeout(
            std::time::Duration::from_secs(CHAT_TIMEOUT_SECS),
            provider.chat(messages.clone(), tool_defs.clone()),
        )
        .await
        {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => return Err(MagiError::provider(e)),
            Err(_) => {
                warn!(
                    session = %session_id,
                    step,
                    timeout_s = CHAT_TIMEOUT_SECS,
                    "Balthasar: chat timed out"
                );
                if let Some(p) = progress {
                    p.heartbeat(format!(
                        "Auto step {step}: LLM call timed out after {CHAT_TIMEOUT_SECS}s"
                    ));
                }
                return Err(MagiError::parse(format!(
                    "Auto LLM call timed out after {CHAT_TIMEOUT_SECS}s (step {step})"
                )));
            }
        };

        // Accumulate text content
        if !response.content.is_empty() {
            if !final_output.is_empty() {
                final_output.push('\n');
            }
            final_output.push_str(&response.content);
            if let Some(p) = progress {
                let preview: String = response.content.chars().take(200).collect();
                p.emit(StreamEvent::TeamAgentOutput {
                    agent_id: p.agent_id.clone(),
                    content: format!("{preview}\n"),
                });
            }
        }

        // Build assistant message
        let has_tool_calls = !response.tool_calls.is_empty();
        let assistant_msg = Message {
            role: Role::Assistant,
            content: MessageContent::Text(response.content.clone()),
            name: None,
            tool_calls: if has_tool_calls {
                Some(response.tool_calls.clone())
            } else {
                None
            },
            tool_call_id: None,
            reasoning_content: response.reasoning_content.clone(),
            created_at: 0,
         };
        messages.push(assistant_msg);

        // Execute tool calls if present
        if has_tool_calls {
            debug!(
                session = %session_id,
                step,
                tool_count = response.tool_calls.len(),
                "Balthasar: executing tool calls"
            );

            for tc in &response.tool_calls {
                let tname = tc.function.name.clone();
                if let Some(p) = progress {
                    p.heartbeat(format!("tool `{tname}`…"));
                    p.emit(StreamEvent::ToolStart {
                        id: tc.id.clone(),
                        name: tname.clone(),
                        description: format!("auto / {tname}"),
                        arguments: tc.function.arguments.clone(),
                    });
                }

                let tool_result = execute_balthasar_tool(
                    tools,
                    tc,
                    working_dir,
                    session_id,
                    safety_guard.clone(),
                    permission_hub.clone(),
                    permission_timeout_secs,
                    progress,
                    write_lock,
                )
                .await;

                if let Some(p) = progress {
                    let ok = !tool_result.starts_with("Failed")
                        && !tool_result.starts_with("Tool error")
                        && !tool_result.starts_with("error:");
                    let preview: String = tool_result.chars().take(300).collect();
                    p.emit(StreamEvent::ToolEnd {
                        id: tc.id.clone(),
                        status: if ok {
                            ToolStatus::Success
                        } else {
                            ToolStatus::Error
                        },
                        result: preview.clone(),
                    });
                    p.emit(StreamEvent::TeamAgentOutput {
                        agent_id: p.agent_id.clone(),
                        content: format!("`{tname}` → {preview}\n"),
                    });
                }

                messages.push(Message {
                    role: Role::Tool,
                    content: MessageContent::Text(tool_result),
                    name: None,
                    tool_calls: None,
                    tool_call_id: Some(tc.id.clone()),
                    reasoning_content: None, created_at: 0, });
            }

            // Continue loop so the model can process tool results.
            continue;
        }

        // No tool calls — agent produced a final answer.
        if !response.content.is_empty() {
            debug!(
                session = %session_id,
                step,
                output_len = final_output.len(),
                "Balthasar: execution complete"
            );
            if let Some(p) = progress {
                p.heartbeat(format!("Auto finished at step {step}"));
            }
            return Ok(final_output);
        }

        // Empty response (no content, no tool calls) — nudge once with backoff
        // instead of immediately re-sending the identical request.
        empty_responses += 1;
        warn!(
            session = %session_id,
            step,
            empty_responses,
            "Balthasar: model returned empty response, nudging"
        );
        messages.push(Message {
            role: Role::User,
            content: MessageContent::Text(
                "(Your previous reply was empty. Either call a tool to make progress, \
                 or write a concrete summary of the work. Do not reply with an empty message.)"
                    .into(),
            ),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            created_at: 0,
        });
        if let Some(p) = progress {
            p.heartbeat(format!("Auto step {step}: empty response — retrying"));
        }
        tokio::time::sleep(std::time::Duration::from_millis(500 * empty_responses.min(4) as u64))
            .await;
    }

    // Max steps exhausted without a final answer. This is a failure, not a
    // successful execution — grading a placeholder as the work product is how
    // silent zero-work rounds reached "done".
    warn!(
        session = %session_id,
        steps = max_steps,
        "Balthasar: max steps reached without a final answer"
    );
    if let Some(p) = progress {
        p.heartbeat(format!("Auto hit step budget ({max_steps})"));
    }
    if final_output.is_empty() {
        Err(MagiError::parse(format!(
            "no output within the step budget ({max_steps} steps)"
        )))
    } else {
        Err(MagiError::parse(format!(
            "no final answer within the step budget ({max_steps} steps); partial output: {}",
            final_output.chars().take(300).collect::<String>()
        )))
    }
}

/// Conservative write classification: only known read-only tools skip the
/// cross-spiral write lock (unknown tools are treated as writes).
fn is_write_tool(name: &str) -> bool {
    !matches!(
        name,
        "do_file_read"
            | "do_web_fetch"
            | "do_web_search"
            | "do_deep_search"
            | "do_rss_read"
            | "do_skill_list"
            | "do_task_status"
    )
}

/// Execute a single tool call on behalf of Balthasar and return the
/// result as a plain string for appending to the conversation.
async fn execute_balthasar_tool(
    tools: &ToolRegistry,
    tc: &ToolCall,
    working_dir: &PathBuf,
    session_id: &str,
    safety_guard: Arc<SafetyGuard>,
    permission_hub: Option<Arc<PermissionHub>>,
    permission_timeout_secs: u64,
    progress: Option<&MagiProgress>,
    write_lock: Option<&Arc<tokio::sync::Mutex<()>>>,
) -> String {
    let tool_name = &tc.function.name;

    // Parse arguments
    let args: serde_json::Value = match serde_json::from_str(&tc.function.arguments) {
        Ok(v) => v,
        Err(e) => {
            return format!(
                "Failed to parse arguments for tool '{}': {}",
                tool_name, e
            );
        }
    };

    // Forward permission prompts to the live UI channel so a Confirm-level
    // command can actually be approved; just draining them made every prompt
    // block for `permission_timeout_secs` and then deny.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let sid = session_id.to_string();
    let tname = tool_name.clone();
    let fwd: Option<(MagiProgressTx, String)> =
        progress.map(|p| (p.tx.clone(), p.agent_id.clone()));
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            if matches!(&event, StreamEvent::PermissionRequest { .. }) {
                match &fwd {
                    Some((tx, agent_id)) => {
                        let _ = tx.send(StreamEvent::TeamAgentOutput {
                            agent_id: agent_id.clone(),
                            content: "🔐 permission requested — awaiting confirmation\n".into(),
                        });
                        // The real prompt, so the UI can render + approve it.
                        let _ = tx.send(event);
                    }
                    None => warn!(
                        session = %sid,
                        tool = %tname,
                        "Balthasar: permission request with no UI channel — will be denied on timeout"
                    ),
                }
            } else {
                debug!(
                    session = %sid,
                    tool = %tname,
                    event = ?event,
                    "Balthasar: tool event consumed"
                );
            }
        }
    });

    let mut ctx = ToolContext::simple(
        working_dir.clone(),
        session_id,
        tc.id.clone(),
        tx,
        safety_guard,
    );
    ctx.permission_hub = permission_hub;
    ctx.permission_timeout_secs = permission_timeout_secs;

    // Parallel spirals share one checkout: serialize mutating tools so two
    // Balthasar rounds cannot interleave writes to the same files.
    let _write_guard = match (write_lock, is_write_tool(tool_name)) {
        (Some(lock), true) => Some(lock.lock().await),
        _ => None,
    };

    let fut = tools.execute(tool_name, args, &ctx);
    // A panicking tool would otherwise unwind the whole concurrent batch,
    // killing sibling spirals mid-write. Convert a panic into a tool error.
    let guarded = std::panic::AssertUnwindSafe(fut).catch_unwind();
    match tokio::time::timeout(std::time::Duration::from_secs(TOOL_TIMEOUT_SECS), guarded).await {
        Ok(Ok(Ok(result))) => {
            let raw = if result.success {
                result.output
            } else {
                result.error.as_deref().unwrap_or(&result.output).to_string()
            };
            if raw.chars().count() > MAX_TOOL_RESULT_CHARS {
                let head: String = raw.chars().take(MAX_TOOL_RESULT_CHARS - 60).collect();
                format!("{head}\n…[truncated]…")
            } else {
                raw
            }
        }
        Ok(Ok(Err(e))) => e.to_string(),
        Ok(Err(_panic)) => format!("Tool '{tool_name}' panicked while executing"),
        Err(_) => format!(
            "Tool '{}' timed out after {}s",
            tool_name, TOOL_TIMEOUT_SECS
        ),
    }
}

/// Build the user prompt for Balthasar's execution.
fn build_execution_prompt(prd: &str, scrutiny: &str) -> String {
    format!(
        "## PRD (Product Requirements Document)\n\n{}\n\n\
         ## Casper's Scrutiny Report\n\n{}\n\n\
         ## Your Task\n\n\
         Implement the changes highlighted in Casper's review. \
         Use the available tools to read files, write code, run tests, \
         and execute commands. Focus on the gaps and action items identified above.\n\n\
         When you are finished, provide a clear summary of what you accomplished.",
        prd, scrutiny
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use futures::stream::Stream;
    use std::pin::Pin;

    use crate::providers::trait_def::{ChatResponse, ProviderError};
    use crate::tools::trait_def::{Tool, ToolResult};

    // ------------------------------------------------------------------
    // Stub provider
    // ------------------------------------------------------------------

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
            Ok(guard.remove(0))
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
            unimplemented!()
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
    async fn test_execute_simple_text_response() {
        let provider = StubProvider::new(vec![ChatResponse {
            content: "I implemented the feature.".into(),
            tool_calls: vec![],
            usage: None,
            reasoning_content: None, }]);

        let mut registry = ToolRegistry::new();
        registry.register(EchoTool);

        let result = execute_subtask(
            &provider,
            &Arc::new(registry),
            &PathBuf::from("/tmp"),
            "test-session",
            "PRD: Build a hello world",
            "Scrutiny: Looks fine",
            10,
            None,
            Arc::new(SafetyGuard::new(&[], true)),
            None,
            120,
            None,
            None,
        )
        .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "I implemented the feature.");
    }

    #[tokio::test]
    async fn test_execute_with_tool_calls() {
        use crate::providers::trait_def::FunctionCall;

        let provider = StubProvider::new(vec![
            // First response: request a tool call
            ChatResponse {
                content: String::new(),
                tool_calls: vec![ToolCall {
                    id: "call_1".into(),
                    call_type: "function".into(),
                    function: FunctionCall {
                        name: "do_echo".into(),
                        arguments: r#"{"message": "hello world"}"#.into(),
                    },
                }],
                usage: None,
                reasoning_content: None, },
            // Second response: final answer
            ChatResponse {
                content: "Done with the echo test.".into(),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None, },
        ]);

        let mut registry = ToolRegistry::new();
        registry.register(EchoTool);

        let result = execute_subtask(
            &provider,
            &Arc::new(registry),
            &PathBuf::from("/tmp"),
            "test-session",
            "PRD: Test echo tool",
            "Scrutiny: Verify echo works",
            10,
            None,
            Arc::new(SafetyGuard::new(&[], true)),
            None,
            120,
            None,
            None,
        )
        .await;

        assert!(result.is_ok());
        assert!(result.unwrap().contains("Done"));
    }

    #[tokio::test]
    async fn test_execute_max_steps_exhausted() {
        // Provider always returns tool calls so the loop never ends naturally
        use crate::providers::trait_def::FunctionCall;

        let responses: Vec<ChatResponse> = (0..20)
            .map(|i| ChatResponse {
                content: String::new(),
                tool_calls: vec![ToolCall {
                    id: format!("call_{}", i),
                    call_type: "function".into(),
                    function: FunctionCall {
                        name: "do_echo".into(),
                        arguments: r#"{"message": "loop"}"#.into(),
                    },
                }],
                usage: None,
                reasoning_content: None, })
            .collect();

        let provider = StubProvider::new(responses);

        let mut registry = ToolRegistry::new();
        registry.register(EchoTool);

        let result = execute_subtask(
            &provider,
            &Arc::new(registry),
            &PathBuf::from("/tmp"),
            "test-session",
            "PRD: Loop forever",
            "Scrutiny: needs work",
            3,
            None,
            Arc::new(SafetyGuard::new(&[], true)),
            None,
            120,
            None,
            None,
        )
        .await;

        // Step-budget exhaustion is a failure, not a successful execution.
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("step budget"),
            "expected step budget error, got: {msg}"
        );
    }

    #[test]
    fn test_build_execution_prompt() {
        let prompt = build_execution_prompt("Build API", "Add auth layer");
        assert!(prompt.contains("Build API"));
        assert!(prompt.contains("Add auth layer"));
        assert!(prompt.contains("Your Task"));
    }
}
