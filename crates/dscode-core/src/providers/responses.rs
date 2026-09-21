//! OpenAI Responses API provider — wire-compatible with DeepSeek's
//! `/responses` endpoint (`https://api.deepseek.com/responses`).
//!
//! The Responses API is OpenAI's newer request format: `input` items +
//! `instructions` instead of `messages`, and semantic SSE events (no
//! `data: [DONE]` terminator — streams end with `response.completed`,
//! `response.incomplete` or `response.failed`).

use super::trait_def::*;
use async_trait::async_trait;
use futures::stream::Stream;
use reqwest::Client;
use std::pin::Pin;
use std::time::Duration;
use tokio_stream::StreamExt;

pub struct ResponsesProvider {
    pub api_key: String,
    /// Base URL WITHOUT `/v1` suffix (e.g. `https://api.deepseek.com`).
    pub base_url: String,
    pub model: String,
    /// Max output tokens per response
    pub max_tokens: u32,
    /// Sampling temperature
    pub temperature: f64,
    /// Reasoning effort (low/medium/high/max) — sent via `reasoning.effort`
    pub reasoning_effort: Option<String>,
    /// Unary requests (180 s total timeout).
    client: Client,
    /// Streaming requests (no total timeout — see [`http_client`]).
    stream_client: Client,
}

impl ResponsesProvider {
    /// Configure from app settings.
    pub fn from_config(model: &str, conf: &crate::config::settings::Config) -> Self {
        let provider_conf = conf.provider_for_model(model).unwrap_or_else(|| {
            crate::config::settings::ProviderConfig {
                api_key: String::new(),
                base_url: "https://api.deepseek.com".into(),
                enabled: true,
                use_proxy: false,
                ..Default::default()
            }
        });

        // The Responses endpoint lives at the bare base URL — strip any /v1
        // suffix. An empty base_url (hand-edited config) falls back to the
        // channel default instead of producing a relative request URL.
        let base_url = if provider_conf.base_url.trim().is_empty() {
            "https://api.deepseek.com".to_string()
        } else {
            normalize_api_base_url(&provider_conf.base_url)
        };

        // Strip provider prefix (deepseek/deepseek-v4-flash -> deepseek-v4-flash)
        let actual_model = match model.split_once('/') {
            Some((_, m)) => m.to_string(),
            None => model.to_string(),
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
        }
    }

    /// Build the `/responses` request body from internal messages + tools.
    fn build_request_body(
        &self,
        messages: Vec<Message>,
        tools: Vec<ToolDef>,
        stream: bool,
    ) -> serde_json::Value {
        let (instructions, items) = messages_to_responses_input(&messages);

        // Reasoning models (o-series / gpt-5) reject `temperature`; every other
        // model keeps it, including the deterministic 0.0.
        let mut body = serde_json::json!({
            "model": self.model,
            "input": items,
            "stream": stream,
        });
        if !is_responses_reasoning_model(&self.model) {
            body["temperature"] = serde_json::json!(self.temperature);
        }
        if self.max_tokens > 0 {
            body["max_output_tokens"] =
                serde_json::Value::Number(serde_json::Number::from(self.max_tokens));
        }
        if let Some(inst) = instructions {
            if !inst.is_empty() {
                body["instructions"] = serde_json::Value::String(inst);
            }
        }
        if !tools.is_empty() {
            let tools_value: Vec<serde_json::Value> =
                tools.iter().map(responses_tool_def).collect();
            body["tools"] = serde_json::Value::Array(tools_value);
        }
        // Reasoning effort → reasoning.effort (DeepSeek responses accepts it;
        // plain OpenAI models such as gpt-4o reject the field).
        if let Some(effort) = self.effective_reasoning_effort() {
            if accepts_reasoning_effort(&self.model, self.reasoning_channel()) {
                body["reasoning"] = serde_json::json!({ "effort": effort });
            } else {
                tracing::debug!(
                    model = %self.model,
                    "model does not accept `reasoning.effort` — omitting it"
                );
            }
        }
        body
    }

    /// Channel style for thinking intensity — DeepSeek accepts `max`;
    /// generic OpenAI-compatible endpoints map it to `high`.
    fn reasoning_channel(&self) -> ReasoningChannel {
        if self.base_url.to_ascii_lowercase().contains("deepseek") {
            ReasoningChannel::DeepSeek
        } else {
            ReasoningChannel::OpenAiCompat
        }
    }

    fn effective_reasoning_effort(&self) -> Option<String> {
        let raw = self.reasoning_effort.as_ref()?.trim();
        if raw.is_empty() || raw.eq_ignore_ascii_case("off") || raw.eq_ignore_ascii_case("none") {
            return None;
        }
        let r = raw.to_ascii_lowercase();
        Some(match self.reasoning_channel() {
            ReasoningChannel::DeepSeek => match r.as_str() {
                "low" | "minimal" => "low".into(),
                "medium" | "med" => "medium".into(),
                "high" => "high".into(),
                "max" | "ultra" | "maximum" => "max".into(),
                other => other.to_string(),
            },
            ReasoningChannel::OpenAiCompat => match r.as_str() {
                "low" | "minimal" => "low".into(),
                "medium" | "med" => "medium".into(),
                _ => "high".into(),
            },
        })
    }
}

#[async_trait]
impl LlmProvider for ResponsesProvider {
    async fn chat(
        &self,
        messages: Vec<Message>,
        tools: Vec<ToolDef>,
    ) -> Result<ChatResponse, ProviderError> {
        if self.api_key.trim().is_empty() {
            return Err(ProviderError::NoApiKey);
        }

        let request_body = self.build_request_body(messages, tools, false);
        let resp = self
            .client
            .post(format!("{}/responses", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
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

        parse_responses_body(&body)
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

        let request_body = self.build_request_body(messages, tools, true);
        let resp = self
            .stream_client
            .post(format!("{}/responses", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request_body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let raw_body = resp.text().await?;
            return Err(api_error_from_body(status.as_u16(), &raw_body, &self.model));
        }

        // Byte-buffer SSE reader with per-chunk timeout (same pattern as openai.rs).
        let byte_stream = resp.bytes_stream();
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<String, String>>(128);

        tokio::spawn(async move {
            let mut buf: Vec<u8> = Vec::new();
            futures::pin_mut!(byte_stream);
            loop {
                let result =
                    tokio::time::timeout(Duration::from_secs(90), byte_stream.next()).await;
                match result {
                    Ok(Some(Ok(bytes))) => {
                        buf.extend_from_slice(&bytes);
                        while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                            let line_bytes = buf.drain(..=pos).collect::<Vec<_>>();
                            let line = String::from_utf8_lossy(&line_bytes[..line_bytes.len() - 1])
                                .trim_end_matches('\r')
                                .to_string();
                            if sse_field(&line, "data").is_some() {
                                if tx.send(Ok(line.to_string())).await.is_err() {
                                    return;
                                }
                            }
                        }
                    }
                    Ok(Some(Err(e))) => {
                        let _ = tx.send(Err(format!("byte stream: {e}"))).await;
                        return;
                    }
                    Ok(None) => break,
                    Err(_timeout) => {
                        tracing::warn!("responses stream chunk read timeout, closing stream");
                        let _ = tx.send(Err("chunk read timeout (90s)".into())).await;
                        return;
                    }
                }
            }
            if !buf.is_empty() {
                let line = String::from_utf8_lossy(&buf);
                if sse_field(&line, "data").is_some() {
                    let _ = tx.send(Ok(line.to_string())).await;
                }
            }
        });

        let stream = tokio_stream::wrappers::ReceiverStream::new(rx)
            .map(|item| match item {
                Ok(line) => parse_responses_sse_event(&line),
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
        })
    }
}

/// How to map `generation.reasoning_effort` for this endpoint.
#[derive(Debug, Clone, Copy)]
enum ReasoningChannel {
    DeepSeek,
    OpenAiCompat,
}

/// Whether this model accepts the `reasoning` object. Sending it to a plain
/// model (e.g. `gpt-4o`) is `400 Unsupported parameter: 'reasoning'`.
fn accepts_reasoning_effort(model: &str, channel: ReasoningChannel) -> bool {
    let m = model.trim().to_ascii_lowercase();
    match channel {
        ReasoningChannel::DeepSeek => m.contains("deepseek"),
        ReasoningChannel::OpenAiCompat => {
            m.starts_with("o1")
                || m.starts_with("o3")
                || m.starts_with("o4")
                || m.starts_with("gpt-5")
                || m.contains("reasoner")
                || m.contains("thinking")
        }
    }
}

/// OpenAI reasoning models reject `temperature` on the Responses API too.
///
/// Limited to OpenAI's own families on purpose: DeepSeek (this provider's
/// default channel) has always accepted `temperature` here.
fn is_responses_reasoning_model(model: &str) -> bool {
    let m = model.trim().to_ascii_lowercase();
    m.starts_with("o1")
        || m.starts_with("o3")
        || m.starts_with("o4")
        || m.starts_with("gpt-5")
        || m.starts_with("gpt-oss")
}

fn content_text(content: &MessageContent) -> String {
    match content {
        MessageContent::Text(t) => t.clone(),
        MessageContent::Parts(parts) => parts
            .iter()
            .filter_map(|p| match p {
                ContentPart::Text { text } => Some(text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

/// Convert internal messages to Responses `input` items.
///
/// Returns `(instructions, input items)`. The first system message becomes
/// `instructions`; assistant tool calls become `function_call` items and tool
/// results become `function_call_output` items (DeepSeek-compatible — verified
/// against the live endpoint).
fn messages_to_responses_input(msgs: &[Message]) -> (Option<String>, Vec<serde_json::Value>) {
    let mut instructions: Option<String> = None;
    let mut items: Vec<serde_json::Value> = Vec::new();

    // A `function_call` without a matching `function_call_output` makes the API
    // reject the whole request ("No tool output found for function call …"), so
    // calls whose output is missing (or whose id is unusable) are dropped along
    // with it — the input stays internally consistent either way.
    let answered: std::collections::HashSet<&str> = msgs
        .iter()
        .filter(|m| m.role == Role::Tool)
        .filter_map(|m| m.tool_call_id.as_deref())
        .filter(|id| !id.is_empty())
        .collect();

    for m in msgs {
        match m.role {
            Role::System => {
                let text = content_text(&m.content);
                if text.is_empty() {
                    continue;
                }
                if instructions.is_none() {
                    instructions = Some(text);
                } else {
                    items.push(serde_json::json!({
                        "type": "message",
                        "role": "system",
                        "content": [{"type": "input_text", "text": text}],
                    }));
                }
            }
            Role::User => {
                let text = content_text(&m.content);
                if text.is_empty() {
                    continue;
                }
                items.push(serde_json::json!({
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": text}],
                }));
            }
            Role::Assistant => {
                let text = content_text(&m.content);
                let has_calls = m
                    .tool_calls
                    .as_ref()
                    .map(|c| !c.is_empty())
                    .unwrap_or(false);
                if !text.is_empty() {
                    items.push(serde_json::json!({
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": text}],
                    }));
                }
                if let Some(calls) = &m.tool_calls {
                    for tc in calls {
                        if !answered.contains(tc.id.as_str()) {
                            tracing::warn!(
                                call_id = %tc.id,
                                "dropping function_call with no matching tool result"
                            );
                            continue;
                        }
                        items.push(serde_json::json!({
                            "type": "function_call",
                            "call_id": tc.id,
                            "name": tc.function.name,
                            "arguments": tc.function.arguments,
                        }));
                    }
                }
                if text.is_empty() && !has_calls {
                    continue;
                }
            }
            Role::Tool => {
                let text = content_text(&m.content);
                match m.tool_call_id.as_deref().filter(|id| !id.is_empty()) {
                    Some(tc_id) => items.push(serde_json::json!({
                        "type": "function_call_output",
                        "call_id": tc_id,
                        "output": text,
                    })),
                    // `call_id: ""` is rejected; the output is dropped rather
                    // than emitted with an id nothing refers to.
                    None => tracing::warn!(
                        "dropping tool result without a call_id — the API cannot match it"
                    ),
                }
            }
        }
    }
    (instructions, items)
}

/// Internal ToolDef → Responses `function` tool (flat fields, not nested).
fn responses_tool_def(t: &ToolDef) -> serde_json::Value {
    let mut v = serde_json::json!({
        "type": "function",
        "name": t.function.name,
    });
    if let Some(d) = &t.function.description {
        v["description"] = serde_json::Value::String(d.clone());
    }
    if let Some(p) = &t.function.parameters {
        v["parameters"] = p.clone();
    }
    v
}

/// Parse a non-streaming `/responses` response into ChatResponse.
fn parse_responses_body(body: &serde_json::Value) -> Result<ChatResponse, ProviderError> {
    let output = body
        .get("output")
        .and_then(|o| o.as_array())
        .cloned()
        .unwrap_or_default();

    let mut content = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut reasoning = String::new();

    for item in &output {
        match item["type"].as_str() {
            Some("message") => {
                if let Some(parts) = item["content"].as_array() {
                    for p in parts {
                        if p["type"].as_str() == Some("output_text") {
                            if let Some(t) = p["text"].as_str() {
                                content.push_str(t);
                            }
                        }
                    }
                }
            }
            Some("function_call") => {
                // `call_id` is what a later `function_call_output` must
                // reference; when the endpoint omits it (or sends an empty
                // string) the call would be unusable in the history, so give it
                // one we control.
                let id = item["call_id"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .or_else(|| item["id"].as_str().filter(|s| !s.is_empty()))
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("call_{}", uuid::Uuid::new_v4()));
                let name = item["name"].as_str().unwrap_or("").to_string();
                if name.is_empty() {
                    continue;
                }
                let arguments = match item.get("arguments") {
                    Some(serde_json::Value::String(s)) => s.clone(),
                    Some(other) => serde_json::to_string(other).unwrap_or_else(|_| "{}".into()),
                    None => "{}".into(),
                };
                tool_calls.push(ToolCall {
                    id,
                    call_type: "function".into(),
                    function: FunctionCall { name, arguments },
                });
            }
            Some("reasoning") => {
                if let Some(parts) = item["content"].as_array() {
                    for p in parts {
                        if let Some(t) = p["text"].as_str() {
                            if !reasoning.is_empty() {
                                reasoning.push('\n');
                            }
                            reasoning.push_str(t);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    let usage = parse_responses_usage(body);
    let reasoning_content = if reasoning.is_empty() {
        None
    } else {
        Some(reasoning)
    };

    Ok(ChatResponse {
        content,
        tool_calls,
        usage,
        reasoning_content,
    })
}

fn parse_responses_usage(v: &serde_json::Value) -> Option<crate::agent::stream::UsageInfo> {
    let usage = v.get("usage")?;
    Some(crate::agent::stream::UsageInfo {
        input_tokens: usage["input_tokens"].as_u64().unwrap_or(0),
        output_tokens: usage["output_tokens"].as_u64().unwrap_or(0),
        cache_read_tokens: usage["input_tokens_details"]["cached_tokens"].as_u64().unwrap_or(0),
        cache_write_tokens: 0,
    })
}

/// Map one SSE `data:` line (a Responses event) to a StreamChunk.
///
/// Streams end with `response.completed` / `response.incomplete` /
/// `response.failed` (there is no `data: [DONE]` in the Responses API).
/// Function-call name + id are announced once by `response.output_item.added`;
/// `response.function_call_arguments.delta` only carries argument fragments —
/// the forge consumer accumulates them by `output_index`.
fn parse_responses_sse_event(text: &str) -> Result<StreamChunk, ProviderError> {
    let mut result = StreamChunk {
        content: None,
        tool_calls: None,
        reasoning_content: None,
        finish_reason: None,
        usage: None,
    };

    for line in text.lines() {
        let data = match sse_field(line, "data") {
            Some(d) => d.trim(),
            None => continue,
        };
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let ev: serde_json::Value = serde_json::from_str(data).map_err(|e| {
            // Silently dropping the frame used to lose whole output deltas and
            // function-call fragments with no warning and no retry.
            tracing::warn!(data = %data, "Responses SSE frame is not valid JSON");
            ProviderError::Parse(format!("SSE JSON parse error: {}. Data: {}", e, data))
        })?;
        let Some(etype) = ev["type"].as_str() else {
            continue;
        };

        match etype {
            "response.output_text.delta" => {
                if let Some(d) = ev["delta"].as_str() {
                    result.content = Some(d.to_string());
                }
            }
            "response.reasoning_text.delta" => {
                if let Some(d) = ev["delta"].as_str() {
                    result.reasoning_content = Some(d.to_string());
                }
            }
            "response.output_item.added" => {
                // function_call items announce call_id + name once.
                if ev["item"]["type"].as_str() == Some("function_call") {
                    let idx = ev["output_index"].as_u64().unwrap_or(0) as u32;
                    let id = ev["item"]["call_id"].as_str().map(|s| s.to_string());
                    let name = ev["item"]["name"].as_str().map(|s| s.to_string());
                    result.tool_calls = Some(vec![ToolCallDelta {
                        index: idx,
                        id,
                        function: Some(FunctionDelta { name, arguments: None }),
                    }]);
                }
            }
            "response.function_call_arguments.delta" => {
                let idx = ev["output_index"].as_u64().unwrap_or(0) as u32;
                let delta = ev["delta"].as_str().unwrap_or("").to_string();
                result.tool_calls = Some(vec![ToolCallDelta {
                    index: idx,
                    id: None,
                    function: Some(FunctionDelta {
                        name: None,
                        arguments: Some(delta),
                    }),
                }]);
            }
            "response.completed" | "response.incomplete" => {
                result.usage = parse_responses_usage(&ev["response"]);
                let reason = if etype == "response.incomplete" {
                    "length"
                } else {
                    "stop"
                };
                result.finish_reason = Some(reason.into());
            }
            "response.failed" => {
                // `status: 0` used to make a failed response indistinguishable
                // from a client error; use the code/status the API reports.
                let code = ev["response"]["error"]["code"]
                    .as_str()
                    .or_else(|| ev["error"]["code"].as_str())
                    .unwrap_or("");
                let msg = ev["response"]["error"]["message"]
                    .as_str()
                    .or_else(|| ev["error"]["message"].as_str())
                    .unwrap_or("response failed")
                    .to_string();
                let status = match code {
                    "rate_limit_exceeded" => 429,
                    "invalid_request_error" | "invalid_prompt" => 400,
                    "authentication_error" | "invalid_api_key" => 401,
                    "insufficient_quota" => 402,
                    "server_error" | "" => 500,
                    _ => 500,
                };
                tracing::warn!(kind = %code, status, "Responses stream failed: {msg}");
                return Err(ProviderError::ApiDetail {
                    status,
                    kind: code.to_string(),
                    code: if code.is_empty() {
                        None
                    } else {
                        Some(code.to_string())
                    },
                    param: None,
                    message: msg,
                });
            }
            _ => {
                // response.created / response.in_progress / output_text.done /
                // content_part.* / reasoning_text.done etc: nothing to emit.
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: Role, text: &str) -> Message {
        Message {
            role,
            content: MessageContent::Text(text.into()),
            ..Default::default()
        }
    }

    #[test]
    fn system_becomes_instructions() {
        let msgs = vec![msg(Role::System, "be terse"), msg(Role::User, "hi")];
        let (inst, items) = messages_to_responses_input(&msgs);
        assert_eq!(inst.as_deref(), Some("be terse"));
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["role"], "user");
    }

    #[test]
    fn tool_chain_becomes_function_call_items() {
        let mut asst = msg(Role::Assistant, "let me check");
        asst.tool_calls = Some(vec![ToolCall {
            id: "call_1".into(),
            call_type: "function".into(),
            function: FunctionCall {
                name: "get_weather".into(),
                arguments: r#"{"city":"北京"}"#.into(),
            },
        }]);
        let tool = Message {
            role: Role::Tool,
            content: MessageContent::Text("sunny".into()),
            tool_call_id: Some("call_1".into()),
            ..Default::default()
        };
        let msgs = vec![
            msg(Role::User, "weather?"),
            asst,
            tool,
            msg(Role::User, "thanks"),
        ];
        let (inst, items) = messages_to_responses_input(&msgs);
        assert!(inst.is_none());
        // user → assistant(msg) → function_call → function_call_output → user
        assert_eq!(items.len(), 5);
        assert_eq!(items[0]["role"], "user");
        assert_eq!(items[1]["type"], "message");
        assert_eq!(items[1]["role"], "assistant");
        assert_eq!(items[2]["type"], "function_call");
        assert_eq!(items[2]["call_id"], "call_1");
        assert_eq!(items[2]["name"], "get_weather");
        assert_eq!(items[3]["type"], "function_call_output");
        assert_eq!(items[3]["call_id"], "call_1");
        assert_eq!(items[3]["output"], "sunny");
        assert_eq!(items[4]["role"], "user");
    }

    #[test]
    fn tool_def_conversion_is_flat() {
        let t = ToolDef::new(
            "get_weather",
            "query weather",
            serde_json::json!({"type":"object","properties":{"city":{"type":"string"}}}),
        );
        let v = responses_tool_def(&t);
        assert_eq!(v["type"], "function");
        assert_eq!(v["name"], "get_weather");
        assert_eq!(v["parameters"]["properties"]["city"]["type"], "string");
        assert!(v.get("function").is_none());
    }

    #[test]
    fn parses_non_stream_body_with_tools_and_reasoning() {
        let body = serde_json::json!({
            "output": [
                {"type":"reasoning","content":[{"type":"reasoning_text","text":"think..."}]},
                {"type":"message","content":[{"type":"output_text","text":"ok"}]},
                {"type":"function_call","call_id":"c1","name":"get_weather","arguments":"{\"city\":\"北京\"}"}
            ],
            "usage": {"input_tokens":10,"input_tokens_details":{"cached_tokens":4},"output_tokens":5,"output_tokens_details":{"reasoning_tokens":2}}
        });
        let r = parse_responses_body(&body).unwrap();
        assert_eq!(r.content, "ok");
        assert_eq!(r.tool_calls.len(), 1);
        assert_eq!(r.tool_calls[0].id, "c1");
        assert_eq!(r.tool_calls[0].function.name, "get_weather");
        assert_eq!(r.reasoning_content.as_deref(), Some("think..."));
        let u = r.usage.unwrap();
        assert_eq!(u.input_tokens, 10);
        assert_eq!(u.cache_read_tokens, 4);
    }

    #[test]
    fn parses_stream_events() {
        let added = parse_responses_sse_event(
            r#"data: {"type":"response.output_item.added","output_index":2,"item":{"type":"function_call","call_id":"call_x","name":"get_weather","arguments":""}}"#,
        )
        .unwrap();
        let tc = &added.tool_calls.as_ref().unwrap()[0];
        assert_eq!(tc.index, 2);
        assert_eq!(tc.id.as_deref(), Some("call_x"));
        assert_eq!(tc.function.as_ref().unwrap().name.as_deref(), Some("get_weather"));

        let delta = parse_responses_sse_event(
            r#"data: {"type":"response.function_call_arguments.delta","output_index":2,"delta":"{\"city\":"}"#,
        )
        .unwrap();
        let tc = &delta.tool_calls.as_ref().unwrap()[0];
        assert_eq!(tc.index, 2);
        assert_eq!(
            tc.function.as_ref().unwrap().arguments.as_deref(),
            Some("{\"city\":")
        );

        let txt = parse_responses_sse_event(
            r#"data: {"type":"response.output_text.delta","output_index":1,"delta":"你好"}"#,
        )
        .unwrap();
        assert_eq!(txt.content.as_deref(), Some("你好"));

        let done = parse_responses_sse_event(
            r#"data: {"type":"response.completed","response":{"usage":{"input_tokens":7,"output_tokens":3,"input_tokens_details":{"cached_tokens":1}}}}"#,
        )
        .unwrap();
        assert_eq!(done.finish_reason.as_deref(), Some("stop"));
        assert_eq!(done.usage.unwrap().input_tokens, 7);

        let failed = parse_responses_sse_event(
            r#"data: {"type":"response.failed","error":{"message":"boom"}}"#,
        );
        let err = failed.unwrap_err();
        assert_eq!(err.status(), Some(500));
    }

    #[test]
    fn failed_event_status_is_meaningful() {
        let err = parse_responses_sse_event(
            r#"data: {"type":"response.failed","response":{"error":{"code":"rate_limit_exceeded","message":"slow down"}}}"#,
        )
        .unwrap_err();
        assert_eq!(err.status(), Some(429));
        assert!(err.is_retryable());
    }

    #[test]
    fn unparseable_frame_is_reported_not_dropped() {
        let err = parse_responses_sse_event("data: {\"type\":").unwrap_err();
        assert!(matches!(err, ProviderError::Parse(_)));
    }

    #[test]
    fn function_call_without_output_is_dropped() {
        let mut asst = msg(Role::Assistant, "");
        asst.tool_calls = Some(vec![ToolCall {
            id: "call_orphan".into(),
            call_type: "function".into(),
            function: FunctionCall {
                name: "get_weather".into(),
                arguments: "{}".into(),
            },
        }]);
        let (_, items) = messages_to_responses_input(&[asst]);
        // Emitting the call alone would make the API reject the request.
        assert!(items.is_empty());
    }

    #[test]
    fn tool_result_without_call_id_is_dropped() {
        let tool = Message {
            role: Role::Tool,
            content: MessageContent::Text("sunny".into()),
            tool_call_id: Some(String::new()),
            ..Default::default()
        };
        let (_, items) = messages_to_responses_input(&[tool]);
        assert!(items.is_empty());
    }

    fn provider_at(base: &str, model: &str, effort: &str) -> ResponsesProvider {
        ResponsesProvider {
            api_key: "k".into(),
            base_url: base.into(),
            model: model.into(),
            max_tokens: 1024,
            temperature: 0.0,
            reasoning_effort: Some(effort.into()),
            client: Client::new(),
            stream_client: Client::new(),
        }
    }

    fn provider(model: &str, effort: &str) -> ResponsesProvider {
        provider_at("https://api.deepseek.com", model, effort)
    }

    #[test]
    fn non_reasoning_model_gets_no_temperature_or_reasoning() {
        let p = provider_at("https://api.openai.com/v1", "gpt-4o", "max");
        let body = p.build_request_body(vec![], vec![], false);
        assert!(body.get("reasoning").is_none());
        // ... but 0.0 is still a real setting for a model that accepts it.
        assert_eq!(body["temperature"], 0.0);
    }

    #[test]
    fn reasoning_model_gets_effort_and_no_temperature() {
        let p = provider_at("https://api.openai.com/v1", "o3-mini", "max");
        let body = p.build_request_body(vec![], vec![], false);
        assert_eq!(body["reasoning"]["effort"], "high");
        assert!(body.get("temperature").is_none());
    }

    #[test]
    fn deepseek_channel_keeps_effort() {
        let p = provider("deepseek-reasoner", "max");
        let body = p.build_request_body(vec![], vec![], false);
        assert_eq!(body["reasoning"]["effort"], "max");
    }
}
