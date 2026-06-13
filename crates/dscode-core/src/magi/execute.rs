//! Balthasar brain — executes tasks based on scrutiny feedback.
//!
//! Balthasar wraps a tool-equipped ReAct agent loop. It receives the PRD
//! and Casper's scrutiny report, runs the agent with access to the full
//! tool registry, and returns the combined execution output.

use std::path::PathBuf;
use std::sync::Arc;

use tracing::{debug, warn};

#[allow(unused_imports)]
use crate::providers::trait_def::{
    LlmProvider, Message, MessageContent, Role, ToolCall, ToolDef,
};
use crate::tools::registry::ToolRegistry;
#[allow(unused_imports)]
use crate::tools::trait_def::{ToolContext, ToolError};

use super::scheduler::MagiError;

/// The system prompt that primes Balthasar for execution.
const BALTHASAR_SYSTEM_PROMPT: &str = r#"You are Balthasar, the execution brain of the MAGI system.
Your job is to implement code changes based on:

1. The PRD (Product Requirements Document)
2. Casper's scrutiny report with specific focus areas

Use the available tools to read, write, edit, and execute code.
Focus on the areas highlighted by Casper's review.
Produce clean, well-tested, working code.

After completing your work, provide a summary of:
- What you changed or created
- What files were modified
- What still needs attention (if anything)"#;

/// Execute a subtask using the agent loop.
///
/// # Arguments
/// * `provider` — the primary LLM provider for the agent loop.
/// * `tools` — the shared tool registry.
/// * `working_dir` — the working directory for path resolution.
/// * `session_id` — session identifier for logging.
/// * `prd` — the full PRD text.
/// * `scrutiny` — Casper's latest scrutiny report.
/// * `max_steps` — maximum ReAct iterations before giving up.
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
) -> Result<String, MagiError> {
    let tool_defs = tools.to_openai_tools();

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

    // ── ReAct loop ──
    for step in 1..=max_steps {
        debug!(
            session = %session_id,
            step,
            max_steps,
            msg_count = messages.len(),
            "Balthasar: calling provider"
        );

        let response = provider.chat(messages.clone(), tool_defs.clone()).await?;

        // Accumulate text content
        if !response.content.is_empty() {
            if !final_output.is_empty() {
                final_output.push('\n');
            }
            final_output.push_str(&response.content);
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
                let tool_result = execute_balthasar_tool(
                    tools,
                    tc,
                    working_dir,
                    session_id,
                )
                .await;

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
            return Ok(final_output);
        }

        // Empty response (no content, no tool calls) — treat as a warning but continue.
        warn!(
            session = %session_id,
            step,
            "Balthasar: model returned empty response, retrying"
        );
        // Don't fail immediately — give the model another chance.
    }

    // Max steps exhausted — return whatever output we have.
    warn!(
        session = %session_id,
        steps = max_steps,
        "Balthasar: max steps reached, returning accumulated output"
    );
    if final_output.is_empty() {
        final_output = "(Balthasar produced no output within the step budget)".to_string();
    }
    Ok(final_output)
}

/// Execute a single tool call on behalf of Balthasar and return the
/// result as a plain string for appending to the conversation.
async fn execute_balthasar_tool(
    tools: &ToolRegistry,
    tc: &ToolCall,
    working_dir: &PathBuf,
    session_id: &str,
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

    // Create a proper event channel so tool events are consumed rather than
    // dropped. Spawn a background task that logs or forwards events to a
    // debug callback. This prevents the "dropped receiver" warning that
    // occurs when tools emit events but nobody is listening.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let sid = session_id.to_string();
    let tname = tool_name.clone();
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            debug!(
                session = %sid,
                tool = %tname,
                event = ?event,
                "Balthasar: tool event consumed"
            );
        }
    });

    let ctx = ToolContext {
        working_dir: working_dir.clone(),
        session_id: session_id.to_string(),
        tool_call_id: tc.id.clone(),
        sender: tx,
    };

    match tools.execute(tool_name, args, &ctx).await {
        Ok(result) => {
            if result.success {
                result.output
            } else {
                result.error.as_deref().unwrap_or(&result.output).to_string()
            }
        }
        Err(e) => e.to_string(),
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
        )
        .await;

        // Should succeed but with accumulated output warning
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(
            output.contains("step budget") || output.is_empty(),
            "expected step budget warning or empty output"
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
