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
    client: Client,
}

/// Beta features we opt into on every Messages request.
/// - `context-1m-2025-08-07`: 1M context
/// - `interleaved-thinking-2025-05-14`: thinking + tool use in one turn
/// - `max-tokens-3-5-sonnet-2024-07-15`: larger max_tokens on older paths
fn anthropic_beta_header(with_thinking: bool) -> &'static str {
    if with_thinking {
        "context-1m-2025-08-07,interleaved-thinking-2025-05-14,max-tokens-3-5-sonnet-2024-07-15"
    } else {
        "context-1m-2025-08-07,max-tokens-3-5-sonnet-2024-07-15"
    }
}

/// Map UI effort → thinking budget_tokens (Claude extended thinking).
fn effort_to_budget(effort: &str) -> Option<u32> {
    let e = effort.trim().to_ascii_lowercase();
    if e.is_empty() || e == "off" || e == "none" {
        return None;
    }
    Some(match e.as_str() {
        "low" | "minimal" => 4_096,
        "medium" | "med" => 10_000,
        "high" => 16_000,
        "max" | "ultra" | "maximum" => 32_000,
        _ => 10_000,
    })
}

impl AnthropicProvider {
    fn thinking_enabled(&self) -> bool {
        self.reasoning_effort
            .as_ref()
            .and_then(|e| effort_to_budget(e))
            .is_some()
    }

    /// Shared request headers for Messages API.
    fn apply_auth_headers(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        req.header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header(
                "anthropic-beta",
                anthropic_beta_header(self.thinking_enabled()),
            )
            .header("Content-Type", "application/json")
    }

    /// Create a provider with default settings.
    pub fn new(api_key: String, model: String) -> Self {
        let base_url = "https://api.anthropic.com".to_string();
        let client = crate::config::settings::build_http_client(None)
            .expect("Failed to build HTTP client");
        Self {
            api_key,
            base_url,
            model,
            max_tokens: 8192,
            temperature: 0.0,
            reasoning_effort: Some("max".into()),
            client,
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

        let base_url = provider_conf.base_url.trim_end_matches('/').to_string();

        // Strip provider prefix (anthropic/claude-sonnet-4 -> claude-sonnet-4)
        let actual_model = match model.split_once('/') {
            Some((_, m)) => m.to_string(),
            None => model.to_string(),
        };

        let client = crate::config::settings::build_http_client(conf.proxy_for_model(model))
            .expect("Failed to build HTTP client");

        Self {
            api_key: provider_conf.api_key,
            base_url,
            model: actual_model,
            max_tokens: conf.generation.max_tokens,
            temperature: conf.generation.temperature,
            reasoning_effort: Some(conf.generation.reasoning_effort.clone()),
            client,
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
            let error_msg = serde_json::from_str::<serde_json::Value>(&raw_body)
                .ok()
                .and_then(|v| {
                    v["error"]["message"]
                        .as_str()
                        .map(|s| s.to_string())
                })
                .unwrap_or(raw_body);
            return Err(ProviderError::Api {
                status: status.as_u16(),
                message: error_msg,
            });
        }

        let body: serde_json::Value = serde_json::from_str(&raw_body).map_err(|e| {
            let preview: String = raw_body.chars().take(200).collect();
            ProviderError::Parse(format!("JSON parse error: {}. Body: {}", e, preview))
        })?;

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
                self.client
                    .post(format!("{}/v1/messages", self.base_url)),
            )
            .json(&request_body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let raw_body = resp.text().await?;
            let error_msg = serde_json::from_str::<serde_json::Value>(&raw_body)
                .ok()
                .and_then(|v| {
                    v["error"]["message"]
                        .as_str()
                        .map(|s| s.to_string())
                })
                .unwrap_or(raw_body);
            return Err(ProviderError::Api {
                status: status.as_u16(),
                message: error_msg,
            });
        }

        // SSE byte-stream -> parsed frames -> StreamChunk stream
        let byte_stream = resp.bytes_stream();
        let (tx, rx) = tokio::sync::mpsc::channel::<SseFrame>(128);

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
                                // Empty line delimits the end of an SSE event
                                if !current_data.is_empty() {
                                    let frame = SseFrame {
                                        event: std::mem::take(&mut current_event),
                                        data: std::mem::take(&mut current_data),
                                    };
                                    if tx.send(frame).await.is_err() {
                                        return;
                                    }
                                }
                            } else if let Some(ev) = line.strip_prefix("event: ") {
                                current_event = ev.trim().to_string();
                            } else if let Some(d) = line.strip_prefix("data: ") {
                                if !current_data.is_empty() {
                                    current_data.push('\n');
                                }
                                current_data.push_str(d.trim());
                            }
                            // Ignore comment lines (starting with ':') and unknown prefixes
                        }
                    }
                    Ok(Some(Err(_))) => {
                        return;
                    }
                    Ok(None) => {
                        break;
                    }
                    Err(_timeout) => {
                        tracing::warn!("chat_stream chunk read timeout, closing stream");
                        return;
                    }
                }
            }

            // Flush any remaining partial frame
            if !current_data.is_empty() {
                let frame = SseFrame {
                    event: current_event,
                    data: current_data,
                };
                let _ = tx.send(frame).await;
            }
        });

        let stream = tokio_stream::wrappers::ReceiverStream::new(rx)
            .map(|frame| parse_anthropic_sse_frame(&frame));

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
        let (system, anthropic_messages) = build_anthropic_messages(messages);

        let budget = self
            .reasoning_effort
            .as_deref()
            .and_then(effort_to_budget);

        // Claude requires max_tokens > budget_tokens when thinking is on.
        let mut max_tokens = if self.max_tokens > 0 {
            self.max_tokens
        } else {
            16_384
        };
        if let Some(b) = budget {
            let need = b.saturating_add(4_096);
            if max_tokens <= b {
                max_tokens = need;
            }
        }

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

        // Extended thinking (Claude 3.7 / 4.x). Same UI knob as DeepSeek effort.
        if let Some(b) = budget {
            body["thinking"] = serde_json::json!({
                "type": "enabled",
                "budget_tokens": b,
            });
            // Anthropic docs: temperature must be 1 when thinking is enabled
            // (or omit). Force 1 so gateways don't reject.
            body["temperature"] = serde_json::json!(1.0);
            // Newer models also accept effort on output_config — harmless if ignored
            let effort_str = match b {
                0..=5_000 => "low",
                5_001..=12_000 => "medium",
                12_001..=20_000 => "high",
                _ => "max",
            };
            body["output_config"] = serde_json::json!({ "effort": effort_str });
        } else if self.temperature > 0.0 {
            body["temperature"] = serde_json::json!(self.temperature);
        }

        body
    }
}

/// Convert project `Message` objects into Anthropic's content-blocks format.
///
/// Returns `(system_prompt, messages_vec)` where system messages are extracted
/// into a single concatenated string for the top-level `system` field, and all
/// other messages are serialized per the Anthropic Messages API spec.
fn build_anthropic_messages(messages: &[Message]) -> (Option<String>, Vec<serde_json::Value>) {
    let mut system_text = String::new();
    let mut anthropic_msgs = Vec::new();

    for msg in messages {
        match msg.role {
            Role::System => {
                let text = msg.content.as_text().unwrap_or("");
                if !system_text.is_empty() && !text.is_empty() {
                    system_text.push_str("\n\n");
                }
                system_text.push_str(text);
            }
            Role::User => {
                let content_blocks = build_user_content_blocks(&msg.content, msg.tool_call_id.as_deref());
                anthropic_msgs.push(serde_json::json!({
                    "role": "user",
                    "content": content_blocks,
                }));
            }
            Role::Assistant => {
                let content_blocks = build_assistant_content_blocks(
                    &msg.content,
                    msg.tool_calls.as_deref(),
                );
                anthropic_msgs.push(serde_json::json!({
                    "role": "assistant",
                    "content": content_blocks,
                }));
            }
            Role::Tool => {
                // In Anthropic's API, tool results use role:"user" with tool_result blocks
                let tool_call_id = msg.tool_call_id.clone().unwrap_or_default();
                let result_text = msg.content.as_text().unwrap_or("");
                anthropic_msgs.push(serde_json::json!({
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": tool_call_id,
                        "content": result_text,
                    }],
                }));
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

/// Build content blocks for a user message.  If a `tool_call_id` is present the
/// message represents a tool result and only a single `tool_result` block is emitted.
fn build_user_content_blocks(
    content: &MessageContent,
    tool_call_id: Option<&str>,
) -> Vec<serde_json::Value> {
    // Tool-result variant: the caller already routes via the `tool_call_id` field
    // on the message, but guard against a `Tool` role leaking into this helper.
    if let Some(tc_id) = tool_call_id {
        let text = content.as_text().unwrap_or("");
        return vec![serde_json::json!({
            "type": "tool_result",
            "tool_use_id": tc_id,
            "content": text,
        })];
    }

    match content {
        MessageContent::Text(text) => vec![serde_json::json!({
            "type": "text",
            "text": text,
        })],
        MessageContent::Parts(parts) => parts
            .iter()
            .map(|part| match part {
                ContentPart::Text { text } => serde_json::json!({
                    "type": "text",
                    "text": text,
                }),
                ContentPart::ToolUse { id, name, input } => serde_json::json!({
                    "type": "tool_use",
                    "id": id,
                    "name": name,
                    "input": input,
                }),
            })
            .collect(),
    }
}

/// Build content blocks for an assistant message.  Tool calls from the
/// `tool_calls` field are appended as `tool_use` blocks after any text content.
fn build_assistant_content_blocks(
    content: &MessageContent,
    tool_calls: Option<&[ToolCall]>,
) -> Vec<serde_json::Value> {
    let mut blocks = Vec::new();

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
                        blocks.push(serde_json::json!({
                            "type": "text",
                            "text": text,
                        }));
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
            let input: serde_json::Value =
                serde_json::from_str(&tc.function.arguments).unwrap_or(serde_json::Value::Object(
                    serde_json::Map::new(),
                ));
            blocks.push(serde_json::json!({
                "type": "tool_use",
                "id": tc.id,
                "name": tc.function.name,
                "input": input,
            }));
        }
    }

    // Anthropic requires at least one content block for assistant messages
    if blocks.is_empty() {
        blocks.push(serde_json::json!({
            "type": "text",
            "text": "",
        }));
    }

    blocks
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
                let arguments = block["input"].to_string();
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

/// Parse a single Anthropic SSE frame into a `StreamChunk`.
fn parse_anthropic_sse_frame(frame: &SseFrame) -> Result<StreamChunk, ProviderError> {
    // Keep-alive pings — emit an all-None chunk
    if frame.event == "ping" {
        return Ok(StreamChunk {
            content: None,
            tool_calls: None,
            reasoning_content: None,
            finish_reason: None,
            usage: None,
        });
    }

    // Server-side error event
    if frame.event == "error" {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&frame.data) {
            let msg = json["error"]["message"]
                .as_str()
                .unwrap_or("Unknown stream error")
                .to_string();
            return Err(ProviderError::Api {
                status: 500,
                message: msg,
            });
        }
        return Err(ProviderError::Parse("Unparseable error event".into()));
    }

    let data: serde_json::Value = serde_json::from_str(&frame.data).map_err(|e| {
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
                _ => {
                    // text block start — no actionable data
                    Ok(StreamChunk {
                        content: None,
                        tool_calls: None,
                        reasoning_content: None,
                        finish_reason: None,
                        usage: None,
                    })
                }
            }
        }

        "content_block_delta" => {
            let delta = &data["delta"];
            let delta_type = delta["type"].as_str().unwrap_or("");

            match delta_type {
                "text_delta" => {
                    let text = delta["text"].as_str().map(|s| s.to_string());
                    Ok(StreamChunk {
                        content: text,
                        tool_calls: None,
                        reasoning_content: None,
                        finish_reason: None,
                        usage: None,
                    })
                }
                "input_json_delta" => {
                    let index = data["index"].as_u64().unwrap_or(0) as u32;
                    let partial = delta["partial_json"].as_str().unwrap_or("").to_string();
                    Ok(StreamChunk {
                        content: None,
                        tool_calls: Some(vec![ToolCallDelta {
                            index,
                            id: None,
                            function: Some(FunctionDelta {
                                name: None,
                                arguments: Some(partial),
                            }),
                        }]),
                        reasoning_content: None,
                        finish_reason: None,
                        usage: None,
                    })
                }
                "thinking_delta" => {
                    let t = delta["thinking"]
                        .as_str()
                        .or_else(|| delta["text"].as_str())
                        .map(|s| s.to_string());
                    Ok(StreamChunk {
                        content: None,
                        tool_calls: None,
                        reasoning_content: t,
                        finish_reason: None,
                        usage: None,
                    })
                }
                _ => {
                    // Unknown delta type — ignore
                    Ok(StreamChunk {
                        content: None,
                        tool_calls: None,
                        reasoning_content: None,
                        finish_reason: None,
                        usage: None,
                    })
                }
            }
        }

        "message_delta" => {
            let delta = &data["delta"];
            let finish_reason = delta["stop_reason"].as_str().map(|s| s.to_string());
            let usage = data.get("usage").map(|u| crate::agent::stream::UsageInfo {
                input_tokens: 0,
                output_tokens: u["output_tokens"].as_u64().unwrap_or(0),
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            });

            Ok(StreamChunk {
                content: None,
                tool_calls: None,
                reasoning_content: None,
                finish_reason,
                usage,
            })
        }

        "message_stop" => {
            // message_delta already provides the authoritative stop_reason
            // (end_turn, max_tokens, stop_sequence, tool_use); do not overwrite
            // it with a generic "stop" here.
            Ok(StreamChunk {
                content: None,
                tool_calls: None,
                reasoning_content: None,
                finish_reason: None,
                usage: None,
            })
        }

        _ => {
            // Unknown event type — emit empty chunk
            Ok(StreamChunk {
                content: None,
                tool_calls: None,
                reasoning_content: None,
                finish_reason: None,
                usage: None,
            })
        }
    }
}

#[cfg(test)]
mod thinking_effort_tests {
    use super::*;

    fn provider(effort: &str, max_tokens: u32) -> AnthropicProvider {
        AnthropicProvider {
            api_key: "k".into(),
            base_url: "https://api.anthropic.com".into(),
            model: "claude-sonnet-4".into(),
            max_tokens,
            temperature: 0.0,
            reasoning_effort: Some(effort.into()),
            client: Client::new(),
        }
    }

    #[test]
    fn effort_maps_to_thinking_budget() {
        let p = provider("max", 8192);
        let body = p.build_request_body(&[], &[], false);
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["thinking"]["budget_tokens"], 32000);
        // max_tokens must exceed budget
        assert!(body["max_tokens"].as_u64().unwrap() > 32000);
        assert_eq!(body["temperature"], 1.0);
    }

    #[test]
    fn medium_budget() {
        let p = provider("medium", 20000);
        let body = p.build_request_body(&[], &[], false);
        assert_eq!(body["thinking"]["budget_tokens"], 10000);
    }

    #[test]
    fn off_no_thinking() {
        let p = provider("off", 8192);
        let body = p.build_request_body(&[], &[], false);
        assert!(body.get("thinking").is_none());
    }

    #[test]
    fn parse_thinking_block() {
        let body = serde_json::json!({
            "content": [
                {"type": "thinking", "thinking": "step by step"},
                {"type": "text", "text": "answer"}
            ],
            "usage": {"input_tokens": 1, "output_tokens": 2}
        });
        let r = parse_anthropic_response(&body).unwrap();
        assert_eq!(r.content, "answer");
        assert_eq!(r.reasoning_content.as_deref(), Some("step by step"));
    }
}
