//! Anthropic Claude provider — native Messages API implementation.
//!
//! Implements the full `LlmProvider` trait using Anthropic's Messages API,
//! including both non-streaming chat and SSE streaming, tool definitions,
//! and the unique content-blocks message format.

use super::trait_def::*;
use async_trait::async_trait;
use futures::stream::Stream;
use reqwest::Client;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_stream::StreamExt;

// ---------------------------------------------------------------------------
// Provider struct
// ---------------------------------------------------------------------------

pub struct AnthropicProvider {
    pub api_key: String,
    /// Base URL for Anthropic API, e.g. `https://api.anthropic.com` (no `/v1` suffix).
    pub base_url: String,
    pub model: String,
    pub max_tokens: u32,
    pub temperature: f64,
    /// UI `generation.reasoning_effort` → Claude extended thinking budget.
    pub reasoning_effort: Option<String>,
    /// Unary requests (180 s total timeout).
    client: Client,
    /// Streaming requests (no total timeout — see [`http_client`]).
    stream_client: Client,
    /// Thinking blocks captured from the most recent assistant turn, so they
    /// can be replayed verbatim alongside its `tool_use` blocks. Shared with
    /// the SSE parser task that fills it.
    state: Arc<Mutex<TurnState>>,
}

/// Beta features we opt into on Messages requests.
/// - `context-1m-2025-08-07`: 1M context
/// - `interleaved-thinking-2025-05-14`: thinking + tool use in one turn, needed
///   by the fixed-budget families; 4.6+ enable it automatically.
/// - `max-tokens-3-5-sonnet-2024-07-15`: larger max_tokens — only that model
///   understands it, so it is not sent anywhere else.
fn anthropic_beta_header(with_thinking: bool, model: &str) -> String {
    let mut betas: Vec<&str> = vec!["context-1m-2025-08-07"];
    // Only the fixed-budget families need this beta to interleave thinking with
    // tool use; 4.6+/5.x do it natively, so opting them in is at best a no-op
    // and at worst a rejected flag. Gating on the thinking dialect keeps the
    // header honest instead of sending it to every model.
    if with_thinking && thinking_mode_for_model(model) == ThinkingMode::Budget {
        betas.push("interleaved-thinking-2025-05-14");
    }
    if model.to_ascii_lowercase().contains("3-5-sonnet") {
        betas.push("max-tokens-3-5-sonnet-2024-07-15");
    }
    betas.join(",")
}

/// Which extended-thinking dialect the target model speaks.
///
/// Sending `thinking` to a model that has no such feature is a 400 on *every*
/// request, so an unrecognised id degrades to [`ThinkingMode::Unsupported`]
/// (no thinking) rather than guessing "on".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ThinkingMode {
    /// The model has no extended-thinking support — never send `thinking`.
    Unsupported,
    /// `{ "type": "enabled", "budget_tokens": N }` — Claude 3.7 / 4.0 / 4.1 /
    /// 4.5 / Haiku 4.5.
    Budget,
    /// `{ "type": "adaptive" }` plus `output_config.effort` — Claude 4.6+ / 5.x.
    Adaptive,
}

/// Best-effort capability detection from the model id.
fn thinking_mode_for_model(model: &str) -> ThinkingMode {
    let lower = model.trim().to_ascii_lowercase();
    // Drop any surviving `provider/` prefix.
    let m = lower.rsplit('/').next().unwrap_or(lower.as_str());
    if !m.starts_with("claude") {
        return ThinkingMode::Unsupported;
    }
    // Claude 3.x predates extended thinking, except 3.7.
    if m.contains("claude-3-7") {
        return ThinkingMode::Budget;
    }
    if m.contains("claude-3") {
        return ThinkingMode::Unsupported;
    }
    // 4.6 / 4.7 / 4.8 and the Fable family use adaptive thinking.
    if m.contains("4-6") || m.contains("4-7") || m.contains("4-8") || m.contains("fable") {
        return ThinkingMode::Adaptive;
    }
    // A bare `-5` (claude-opus-5, claude-sonnet-5) — but not `4-5`/`3-5`.
    if m.contains("-5") && !m.contains("4-5") {
        return ThinkingMode::Adaptive;
    }
    // 4.0 / 4.1 / 4.5 / Haiku 4.5 take a fixed budget_tokens.
    if m.contains("-4") {
        return ThinkingMode::Budget;
    }
    ThinkingMode::Unsupported
}

/// Map the UI effort knob → (`budget_tokens`, `output_config.effort`).
/// `None` when the user turned thinking off.
fn effort_to_budget(effort: &str) -> Option<(u32, &'static str)> {
    let e = effort.trim().to_ascii_lowercase();
    if e.is_empty() || e == "off" || e == "none" {
        return None;
    }
    Some(match e.as_str() {
        "low" | "minimal" => (4_096, "low"),
        "medium" | "med" => (10_000, "medium"),
        "high" => (16_000, "high"),
        "max" | "ultra" | "maximum" => (32_000, "max"),
        _ => (10_000, "medium"),
    })
}

/// A validated request to turn extended thinking on for this model.
#[derive(Debug, Clone, Copy)]
struct ThinkingPlan {
    mode: ThinkingMode,
    /// `budget_tokens` for [`ThinkingMode::Budget`]; unused for adaptive models.
    budget: u32,
    /// `output_config.effort` for [`ThinkingMode::Adaptive`].
    effort: &'static str,
}

/// Wire-level thinking blocks captured from one assistant turn, replayed
/// verbatim. Anthropic validates the signature against the block's own content,
/// so these are kept exactly as the API produced them.
///
/// A turn is published only when it actually contained a `tool_use` block: a
/// turn with no tool call never needs its thinking replayed, and publishing it
/// would evict the capture the *next* request still needs (the compression
/// pipeline and the empty-response retry both send such turns on the same
/// provider instance).
#[derive(Debug, Default)]
struct TurnState {
    /// First `tool_use` id of the published turn — the key an outgoing
    /// assistant message is matched against.
    key: Option<String>,
    /// Completed `thinking` / `redacted_thinking` blocks of the published turn.
    blocks: Vec<serde_json::Value>,
    /// Blocks accumulated for the turn currently in flight.
    pending: Vec<serde_json::Value>,
    /// First `tool_use` id of the in-flight turn (None = no tool use yet).
    pending_key: Option<String>,
    /// Thinking block currently being assembled from `*_delta` frames.
    open: Option<(String, Option<String>)>,
    /// Usage merged across `message_start` (input/cache) and `message_delta`
    /// (output). Both events are needed, and the agent overwrites its running
    /// usage with the last chunk it sees.
    usage: Option<crate::agent::stream::UsageInfo>,
}

impl TurnState {
    /// Start a new turn: the published capture survives until this turn proves
    /// to be a tool-calling turn.
    fn begin_turn(&mut self) {
        self.pending.clear();
        self.pending_key = None;
        self.open = None;
        self.usage = None;
    }

    /// Move the in-flight turn's blocks into the published capture, or discard
    /// them when the turn called no tools.
    fn publish_turn(&mut self) {
        match self.pending_key.take() {
            Some(key) => {
                self.key = Some(key);
                self.blocks = std::mem::take(&mut self.pending);
            }
            None => self.pending.clear(),
        }
    }

    fn begin_thinking(&mut self) {
        self.open = Some((String::new(), None));
    }

    fn push_thinking(&mut self, text: &str) {
        if let Some((buf, _)) = self.open.as_mut() {
            buf.push_str(text);
        }
    }

    fn set_signature(&mut self, signature: &str) {
        if let Some((_, sig)) = self.open.as_mut() {
            *sig = Some(signature.to_string());
        }
    }

    /// Finish the open thinking block. A block without a signature cannot be
    /// replayed (the API rejects it), so it is dropped loudly rather than
    /// silently poisoning the next request.
    fn close_thinking(&mut self) {
        if let Some((text, sig)) = self.open.take() {
            self.push_thinking_block(&text, sig.as_deref());
        }
    }

    /// Record a complete thinking block for the in-flight turn.
    fn push_thinking_block(&mut self, text: &str, signature: Option<&str>) {
        match signature.filter(|s| !s.is_empty()) {
            Some(signature) => self.pending.push(serde_json::json!({
                "type": "thinking",
                "thinking": text,
                "signature": signature,
            })),
            None => tracing::warn!(
                "Anthropic returned a thinking block without a signature — \
                 it cannot be replayed on the next turn"
            ),
        }
    }

    fn push_redacted(&mut self, data: &str) {
        self.pending.push(serde_json::json!({
            "type": "redacted_thinking",
            "data": data,
        }));
    }

    fn note_tool_use(&mut self, id: &str) {
        if self.pending_key.is_none() && !id.is_empty() {
            self.pending_key = Some(id.to_string());
        }
    }
}

/// Result of looking for thinking blocks to replay for the assistant turn whose
/// `tool_result`s this request is answering.
#[derive(Debug)]
enum Replay {
    /// The history has no assistant tool-use turn — nothing to replay.
    NotNeeded,
    /// Blocks captured for that turn, tagged with its first `tool_use` id.
    Blocks(String, Vec<serde_json::Value>),
    /// A tool-use turn exists but its thinking blocks are gone (e.g. the
    /// session was resumed from disk). Sending the turn without them is a 400,
    /// so the caller must not enable thinking for this request.
    Missing,
}

impl AnthropicProvider {
    fn thinking_enabled(&self) -> bool {
        self.reasoning_effort
            .as_deref()
            .and_then(|e| effort_to_budget(e))
            .is_some()
            && thinking_mode_for_model(&self.model) != ThinkingMode::Unsupported
    }

    /// Validate the user's effort setting against this model's capabilities and
    /// the configured `max_tokens`.
    ///
    /// Never raises `max_tokens` to make room for thinking: the old code turned
    /// a configured 8192 into 36096 behind the user's back. The budget is
    /// shrunk to fit instead, and thinking is turned off when nothing sensible
    /// fits.
    fn thinking_plan(&self) -> Option<ThinkingPlan> {
        let mode = thinking_mode_for_model(&self.model);
        if mode == ThinkingMode::Unsupported {
            return None;
        }
        let (desired, effort) = effort_to_budget(self.reasoning_effort.as_deref()?)?;
        let max_tokens = if self.max_tokens > 0 {
            self.max_tokens
        } else {
            16_384
        };

        let budget = match mode {
            ThinkingMode::Budget => {
                // Leave room for the visible answer after the thinking budget.
                const ANSWER_HEADROOM: u32 = 4_096;
                // Anthropic's documented floor for budget_tokens.
                const MIN_BUDGET: u32 = 1_024;
                if max_tokens < ANSWER_HEADROOM + MIN_BUDGET {
                    tracing::warn!(
                        max_tokens,
                        model = %self.model,
                        "extended thinking disabled: generation.max_tokens leaves no room \
                         for a thinking budget"
                    );
                    return None;
                }
                let budget = desired.min(max_tokens - ANSWER_HEADROOM);
                if budget < desired {
                    // debug, not warn: this is the expected result of
                    // `reasoning_effort = "max"` with a small max_tokens, and it
                    // would otherwise log on every single request.
                    tracing::debug!(
                        configured_budget = desired,
                        used_budget = budget,
                        max_tokens,
                        "thinking budget clamped to fit generation.max_tokens"
                    );
                }
                budget
            }
            // Adaptive thinking sizes itself via output_config.effort.
            ThinkingMode::Adaptive => desired,
            ThinkingMode::Unsupported => return None,
        };

        Some(ThinkingPlan {
            mode,
            budget,
            effort,
        })
    }

    /// Thinking blocks to replay for the assistant turn that the pending
    /// `tool_result`s answer.
    ///
    /// Only that turn needs them: once a later assistant turn (or a new user
    /// turn) exists, the older thinking blocks may be omitted, which is what
    /// every Claude client does to keep the history small — and it is why an
    /// older tool-use turn without thinking blocks is not an error.
    fn replay_for(&self, messages: &[Message]) -> Replay {
        let target = messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, Role::Assistant | Role::User));
        let Some(target) = target else {
            return Replay::NotNeeded;
        };
        if target.role != Role::Assistant {
            // A new user turn is pending — nothing has to be replayed.
            return Replay::NotNeeded;
        }
        let Some(first_id) = target
            .tool_calls
            .as_ref()
            .and_then(|t| t.first())
            .map(|t| t.id.clone())
        else {
            return Replay::NotNeeded;
        };

        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.key.as_deref() == Some(first_id.as_str()) && !state.blocks.is_empty() {
            Replay::Blocks(first_id, state.blocks.clone())
        } else {
            Replay::Missing
        }
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, TurnState> {
        // A panic while holding the lock must not take the whole provider down.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Shared request headers for Messages API.
    fn apply_auth_headers(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let model = self.model.clone();
        req.header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header(
                "anthropic-beta",
                anthropic_beta_header(self.thinking_enabled(), &model),
            )
            .header("Content-Type", "application/json")
    }

    /// Create a provider with default settings.
    pub fn new(api_key: String, model: String) -> Self {
        let base_url = "https://api.anthropic.com".to_string();
        Self {
            api_key,
            base_url,
            model,
            max_tokens: 8192,
            temperature: 0.0,
            reasoning_effort: Some("max".into()),
            client: http_client(None, false),
            stream_client: http_client(None, true),
            state: Arc::new(Mutex::new(TurnState::default())),
        }
    }

    /// Create a provider from the application Config.
    pub fn from_config(model: &str, conf: &crate::config::settings::Config) -> Self {
        let provider_conf = conf
            .provider_for_model(model)
            .unwrap_or_else(|| crate::config::settings::ProviderConfig {
                api_key: String::new(),
                base_url: "https://api.anthropic.com".into(),
                enabled: true,
                use_proxy: false,
                ..Default::default()
            });

        // Strip provider prefix (anthropic/claude-sonnet-4 -> claude-sonnet-4)
        let actual_model = match model.split_once('/') {
            Some((_, m)) => m.to_string(),
            None => model.to_string(),
        };

        // A channel table can exist with an empty `base_url` (hand-edited
        // config); the request URL would then be relative and reqwest would
        // fail with "relative URL without a base".
        let base_url = if provider_conf.base_url.trim().is_empty() {
            "https://api.anthropic.com".to_string()
        } else {
            normalize_api_base_url(&provider_conf.base_url)
        };

        let proxy = conf.proxy_for_model(model);
        Self {
            api_key: provider_conf.api_key,
            base_url,
            model: actual_model,
            max_tokens: conf.generation.max_tokens,
            temperature: conf.generation.temperature,
            reasoning_effort: Some(conf.generation.reasoning_effort.clone()),
            client: http_client(proxy, false),
            stream_client: http_client(proxy, true),
            state: Arc::new(Mutex::new(TurnState::default())),
        }
    }
}

// ---------------------------------------------------------------------------
// LlmProvider trait implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl LlmProvider for AnthropicProvider {
    async fn chat(
        &self,
        messages: Vec<Message>,
        tools: Vec<ToolDef>,
    ) -> Result<ChatResponse, ProviderError> {
        if self.api_key.trim().is_empty() {
            return Err(ProviderError::NoApiKey);
        }

        let request_body = self.build_request_body(&messages, &tools, false);
        let resp = self
            .apply_auth_headers(
                self.client
                    .post(format!("{}/v1/messages", self.base_url)),
            )
            .json(&request_body)
            .send()
            .await?;

        let status = resp.status();
        let raw_body = resp.text().await?;

        if !status.is_success() {
            return Err(api_error_from_body(status.as_u16(), &raw_body, &self.model));
        }

        let body: serde_json::Value = serde_json::from_str(&raw_body).map_err(|e| {
            let preview: String = raw_body.chars().take(200).collect();
            ProviderError::Parse(format!("JSON parse error: {}. Body: {}", e, preview))
        })?;

        // Keep this turn's thinking blocks so the next request (which carries
        // the tool_result) can replay them verbatim. Only a turn that actually
        // used tools is published, so the previous capture survives a turn that
        // did not.
        {
            let mut state = self.lock_state();
            state.begin_turn();
            capture_thinking_from_response(&body, &mut state);
            state.publish_turn();
        }

        parse_anthropic_response(&body)
    }

    async fn chat_stream(
        &self,
        messages: Vec<Message>,
        tools: Vec<ToolDef>,
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<StreamChunk, ProviderError>> + Send>>,
        ProviderError,
    > {
        if self.api_key.trim().is_empty() {
            return Err(ProviderError::NoApiKey);
        }

        let request_body = self.build_request_body(&messages, &tools, true);
        let resp = self
            .apply_auth_headers(
                self.stream_client
                    .post(format!("{}/v1/messages", self.base_url)),
            )
            .json(&request_body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let raw_body = resp.text().await?;
            return Err(api_error_from_body(status.as_u16(), &raw_body, &self.model));
        }

        // Begin a fresh turn — only after the request was accepted, so a failed
        // request keeps the previous turn's capture. The body built above
        // already read it.
        self.lock_state().begin_turn();

        // SSE byte-stream -> parsed frames -> StreamChunk stream
        let byte_stream = resp.bytes_stream();
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<SseFrame, String>>(128);

        tokio::spawn(async move {
            let mut buf: Vec<u8> = Vec::new();
            let mut current_event = String::new();
            let mut current_data = String::new();
            futures::pin_mut!(byte_stream);

            loop {
                let result =
                    tokio::time::timeout(Duration::from_secs(90), byte_stream.next()).await;
                match result {
                    Ok(Some(Ok(bytes))) => {
                        buf.extend_from_slice(&bytes);
                        // Drain complete lines from buffer
                        while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                            let line_bytes = buf.drain(..=pos).collect::<Vec<_>>();
                            let line = String::from_utf8_lossy(
                                &line_bytes[..line_bytes.len() - 1],
                            )
                            .trim()
                            .to_string();

                            if line.is_empty() {
                                // A blank line dispatches the event and resets
                                // *both* buffers (SSE spec), even when the event
                                // carried no data.
                                if current_data.is_empty() {
                                    current_event.clear();
                                } else {
                                    let frame = SseFrame {
                                        event: std::mem::take(&mut current_event),
                                        data: std::mem::take(&mut current_data),
                                    };
                                    if tx.send(Ok(frame)).await.is_err() {
                                        return;
                                    }
                                }
                            } else if let Some(ev) = sse_field(&line, "event") {
                                current_event = ev.trim().to_string();
                            } else if let Some(d) = sse_field(&line, "data") {
                                if !current_data.is_empty() {
                                    current_data.push('\n');
                                }
                                current_data.push_str(d.trim());
                            }
                            // Ignore comment lines (starting with ':') and unknown prefixes
                        }
                    }
                    Ok(Some(Err(e))) => {
                        let _ = tx.send(Err(format!("byte stream: {e}"))).await;
                        return;
                    }
                    Ok(None) => {
                        break;
                    }
                    Err(_timeout) => {
                        tracing::warn!("chat_stream chunk read timeout, closing stream");
                        let _ = tx.send(Err("chunk read timeout (90s)".into())).await;
                        return;
                    }
                }
            }

            // Flush any remaining partial frame (a truncated final frame now
            // surfaces as a parse error instead of being dropped silently).
            if !current_data.is_empty() {
                let frame = SseFrame {
                    event: current_event,
                    data: current_data,
                };
                let _ = tx.send(Ok(frame)).await;
            }
        });

        let parser_state = self.state.clone();
        let stream = tokio_stream::wrappers::ReceiverStream::new(rx)
            .map(move |item| match item {
                Ok(frame) => parse_anthropic_sse_frame(&frame, &parser_state),
                Err(e) => Err(ProviderError::StreamInterrupted(e)),
            });

        Ok(Box::pin(stream))
    }

    fn clone_box(&self) -> Box<dyn LlmProvider> {
        Box::new(Self {
            api_key: self.api_key.clone(),
            base_url: self.base_url.clone(),
            model: self.model.clone(),
            max_tokens: self.max_tokens,
            temperature: self.temperature,
            reasoning_effort: self.reasoning_effort.clone(),
            client: self.client.clone(),
            stream_client: self.stream_client.clone(),
            // Each clone owns its conversation: an inherited capture would
            // replay another session's thinking blocks.
            state: Arc::new(Mutex::new(TurnState::default())),
        })
    }
}

// ---------------------------------------------------------------------------
// Internal helpers – request building
// ---------------------------------------------------------------------------

impl AnthropicProvider {
    fn build_request_body(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
        stream: bool,
    ) -> serde_json::Value {
        let mut plan = self.thinking_plan();

        // Replaying the thinking blocks of the turn whose tool_result this
        // request carries is mandatory while thinking is on. Without them the
        // API answers 400 "Expected 'thinking' ... but found 'text'" — so when
        // they are unavailable (history resumed from disk, turn already
        // evicted) drop thinking for this one request rather than fail every
        // turn.
        let replay = if plan.is_some() {
            self.replay_for(messages)
        } else {
            Replay::NotNeeded
        };
        if matches!(replay, Replay::Missing) {
            tracing::warn!(
                model = %self.model,
                "extended thinking disabled for this request: the assistant turn that used \
                 tools has no replayed thinking blocks (e.g. a session resumed from disk). \
                 Set generation.reasoning_effort = \"off\" to silence this."
            );
            plan = None;
        }

        let (system, anthropic_messages) = self.build_anthropic_messages(messages, &replay);

        let max_tokens = if self.max_tokens > 0 {
            self.max_tokens
        } else {
            16_384
        };

        let mut body = serde_json::json!({
            "model": self.model,
            "max_tokens": max_tokens,
            "messages": anthropic_messages,
            "stream": stream,
        });

        if let Some(sys) = system {
            body["system"] = serde_json::Value::String(sys);
        }

        if !tools.is_empty() {
            body["tools"] = serde_json::json!(build_anthropic_tools(tools));
        }

        match plan {
            Some(plan) => match plan.mode {
                ThinkingMode::Budget => {
                    body["thinking"] = serde_json::json!({
                        "type": "enabled",
                        "budget_tokens": plan.budget,
                    });
                    // Anthropic requires temperature 1 (or unset) with thinking.
                    body["temperature"] = serde_json::json!(1.0);
                }
                ThinkingMode::Adaptive => {
                    // Claude 4.6+ / 5.x: `budget_tokens` is rejected, thinking
                    // is adaptive, and sampling parameters are rejected too.
                    body["thinking"] = serde_json::json!({ "type": "adaptive" });
                    body["output_config"] = serde_json::json!({ "effort": plan.effort });
                }
                ThinkingMode::Unsupported => {}
            },
            None => {
                // No thinking: send the configured temperature even when it is
                // 0.0 — deterministic sampling is a real setting, not "unset".
                body["temperature"] = serde_json::json!(self.temperature);
            }
        }

        body
    }
}

impl AnthropicProvider {
    /// Convert project `Message` objects into Anthropic's content-blocks format.
    ///
    /// Returns `(system_prompt, messages_vec)` where system messages are extracted
    /// into a single concatenated string for the top-level `system` field, and all
    /// other messages are serialized per the Anthropic Messages API spec.
    ///
    /// Consecutive same-role messages are merged into one: Anthropic does not
    /// merge them itself, and forge emits one `Role::Tool` message per parallel
    /// tool call, which would otherwise become consecutive `user` messages.
    fn build_anthropic_messages(
        &self,
        messages: &[Message],
        replay: &Replay,
    ) -> (Option<String>, Vec<serde_json::Value>) {
        let mut system_text = String::new();
        let mut anthropic_msgs: Vec<serde_json::Value> = Vec::new();

        for msg in messages {
            match msg.role {
                Role::System => {
                    let text = msg.content.as_text().unwrap_or("");
                    if !system_text.is_empty() && !text.is_empty() {
                        system_text.push_str("\n\n");
                    }
                    system_text.push_str(text);
                    // Emits no message, so a run of tool results is not broken.
                }
                Role::User => {
                    let blocks =
                        build_user_content_blocks(&msg.content, msg.tool_call_id.as_deref());
                    push_blocks(&mut anthropic_msgs, "user", blocks);
                }
                Role::Assistant => {
                    let thinking: &[serde_json::Value] = match replay {
                        Replay::Blocks(id, blocks)
                            if msg
                                .tool_calls
                                .as_ref()
                                .and_then(|t| t.first())
                                .map(|t| t.id.as_str())
                                == Some(id.as_str()) =>
                        {
                            blocks.as_slice()
                        }
                        _ => &[],
                    };
                    let blocks = build_assistant_content_blocks(
                        &msg.content,
                        msg.tool_calls.as_deref(),
                        thinking,
                    );
                    // An empty assistant message (a reasoning-only turn with no
                    // replayed thinking, say) is rejected by the API — drop it
                    // rather than emit an empty text block.
                    if !blocks.is_empty() {
                        push_blocks(&mut anthropic_msgs, "assistant", blocks);
                    }
                }
                Role::Tool => {
                    // In Anthropic's API, tool results use role:"user" with
                    // tool_result blocks. All of them answering one assistant
                    // tool_use turn must live in the single user message right
                    // after it, which `push_blocks` handles by merging into the
                    // preceding user message.
                    let tool_call_id = msg.tool_call_id.clone().unwrap_or_default();
                    let result_text = msg.content.as_text().unwrap_or("");
                    let block = serde_json::json!({
                        "type": "tool_result",
                        "tool_use_id": tool_call_id,
                        "content": result_text,
                    });
                    push_blocks(&mut anthropic_msgs, "user", vec![block]);
                }
            }
        }

        let system = if system_text.is_empty() {
            None
        } else {
            Some(system_text)
        };
        (system, anthropic_msgs)
    }
}

/// Push a message, merging into the previous one when the role repeats —
/// Anthropic rejects consecutive same-role messages.
///
/// `tool_result` blocks must lead a user message, so a run of them is inserted
/// after the existing tool results rather than blindly appended.
fn push_blocks(msgs: &mut Vec<serde_json::Value>, role: &str, blocks: Vec<serde_json::Value>) {
    if blocks.is_empty() {
        return;
    }
    if let Some(last) = msgs.last_mut() {
        if last["role"].as_str() == Some(role) {
            if let Some(arr) = last["content"].as_array_mut() {
                let tool_results_first = blocks
                    .first()
                    .map(|b| b["type"] == "tool_result")
                    .unwrap_or(false);
                if tool_results_first && role == "user" {
                    let pos = arr
                        .iter()
                        .rposition(|b| b["type"] == "tool_result")
                        .map(|i| i + 1)
                        .unwrap_or(0);
                    arr.splice(pos..pos, blocks);
                } else {
                    arr.extend(blocks);
                }
                return;
            }
        }
    }
    msgs.push(serde_json::json!({ "role": role, "content": blocks }));
}

/// Build content blocks for a user message.  If a `tool_call_id` is present the
/// message represents a tool result and only a single `tool_result` block is emitted.
fn build_user_content_blocks(
    content: &MessageContent,
    tool_call_id: Option<&str>,
) -> Vec<serde_json::Value> {
    // Tool-result variant: the caller already routes via the `tool_call_id` field
    // on the message, but guard against a `Tool` role leaking into this helper.
    if let Some(tc_id) = tool_call_id {
        if tc_id.is_empty() {
            // A tool_result with an empty tool_use_id is rejected; keep the
            // output visible to the model as plain text instead.
            tracing::warn!("tool result without a tool_call_id — sending it as plain text");
            let text = content.as_text().unwrap_or("");
            return if text.is_empty() {
                Vec::new()
            } else {
                vec![serde_json::json!({ "type": "text", "text": text })]
            };
        }
        let text = content.as_text().unwrap_or("");
        return vec![serde_json::json!({
            "type": "tool_result",
            "tool_use_id": tc_id,
            "content": text,
        })];
    }

    match content {
        MessageContent::Text(text) => {
            if text.is_empty() {
                // Anthropic rejects empty text blocks.
                Vec::new()
            } else {
                vec![serde_json::json!({ "type": "text", "text": text })]
            }
        }
        MessageContent::Parts(parts) => parts
            .iter()
            .filter_map(|part| match part {
                ContentPart::Text { text } => {
                    if text.is_empty() {
                        None
                    } else {
                        Some(serde_json::json!({ "type": "text", "text": text }))
                    }
                }
                ContentPart::ToolUse { id, name, input } => Some(serde_json::json!({
                    "type": "tool_use",
                    "id": id,
                    "name": name,
                    "input": input,
                })),
            })
            .collect(),
    }
}

/// Build content blocks for an assistant message.
///
/// `thinking` holds the wire-level thinking blocks captured for this turn; they
/// must precede the `tool_use` blocks they produced, exactly as the API emitted
/// them, because Anthropic validates each signature against its own block.
fn build_assistant_content_blocks(
    content: &MessageContent,
    tool_calls: Option<&[ToolCall]>,
    thinking: &[serde_json::Value],
) -> Vec<serde_json::Value> {
    let mut blocks: Vec<serde_json::Value> = Vec::with_capacity(thinking.len() + 2);
    blocks.extend(thinking.iter().cloned());

    // Inline content (text or pre-parsed parts)
    match content {
        MessageContent::Text(text) => {
            if !text.is_empty() {
                blocks.push(serde_json::json!({
                    "type": "text",
                    "text": text,
                }));
            }
        }
        MessageContent::Parts(parts) => {
            for part in parts {
                match part {
                    ContentPart::Text { text } => {
                        if !text.is_empty() {
                            blocks.push(serde_json::json!({
                                "type": "text",
                                "text": text,
                            }));
                        }
                    }
                    ContentPart::ToolUse { id, name, input } => {
                        blocks.push(serde_json::json!({
                            "type": "tool_use",
                            "id": id,
                            "name": name,
                            "input": input,
                        }));
                    }
                }
            }
        }
    }

    // Explicit tool_calls field
    if let Some(tcs) = tool_calls {
        for tc in tcs {
            blocks.push(serde_json::json!({
                "type": "tool_use",
                "id": tc.id,
                "name": tc.function.name,
                "input": tool_input(&tc.function.arguments),
            }));
        }
    }

    // An empty result means the caller drops the message: Anthropic rejects
    // both an empty `content` array and an empty `text` block.
    blocks
}

/// Parse accumulated tool-call arguments into an object.
///
/// `input` must be a JSON object: a zero-argument call is `{}`, never the JSON
/// literal `null` (or an empty string), both of which the API rejects on replay.
fn tool_input(arguments: &str) -> serde_json::Value {
    match serde_json::from_str::<serde_json::Value>(arguments) {
        Ok(v) if v.is_object() => v,
        _ => serde_json::Value::Object(serde_json::Map::new()),
    }
}

/// Convert project `ToolDef` objects into Anthropic format (using `input_schema`
/// instead of OpenAIs `parameters`).
fn build_anthropic_tools(tools: &[ToolDef]) -> Vec<serde_json::Value> {
    tools
        .iter()
        .map(|t| {
            let mut tool = serde_json::json!({
                "name": t.function.name,
                "input_schema": t
                    .function
                    .parameters
                    .clone()
                    .unwrap_or(serde_json::json!({"type": "object", "properties": {}})),
            });
            if let Some(ref desc) = t.function.description {
                tool["description"] = serde_json::Value::String(desc.clone());
            }
            tool
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Internal helpers – response parsing (non-streaming)
// ---------------------------------------------------------------------------

/// Parse a non-streaming Anthropic Messages API response.
fn parse_anthropic_response(body: &serde_json::Value) -> Result<ChatResponse, ProviderError> {
    let content_blocks = body
        .get("content")
        .and_then(|c| c.as_array())
        .ok_or_else(|| ProviderError::Parse("No 'content' array in response".into()))?;

    let mut text_content = String::new();
    let mut reasoning = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();

    for block in content_blocks {
        match block["type"].as_str() {
            Some("text") => {
                if let Some(t) = block["text"].as_str() {
                    text_content.push_str(t);
                }
            }
            Some("thinking") => {
                // Extended thinking block
                if let Some(t) = block["thinking"].as_str() {
                    reasoning.push_str(t);
                }
            }
            Some("redacted_thinking") => {
                reasoning.push_str("[redacted thinking]\n");
            }
            Some("tool_use") => {
                let id = block["id"].as_str().unwrap_or("").to_string();
                let name = block["name"].as_str().unwrap_or("").to_string();
                let arguments = match block.get("input") {
                    Some(v) if v.is_object() => v.to_string(),
                    // A zero-argument call must replay as `{}`; `null` and `""`
                    // are both rejected by the API on the next request.
                    _ => "{}".to_string(),
                };
                tool_calls.push(ToolCall {
                    id,
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name,
                        arguments,
                    },
                });
            }
            _ => {} // ignore unknown content block types gracefully
        }
    }

    let usage = body.get("usage").map(|u| crate::agent::stream::UsageInfo {
        input_tokens: u["input_tokens"].as_u64().unwrap_or(0),
        output_tokens: u["output_tokens"].as_u64().unwrap_or(0),
        cache_read_tokens: u["cache_read_input_tokens"].as_u64().unwrap_or(0),
        cache_write_tokens: u["cache_creation_input_tokens"]
            .as_u64()
            .unwrap_or(0),
    });

    Ok(ChatResponse {
        content: text_content,
        tool_calls,
        usage,
        reasoning_content: if reasoning.is_empty() {
            None
        } else {
            Some(reasoning)
        },
    })
}

// ---------------------------------------------------------------------------
// Internal helpers – streaming (SSE)
// ---------------------------------------------------------------------------

/// A single SSE event extracted from the byte stream.
#[derive(Debug)]
struct SseFrame {
    event: String,
    data: String,
}

fn empty_chunk() -> StreamChunk {
    StreamChunk {
        content: None,
        tool_calls: None,
        reasoning_content: None,
        finish_reason: None,
        usage: None,
    }
}

/// Anthropic `error.type` → HTTP status, so retry/backoff logic (and the user)
/// can tell a retryable overload from a permanent client error. Previously every
/// stream error was fabricated as a 500.
fn status_for_anthropic_error(kind: &str) -> u16 {
    match kind {
        "invalid_request_error" => 400,
        "authentication_error" => 401,
        "permission_error" => 403,
        "not_found_error" => 404,
        "request_too_large" => 413,
        "rate_limit_error" => 429,
        "api_error" => 500,
        "overloaded_error" => 529,
        _ => 500,
    }
}

/// Merge a usage object into the running totals for this turn.
///
/// `message_start` carries `input_tokens` and the cache counters,
/// `message_delta` carries `output_tokens`; neither is complete on its own and
/// the agent overwrites its running usage with the last chunk it sees.
fn merge_usage(
    prev: Option<crate::agent::stream::UsageInfo>,
    usage: &serde_json::Value,
) -> crate::agent::stream::UsageInfo {
    let mut out = prev.unwrap_or_default();
    if let Some(v) = usage.get("input_tokens").and_then(|v| v.as_u64()) {
        out.input_tokens = v;
    }
    if let Some(v) = usage.get("output_tokens").and_then(|v| v.as_u64()) {
        out.output_tokens = v;
    }
    if let Some(v) = usage.get("cache_read_input_tokens").and_then(|v| v.as_u64()) {
        out.cache_read_tokens = v;
    }
    if let Some(v) = usage
        .get("cache_creation_input_tokens")
        .and_then(|v| v.as_u64())
    {
        out.cache_write_tokens = v;
    }
    out
}

/// Capture the thinking blocks of a non-streaming response so the next request
/// (which carries the matching `tool_result`) can replay them verbatim.
fn capture_thinking_from_response(body: &serde_json::Value, state: &mut TurnState) {
    let Some(blocks) = body.get("content").and_then(|c| c.as_array()) else {
        return;
    };
    for block in blocks {
        match block["type"].as_str() {
            Some("thinking") => {
                state.push_thinking_block(
                    block["thinking"].as_str().unwrap_or(""),
                    block["signature"].as_str(),
                );
            }
            Some("redacted_thinking") => {
                if let Some(data) = block["data"].as_str() {
                    state.push_redacted(data);
                }
            }
            Some("tool_use") => {
                if let Some(id) = block["id"].as_str() {
                    state.note_tool_use(id);
                }
            }
            _ => {}
        }
    }
}

/// Parse a single Anthropic SSE frame into a `StreamChunk`, recording the
/// thinking blocks and usage the turn needs on replay.
fn parse_anthropic_sse_frame(
    frame: &SseFrame,
    state: &Arc<Mutex<TurnState>>,
) -> Result<StreamChunk, ProviderError> {
    // Keep-alive pings — emit an all-None chunk
    if frame.event == "ping" {
        return Ok(empty_chunk());
    }

    // Server-side error event
    if frame.event == "error" {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&frame.data) {
            let err = &json["error"];
            let msg = err["message"]
                .as_str()
                .unwrap_or("Unknown stream error")
                .to_string();
            let kind = err["type"].as_str().unwrap_or("").to_string();
            tracing::warn!(kind = %kind, "Anthropic stream error: {msg}");
            return Err(ProviderError::ApiDetail {
                status: status_for_anthropic_error(&kind),
                kind,
                code: None,
                param: None,
                message: msg,
            });
        }
        tracing::warn!(data = %frame.data, "unparseable Anthropic error event");
        return Err(ProviderError::Parse(format!(
            "Unparseable error event: {}",
            frame.data
        )));
    }

    let data: serde_json::Value = serde_json::from_str(&frame.data).map_err(|e| {
        tracing::warn!(data = %frame.data, "Anthropic SSE frame is not valid JSON");
        ProviderError::Parse(format!("SSE JSON parse error: {}. Data: {}", e, frame.data))
    })?;

    match frame.event.as_str() {
        "content_block_start" => {
            let block = &data["content_block"];
            match block["type"].as_str() {
                Some("tool_use") => {
                    let index = data["index"].as_u64().unwrap_or(0) as u32;
                    let id = block["id"].as_str().unwrap_or("").to_string();
                    let name = block["name"].as_str().unwrap_or("").to_string();
                    state.lock().unwrap_or_else(|e| e.into_inner()).note_tool_use(&id);
                    Ok(StreamChunk {
                        content: None,
                        tool_calls: Some(vec![ToolCallDelta {
                            index,
                            id: Some(id),
                            function: Some(FunctionDelta {
                                name: Some(name),
                                arguments: None,
                            }),
                        }]),
                        reasoning_content: None,
                        finish_reason: None,
                        usage: None,
                    })
                }
                Some("thinking") => {
                    state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .begin_thinking();
                    Ok(empty_chunk())
                }
                Some("redacted_thinking") => {
                    if let Some(d) = block["data"].as_str() {
                        state
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .push_redacted(d);
                    }
                    // Redacted blocks are opaque and must not be surfaced as
                    // reasoning text: the marker is display-only.
                    Ok(StreamChunk {
                        reasoning_content: Some("[redacted thinking]".to_string()),
                        ..empty_chunk()
                    })
                }
                _ => {
                    // text block start — no actionable data
                    Ok(empty_chunk())
                }
            }
        }

        "content_block_stop" => {
            state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .close_thinking();
            Ok(empty_chunk())
        }

        "content_block_delta" => {
            let delta = &data["delta"];
            let delta_type = delta["type"].as_str().unwrap_or("");

            match delta_type {
                "text_delta" => {
                    let text = delta["text"].as_str().map(|s| s.to_string());
                    Ok(StreamChunk {
                        content: text,
                        ..empty_chunk()
                    })
                }
                "input_json_delta" => {
                    let index = data["index"].as_u64().unwrap_or(0) as u32;
                    let partial = delta["partial_json"].as_str().unwrap_or("").to_string();
                    Ok(StreamChunk {
                        tool_calls: Some(vec![ToolCallDelta {
                            index,
                            id: None,
                            function: Some(FunctionDelta {
                                name: None,
                                arguments: Some(partial),
                            }),
                        }]),
                        ..empty_chunk()
                    })
                }
                "thinking_delta" => {
                    let t = delta["thinking"]
                        .as_str()
                        .or_else(|| delta["text"].as_str())
                        .map(|s| s.to_string());
                    if let Some(text) = t.as_deref() {
                        state
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .push_thinking(text);
                    }
                    Ok(StreamChunk {
                        reasoning_content: t,
                        ..empty_chunk()
                    })
                }
                "signature_delta" => {
                    // The signature arrives after the thinking text and is what
                    // makes the block replayable on the next request.
                    if let Some(sig) = delta["signature"].as_str() {
                        state
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .set_signature(sig);
                    }
                    Ok(empty_chunk())
                }
                _ => {
                    // Unknown delta type — ignore
                    Ok(empty_chunk())
                }
            }
        }

        "message_start" => {
            let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
            guard.begin_turn();
            let usage = data
                .get("message")
                .and_then(|m| m.get("usage"))
                .map(|u| merge_usage(guard.usage.take(), u));
            guard.usage = usage.clone();
            Ok(StreamChunk {
                usage,
                ..empty_chunk()
            })
        }

        "message_delta" => {
            let delta = &data["delta"];
            let finish_reason = delta["stop_reason"].as_str().map(|s| s.to_string());
            let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
            let usage = data
                .get("usage")
                .map(|u| merge_usage(guard.usage.take(), u));
            guard.usage = usage.clone();

            Ok(StreamChunk {
                finish_reason,
                usage,
                ..empty_chunk()
            })
        }

        "message_stop" => {
            // message_delta already provides the authoritative stop_reason
            // (end_turn, max_tokens, stop_sequence, tool_use); do not overwrite
            // it with a generic "stop" here.
            let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
            guard.close_thinking();
            guard.publish_turn();
            Ok(empty_chunk())
        }

        _ => {
            // Unknown event type — emit empty chunk
            Ok(empty_chunk())
        }
    }
}

#[cfg(test)]
mod thinking_effort_tests {
    use super::*;

    fn provider_with(model: &str, effort: &str, max_tokens: u32) -> AnthropicProvider {
        AnthropicProvider {
            api_key: "k".into(),
            base_url: "https://api.anthropic.com".into(),
            model: model.into(),
            max_tokens,
            temperature: 0.0,
            reasoning_effort: Some(effort.into()),
            client: Client::new(),
            stream_client: Client::new(),
            state: Arc::new(Mutex::new(TurnState::default())),
        }
    }

    fn provider(effort: &str, max_tokens: u32) -> AnthropicProvider {
        provider_with("claude-sonnet-4", effort, max_tokens)
    }

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

    #[test]
    fn budget_is_clamped_to_the_configured_max_tokens() {
        let p = provider("max", 8192);
        let body = p.build_request_body(&[], &[], false);
        assert_eq!(body["thinking"]["type"], "enabled");
        // The user's 8192 must not be inflated to 36096 behind their back.
        assert_eq!(body["max_tokens"], 8192);
        // ... and Anthropic still requires max_tokens > budget_tokens.
        assert_eq!(body["thinking"]["budget_tokens"], 4096);
        assert_eq!(body["temperature"], 1.0);
        // output_config.effort is rejected by the fixed-budget families.
        assert!(body.get("output_config").is_none());
    }

    #[test]
    fn medium_budget_unclamped_when_it_fits() {
        let p = provider("medium", 20000);
        let body = p.build_request_body(&[], &[], false);
        assert_eq!(body["thinking"]["budget_tokens"], 10000);
        assert_eq!(body["max_tokens"], 20000);
    }

    #[test]
    fn tiny_max_tokens_disables_thinking() {
        let p = provider("max", 1024);
        let body = p.build_request_body(&[], &[], false);
        assert!(body.get("thinking").is_none());
        assert!(body.get("output_config").is_none());
    }

    #[test]
    fn off_no_thinking() {
        let p = provider("off", 8192);
        let body = p.build_request_body(&[], &[], false);
        assert!(body.get("thinking").is_none());
        // 0.0 is a real setting (deterministic), not "unset".
        assert_eq!(body["temperature"], 0.0);
    }

    #[test]
    fn non_thinking_model_gets_no_thinking_field() {
        let p = provider_with("claude-3-5-sonnet-20241022", "max", 8192);
        let body = p.build_request_body(&[], &[], false);
        assert!(body.get("thinking").is_none(), "3.5 Sonnet has no thinking");
        assert!(body.get("output_config").is_none());
        assert_eq!(body["temperature"], 0.0);
    }

    #[test]
    fn adaptive_model_uses_adaptive_thinking() {
        let p = provider_with("claude-opus-5", "max", 8192);
        let body = p.build_request_body(&[], &[], false);
        assert_eq!(body["thinking"]["type"], "adaptive");
        assert_eq!(body["output_config"]["effort"], "max");
        assert!(body.get("temperature").is_none(), "5.x rejects sampling params");
    }

    #[test]
    fn beta_header_omits_irrelevant_flags() {
        assert!(anthropic_beta_header(true, "claude-sonnet-4")
            .contains("interleaved-thinking-2025-05-14"));
        assert!(!anthropic_beta_header(true, "claude-opus-5")
            .contains("interleaved-thinking-2025-05-14"));
        assert!(!anthropic_beta_header(false, "claude-opus-5")
            .contains("max-tokens-3-5-sonnet-2024-07-15"));
        assert!(anthropic_beta_header(false, "claude-3-5-sonnet-20241022")
            .contains("max-tokens-3-5-sonnet-2024-07-15"));
    }

    #[test]
    fn parse_thinking_block() {
        let body = serde_json::json!({
            "content": [
                {"type": "thinking", "thinking": "step by step", "signature": "sig"},
                {"type": "text", "text": "answer"}
            ],
            "usage": {"input_tokens": 1, "output_tokens": 2}
        });
        let r = parse_anthropic_response(&body).unwrap();
        assert_eq!(r.content, "answer");
        assert_eq!(r.reasoning_content.as_deref(), Some("step by step"));
    }

    #[test]
    fn zero_argument_tool_call_replays_as_empty_object() {
        let body = serde_json::json!({
            "content": [{"type": "tool_use", "id": "t1", "name": "list_dir"}],
            "usage": {"input_tokens": 1, "output_tokens": 2}
        });
        let r = parse_anthropic_response(&body).unwrap();
        assert_eq!(r.tool_calls[0].function.arguments, "{}");
        assert_eq!(tool_input("null"), serde_json::json!({}));
        assert_eq!(tool_input(""), serde_json::json!({}));
        assert_eq!(tool_input(r#"{"a":1}"#), serde_json::json!({"a": 1}));
    }

    #[test]
    fn parallel_tool_results_are_merged_into_one_user_message() {
        let p = provider("off", 8192);
        let mut assistant = Message {
            role: Role::Assistant,
            content: MessageContent::Text(String::new()),
            ..Default::default()
        };
        assistant.tool_calls = Some(vec![
            tool_call("t1", "do_web_fetch"),
            tool_call("t2", "do_web_fetch"),
        ]);
        let messages = vec![
            Message {
                role: Role::User,
                content: MessageContent::Text("go".into()),
                ..Default::default()
            },
            assistant,
            Message {
                role: Role::Tool,
                content: MessageContent::Text("A".into()),
                tool_call_id: Some("t1".into()),
                ..Default::default()
            },
            Message {
                role: Role::Tool,
                content: MessageContent::Text("B".into()),
                tool_call_id: Some("t2".into()),
                ..Default::default()
            },
        ];
        let (_, msgs) = p.build_anthropic_messages(&messages, &Replay::NotNeeded);
        // user → assistant → user(tool_result A + B): roles must alternate.
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[2]["role"], "user");
        let blocks = msgs[2]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["tool_use_id"], "t1");
        assert_eq!(blocks[1]["tool_use_id"], "t2");
    }

    #[test]
    fn empty_assistant_message_is_dropped() {
        let p = provider("off", 8192);
        let messages = vec![
            Message {
                role: Role::User,
                content: MessageContent::Text("hi".into()),
                ..Default::default()
            },
            Message {
                role: Role::Assistant,
                content: MessageContent::Text(String::new()),
                reasoning_content: Some("thought only".into()),
                ..Default::default()
            },
        ];
        let (_, msgs) = p.build_anthropic_messages(&messages, &Replay::NotNeeded);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "user");
    }

    #[test]
    fn captured_thinking_is_replayed_before_tool_use() {
        let p = provider("max", 32000);
        let mut assistant = Message {
            role: Role::Assistant,
            content: MessageContent::Text(String::new()),
            ..Default::default()
        };
        assistant.tool_calls = Some(vec![tool_call("t1", "list_dir")]);

        // Simulate the turn that produced this assistant message.
        {
            let mut st = p.lock_state();
            st.begin_turn();
            st.note_tool_use("t1");
            st.pending.push(serde_json::json!({
                "type": "thinking", "thinking": "why", "signature": "sig"
            }));
            st.publish_turn();
        }

        let messages = vec![
            assistant,
            Message {
                role: Role::Tool,
                content: MessageContent::Text("out".into()),
                tool_call_id: Some("t1".into()),
                ..Default::default()
            },
        ];
        let replay = p.replay_for(&messages);
        let body = p.build_request_body(&messages, &[], false);
        assert!(body.get("thinking").is_some(), "replay must keep thinking on");
        let blocks = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(blocks[0]["type"], "thinking");
        assert_eq!(blocks[0]["signature"], "sig");
        assert_eq!(blocks[1]["type"], "tool_use");
        assert!(matches!(replay, Replay::Blocks(id, _) if id.as_str() == "t1"));
    }

    #[test]
    fn missing_thinking_blocks_disable_thinking_instead_of_400() {
        let p = provider("max", 32000);
        let mut assistant = Message {
            role: Role::Assistant,
            content: MessageContent::Text(String::new()),
            ..Default::default()
        };
        assistant.tool_calls = Some(vec![tool_call("t1", "list_dir")]);
        let messages = vec![
            assistant,
            Message {
                role: Role::Tool,
                content: MessageContent::Text("out".into()),
                tool_call_id: Some("t1".into()),
                ..Default::default()
            },
        ];
        // Nothing captured for t1 (e.g. a session resumed from disk).
        let body = p.build_request_body(&messages, &[], false);
        assert!(body.get("thinking").is_none());
    }

    #[test]
    fn an_older_tool_turn_does_not_force_a_replay() {
        let p = provider("max", 32000);
        let mut assistant = Message {
            role: Role::Assistant,
            content: MessageContent::Text(String::new()),
            ..Default::default()
        };
        assistant.tool_calls = Some(vec![tool_call("t1", "list_dir")]);
        let messages = vec![
            assistant,
            Message {
                role: Role::Tool,
                content: MessageContent::Text("out".into()),
                tool_call_id: Some("t1".into()),
                ..Default::default()
            },
            Message {
                role: Role::Assistant,
                content: MessageContent::Text("done".into()),
                ..Default::default()
            },
            Message {
                role: Role::User,
                content: MessageContent::Text("next".into()),
                ..Default::default()
            },
        ];
        // The old tool turn's thinking blocks may be omitted — thinking must
        // stay on for the new user turn.
        let body = p.build_request_body(&messages, &[], false);
        assert!(body.get("thinking").is_some());
    }

    #[test]
    fn stream_captures_signature_and_input_usage() {
        let state = Arc::new(Mutex::new(TurnState::default()));
        let frame = |event: &str, data: &str| SseFrame {
            event: event.into(),
            data: data.into(),
        };

        parse_anthropic_sse_frame(
            &frame(
                "message_start",
                r#"{"message":{"usage":{"input_tokens":11,"output_tokens":1,"cache_read_input_tokens":7}}}"#,
            ),
            &state,
        )
        .unwrap();
        parse_anthropic_sse_frame(
            &frame(
                "content_block_start",
                r#"{"index":0,"content_block":{"type":"thinking","thinking":""}}"#,
            ),
            &state,
        )
        .unwrap();
        parse_anthropic_sse_frame(
            &frame(
                "content_block_delta",
                r#"{"index":0,"delta":{"type":"thinking_delta","thinking":"hmm"}}"#,
            ),
            &state,
        )
        .unwrap();
        parse_anthropic_sse_frame(
            &frame(
                "content_block_delta",
                r#"{"index":0,"delta":{"type":"signature_delta","signature":"SIG"}}"#,
            ),
            &state,
        )
        .unwrap();
        parse_anthropic_sse_frame(&frame("content_block_stop", r#"{"index":0}"#), &state).unwrap();
        parse_anthropic_sse_frame(
            &frame(
                "content_block_start",
                r#"{"index":1,"content_block":{"type":"tool_use","id":"t9","name":"list_dir"}}"#,
            ),
            &state,
        )
        .unwrap();
        let last = parse_anthropic_sse_frame(
            &frame(
                "message_delta",
                r#"{"delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":42}}"#,
            ),
            &state,
        )
        .unwrap();
        parse_anthropic_sse_frame(&frame("message_stop", "{}"), &state).unwrap();

        // message_start's input/cache counters survive the message_delta overwrite.
        let usage = last.usage.unwrap();
        assert_eq!(usage.input_tokens, 11);
        assert_eq!(usage.cache_read_tokens, 7);
        assert_eq!(usage.output_tokens, 42);
        assert_eq!(last.finish_reason.as_deref(), Some("tool_use"));

        let st = state.lock().unwrap();
        assert_eq!(st.key.as_deref(), Some("t9"));
        assert_eq!(st.blocks[0]["type"], "thinking");
        assert_eq!(st.blocks[0]["signature"], "SIG");
    }

    #[test]
    fn a_tool_less_turn_keeps_the_previous_capture() {
        let state = Arc::new(Mutex::new(TurnState::default()));
        let frame = |event: &str, data: &str| SseFrame {
            event: event.into(),
            data: data.into(),
        };
        let play = |events: &[(&str, &str)]| {
            for &(e, d) in events {
                let _ = parse_anthropic_sse_frame(&frame(e, d), &state);
            }
        };

        // Turn 1: thinking + tool_use.
        play(&[
            ("message_start", r#"{"message":{"usage":{"input_tokens":1}}}"#),
            (
                "content_block_start",
                r#"{"index":0,"content_block":{"type":"thinking"}}"#,
            ),
            (
                "content_block_delta",
                r#"{"index":0,"delta":{"type":"thinking_delta","thinking":"a"}}"#,
            ),
            (
                "content_block_delta",
                r#"{"index":0,"delta":{"type":"signature_delta","signature":"SIG1"}}"#,
            ),
            ("content_block_stop", r#"{"index":0}"#),
            (
                "content_block_start",
                r#"{"index":1,"content_block":{"type":"tool_use","id":"t1","name":"x"}}"#,
            ),
            ("message_stop", "{}"),
        ]);
        assert_eq!(state.lock().unwrap().key.as_deref(), Some("t1"));

        // Turn 2 (a compression / summary call): thinking but no tool_use.
        play(&[
            ("message_start", r#"{"message":{"usage":{"input_tokens":1}}}"#),
            (
                "content_block_start",
                r#"{"index":0,"content_block":{"type":"thinking"}}"#,
            ),
            (
                "content_block_delta",
                r#"{"index":0,"delta":{"type":"thinking_delta","thinking":"b"}}"#,
            ),
            (
                "content_block_delta",
                r#"{"index":0,"delta":{"type":"signature_delta","signature":"SIG2"}}"#,
            ),
            ("content_block_stop", r#"{"index":0}"#),
            ("message_stop", "{}"),
        ]);

        // The capture the pending tool_result still needs must survive.
        let st = state.lock().unwrap();
        assert_eq!(st.key.as_deref(), Some("t1"));
        assert_eq!(st.blocks.len(), 1);
        assert_eq!(st.blocks[0]["signature"], "SIG1");
    }

    #[test]
    fn stream_error_event_maps_to_a_real_status() {
        let state = Arc::new(Mutex::new(TurnState::default()));
        let err = parse_anthropic_sse_frame(
            &SseFrame {
                event: "error".into(),
                data: r#"{"type":"error","error":{"type":"overloaded_error","message":"busy"}}"#
                    .into(),
            },
            &state,
        )
        .unwrap_err();
        assert_eq!(err.status(), Some(529));
        assert_eq!(err.error_kind(), Some("overloaded_error"));
        assert!(err.is_retryable());
    }

    #[test]
    fn sse_field_tolerates_missing_space() {
        assert_eq!(sse_field("data:{\"a\":1}", "data"), Some("{\"a\":1}"));
        assert_eq!(sse_field("data: {\"a\":1}", "data"), Some("{\"a\":1}"));
        assert_eq!(sse_field("event: ping", "event"), Some("ping"));
        assert_eq!(sse_field("data-only", "data"), None);
    }

    #[test]
    fn thinking_mode_detection() {
        assert_eq!(
            thinking_mode_for_model("claude-3-5-sonnet-20241022"),
            ThinkingMode::Unsupported
        );
        assert_eq!(
            thinking_mode_for_model("anthropic/claude-3-7-sonnet"),
            ThinkingMode::Budget
        );
        assert_eq!(
            thinking_mode_for_model("claude-haiku-4-5-20251001"),
            ThinkingMode::Budget
        );
        assert_eq!(
            thinking_mode_for_model("claude-sonnet-4-5"),
            ThinkingMode::Budget
        );
        assert_eq!(
            thinking_mode_for_model("claude-opus-4-6"),
            ThinkingMode::Adaptive
        );
        assert_eq!(thinking_mode_for_model("claude-opus-5"), ThinkingMode::Adaptive);
        assert_eq!(
            thinking_mode_for_model("claude-fable-5-1"),
            ThinkingMode::Adaptive
        );
        // Unknown ids must fail safe (no thinking), never 400 every request.
        assert_eq!(
            thinking_mode_for_model("some-gateway/claude-next"),
            ThinkingMode::Unsupported
        );
    }
}
