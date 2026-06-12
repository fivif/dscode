//! OpenAI-compatible provider — works with DeepSeek, Groq, OpenAI, and any
//! OpenAI-compatible API endpoint. This is the default provider.

use super::trait_def::*;
use async_trait::async_trait;
use futures::stream::Stream;
use reqwest::Client;
use std::pin::Pin;
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
        Self {
            api_key,
            base_url,
            model,
            max_tokens: 8192,
            temperature: 0.0,
            reasoning_effort: Some("max".into()),
            client: Client::new(),
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

        Self {
            api_key: provider_conf.api_key,
            base_url,
            model: actual_model,
            max_tokens: conf.generation.max_tokens,
            temperature: conf.generation.temperature,
            reasoning_effort: Some(conf.generation.reasoning_effort.clone()),
            client: Client::new(),
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
        let body: serde_json::Value = serde_json::from_str(&raw_body)
            .map_err(|e| ProviderError::Parse(format!("JSON parse error: {}. Body: {}", e, &raw_body[..raw_body.len().min(200)])))?;

        if !status.is_success() {
            let msg = body["error"]["message"]
                .as_str()
                .unwrap_or("Unknown error")
                .to_string();
            return Err(ProviderError::Api {
                status: status.as_u16(),
                message: msg,
            });
        }

        let choice = &body["choices"][0];
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
            let body = resp.text().await?;
            return Err(ProviderError::Api {
                status: status.as_u16(),
                message: body,
            });
        }

        let stream = resp
            .bytes_stream()
            .map(|result| -> Result<StreamChunk, ProviderError> {
                let bytes = result?;
                let text = String::from_utf8_lossy(&bytes);
                parse_sse_chunk(&text)
            });

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

        // DeepSeek-specific: reasoning_effort via extra_body or direct param
        if let Some(ref effort) = self.reasoning_effort {
            body["reasoning_effort"] = serde_json::Value::String(effort.clone());
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
            if let Some(ref tc_id) = m.tool_call_id {
                value["tool_call_id"] = serde_json::Value::String(tc_id.clone());
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
    // SSE format: "data: {...}\n\n"
    let mut result = StreamChunk {
        content: None,
        tool_calls: None,
        reasoning_content: None,
        finish_reason: None,
        usage: None,
    };

    for line in text.lines() {
        let data = match line.strip_prefix("data: ") {
            Some(d) => d.trim(),
            None => continue,
        };
        if data == "[DONE]" {
            result.finish_reason = Some("stop".into());
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
                        }
                        if let Some(rc) = delta["reasoning_content"].as_str() {
                            result.reasoning_content = Some(rc.to_string());
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
                    cache_read_tokens: usage["prompt_cache_hit_tokens"]
                        .as_u64()
                        .unwrap_or(0),
                    cache_write_tokens: usage["prompt_cache_miss_tokens"]
                        .as_u64()
                        .unwrap_or(0),
                });
            }
        }
    }

    Ok(result)
}
