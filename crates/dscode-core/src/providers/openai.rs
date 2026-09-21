//! OpenAI-compatible provider — works with DeepSeek, Groq, OpenAI, and any
//! OpenAI-compatible API endpoint. This is the default provider.

use super::trait_def::*;
use async_trait::async_trait;
use futures::stream::Stream;
use reqwest::Client;
use std::pin::Pin;
use std::time::Duration;
use tokio_stream::StreamExt;

pub struct OpenAiProvider {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    /// Max tokens per response
    pub max_tokens: u32,
    /// Temperature
    pub temperature: f64,
    /// Reasoning effort for DeepSeek (low/medium/high/max)
    pub reasoning_effort: Option<String>,
    /// Unary requests (180 s total timeout).
    client: Client,
    /// Streaming requests (no total timeout — see [`http_client`]).
    stream_client: Client,
}

/// Fallback base URL when the channel table exists but `base_url` is empty
/// (a hand-edited config); a relative URL would otherwise fail with
/// "relative URL without a base".
fn default_base_url_for_channel(channel: &str) -> &'static str {
    match channel {
        "openai" => "https://api.openai.com/v1",
        "ollama" => "http://localhost:11434/v1",
        _ => "https://api.deepseek.com/v1",
    }
}

/// Models documented to accept OpenAI's `reasoning_effort` parameter. Sending
/// it to anything else is a 400 (`Unsupported value: 'reasoning_effort'` for
/// `gpt-4o`) or an unknown-field rejection on gateways such as Ollama.
fn accepts_reasoning_effort(model: &str) -> bool {
    let m = model.trim().to_ascii_lowercase();
    m.starts_with("o1")
        || m.starts_with("o3")
        || m.starts_with("o4")
        || m.starts_with("gpt-5")
        || m.starts_with("gpt-oss")
        || m.contains("reasoner")
        || m.contains("thinking")
        || m.contains("qwq")
        || m.contains("deepseek")
}

/// Models that reject `max_tokens` and `temperature` and require
/// `max_completion_tokens` instead (OpenAI o-series / gpt-5 reasoning models).
///
/// Deliberately limited to OpenAI's own families: DeepSeek's reasoner still
/// takes `max_tokens`, and sending it `max_completion_tokens` would risk a 400
/// on this repository's default channel.
fn is_openai_reasoning_model(model: &str) -> bool {
    let m = model.trim().to_ascii_lowercase();
    m.starts_with("o1")
        || m.starts_with("o3")
        || m.starts_with("o4")
        || m.starts_with("gpt-5")
        || m.starts_with("gpt-oss")
}

impl OpenAiProvider {
    pub fn new(api_key: String, base_url: String, model: String) -> Self {
        let base_url = base_url.trim_end_matches('/').to_string();
        Self {
            api_key,
            base_url,
            model,
            max_tokens: 8192,
            temperature: 0.0,
            reasoning_effort: Some("max".into()),
            client: http_client(None, false),
            stream_client: http_client(None, true),
        }
    }

    /// Configure with settings from Config.
    pub fn from_config(model: &str, conf: &crate::config::settings::Config) -> Self {
        let channel = conf.provider_key_for_model(model);
        let provider_conf = conf
            .provider_for_model(model)
            .unwrap_or_else(|| crate::config::settings::ProviderConfig {
                api_key: String::new(),
                base_url: "https://api.deepseek.com/v1".into(),
                enabled: true,
                use_proxy: false,
                ..Default::default()
            });

        let mut base_url = if provider_conf.base_url.trim().is_empty() {
            default_base_url_for_channel(&channel).to_string()
        } else {
            provider_conf.base_url.trim().trim_end_matches('/').to_string()
        };
        // Ensure /v1 path for OpenAI-compatible endpoints
        if !base_url.ends_with("/v1") && !base_url.contains("/v1/") {
            base_url = format!("{}/v1", base_url);
        }

        // Strip provider prefix (openai/gpt-4o -> gpt-4o)
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
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    async fn chat(
        &self,
        messages: Vec<Message>,
        tools: Vec<ToolDef>,
    ) -> Result<ChatResponse, ProviderError> {
        // P10: Check for empty API key before making request
        if self.api_key.trim().is_empty() {
            return Err(ProviderError::NoApiKey);
        }

        let request_body = self.build_request_body(messages, tools, false);
        let resp = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
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

        // Parse body after confirming success
        // P4: Use chars().take(200) to avoid mid-UTF-8 slice panic
        let body: serde_json::Value = serde_json::from_str(&raw_body)
            .map_err(|e| {
                let preview: String = raw_body.chars().take(200).collect();
                ProviderError::Parse(format!("JSON parse error: {}. Body: {}", e, preview))
            })?;

        // P6: Use .get(0) for bounds safety
        let choice = body["choices"]
            .as_array()
            .and_then(|arr| arr.get(0))
            .ok_or_else(|| ProviderError::Parse("No choices in response".into()))?;
        let msg = &choice["message"];

        let content = msg["content"].as_str().unwrap_or("").to_string();
        let tool_calls = parse_tool_calls(msg);
        let usage = parse_usage(&body);
        let reasoning = msg["reasoning_content"].as_str().map(|s| s.to_string());

        Ok(ChatResponse {
            content,
            tool_calls,
            usage,
            reasoning_content: reasoning,
        })
    }

    async fn chat_stream(
        &self,
        messages: Vec<Message>,
        tools: Vec<ToolDef>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, ProviderError>> + Send>>, ProviderError>
    {
        // P10: Check for empty API key before making request
        if self.api_key.trim().is_empty() {
            return Err(ProviderError::NoApiKey);
        }

        let request_body = self.build_request_body(messages, tools, true);
        let resp = self
            .stream_client
            .post(format!("{}/chat/completions", self.base_url))
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

        // P1: Use byte buffer with per-chunk read timeout to prevent hangs
        let byte_stream = resp.bytes_stream();
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<String, String>>(128);

        tokio::spawn(async move {
            let mut buf: Vec<u8> = Vec::new();
            futures::pin_mut!(byte_stream);
            loop {
                let result = tokio::time::timeout(Duration::from_secs(90), byte_stream.next()).await;
                match result {
                    Ok(Some(Ok(bytes))) => {
                        buf.extend_from_slice(&bytes);
                        while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                            let line_bytes = buf.drain(..=pos).collect::<Vec<_>>();
                            let line = String::from_utf8_lossy(&line_bytes[..line_bytes.len()-1])
                                .trim_end_matches('\r')
                                .to_string();
                            if is_sse_data_line(&line) {
                                if tx.send(Ok(line.to_string())).await.is_err() { return; }
                            }
                        }
                    }
                    Ok(Some(Err(e))) => {
                        let _ = tx.send(Err(format!("byte stream: {e}"))).await;
                        return;
                    }
                    Ok(None) => { break; }
                    Err(_timeout) => {
                        tracing::warn!("chat_stream chunk read timeout, closing stream");
                        let _ = tx.send(Err("chunk read timeout (90s)".into())).await;
                        return;
                    }
                }
            }
            if !buf.is_empty() {
                let line = String::from_utf8_lossy(&buf);
                if is_sse_data_line(&line) {
                    let _ = tx.send(Ok(line.to_string())).await;
                }
            }
        });

        // Tool-call deltas that omit `index` are assigned a synthetic one; the
        // counter has to survive across chunks, so it lives in the map closure.
        let mut next_synthetic_index: u32 = 0;
        let stream = tokio_stream::wrappers::ReceiverStream::new(rx)
            .map(move |item| match item {
                Ok(line) => parse_sse_chunk(&line, &mut next_synthetic_index),
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

/// True for a line that carries SSE data (used by the byte-stream reader to
/// decide what to forward to the parser).
fn is_sse_data_line(line: &str) -> bool {
    sse_field(line, "data").is_some()
}

impl OpenAiProvider {
    fn build_request_body(
        &self,
        messages: Vec<Message>,
        tools: Vec<ToolDef>,
        stream: bool,
    ) -> serde_json::Value {
        let reasoning_model = is_openai_reasoning_model(&self.model);

        let mut body = serde_json::json!({
            "model": self.model,
            "messages": serialize_messages(&messages),
            "stream": stream,
        });

        // o-series / gpt-5 accept only the default temperature, and take
        // `max_completion_tokens` — `temperature` and `max_tokens` are rejected.
        if !reasoning_model {
            body["temperature"] = serde_json::json!(self.temperature);
        } else if self.temperature > 0.0 {
            tracing::debug!(
                model = %self.model,
                temperature = self.temperature,
                "reasoning models ignore `temperature` — omitting it"
            );
        }
        if self.max_tokens > 0 {
            let key = if reasoning_model {
                "max_completion_tokens"
            } else {
                "max_tokens"
            };
            body[key] = serde_json::Value::Number(serde_json::Number::from(self.max_tokens));
        }

        if stream && self.reasoning_channel() != ReasoningChannel::DeepSeek {
            // OpenAI omits streamed usage unless asked for it; without this the
            // cost meter reads zero for every OpenAI-compatible channel.
            // Deliberately not sent to the DeepSeek channel, which already
            // reports usage and is the one endpoint whose tolerance of the
            // field is unverified.
            body["stream_options"] = serde_json::json!({ "include_usage": true });
        }

        if !tools.is_empty() {
            body["tools"] = serde_json::to_value(&tools).unwrap();
        }

        // Only for models known to accept it: `gpt-4o` answers
        // `400 Unsupported value: 'reasoning_effort'`, and gateways such as
        // Ollama reject the unknown field outright.
        if let Some(effort) = self.effective_reasoning_effort() {
            if accepts_reasoning_effort(&self.model) {
                body["reasoning_effort"] = serde_json::Value::String(effort);
            } else {
                tracing::debug!(
                    model = %self.model,
                    "model does not accept `reasoning_effort` — omitting it"
                );
            }
        }

        body
    }

    /// Channel style for thinking intensity (not model-name gated).
    fn reasoning_channel(&self) -> ReasoningChannel {
        let base = self.base_url.to_ascii_lowercase();
        if base.contains("deepseek") {
            ReasoningChannel::DeepSeek
        } else {
            // openai / ollama / custom OpenAI-compatible gateways
            ReasoningChannel::OpenAiCompat
        }
    }

    /// Map UI setting → API value. `None` only when user disabled effort.
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
                // OpenAI-compatible: low | medium | high (no official `max`)
                "low" | "minimal" => "low".into(),
                "medium" | "med" => "medium".into(),
                "high" | "max" | "ultra" | "maximum" => "high".into(),
                _ => "medium".into(),
            },
        })
    }
}

/// How to map `generation.reasoning_effort` for this OpenAI-compatible endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReasoningChannel {
    DeepSeek,
    OpenAiCompat,
}

fn serialize_messages(msgs: &[Message]) -> Vec<serde_json::Value> {
    // Tool chain validation is done in validate_tool_chain_for_provider.
    // Here we just serialize cleanly: tool_call_id ONLY on Tool messages.
    msgs.iter()
        .map(|m| {
            let mut value = serde_json::json!({ "role": m.role });

            match &m.content {
                MessageContent::Text(text) => value["content"] = serde_json::Value::String(text.clone()),
                MessageContent::Parts(parts) => value["content"] = serde_json::to_value(parts).unwrap(),
            }

            if let Some(ref name) = m.name {
                value["name"] = serde_json::Value::String(name.clone());
            }
            if let Some(ref tool_calls) = m.tool_calls {
                if !tool_calls.is_empty() {
                    value["tool_calls"] = serde_json::to_value(tool_calls).unwrap();
                }
            }
            if let Some(ref tc_id) = m.tool_call_id {
                if m.role == Role::Tool {
                    value["tool_call_id"] = serde_json::Value::String(tc_id.clone());
                }
            }
            // `reasoning_content` is deliberately NOT sent back: DeepSeek
            // documents it as output-only and rejects it in the input messages
            // (400). It stays on the local message for display. (UNVERIFIED
            // against the live endpoint — see the provider review.)

            value
        })
        .collect()
}

fn parse_tool_calls(msg: &serde_json::Value) -> Vec<ToolCall> {
    let arr = match msg.get("tool_calls") {
        Some(serde_json::Value::Array(arr)) => arr.clone(),
        _ => return vec![],
    };
    arr.iter()
        .filter_map(|tc| {
            Some(ToolCall {
                id: tc["id"].as_str()?.to_string(),
                call_type: tc["type"].as_str().unwrap_or("function").to_string(),
                function: FunctionCall {
                    name: tc["function"]["name"].as_str()?.to_string(),
                    arguments: tc["function"]["arguments"].as_str().unwrap_or("{}").to_string(),
                },
            })
        })
        .collect()
}

fn parse_usage(body: &serde_json::Value) -> Option<crate::agent::stream::UsageInfo> {
    let usage = body.get("usage")?;
    Some(crate::agent::stream::UsageInfo {
        input_tokens: usage["prompt_tokens"].as_u64().unwrap_or(0),
        output_tokens: usage["completion_tokens"].as_u64().unwrap_or(0),
        cache_read_tokens: usage["prompt_cache_hit_tokens"].as_u64().unwrap_or(0),
        cache_write_tokens: usage["prompt_cache_miss_tokens"].as_u64().unwrap_or(0),
    })
}

/// Parse one SSE line into a `StreamChunk`.
///
/// `next_synthetic_index` tracks the tool-call slot a delta without an `index`
/// field belongs to. Several OpenAI-compatible gateways omit `index`; defaulting
/// it to 0 merged every parallel call into slot 0, concatenating names and
/// arguments until the JSON no longer parsed.
fn parse_sse_chunk(
    text: &str,
    next_synthetic_index: &mut u32,
) -> Result<StreamChunk, ProviderError> {
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
        if data == "[DONE]" {
            // P2: Only set finish_reason to "stop" if not already set from a prior delta
            if result.finish_reason.is_none() {
                result.finish_reason = Some("stop".into());
            }
            continue;
        }
        if data.is_empty() {
            // Keep-alive frame (`data:` with nothing after it).
            continue;
        }

        let chunk: serde_json::Value = serde_json::from_str(data).map_err(|e| {
            // Silently dropping this used to lose whole content deltas and
            // tool-call fragments; surface it instead.
            tracing::warn!(data = %data, "OpenAI SSE frame is not valid JSON");
            ProviderError::Parse(format!("SSE JSON parse error: {}. Data: {}", e, data))
        })?;

        if let Some(choices) = chunk["choices"].as_array() {
            if let Some(choice) = choices.first() {
                if let Some(delta) = choice.get("delta") {
                    if let Some(c) = delta["content"].as_str() {
                        result.content = Some(format!(
                            "{}{}",
                            result.content.as_deref().unwrap_or(""),
                            c
                        ));
                    }
                    if let Some(rc) = delta["reasoning_content"].as_str() {
                        // P3: Accumulate reasoning_content across deltas with push_str / +=
                        let mut accumulated = result.reasoning_content.unwrap_or_default();
                        accumulated.push_str(rc);
                        result.reasoning_content = Some(accumulated);
                    }
                    // Accumulate tool calls from delta
                    if let Some(tc_deltas) = delta["tool_calls"].as_array() {
                        let mut parsed: Vec<ToolCallDelta> = vec![];
                        for tc in tc_deltas {
                            let index = match tc["index"].as_u64() {
                                Some(i) => {
                                    *next_synthetic_index = (*next_synthetic_index).max(i as u32 + 1);
                                    i as u32
                                }
                                None => {
                                    // A delta that names a new call opens a new
                                    // slot; a bare argument fragment continues
                                    // the slot we handed out last.
                                    if tc["id"].as_str().is_some() {
                                        let i = *next_synthetic_index;
                                        *next_synthetic_index = i.saturating_add(1);
                                        i
                                    } else {
                                        next_synthetic_index.saturating_sub(1)
                                    }
                                }
                            };
                            parsed.push(ToolCallDelta {
                                index,
                                id: tc["id"].as_str().map(|s| s.to_string()),
                                function: tc.get("function").map(|f| FunctionDelta {
                                    name: f["name"].as_str().map(|s| s.to_string()),
                                    arguments: f["arguments"].as_str().map(|s| s.to_string()),
                                }),
                            });
                        }
                        if !parsed.is_empty() {
                            result.tool_calls = Some(parsed);
                        }
                    }
                }
                if let Some(fr) = choice["finish_reason"].as_str() {
                    result.finish_reason = Some(fr.to_string());
                }
            }
        }
        if let Some(usage) = chunk.get("usage") {
            result.usage = Some(crate::agent::stream::UsageInfo {
                input_tokens: usage["prompt_tokens"].as_u64().unwrap_or(0),
                output_tokens: usage["completion_tokens"].as_u64().unwrap_or(0),
                cache_read_tokens: usage["prompt_cache_hit_tokens"].as_u64().unwrap_or(0),
                cache_write_tokens: usage["prompt_cache_miss_tokens"].as_u64().unwrap_or(0),
            });
        }
    }

    // If no meaningful data was found (e.g., keep-alive frame "data:\n"),
    // return an all-None chunk. The caller safely ignores it — all fields
    // are Optional and the forge consumer gates every access on is_some().
    // This is normal SSE protocol behavior, not an error.
    Ok(result)
}

#[cfg(test)]
mod reasoning_effort_tests {
    use super::*;

    fn provider(base: &str, model: &str, effort: &str) -> OpenAiProvider {
        OpenAiProvider {
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

    #[test]
    fn deepseek_channel_keeps_max_for_deepseek_models() {
        let p = provider("https://api.deepseek.com/v1", "deepseek-reasoner", "max");
        let body = p.build_request_body(vec![], vec![], false);
        assert_eq!(body["reasoning_effort"], "max");
    }

    #[test]
    fn non_reasoning_model_gets_no_reasoning_effort() {
        // `gpt-4o` answers 400 Unsupported value: 'reasoning_effort'.
        let p = provider("https://api.openai.com/v1", "gpt-4o", "max");
        let body = p.build_request_body(vec![], vec![], false);
        assert!(body.get("reasoning_effort").is_none());
        // ... and it still gets the classic parameters.
        assert_eq!(body["temperature"], 0.0);
        assert_eq!(body["max_tokens"], 1024);
        assert!(body.get("max_completion_tokens").is_none());
    }

    #[test]
    fn ollama_model_gets_no_reasoning_effort() {
        let p = provider("http://localhost:11434/v1", "llama3.2", "max");
        let body = p.build_request_body(vec![], vec![], false);
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn o_series_uses_max_completion_tokens_and_no_temperature() {
        let p = provider("https://api.openai.com/v1", "o3-mini", "max");
        let body = p.build_request_body(vec![], vec![], false);
        assert_eq!(body["reasoning_effort"], "high");
        assert_eq!(body["max_completion_tokens"], 1024);
        assert!(body.get("max_tokens").is_none());
        assert!(body.get("temperature").is_none());
    }

    #[test]
    fn off_disables_effort() {
        let p = provider("https://api.deepseek.com/v1", "deepseek-reasoner", "off");
        let body = p.build_request_body(vec![], vec![], false);
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn streaming_requests_ask_for_usage() {
        let p = provider("https://api.openai.com/v1", "gpt-4o", "off");
        let body = p.build_request_body(vec![], vec![], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
        let unary = p.build_request_body(vec![], vec![], false);
        assert!(unary.get("stream_options").is_none());
        // DeepSeek already reports streamed usage; do not add a field its
        // tolerance of is unverified.
        let ds = provider("https://api.deepseek.com/v1", "deepseek-chat", "off");
        assert!(ds.build_request_body(vec![], vec![], true).get("stream_options").is_none());
    }

    #[test]
    fn reasoning_content_is_not_echoed_back() {
        let msg = Message {
            role: Role::Assistant,
            content: MessageContent::Text("hi".into()),
            reasoning_content: Some("internal monologue".into()),
            ..Default::default()
        };
        let out = serialize_messages(&[msg]);
        assert!(out[0].get("reasoning_content").is_none());
    }

    #[test]
    fn tool_call_delta_without_index_opens_a_new_slot() {
        let mut next = 0u32;
        let first = parse_sse_chunk(
            r#"data: {"choices":[{"delta":{"tool_calls":[{"id":"c1","function":{"name":"a","arguments":"{"}}]}}]}"#,
            &mut next,
        )
        .unwrap();
        assert_eq!(first.tool_calls.as_ref().unwrap()[0].index, 0);

        let fragment = parse_sse_chunk(
            r#"data: {"choices":[{"delta":{"tool_calls":[{"function":{"arguments":"}"}}]}}]}"#,
            &mut next,
        )
        .unwrap();
        assert_eq!(fragment.tool_calls.as_ref().unwrap()[0].index, 0);

        let second = parse_sse_chunk(
            r#"data: {"choices":[{"delta":{"tool_calls":[{"id":"c2","function":{"name":"b","arguments":"{}"}}]}}]}"#,
            &mut next,
        )
        .unwrap();
        // Must not collapse into slot 0 and concatenate both calls.
        assert_eq!(second.tool_calls.as_ref().unwrap()[0].index, 1);
    }

    #[test]
    fn parses_data_prefix_without_a_space() {
        let mut next = 0u32;
        let chunk = parse_sse_chunk(
            r#"data:{"choices":[{"delta":{"content":"hi"}}]}"#,
            &mut next,
        )
        .unwrap();
        assert_eq!(chunk.content.as_deref(), Some("hi"));
    }

    #[test]
    fn unparseable_frame_is_reported_not_dropped() {
        let mut next = 0u32;
        let err = parse_sse_chunk("data: {\"choices\":[", &mut next).unwrap_err();
        assert!(matches!(err, ProviderError::Parse(_)));
    }
}
