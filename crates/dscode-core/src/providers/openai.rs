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
    client: Client,
}

impl OpenAiProvider {
    pub fn new(api_key: String, base_url: String, model: String) -> Self {
        let base_url = base_url.trim_end_matches('/').to_string();
        let client = build_client("");
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

    /// Configure with settings from Config.
    pub fn from_config(model: &str, conf: &crate::config::settings::Config) -> Self {
        let provider_conf = conf
            .provider_for_model(model)
            .unwrap_or_else(|| crate::config::settings::ProviderConfig {
                api_key: String::new(),
                base_url: "https://api.deepseek.com/v1".into(),
                enabled: true,
            });

        let mut base_url = provider_conf.base_url.trim_end_matches('/').to_string();
        // Ensure /v1 path for OpenAI-compatible endpoints
        if !base_url.ends_with("/v1") && !base_url.contains("/v1/") {
            base_url = format!("{}/v1", base_url);
        }

        // Strip provider prefix (openai/gpt-4o -> gpt-4o)
        let actual_model = match model.split_once('/') {
            Some((_, m)) => m.to_string(),
            None => model.to_string(),
        };

        let client = build_client(&conf.generation.proxy_url);

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

/// Build a reqwest Client with optional proxy support.
fn build_client(proxy_url: &str) -> Client {
    let mut builder = Client::builder().timeout(Duration::from_secs(300));
    if !proxy_url.is_empty() {
        let proxy = reqwest::Proxy::all(proxy_url)
            .expect("Failed to build proxy");
        builder = builder.proxy(proxy);
    }
    builder.build().expect("Failed to build HTTP client")
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
            // P5: Try to parse error.message from JSON, fall back to raw text
            let error_msg = serde_json::from_str::<serde_json::Value>(&raw_body)
                .ok()
                .and_then(|v| v["error"]["message"].as_str().map(|s| s.to_string()))
                .unwrap_or(raw_body);
            return Err(ProviderError::Api {
                status: status.as_u16(),
                message: error_msg,
            });
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
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request_body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            // P5: Try to parse error.message from JSON, fall back to raw text
            let raw_body = resp.text().await?;
            let error_msg = serde_json::from_str::<serde_json::Value>(&raw_body)
                .ok()
                .and_then(|v| v["error"]["message"].as_str().map(|s| s.to_string()))
                .unwrap_or(raw_body);
            return Err(ProviderError::Api {
                status: status.as_u16(),
                message: error_msg,
            });
        }

        // P1: Buffer TCP chunks into complete SSE events (delimited by \n\n)
        let byte_stream = resp.bytes_stream();
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<String, ProviderError>>(64);

        tokio::spawn(async move {
            let mut buffer = String::new();
            futures::pin_mut!(byte_stream);
            while let Some(result) = tokio_stream::StreamExt::next(&mut byte_stream).await {
                match result {
                    Ok(bytes) => {
                        buffer.push_str(&String::from_utf8_lossy(&bytes));
                        while let Some(pos) = buffer.find("\n\n") {
                            let event = buffer[..pos].to_string();
                            buffer = buffer[pos + 2..].to_string();
                            if tx.send(Ok(event)).await.is_err() {
                                return; // receiver dropped
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(ProviderError::Http(e))).await;
                        return;
                    }
                }
            }
        });

        let stream = tokio_stream::wrappers::ReceiverStream::new(rx)
            .map(|result| result.and_then(|text| parse_sse_chunk(&text)));

        Ok(Box::pin(stream))
    }
}

impl OpenAiProvider {
    fn build_request_body(
        &self,
        messages: Vec<Message>,
        tools: Vec<ToolDef>,
        stream: bool,
    ) -> serde_json::Value {
        let mut body = serde_json::json!({
            "model": self.model,
            "messages": serialize_messages(&messages),
            "temperature": self.temperature,
            "stream": stream,
        });
        if self.max_tokens > 0 {
            body["max_tokens"] = serde_json::Value::Number(serde_json::Number::from(self.max_tokens));
        }

        if !tools.is_empty() {
            body["tools"] = serde_json::to_value(&tools).unwrap();
        }

        // P8: DeepSeek-specific reasoning_effort — only include for DeepSeek
        // to avoid 400 errors on OpenAI/Groq endpoints.
        let is_deepseek = self.base_url.contains("deepseek");
        if is_deepseek {
            if let Some(ref effort) = self.reasoning_effort {
                if !effort.is_empty() {
                    body["reasoning_effort"] = serde_json::Value::String(effort.clone());
                }
            }
        }

        body
    }
}

fn serialize_messages(msgs: &[Message]) -> Vec<serde_json::Value> {
    msgs.iter()
        .map(|m| {
            let mut value = serde_json::json!({
                "role": m.role,
            });

            match &m.content {
                MessageContent::Text(text) => {
                    value["content"] = serde_json::Value::String(text.clone());
                }
                MessageContent::Parts(parts) => {
                    value["content"] = serde_json::to_value(parts).unwrap();
                }
            }

            if let Some(ref name) = m.name {
                value["name"] = serde_json::Value::String(name.clone());
            }
            if let Some(ref tool_calls) = m.tool_calls {
                value["tool_calls"] = serde_json::to_value(tool_calls).unwrap();
            }
            // tool_call_id belongs ONLY on Role::Tool messages per OpenAI protocol.
            // Assistant messages carry tool call IDs exclusively in tool_calls[].id.
            if m.role == Role::Tool {
                if let Some(ref tc_id) = m.tool_call_id {
                    value["tool_call_id"] = serde_json::Value::String(tc_id.clone());
                }
            }
            if let Some(ref reasoning) = m.reasoning_content {
                value["reasoning_content"] = serde_json::Value::String(reasoning.clone());
            }

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

fn parse_sse_chunk(text: &str) -> Result<StreamChunk, ProviderError> {
    let mut result = StreamChunk {
        content: None,
        tool_calls: None,
        reasoning_content: None,
        finish_reason: None,
        usage: None,
    };

    let mut has_valid_data = false;

    for line in text.lines() {
        let data = match line.strip_prefix("data: ") {
            Some(d) => d.trim(),
            None => continue,
        };
        if data == "[DONE]" {
            // P2: Only set finish_reason to "stop" if not already set from a prior delta
            if result.finish_reason.is_none() {
                result.finish_reason = Some("stop".into());
            }
            has_valid_data = true;
            continue;
        }
        if let Ok(chunk) = serde_json::from_str::<serde_json::Value>(data) {
            if let Some(choices) = chunk["choices"].as_array() {
                if let Some(choice) = choices.first() {
                    if let Some(delta) = choice.get("delta") {
                        if let Some(c) = delta["content"].as_str() {
                            result.content = Some(format!(
                                "{}{}",
                                result.content.as_deref().unwrap_or(""),
                                c
                            ));
                            has_valid_data = true;
                        }
                        if let Some(rc) = delta["reasoning_content"].as_str() {
                            // P3: Accumulate reasoning_content across deltas with push_str / +=
                            let mut accumulated = result.reasoning_content.unwrap_or_default();
                            accumulated.push_str(rc);
                            result.reasoning_content = Some(accumulated);
                            has_valid_data = true;
                        }
                        // Accumulate tool calls from delta
                        if let Some(tc_deltas) = delta["tool_calls"].as_array() {
                            let mut parsed: Vec<ToolCallDelta> = vec![];
                            for tc in tc_deltas {
                                parsed.push(ToolCallDelta {
                                    index: tc["index"].as_u64().unwrap_or(0) as u32,
                                    id: tc["id"].as_str().map(|s| s.to_string()),
                                    function: tc.get("function").map(|f| FunctionDelta {
                                        name: f["name"].as_str().map(|s| s.to_string()),
                                        arguments: f["arguments"]
                                            .as_str()
                                            .map(|s| s.to_string()),
                                    }),
                                });
                            }
                            if !parsed.is_empty() {
                                result.tool_calls = Some(parsed);
                                has_valid_data = true;
                            }
                        }
                    }
                    if let Some(fr) = choice["finish_reason"].as_str() {
                        result.finish_reason = Some(fr.to_string());
                        has_valid_data = true;
                    }
                }
            }
            if let Some(usage) = chunk.get("usage") {
                result.usage = Some(crate::agent::stream::UsageInfo {
                    input_tokens: usage["prompt_tokens"].as_u64().unwrap_or(0),
                    output_tokens: usage["completion_tokens"].as_u64().unwrap_or(0),
                    cache_read_tokens: usage["prompt_cache_hit_tokens"]
                        .as_u64()
                        .unwrap_or(0),
                    cache_write_tokens: usage["prompt_cache_miss_tokens"]
                        .as_u64()
                        .unwrap_or(0),
                });
                has_valid_data = true;
            }
        }
    }

    // P9: Filter out empty chunks — return Err to signal "no meaningful data"
    if !has_valid_data {
        return Err(ProviderError::Parse("Empty SSE chunk".into()));
    }

    Ok(result)
}
