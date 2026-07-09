//! Integration tests: Forge ReAct loop + MAGI spiral with a scripted mock provider.
//!
//! These tests exercise the full agent path without network access.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use dscode_core::agent::forge::Forge;
use dscode_core::agent::stream::StreamEvent;
use dscode_core::magi::scheduler::MagiScheduler;
use dscode_core::providers::trait_def::{
    ChatResponse, LlmProvider, Message, ProviderError, StreamChunk, ToolDef,
};
use dscode_core::tools::registry::ToolRegistry;
use dscode_core::tools::trait_def::{Tool, ToolContext, ToolError, ToolResult};
use futures::stream::{self, Stream};
use tokio::sync::mpsc;

// ── Scripted mock provider ──────────────────────────────────────────────────

struct ScriptedProvider {
    responses: Mutex<VecDeque<ChatResponse>>,
    /// If true, chat_stream yields content token-by-token then finish.
    stream_mode: bool,
}

impl ScriptedProvider {
    fn new(responses: Vec<ChatResponse>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            stream_mode: false,
        }
    }

    fn with_stream(mut self) -> Self {
        self.stream_mode = true;
        self
    }

    fn pop(&self) -> ChatResponse {
        let mut q = self.responses.lock().unwrap();
        q.pop_front().unwrap_or(ChatResponse {
            content: "STOP: true\nREASON: default\nQUALITY: 90\nFOCUS: none".into(),
            tool_calls: vec![],
            usage: None,
            reasoning_content: None,
        })
    }
}

#[async_trait]
impl LlmProvider for ScriptedProvider {
    async fn chat(
        &self,
        _messages: Vec<Message>,
        _tools: Vec<ToolDef>,
    ) -> Result<ChatResponse, ProviderError> {
        Ok(self.pop())
    }

    async fn chat_stream(
        &self,
        messages: Vec<Message>,
        tools: Vec<ToolDef>,
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<StreamChunk, ProviderError>> + Send>>,
        ProviderError,
    > {
        if !self.stream_mode {
            // Simulate stream open failure path by returning an error once —
            // forge should fall back to chat(). Use successful single-chunk stream instead
            // so stream path is also tested when stream_mode is false: emit full content as one chunk.
        }
        let r = self.chat(messages, tools).await?;
        let mut chunks = Vec::new();

        if let Some(ref rc) = r.reasoning_content {
            chunks.push(Ok(StreamChunk {
                content: None,
                tool_calls: None,
                reasoning_content: Some(rc.clone()),
                finish_reason: None,
                usage: None,
            }));
        }

        if self.stream_mode && !r.content.is_empty() {
            // Split into small SSE-like deltas
            for part in r.content.as_bytes().chunks(8) {
                let s = String::from_utf8_lossy(part).to_string();
                chunks.push(Ok(StreamChunk {
                    content: Some(s),
                    tool_calls: None,
                    reasoning_content: None,
                    finish_reason: None,
                    usage: None,
                }));
            }
        } else if !r.content.is_empty() {
            chunks.push(Ok(StreamChunk {
                content: Some(r.content.clone()),
                tool_calls: None,
                reasoning_content: None,
                finish_reason: None,
                usage: None,
            }));
        }

        // Stream tool calls as a final assembled delta set
        if !r.tool_calls.is_empty() {
            use dscode_core::providers::trait_def::{FunctionDelta, ToolCallDelta};
            let deltas: Vec<ToolCallDelta> = r
                .tool_calls
                .iter()
                .enumerate()
                .map(|(i, tc)| ToolCallDelta {
                    index: i as u32,
                    id: Some(tc.id.clone()),
                    function: Some(FunctionDelta {
                        name: Some(tc.function.name.clone()),
                        arguments: Some(tc.function.arguments.clone()),
                    }),
                })
                .collect();
            chunks.push(Ok(StreamChunk {
                content: None,
                tool_calls: Some(deltas),
                reasoning_content: None,
                finish_reason: Some("tool_calls".into()),
                usage: None,
            }));
        } else {
            chunks.push(Ok(StreamChunk {
                content: None,
                tool_calls: None,
                reasoning_content: None,
                finish_reason: Some("stop".into()),
                usage: r.usage.clone(),
            }));
        }

        Ok(Box::pin(stream::iter(chunks)))
    }

    fn clone_box(&self) -> Box<dyn LlmProvider> {
        let q = self.responses.lock().unwrap().clone();
        Box::new(Self {
            responses: Mutex::new(q),
            stream_mode: self.stream_mode,
        })
    }
}

// ── Stub tool ───────────────────────────────────────────────────────────────

struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        "do_echo"
    }
    fn description(&self) -> &str {
        "Echo args"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "text": { "type": "string" } }
        })
    }
    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let text = args
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        Ok(ToolResult::ok(format!("echoed:{text}")))
    }
}

fn collect_events(
    mut rx: mpsc::UnboundedReceiver<StreamEvent>,
) -> tokio::task::JoinHandle<Vec<StreamEvent>> {
    tokio::spawn(async move {
        let mut out = Vec::new();
        while let Some(ev) = rx.recv().await {
            out.push(ev);
        }
        out
    })
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn forge_final_answer_via_sse_stream() {
    let provider = ScriptedProvider::new(vec![ChatResponse {
        content: "Hello from stream".into(),
        tool_calls: vec![],
        usage: None,
        reasoning_content: Some("thinking...".into()),
    }])
    .with_stream();

    let mut tools = ToolRegistry::new();
    tools.register(EchoTool);

    let forge = Forge::new(
        Box::new(provider),
        Arc::new(tools),
        PathBuf::from("/tmp"),
    )
    .with_max_iterations(5);

    let (tx, rx) = mpsc::unbounded_channel();
    let collector = collect_events(rx);

    forge
        .execute("hi", "sess-stream", vec![], tx)
        .await
        .expect("forge ok");

    let events = collector.await.unwrap();
    let tokens: String = events
        .iter()
        .filter_map(|e| match e {
            StreamEvent::Token { content } => Some(content.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        tokens.contains("Hello"),
        "expected streamed tokens, got: {tokens:?} events={events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, StreamEvent::Thinking { .. })),
        "expected thinking event"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, StreamEvent::Complete { .. })),
        "expected Complete"
    );
}

#[tokio::test]
async fn forge_tool_call_then_answer() {
    use dscode_core::providers::trait_def::{FunctionCall, ToolCall};

    let provider = ScriptedProvider::new(vec![
        ChatResponse {
            content: String::new(),
            tool_calls: vec![ToolCall {
                id: "call_1".into(),
                call_type: "function".into(),
                function: FunctionCall {
                    name: "do_echo".into(),
                    arguments: r#"{"text":"ping"}"#.into(),
                },
            }],
            usage: None,
            reasoning_content: None,
        },
        ChatResponse {
            content: "Tool done: final".into(),
            tool_calls: vec![],
            usage: None,
            reasoning_content: None,
        },
    ]);

    let mut tools = ToolRegistry::new();
    tools.register(EchoTool);

    let forge = Forge::new(
        Box::new(provider),
        Arc::new(tools),
        PathBuf::from("/tmp"),
    )
    .with_max_iterations(10);

    let (tx, rx) = mpsc::unbounded_channel();
    let collector = collect_events(rx);

    forge
        .execute("use the tool", "sess-tool", vec![], tx)
        .await
        .expect("forge ok");

    let events = collector.await.unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, StreamEvent::ToolStart { name, .. } if name == "do_echo")),
        "expected ToolStart for do_echo: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, StreamEvent::ToolEnd { .. })),
        "expected ToolEnd"
    );
    let tokens: String = events
        .iter()
        .filter_map(|e| match e {
            StreamEvent::Token { content } => Some(content.as_str()),
            _ => None,
        })
        .collect();
    assert!(tokens.contains("final"), "tokens={tokens}");
}

#[tokio::test]
async fn magi_spiral_completes_with_scripted_brains() {
    // Magi uses primary for Casper + Balthasar, runtime for Melchior.
    let primary = ScriptedProvider::new(vec![
        ChatResponse {
            content: "Scrutiny: ok".into(),
            tool_calls: vec![],
            usage: None,
            reasoning_content: None,
        },
        ChatResponse {
            content: "Execution complete".into(),
            tool_calls: vec![],
            usage: None,
            reasoning_content: None,
        },
    ]);
    let runtime = ScriptedProvider::new(vec![ChatResponse {
        content: "STOP: true\nREASON: Task finished\nQUALITY: 95\nFOCUS: None".into(),
        tool_calls: vec![],
        usage: None,
        reasoning_content: None,
    }]);

    let tools = Arc::new(ToolRegistry::new());
    let scheduler = MagiScheduler::new(
        Box::new(primary),
        Box::new(runtime),
        tools,
        PathBuf::from("/tmp"),
    )
    .with_max_rounds(3)
    .with_max_steps_per_round(5);

    let rounds = scheduler
        .run_spiral("Build a hello world module", "magi-sess")
        .await
        .expect("magi spiral ok");

    assert!(!rounds.is_empty());
    let last = rounds.last().unwrap();
    assert!(last.promotion.should_stop);
    assert!(last.promotion.quality_score >= 70.0);
}

#[tokio::test]
async fn plan_llm_json_parse_unit_path() {
    // Smoke: llm_interview parse path (no network)
    use dscode_core::plan::llm_interview::LlmInterviewAction;
    // re-export parse via public API: next_llm_turn needs provider — use scripted
    let provider = ScriptedProvider::new(vec![ChatResponse {
        content: r#"{"action":"ask","question":"Who is the primary user?","recommended":"internal team","auto_notes":["Cargo.toml present"]}"#.into(),
        tool_calls: vec![],
        usage: None,
        reasoning_content: None,
    }]);

    let action = dscode_core::plan::llm_interview::next_llm_turn(
        &provider,
        "build a CLI",
        dscode_core::plan::PlanPhase::Scope,
        &[],
        0,
        "Working directory: /tmp\nTop-level: Cargo.toml",
    )
    .await
    .expect("llm turn");

    match action {
        LlmInterviewAction::Ask { question, recommended, auto_notes } => {
            assert!(question.to_lowercase().contains("user"));
            assert!(!recommended.is_empty());
            assert!(!auto_notes.is_empty());
        }
        other => panic!("expected Ask, got {other:?}"),
    }
}
