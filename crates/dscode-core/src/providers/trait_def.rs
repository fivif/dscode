//! LLM Provider trait — standard interface for all model backends.

use async_trait::async_trait;
use futures::stream::Stream;
use serde::{Deserialize, Serialize};
use std::pin::Pin;
use std::time::Duration;

/// Standard message format (OpenAI/Claude compatible).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: MessageContent,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// DeepSeek reasoning_content (thinking mode)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    /// Unix timestamp (set when loaded from DB).
    #[serde(default)]
    pub created_at: i64,
}

impl Default for Message {
    fn default() -> Self {
        Self {
            role: Role::User, content: MessageContent::Text(String::new()),
            name: None, tool_calls: None, tool_call_id: None,
            reasoning_content: None, created_at: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

impl MessageContent {
    pub fn as_text(&self) -> Option<&str> {
        match self {
            MessageContent::Text(s) => Some(s),
            MessageContent::Parts(parts) => {
                for p in parts {
                    if let ContentPart::Text { text } = p {
                        return Some(text);
                    }
                }
                None
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.as_text().map(|s| s.is_empty()).unwrap_or(true)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text { text: String },
    ToolUse { id: String, name: String, input: serde_json::Value },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type", default = "tool_type_default")]
    pub call_type: String,
    pub function: FunctionCall,
}

fn tool_type_default() -> String { "function".into() }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

/// Tool definition (OpenAI format).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: FunctionDef,
}

fn tool_def_type() -> String { "function".into() }

impl ToolDef {
    pub fn new(name: &str, description: &str, parameters: serde_json::Value) -> Self {
        Self {
            tool_type: tool_def_type(),
            function: FunctionDef {
                name: name.to_string(),
                description: Some(description.to_string()),
                parameters: Some(parameters),
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDef {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<serde_json::Value>,
}

/// Streaming chunk from any LLM provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamChunk {
    /// Text delta token
    pub content: Option<String>,
    /// Tool call delta (for streaming tool calls)
    pub tool_calls: Option<Vec<ToolCallDelta>>,
    /// DeepSeek reasoning content
    pub reasoning_content: Option<String>,
    /// Is this the final chunk?
    pub finish_reason: Option<String>,
    /// Usage info (usually on final chunk)
    pub usage: Option<crate::agent::stream::UsageInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallDelta {
    pub index: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function: Option<FunctionDelta>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
}

/// Non-streaming chat response.
#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    pub usage: Option<crate::agent::stream::UsageInfo>,
    pub reasoning_content: Option<String>,
}

/// The core LLM Provider trait.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Send a chat completion request (non-streaming).
    async fn chat(
        &self,
        messages: Vec<Message>,
        tools: Vec<ToolDef>,
    ) -> Result<ChatResponse, ProviderError>;

    /// Send a chat completion request, returning a stream of chunks.
    async fn chat_stream(
        &self,
        messages: Vec<Message>,
        tools: Vec<ToolDef>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, ProviderError>> + Send>>, ProviderError>;

    /// Clone this provider into a new boxed instance.
    /// Required so Forge can spawn sub-agents that each own their provider.
    fn clone_box(&self) -> Box<dyn LlmProvider>;
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum ProviderError {
    #[error("HTTP error: {0}")]
    Http(String),
    #[error("API error: {status} - {message}")]
    Api { status: u16, message: String },
    /// API error that kept the vendor's structured envelope
    /// (`error.type` / `error.code` / `error.param`), so a caller can tell an
    /// auth failure from a quota, context-overflow or content-policy failure
    /// instead of seeing one flattened message string.
    #[error("API error: {status} - {message}")]
    ApiDetail {
        status: u16,
        /// Vendor error class, e.g. `invalid_request_error`, `overloaded_error`.
        kind: String,
        /// Vendor error code (OpenAI `error.code`), when the vendor sent one.
        code: Option<String>,
        /// Vendor parameter the error points at, when the vendor sent one.
        param: Option<String>,
        message: String,
    },
    #[error("Parse error: {0}")]
    Parse(String),
    #[error("No API key configured for this provider")]
    NoApiKey,
    /// Stream was interrupted mid-flight (byte-stream error or read timeout).
    /// The upstream agent can detect this and retry the turn.
    #[error("stream interrupted: {0}")]
    StreamInterrupted(String),
    /// The channel is switched off in config (`[providers.<key>] enabled = false`).
    /// Retrying can never succeed until the user edits the config.
    #[error("provider channel disabled: {0}")]
    Disabled(String),
}

impl ProviderError {
    /// HTTP status, when the failure came from an API response.
    pub fn status(&self) -> Option<u16> {
        match self {
            ProviderError::Api { status, .. } | ProviderError::ApiDetail { status, .. } => {
                Some(*status)
            }
            _ => None,
        }
    }

    /// Vendor error class (Anthropic `error.type`, OpenAI `error.type`), when known.
    pub fn error_kind(&self) -> Option<&str> {
        match self {
            ProviderError::ApiDetail { kind, .. } if !kind.is_empty() => Some(kind),
            _ => None,
        }
    }

    /// Vendor error code (OpenAI `error.code`), when known.
    pub fn error_code(&self) -> Option<&str> {
        match self {
            ProviderError::ApiDetail { code, .. } => code.as_deref(),
            _ => None,
        }
    }

    /// Whether a failed **stream** may be retried once as a unary request.
    ///
    /// Only transport / parse failures qualify. An `Api` / `ApiDetail` status
    /// means the server received and rejected the very same body — re-sending it
    /// as `chat()` cannot succeed, bills a second generation, and turns a 429
    /// into a worse rate limit. Those belong to the retry-with-backoff path.
    pub fn allows_unary_fallback(&self) -> bool {
        matches!(
            self,
            ProviderError::Http(_)
                | ProviderError::Parse(_)
                | ProviderError::StreamInterrupted(_)
        )
    }

    /// Whether retrying the *same* request could succeed.
    ///
    /// Deliberately conservative: transport hiccups, an interrupted stream and
    /// explicit rate-limit / 5xx statuses are retryable; every other 4xx, a
    /// missing key and a disabled channel are permanent. Consumers deciding
    /// whether to re-send a (possibly already billed) generation must consult
    /// this instead of retrying every `Err`.
    pub fn is_retryable(&self) -> bool {
        match self {
            ProviderError::Api { status, message }
            | ProviderError::ApiDetail { status, message, .. } => {
                matches!(*status, 408 | 409 | 425 | 429 | 500 | 502 | 503 | 504 | 529)
                    || message_looks_transient(message)
            }
            ProviderError::Http(e) => message_looks_transient(e),
            // A truncated stream can be re-requested; its own doc comment
            // promises exactly that.
            ProviderError::StreamInterrupted(_) => true,
            ProviderError::Parse(_) | ProviderError::NoApiKey | ProviderError::Disabled(_) => false,
        }
    }

    /// Whether the provider is telling us the request exceeded the model's
    /// context window.
    ///
    /// This is the one signal that beats every local estimate. The configured
    /// `window_tokens` is a number the user typed and nothing ties it to the
    /// window of the model actually behind the channel, so a pipeline that
    /// only compares against that estimate can sail past the real limit and
    /// never fire — the turn then dies on a 400 the estimate said "cannot
    /// happen". A caller that sees this can compress and re-send the very
    /// same turn instead of failing it.
    ///
    /// Both halves must agree: the status must be 400 (OpenAI, Anthropic and
    /// most gateways) or 413 (some relays), **and** either the vendor's `code`
    /// or the message text must name the context limit. A plain 400
    /// (`invalid model`, a malformed body) and a 401/429 are never classified
    /// here: a false positive re-sends a request that is guaranteed to fail
    /// again and bills a second generation for it.
    pub fn is_context_overflow(&self) -> bool {
        let (status, code, message) = match self {
            ProviderError::Api { status, message } => (*status, None, message.as_str()),
            ProviderError::ApiDetail {
                status,
                code,
                message,
                ..
            } => (*status, code.as_deref(), message.as_str()),
            _ => return false,
        };

        if status != 400 && status != 413 {
            return false;
        }

        // OpenAI's own code, when the gateway preserved the envelope.
        if code.map_or(false, |c| c.eq_ignore_ascii_case("context_length_exceeded")) {
            return true;
        }
        message_names_context_limit(message)
    }
}

/// Substrings that mark a 4xx body as "your prompt is over the model's context
/// window" rather than some other bad request.
///
/// The wording is not part of any contract and differs per vendor: OpenAI
/// sends `code: "context_length_exceeded"` with "This model's maximum context
/// length is N tokens", Anthropic "prompt is too long: N tokens > M maximum",
/// and Chinese relays say "输入过长" / "上下文…". Matching the message text is
/// the only portable option; the status check in
/// [`ProviderError::is_context_overflow`] keeps the false-positive rate down.
fn message_names_context_limit(message: &str) -> bool {
    let m = message.to_lowercase();
    const NEEDLES: &[&str] = &[
        "context_length_exceeded",
        "context length",
        "maximum context",
        "too many tokens",
        "prompt is too long",
        "context window",
        "reduce the length",
        "输入过长",
        "上下文",
    ];
    NEEDLES.iter().any(|needle| m.contains(needle))
}

fn message_looks_transient(message: &str) -> bool {
    let lower = message.to_lowercase();
    lower.contains("timeout")
        || lower.contains("timed out")
        || lower.contains("connection")
        || lower.contains("reset")
        || lower.contains("broken pipe")
        || lower.contains("temporarily")
        || lower.contains("rate limit")
        || lower.contains("overloaded")
        || lower.contains("try again")
        || lower.contains("capacity")
        || lower.contains("429")
        || lower.contains("502")
        || lower.contains("503")
        || lower.contains("504")
        || lower.contains("529")
}

/// Turn a non-2xx response body into a [`ProviderError`].
///
/// All three providers share one vendor envelope shape
/// (`{"error": {"type"|"code"|"param"|"message"}}`), so the parsing lives here
/// instead of being re-flattened — to `error.message` only — in each provider.
///
/// `model` is used to add an actionable hint when the API rejects the request
/// because of extended thinking.
pub fn api_error_from_body(status: u16, raw_body: &str, model: &str) -> ProviderError {
    let parsed = serde_json::from_str::<serde_json::Value>(raw_body).ok();
    let err = parsed.as_ref().and_then(|v| v.get("error"));

    let mut message = err
        .and_then(|e| e.get("message"))
        .and_then(|m| m.as_str())
        .or_else(|| {
            parsed
                .as_ref()
                .and_then(|v| v.get("message"))
                .and_then(|m| m.as_str())
        })
        .map(|s| s.to_string())
        .unwrap_or_else(|| raw_body.to_string());

    let kind = err
        .and_then(|e| e.get("type"))
        .and_then(|t| t.as_str())
        .unwrap_or_default()
        .to_string();
    // Anthropic reports the class in `error.type`; OpenAI puts it in `code`.
    let code = err
        .and_then(|e| e.get("code"))
        .and_then(|c| c.as_str())
        .map(str::to_string);
    let param = err
        .and_then(|e| e.get("param"))
        .and_then(|p| p.as_str())
        .map(str::to_string);

    if status == 400 && rejects_thinking_parameter(&message) {
        message.push_str(&format!(
            " (hint: model `{model}` may not accept extended thinking — \
             set generation.reasoning_effort = \"off\" for this channel)"
        ));
    }

    if kind.is_empty() && code.is_none() && param.is_none() {
        return ProviderError::Api { status, message };
    }
    ProviderError::ApiDetail {
        status,
        kind,
        code,
        param,
        message,
    }
}

/// True when a 400 body is complaining about the `thinking` request parameter
/// itself (as opposed to a malformed thinking *block* in the history).
fn rejects_thinking_parameter(message: &str) -> bool {
    let m = message.to_lowercase();
    let mentions = m.contains("thinking") || m.contains("budget_tokens") || m.contains("output_config");
    let rejected = m.contains("unsupported")
        || m.contains("not supported")
        || m.contains("unexpected")
        || m.contains("unknown")
        || m.contains("extra inputs")
        || m.contains("unrecognized");
    mentions && rejected
}

/// Value of a single-line SSE field, tolerating the spec-optional space after
/// the colon (`data:{…}` is as valid as `data: {…}`).
pub fn sse_field<'a>(line: &'a str, field: &str) -> Option<&'a str> {
    let rest = line.strip_prefix(field)?.strip_prefix(':')?;
    Some(rest.strip_prefix(' ').unwrap_or(rest))
}

/// Normalize a configured API base URL: trim whitespace and trailing `/`, then
/// drop a single trailing `/v1`.
///
/// Both the Anthropic Messages endpoint and the Responses endpoint append their
/// own path, so a base copied from the OpenAI channel (`…/v1`) would otherwise
/// become `…/v1/v1/messages` and 404 on every turn.
pub fn normalize_api_base_url(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    trimmed
        .strip_suffix("/v1")
        .unwrap_or(trimmed)
        .trim_end_matches('/')
        .to_string()
}

/// Build the HTTP client for a provider.
///
/// A bad proxy URL (e.g. `127.0.0.1:7890`, missing scheme) falls back to a
/// direct connection and is logged, instead of panicking inside the provider
/// constructor the way the old `.expect("Failed to build HTTP client")` did.
/// (The last-resort `Client::new()` can still panic, but only if the TLS backend
/// itself fails to initialise.)
///
/// `streaming = true` omits the total request timeout on purpose — reqwest's
/// `Client::timeout` covers the response *body*, i.e. the entire SSE stream, so
/// the 180 s cap used for unary calls silently truncates long thinking turns.
/// The per-chunk idle timeout inside each provider still bounds a stalled
/// stream, and `connect_timeout` stays in place.
pub fn http_client(proxy_url: Option<&str>, streaming: bool) -> reqwest::Client {
    match build_client(proxy_url, streaming) {
        Ok(client) => client,
        Err(e) => {
            tracing::error!(
                error = %e,
                "invalid proxy URL for the provider HTTP client — using a direct connection"
            );
            match build_client(None, streaming) {
                Ok(client) => client,
                Err(e) => {
                    tracing::error!(error = %e, "failed to build HTTP client — using the default client");
                    reqwest::Client::new()
                }
            }
        }
    }
}

fn build_client(proxy_url: Option<&str>, streaming: bool) -> Result<reqwest::Client, String> {
    let mut builder = reqwest::Client::builder().connect_timeout(Duration::from_secs(30));
    if !streaming {
        builder = builder.timeout(Duration::from_secs(180));
    }
    if let Some(url) = proxy_url.map(str::trim).filter(|u| !u.is_empty()) {
        let proxy = reqwest::Proxy::all(url).map_err(|e| format!("无效代理 URL: {e}"))?;
        builder = builder.proxy(proxy);
    }
    builder.build().map_err(|e| e.to_string())
}

impl From<reqwest::Error> for ProviderError {
    fn from(e: reqwest::Error) -> Self {
        ProviderError::Http(e.to_string())
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn api(status: u16, message: &str) -> ProviderError {
        ProviderError::Api {
            status,
            message: message.into(),
        }
    }

    fn api_detail(status: u16, code: Option<&str>, message: &str) -> ProviderError {
        ProviderError::ApiDetail {
            status,
            kind: "invalid_request_error".into(),
            code: code.map(str::to_string),
            param: None,
            message: message.into(),
        }
    }

    /// OpenAI's structured envelope: a 400 whose `code` is the machine-readable
    /// form. This is the case the user's gateway hits.
    #[test]
    fn context_overflow_openai_code() {
        let e = api_detail(
            400,
            Some("context_length_exceeded"),
            "This model's maximum context length is 128000 tokens.",
        );
        assert!(e.is_context_overflow());
    }

    /// Anthropic has no such code — the wording is all there is.
    #[test]
    fn context_overflow_anthropic_wording() {
        let e = api_detail(
            400,
            None,
            "prompt is too long: 210000 tokens > 200000 maximum",
        );
        assert!(e.is_context_overflow());
    }

    /// Relays flatten the envelope into `Api { message }` and are not
    /// consistent about the wording.
    #[test]
    fn context_overflow_relay_wordings() {
        for message in [
            "Input validation error: too many tokens in this request",
            "The request exceeds the context window of this model",
            "Please reduce the length of the messages",
            "maximum context length is 8192 tokens",
            "请求失败:输入过长,请缩短对话",
            "上下文长度超出模型限制",
        ] {
            let e = api(400, message);
            assert!(
                e.is_context_overflow(),
                "should be detected as overflow: {message}"
            );
        }
    }

    /// Some relays answer an oversized body with 413 instead of 400.
    #[test]
    fn context_overflow_accepts_413() {
        let e = api(413, "payload too large: context length exceeded");
        assert!(e.is_context_overflow());
        let e = api_detail(413, Some("context_length_exceeded"), "");
        assert!(e.is_context_overflow());
    }

    /// A false positive re-sends a request that is guaranteed to fail and bills
    /// a second generation — every one of these must stay a plain error.
    #[test]
    fn context_overflow_rejects_other_failures() {
        // Ordinary bad requests.
        assert!(!api_detail(
            400,
            Some("invalid_model"),
            "The model `gpt-5-ultra` does not exist"
        )
        .is_context_overflow());
        assert!(!api(400, "bad request: missing required field 'messages'").is_context_overflow());
        assert!(!api(400, "unsupported parameter: temperature").is_context_overflow());
        assert!(!api_detail(400, Some("content_policy_violation"), "blocked").is_context_overflow());
        // Auth / quota / rate limits — never an overflow, whatever the text.
        assert!(!api(401, "invalid api key").is_context_overflow());
        assert!(!api(403, "forbidden").is_context_overflow());
        assert!(!api(429, "rate limit exceeded, context window quota reached")
            .is_context_overflow());
        // 500s and transport failures carry no context-limit claim.
        assert!(!api(500, "internal error").is_context_overflow());
        assert!(!ProviderError::Http("connection reset by peer".into()).is_context_overflow());
        assert!(!ProviderError::StreamInterrupted("context length".into()).is_context_overflow());
        assert!(!ProviderError::Parse("unexpected token".into()).is_context_overflow());
        assert!(!ProviderError::NoApiKey.is_context_overflow());
        assert!(!ProviderError::Disabled("openai".into()).is_context_overflow());
    }

    /// Regression guard for the reason this method exists: the status gate is
    /// what keeps a *settable* field like `window_tokens` from turning an
    /// unrelated 400 into a billed retry.
    #[test]
    fn context_overflow_requires_a_4xx_body_status() {
        // Same message text, different statuses.
        assert!(api(400, "context window exceeded").is_context_overflow());
        assert!(!api(404, "context window exceeded").is_context_overflow());
        assert!(!api(422, "context window exceeded").is_context_overflow());
        assert!(!api(400, "the word context appears but not as a limit")
            .is_context_overflow());
    }
}
