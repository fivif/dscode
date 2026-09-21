//! `do_image_generate` — text-to-image through the user's configured provider.
//!
//! Talks to the OpenAI-compatible `POST {base_url}/images/generations` endpoint
//! of whichever channel the user configured (third-party gateways included —
//! the `base_url`/`api_key` always come from `~/.dscode/config.toml`, never
//! from a hard-coded vendor host), decodes either response shape
//! (`b64_json` or a `url` to download), saves every image under
//! `~/.dscode/images/`, and reports each file as a markdown line the desktop
//! UI renders inline:
//!
//! ```text
//! ![描述](dscode-image:<绝对路径>)
//! ```

use async_trait::async_trait;
use base64::Engine;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::agent::stream::StreamEvent;
use crate::config::settings::Config;
use crate::tools::trait_def::{Tool, ToolContext, ToolError, ToolResult};

/// Fallback when the selected channel has no `base_url` (hand-edited config).
const DEFAULT_IMAGE_BASE: &str = "https://api.openai.com/v1";
/// `n` is clamped to this; one call must never fan out without bound.
const MAX_IMAGES: u64 = 4;
/// Per-image ceiling, applied to decoded base64 *and* to downloads.
const MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024;
/// Base64 text longer than this cannot decode to ≤ `MAX_IMAGE_BYTES` (4 chars
/// per 3 bytes, plus slack for a `data:` prefix and line breaks), so an
/// oversized inline image is refused *before* the decode allocates its bytes.
const MAX_B64_CHARS: usize = MAX_IMAGE_BYTES / 3 * 4 + 64;
/// Whole JSON response ceiling (4 PNGs as base64 ≈ a few MB; this is slack).
const MAX_JSON_BYTES: usize = 64 * 1024 * 1024;
/// Error bodies are small; only this much is kept for the message.
const MAX_ERROR_BODY_BYTES: usize = 8 * 1024;
/// Image generation is slow. reqwest's `Client::timeout` covers the whole
/// response body, so the shared web client's 30 s would abort a `dall-e-3` call
/// mid-flight; each request overrides it (see `RequestBuilder::timeout`).
const IMAGE_TIMEOUT: Duration = Duration::from_secs(300);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);

const CHANNELS: [&str; 4] = ["deepseek", "openai", "anthropic", "ollama"];

// ── pure helpers (unit-tested below, no network) ───────────────────────────

/// Endpoint for image generation, derived from a configured `base_url`.
///
/// Mirrors how the OpenAI-compatible provider builds its own URL
/// (`providers/openai.rs`: append `/v1` unless a `/v1` segment is already
/// there), so an image call reaches the same host — and the same relay — as the
/// chat traffic. Trailing slashes and a missing/duplicated `/v1` are absorbed:
///
/// - `https://x/v1`          → `https://x/v1/images/generations`
/// - `https://x`             → `https://x/v1/images/generations`
/// - `https://x/v1/`         → `https://x/v1/images/generations`
/// - `https://x/openai/v1`   → `https://x/openai/v1/images/generations`
fn images_endpoint(base_url: &str) -> String {
    let base = base_url.trim().trim_end_matches('/');
    let base = if base.is_empty() { DEFAULT_IMAGE_BASE } else { base };
    if base.ends_with("/v1") || base.contains("/v1/") {
        format!("{base}/images/generations")
    } else {
        format!("{base}/v1/images/generations")
    }
}

/// Filesystem-safe stem from the prompt: letters, digits and CJK survive,
/// everything else collapses to a single `-`. Never empty, never a path
/// separator, never a Windows device name (the timestamp prefix in the file
/// name rules those out on its own), capped at ~40 chars.
fn slugify(prompt: &str) -> String {
    const MAX: usize = 40;
    let mut slug = String::new();
    let mut pending_dash = false;
    for c in prompt.chars() {
        if slug.chars().count() >= MAX {
            break;
        }
        if c.is_alphanumeric() {
            if pending_dash && !slug.is_empty() {
                slug.push('-');
            }
            pending_dash = false;
            slug.push(c);
        } else {
            pending_dash = true;
        }
    }
    while slug.chars().count() > MAX {
        slug.pop();
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "image".to_string()
    } else {
        slug
    }
}

/// Alt text for the markdown line: markdown syntax chars and newlines would
/// break the `![alt](path)` shape the UI parses.
fn sanitize_alt(prompt: &str) -> String {
    const MAX: usize = 60;
    let cleaned: String = prompt
        .chars()
        .map(|c| match c {
            '[' | ']' | '(' | ')' | '`' | '\n' | '\r' | '\t' => ' ',
            other => other,
        })
        .collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return "生成的图片".to_string();
    }
    let mut alt: String = collapsed.chars().take(MAX).collect();
    if collapsed.chars().count() > MAX {
        alt.push('…');
    }
    alt
}

/// `n` as actually sent: 1..=4, and always 1 for `dall-e-3` (the model rejects
/// `n > 1` with a 400, killing the whole call).
fn clamp_n(requested: u64, model: &str) -> u32 {
    if model.to_ascii_lowercase().contains("dall-e-3") {
        return 1;
    }
    requested.clamp(1, MAX_IMAGES) as u32
}

/// `gpt-image-1` rejects `response_format` (it always answers with base64).
fn wants_response_format(model: &str) -> bool {
    !model.to_ascii_lowercase().contains("gpt-image")
}

/// Whether a model id denotes an image-generation model rather than a chat one.
///
/// The chat path uses this to route a turn straight to `/images/generations`:
/// an image-only model rejects `/chat/completions` outright, so there is no
/// agent loop to run — the user's message *is* the prompt.
///
/// Name-based by necessity: neither OpenAI-compatible relays nor their `/models`
/// listings carry a capability field. The list therefore errs toward *not*
/// claiming a model is an image model — a false positive sends a chat turn to
/// the image endpoint and breaks it, while a false negative only means the user
/// goes through the tool path instead.
pub fn is_image_model(model: &str) -> bool {
    let m = model.trim().to_ascii_lowercase();
    if m.is_empty() {
        return false;
    }
    // Drop any surviving `provider/` prefix so `openai/gpt-image-1` matches.
    let m = m.rsplit('/').next().unwrap_or(m.as_str());

    const IMAGE_MARKERS: &[&str] = &[
        "gpt-image",
        "dall-e",
        "dalle",
        "stable-diffusion",
        "sdxl",
        "sd3",
        "imagen",
        "midjourney",
        "nano-banana",
        "seedream",
        "qwen-image",
        "kolors",
        "flux",
    ];
    IMAGE_MARKERS.iter().any(|p| m.contains(p))
}

/// Sizes a model family accepts, or `None` when the family is unknown.
///
/// These sets are **not** interchangeable: `1792x1024` is dall-e-3-only,
/// `1536x1024` and `auto` are gpt-image-only, and `512x512` belongs to dall-e-2.
/// Sending a size a model does not support is a hard 400, so this table has to
/// exist on the request path and not only in the settings UI — the model can
/// pass `size` itself and override the configured value.
///
/// `None` is deliberate for an unrecognised id (a relay's custom name): the
/// caller then sends the size through unchecked rather than rejecting a value
/// that may well be valid there.
fn sizes_for_model(model: &str) -> Option<&'static [&'static str]> {
    let m = model.to_ascii_lowercase();
    if m.contains("gpt-image") {
        Some(&["1024x1024", "1536x1024", "1024x1536", "auto"])
    } else if m.contains("dall-e-3") {
        Some(&["1024x1024", "1792x1024", "1024x1792"])
    } else if m.contains("dall-e-2") {
        Some(&["256x256", "512x512", "1024x1024"])
    } else {
        None
    }
}

/// What a `data[]` entry carries.
#[derive(Debug, PartialEq, Eq)]
enum ImagePayload {
    /// Inline base64 (possibly a `data:` URL).
    B64(String),
    /// A URL that must be downloaded.
    Url(String),
}

/// Parse both documented response shapes. Errors are actionable on purpose —
/// a relay answering with an unexpected body is the most likely first failure.
fn parse_image_payloads(v: &serde_json::Value) -> Result<Vec<ImagePayload>, String> {
    let data = v.get("data").ok_or_else(|| {
        "响应里没有 data 字段（该端点可能不是 OpenAI 兼容的图像接口）".to_string()
    })?;
    let arr = data
        .as_array()
        .ok_or_else(|| "响应 data 不是数组".to_string())?;
    if arr.is_empty() {
        return Err("响应 data 是空数组（没有返回任何图片）".to_string());
    }
    let mut out = Vec::new();
    for item in arr {
        // Some gateways return the payload as a bare base64 string in data[].
        if let Some(s) = item.as_str() {
            let s = s.trim();
            if !s.is_empty() {
                out.push(ImagePayload::B64(s.to_string()));
                continue;
            }
        }
        if let Some(b64) = item
            .get("b64_json")
            .and_then(|x| x.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            out.push(ImagePayload::B64(b64.to_string()));
        } else if let Some(url) = item
            .get("url")
            .and_then(|x| x.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            out.push(ImagePayload::Url(url.to_string()));
        }
    }
    if out.is_empty() {
        Err(format!(
            "响应 data 有 {} 项，但没有一项带 b64_json / url",
            arr.len()
        ))
    } else {
        Ok(out)
    }
}

/// `dall-e-3` rewrites the prompt server-side; surfacing it explains why the
/// picture does not match what was asked for.
fn revised_prompt(v: &serde_json::Value) -> Option<String> {
    v.get("data")?
        .as_array()?
        .iter()
        .find_map(|item| item.get("revised_prompt").and_then(|x| x.as_str()))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Human-readable message out of an error body: OpenAI's `{"error":{"message"}}`,
/// a bare `{"error":"…"}` / `{"message":"…"}`, FastAPI's `{"detail":…}`, or the
/// raw text when the body is not JSON (an HTML error page from a proxy).
fn extract_error_message(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return "（响应体为空）".to_string();
    }
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        for ptr in [
            "/error/message",
            "/error/detail",
            "/error",
            "/message",
            "/detail/0/msg",
            "/detail",
            "/msg",
        ] {
            if let Some(s) = v.pointer(ptr).and_then(|x| x.as_str()) {
                let s = s.trim();
                if !s.is_empty() {
                    return s.to_string();
                }
            }
        }
    }
    let flat = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
    flat.chars().take(300).collect()
}

/// Per-status guidance; the status code alone is rarely enough for the user to
/// act on. Empty for statuses that need no hint.
fn http_error_hint(status: u16) -> &'static str {
    match status {
        400 => "请求被拒绝 — 多半是模型不支持该 size/quality，或中转站不支持这个图像模型",
        401 | 403 => "API key 被拒绝 — 到设置页检查该渠道的 api_key",
        404 => "端点不存在 — 检查该渠道的 base_url 是否指向中转站的 /v1",
        429 => "频率或额度受限 — 稍后重试，或检查中转站余额",
        500..=599 => "服务端错误 — 稍后重试",
        _ => "",
    }
}

/// Image format from the magic bytes, so a JPEG/WebP payload is not written to
/// a `.png` path the UI would fail to decode. `None` = unrecognised.
fn sniff_ext(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some("png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("jpg")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && bytes[8..].starts_with(b"WEBP") {
        Some("webp")
    } else {
        None
    }
}

/// Decode a `b64_json` value: tolerates a `data:image/png;base64,` prefix,
/// embedded whitespace and stripped padding (some relays send all three).
fn decode_b64_payload(raw: &str) -> Result<Vec<u8>, String> {
    let s = raw.trim();
    let s = match s.strip_prefix("data:") {
        Some(rest) => rest.split_once("base64,").map(|(_, b)| b).unwrap_or(rest),
        None => s,
    };
    let cleaned: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if cleaned.is_empty() {
        return Err("b64_json 为空".to_string());
    }
    let pad = (4 - cleaned.len() % 4) % 4;
    let padded = format!("{cleaned}{}", "=".repeat(pad));
    base64::engine::general_purpose::STANDARD
        .decode(padded.as_bytes())
        .map_err(|e| format!("base64 解码失败: {e}"))
}

fn ensure_image_bytes(bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    if bytes.is_empty() {
        return Err("图片数据为空".to_string());
    }
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(format!(
            "图片 {} 字节，超过 {}MB 上限",
            bytes.len(),
            MAX_IMAGE_BYTES / (1024 * 1024)
        ));
    }
    Ok(bytes)
}

/// One `b64_json` entry → raw image bytes, with the size ceiling applied
/// *before* the decode rather than after it.
fn decode_b64_image(raw: &str) -> Result<Vec<u8>, String> {
    let chars = raw.chars().filter(|c| !c.is_whitespace()).count();
    if chars > MAX_B64_CHARS {
        return Err(format!(
            "图片超过 {}MB 上限（内联 base64 有 {} 字符，未解码）",
            MAX_IMAGE_BYTES / (1024 * 1024),
            chars
        ));
    }
    decode_b64_payload(raw).and_then(ensure_image_bytes)
}

// ── filesystem ─────────────────────────────────────────────────────────────

/// Sibling temp file + fsync + rename (same pattern as
/// `tools/file_ops.rs::write_atomic`): a crash mid-write can never leave a
/// half-written image behind, and the target is replaced in one step.
fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "image.png".to_string());
    let tmp = parent.join(format!(".{name}.dscode-{}.tmp", uuid::Uuid::new_v4()));

    let write = || -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()
    };
    if let Err(e) = write() {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// Save one image and return its path. `known_format` is false when the bytes
/// matched no known signature — the caller warns instead of pretending the file
/// is a valid PNG.
fn save_image(
    dir: &Path,
    stamp: &str,
    slug: &str,
    index: usize,
    bytes: &[u8],
) -> Result<(PathBuf, bool), String> {
    let (ext, known_format) = match sniff_ext(bytes) {
        Some(ext) => (ext, true),
        None => ("png", false),
    };
    let mut path = dir.join(format!("{stamp}-{slug}-{index}.{ext}"));
    // Two calls in the same second with the same prompt must not overwrite
    // each other's images.
    if path.exists() {
        let tag = uuid::Uuid::new_v4().simple().to_string();
        path = dir.join(format!("{stamp}-{slug}-{index}-{}.{ext}", &tag[..6]));
    }
    write_bytes_atomic(&path, bytes).map_err(|e| format!("写入 {} 失败: {e}", path.display()))?;
    Ok((path, known_format))
}

// ── network ────────────────────────────────────────────────────────────────

/// Download a `url`-shaped result. The URL comes from the API response (i.e.
/// from the relay, not from the user), so it is SSRF-checked first and the body
/// is capped while streaming rather than after buffering.
async fn download_image(
    client: &reqwest::Client,
    url: &str,
    use_proxy: bool,
) -> Result<Vec<u8>, String> {
    if let Err(reason) = crate::tools::web::validate_target(url, use_proxy).await {
        return Err(format!("下载地址被拒绝: {reason}"));
    }
    let resp = client
        .get(url)
        .timeout(DOWNLOAD_TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("下载失败: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        let (body, _) = crate::tools::web::read_body_capped(resp, MAX_ERROR_BODY_BYTES)
            .await
            .unwrap_or_default();
        let text = String::from_utf8_lossy(&body);
        return Err(format!("下载 HTTP {status}: {}", extract_error_message(&text)));
    }
    let (bytes, truncated) = crate::tools::web::read_body_capped(resp, MAX_IMAGE_BYTES)
        .await
        .map_err(|e| format!("读取图片失败: {e}"))?;
    if truncated {
        return Err(format!(
            "图片超过 {}MB 上限",
            MAX_IMAGE_BYTES / (1024 * 1024)
        ));
    }
    Ok(bytes)
}

fn emit_progress(ctx: &ToolContext, chunk: impl Into<String>) {
    let _ = ctx.sender.send(StreamEvent::ToolProgress {
        id: ctx.tool_call_id.clone(),
        chunk: chunk.into(),
    });
}

// ── do_image_generate ──────────────────────────────────────────────────────

pub struct DoImageGenerate {
    /// Total timeout for the generation request. Held on the tool (rather than
    /// as a constant) so a caller can shorten it without touching the request
    /// builder.
    timeout: Duration,
}

impl DoImageGenerate {
    pub fn new() -> Self {
        Self {
            timeout: IMAGE_TIMEOUT,
        }
    }
}

impl Default for DoImageGenerate {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for DoImageGenerate {
    fn name(&self) -> &str {
        "do_image_generate"
    }

    fn description(&self) -> &str {
        "Generate image(s) from a text prompt and save them locally. Uses the \
         user's configured provider (its base_url + api_key) against the \
         OpenAI-compatible /images/generations endpoint, then saves each image \
         under ~/.dscode/images/ and returns it as markdown that renders in the \
         chat UI. Only call this when the user actually asks for an image — \
         every call costs money/quota. Do not call it to illustrate a normal \
         answer on your own."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "What to draw, in detail (subject, style, composition, lighting). \
                        Prefer the user's own wording; do not pad it with extra requests."
                },
                "size": {
                    "type": "string",
                    "description": "Image size. **Valid values are model-specific** and an \
                        unsupported one is a hard 400: gpt-image -> 1024x1024 / 1536x1024 / \
                        1024x1536 / auto; dall-e-3 -> 1024x1024 / 1792x1024 / 1024x1792; \
                        dall-e-2 -> 256x256 / 512x512 / 1024x1024. Omit it unless the user asked \
                        for a specific shape — the configured default is already valid."
                },
                "model": {
                    "type": "string",
                    "description": "Image model id (e.g. dall-e-3, gpt-image-1). Defaults to the \
                        user's configured image model."
                },
                "n": {
                    "type": "integer",
                    "description": "How many images, 1-4 (default 1). dall-e-3 only supports 1."
                },
                "quality": {
                    "type": "string",
                    "description": "dall-e-3: standard | hd. gpt-image-1: low | medium | high | auto. \
                        Omit unless the user asked for a quality."
                },
                "use_proxy": crate::tools::web::use_proxy_param_schema()
            },
            "required": ["prompt"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let Some(prompt) = args.get("prompt").and_then(|v| v.as_str()) else {
            return Err(ToolError::MissingParameter("prompt".into()));
        };
        let prompt = prompt.trim();
        if prompt.is_empty() {
            return Ok(ToolResult::err(
                "prompt 为空，无法生成图片",
                "prompt must not be empty",
            ));
        }

        let cfg = Config::load()
            .map_err(|e| ToolError::Internal(format!("读取配置失败: {e}")))?;
        if !cfg.generation.image_enabled {
            let msg = "图像生成已被配置关闭（设置 → generation.image_enabled = false）";
            return Ok(ToolResult::err(msg, msg));
        }

        // ── which channel serves images ───────────────────────────────────
        // `generation.image_provider` wins; empty means "same channel as the
        // active model". An unknown key is refused rather than silently
        // falling back to DeepSeek (see `provider_config_by_key`).
        let configured = cfg.generation.image_provider.trim();
        let key = if configured.is_empty() {
            cfg.active_provider.trim().to_ascii_lowercase()
        } else {
            configured.to_ascii_lowercase()
        };
        if !CHANNELS.contains(&key.as_str()) {
            let msg = format!(
                "图像渠道 '{key}' 无法识别（可选: {}）。\
                 请在设置里把 generation.image_provider 设为其中一个，或留空以跟随 active_provider。",
                CHANNELS.join(" / ")
            );
            return Ok(ToolResult::err(msg.clone(), msg));
        }
        let provider = cfg.provider_config_by_key(&key).unwrap_or_default();
        let api_key = provider.api_key.trim();
        if api_key.is_empty() {
            let msg = format!(
                "渠道 '{key}' 没有配置 api_key，无法调用图像接口。\
                 请到设置页填入该渠道的 API Key（生成图片用的就是它的 base_url 与 key）。"
            );
            return Ok(ToolResult::err(msg.clone(), msg));
        }

        // ── effective parameters ──────────────────────────────────────────
        let model = args
            .get("model")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| cfg.generation.image_model.trim())
            .to_string();
        let model = if model.is_empty() {
            "gpt-image-1".to_string()
        } else {
            model
        };
        let size = args
            .get("size")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| cfg.generation.image_size.trim())
            .to_string();
        let size = if size.is_empty() {
            "1024x1024".to_string()
        } else {
            size
        };
        if size.chars().any(|c| c.is_whitespace() || c.is_control()) || size.chars().count() > 32 {
            let msg = format!("size '{size}' 不是合法尺寸，例如 1024x1024");
            return Ok(ToolResult::err(msg.clone(), msg));
        }
        // Each model family accepts a *different* set of sizes and an unsupported
        // one is a hard 400. The settings UI validates the configured value, but
        // the model can pass `size` itself and override it — so the check has to
        // live here too, where it actually guards the request.
        if let Some(valid) = sizes_for_model(&model) {
            if !valid.iter().any(|s| s.eq_ignore_ascii_case(&size)) {
                let msg = format!(
                    "尺寸 '{size}' 不被模型 '{model}' 接受。该模型支持：{}。\
                     请改用其中之一，或省略 size 参数以使用配置里的默认尺寸。",
                    valid.join(" / ")
                );
                return Ok(ToolResult::err(msg.clone(), msg));
            }
        }
        let n = clamp_n(args.get("n").and_then(|v| v.as_u64()).unwrap_or(1), &model);
        let quality = args
            .get("quality")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        let base_url = {
            let b = provider.base_url.trim();
            if b.is_empty() {
                DEFAULT_IMAGE_BASE
            } else {
                b
            }
        };
        let url = images_endpoint(base_url);

        // ── client (proxy) ────────────────────────────────────────────────
        // The configured endpoint is user-owned: it is *not* SSRF-filtered
        // (self-hosted/LAN gateways are legitimate). Only response-supplied
        // download URLs are checked.
        let explicit_use_proxy = args.get("use_proxy").and_then(|v| v.as_bool());
        let mut net_args = args.clone();
        if explicit_use_proxy.is_none() && cfg.proxy_for_provider(&key).is_some() {
            // The channel itself is marked "use proxy" even though the web
            // toggle is off — chat reaches this relay through the proxy, so the
            // image call must too.
            if let Some(obj) = net_args.as_object_mut() {
                obj.insert("use_proxy".into(), serde_json::json!(true));
            }
        }
        let (client, proxy) = crate::tools::web::web_client_for_args(&net_args)?;

        let mut body = serde_json::json!({
            "model": model,
            "prompt": prompt,
            "n": n,
            "size": size,
        });
        if wants_response_format(&model) {
            body["response_format"] = serde_json::json!("b64_json");
        }
        if let Some(q) = &quality {
            body["quality"] = serde_json::json!(q);
        }

        emit_progress(
            ctx,
            format!("⟳ 正在生成图片 · {model} · {size} · n={n}\n"),
        );

        // ── request ───────────────────────────────────────────────────────
        let resp = match client
            .post(url.as_str())
            .header("Authorization", format!("Bearer {api_key}"))
            .header("Content-Type", "application/json")
            .timeout(self.timeout)
            .json(&body)
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(e) => {
                let hint = if proxy.is_none() && crate::tools::web::proxy_configured_url().is_some()
                {
                    "\n提示: 已配置代理但本次未走代理，可用 use_proxy=true 重试。"
                } else {
                    ""
                };
                let msg = format!("请求图像接口失败: {e}\n端点: {url}{hint}");
                return Ok(ToolResult::err(msg.clone(), msg));
            }
        };

        let status = resp.status();
        if !status.is_success() {
            let (bytes, _) = crate::tools::web::read_body_capped(resp, MAX_ERROR_BODY_BYTES)
                .await
                .unwrap_or_default();
            let text = String::from_utf8_lossy(&bytes);
            let detail = extract_error_message(&text);
            let hint = http_error_hint(status.as_u16());
            let mut msg = format!("图像接口返回 HTTP {status}: {detail}\n端点: {url}\n模型: {model}");
            if !hint.is_empty() {
                msg.push_str(&format!("\n{hint}"));
            }
            return Ok(ToolResult::err(msg.clone(), msg));
        }

        let (bytes, truncated) = crate::tools::web::read_body_capped(resp, MAX_JSON_BYTES)
            .await
            .map_err(|e| ToolError::Internal(format!("读取图像响应失败: {e}")))?;
        if truncated {
            let msg = format!(
                "图像响应超过 {}MB，已中止（中转站可能返回了非预期的内容）",
                MAX_JSON_BYTES / (1024 * 1024)
            );
            return Ok(ToolResult::err(msg.clone(), msg));
        }
        let text = String::from_utf8_lossy(&bytes);
        let json: serde_json::Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                let msg = format!(
                    "图像接口响应不是 JSON: {e}\n原始内容: {}",
                    extract_error_message(&text)
                );
                return Ok(ToolResult::err(msg.clone(), msg));
            }
        };
        let payloads = match parse_image_payloads(&json) {
            Ok(p) => p,
            Err(e) => {
                let msg = format!("{e}\n端点: {url}\n模型: {model}");
                return Ok(ToolResult::err(msg.clone(), msg));
            }
        };
        let revised = revised_prompt(&json);

        // ── save ──────────────────────────────────────────────────────────
        let dir = Config::data_dir()
            .map_err(|e| ToolError::Internal(format!("定位数据目录失败: {e}")))?
            .join("images");
        std::fs::create_dir_all(&dir)
            .map_err(|e| ToolError::Internal(format!("创建 {} 失败: {e}", dir.display())))?;

        let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
        let slug = slugify(prompt);
        let alt = sanitize_alt(prompt);

        let mut saved: Vec<(PathBuf, bool)> = Vec::new();
        let mut failures: Vec<String> = Vec::new();

        for (i, payload) in payloads.iter().enumerate() {
            let label = i + 1;
            let bytes = match payload {
                ImagePayload::B64(raw) => decode_b64_image(raw),
                ImagePayload::Url(u) => match download_image(&client, u, proxy.is_some()).await {
                    Ok(b) => ensure_image_bytes(b),
                    Err(e) => Err(e),
                },
            };
            let bytes = match bytes {
                Ok(b) => b,
                Err(e) => {
                    failures.push(format!("第 {label} 张: {e}"));
                    emit_progress(ctx, format!("  ✗ 第 {label} 张失败: {e}\n"));
                    continue;
                }
            };
            match save_image(&dir, &stamp, &slug, saved.len() + 1, &bytes) {
                Ok((path, known_format)) => {
                    emit_progress(
                        ctx,
                        format!("  ✓ 第 {label} 张已保存: {}\n", path.display()),
                    );
                    saved.push((path, known_format));
                }
                Err(e) => {
                    failures.push(format!("第 {label} 张: {e}"));
                    emit_progress(ctx, format!("  ✗ 第 {label} 张保存失败: {e}\n"));
                }
            }
        }

        if saved.is_empty() {
            let msg = format!(
                "图像生成失败：没有拿到任何可用图片。\n端点: {url}\n模型: {model}\n{}",
                failures.join("\n")
            );
            return Ok(ToolResult::err(msg.clone(), msg));
        }

        // ── report ────────────────────────────────────────────────────────
        let mut out = format!(
            "已生成 {} 张图片（模型 {model}，尺寸 {size}）\n保存目录：{}\n",
            saved.len(),
            dir.display()
        );
        if proxy.is_some() {
            out.push_str(&format!(
                "网络：{}\n",
                crate::tools::web::proxy_note(&proxy, explicit_use_proxy)
            ));
        }
        if let Some(r) = &revised {
            if r != prompt {
                out.push_str(&format!("模型改写后的提示词：{r}\n"));
            }
        }
        out.push('\n');
        for (path, known_format) in saved.iter() {
            // Contract with the desktop UI: one line per image, raw absolute
            // filesystem path after `dscode-image:` (no URL encoding).
            out.push_str(&format!("![{alt}](dscode-image:{})\n\n", path.display()));
            if !*known_format {
                out.push_str(&format!(
                    "注意：{} 的格式无法识别（既不是 PNG，也不是 JPEG/WebP），已按 .png 保存，可能无法显示。\n\n",
                    path.display()
                ));
            }
        }
        out.push_str("图片会直接显示在对话里，文件也保存在上面的目录中，可直接打开。\n");
        if !failures.is_empty() {
            out.push_str(&format!("\n部分失败：{}\n", failures.join("；")));
        }
        Ok(ToolResult::ok(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── base URL normalisation ────────────────────────────────────────────

    #[test]
    fn endpoint_normalises_base_url() {
        for (input, expected) in [
            ("https://x/v1", "https://x/v1/images/generations"),
            ("https://x", "https://x/v1/images/generations"),
            ("https://x/v1/", "https://x/v1/images/generations"),
            ("https://x/openai/v1", "https://x/openai/v1/images/generations"),
        ] {
            assert_eq!(images_endpoint(input), expected, "input: {input}");
        }
    }

    #[test]
    fn endpoint_keeps_host_and_port() {
        // The real config shape: a relay on a non-default port.
        assert_eq!(
            images_endpoint("  https://api.relay.example:60000/v1  "),
            "https://api.relay.example:60000/v1/images/generations"
        );
        // No /v1 segment → one is appended, exactly like the chat provider.
        assert_eq!(
            images_endpoint("https://relay.example/api"),
            "https://relay.example/api/v1/images/generations"
        );
        // Empty base (hand-edited config) falls back to api.openai.com.
        assert_eq!(
            images_endpoint("   "),
            "https://api.openai.com/v1/images/generations"
        );
    }

    // ── slug / file name safety ───────────────────────────────────────────

    #[test]
    fn slug_keeps_cjk_and_ascii() {
        assert_eq!(slugify("a cute cat"), "a-cute-cat");
        assert_eq!(slugify("一只在窗台上的橘猫"), "一只在窗台上的橘猫");
    }

    #[test]
    fn slug_strips_path_separators_and_specials() {
        let s = slugify("../../etc/passwd");
        assert!(!s.contains('/') && !s.contains('\\'), "{s}");
        assert!(!s.contains(".."), "{s}");
        let s = slugify(r"C:\Windows\System32\evil:name*?");
        assert!(!s.contains(':') && !s.contains('*') && !s.contains('?'), "{s}");
        assert!(!s.contains('\\'), "{s}");
    }

    #[test]
    fn slug_truncates_and_never_empty() {
        let long = slugify("猫".repeat(200).as_str());
        assert!(long.chars().count() <= 40, "{} chars", long.chars().count());
        assert_eq!(slugify(""), "image");
        assert_eq!(slugify("   "), "image");
        assert_eq!(slugify("🖼🖼🖼"), "image"); // emoji are not alphanumeric
        // No leading/trailing dash, no doubled dash.
        assert_eq!(slugify("  hello --- world  "), "hello-world");
    }

    #[test]
    fn alt_text_cannot_break_the_markdown_line() {
        let alt = sanitize_alt("a [cat] (dog)\nnew line");
        assert!(!alt.contains('[') && !alt.contains(']') && !alt.contains('('));
        assert!(!alt.contains('\n'));
        assert_eq!(sanitize_alt(""), "生成的图片");
        assert_eq!(sanitize_alt("   "), "生成的图片");
    }

    // ── per-model size sets ───────────────────────────────────────────────

    #[test]
    fn image_model_detection_covers_the_common_families() {
        // The family the user actually configures.
        assert!(is_image_model("gpt-image-2.5-flare"));
        assert!(is_image_model("gpt-image-1"));
        assert!(is_image_model("  GPT-Image-1  "));
        assert!(is_image_model("openai/gpt-image-1")); // provider prefix
        assert!(is_image_model("dall-e-3"));
        assert!(is_image_model("stable-diffusion-xl"));
        assert!(is_image_model("flux-pro"));
    }

    #[test]
    fn chat_models_are_not_mistaken_for_image_models() {
        // A false positive here would send a chat turn to the image endpoint.
        for m in [
            "deepseek-v4-pro",
            "deepseek-v4-flash",
            "gpt-4o",
            "claude-sonnet-4",
            "o3-mini",
            "llama3.2",
            "",
            "   ",
        ] {
            assert!(!is_image_model(m), "{m:?} must not route to image generation");
        }
    }

    #[test]
    fn size_sets_are_model_specific_and_do_not_overlap_by_accident() {
        let gpt = sizes_for_model("gpt-image-1").unwrap();
        let d3 = sizes_for_model("dall-e-3").unwrap();
        let d2 = sizes_for_model("dall-e-2").unwrap();

        assert!(gpt.contains(&"auto")); // gpt-image only
        assert!(!d3.contains(&"auto"));
        assert!(d3.contains(&"1792x1024")); // dall-e-3 only
        assert!(!gpt.contains(&"1792x1024"));
        assert!(d2.contains(&"512x512")); // dall-e-2 only
        assert!(!gpt.contains(&"512x512"));
        assert!(!d3.contains(&"512x512"));
        // Every set offers the safe default, and none is empty.
        for set in [gpt, d3, d2] {
            assert!(!set.is_empty());
            assert!(set.contains(&"1024x1024"));
        }
    }

    #[test]
    fn size_lookup_normalises_and_falls_through_for_unknown_ids() {
        assert_eq!(sizes_for_model("  GPT-Image-1  "), sizes_for_model("gpt-image-1"));
        assert_eq!(sizes_for_model("openai/gpt-image-1-mini"), sizes_for_model("gpt-image-1"));
        assert_eq!(sizes_for_model("dall-e-3-hd"), sizes_for_model("dall-e-3"));
        // An unrecognised relay id must NOT be rejected — we cannot know its
        // valid set, so the caller sends the size through unchecked.
        assert!(sizes_for_model("flux-pro").is_none());
        assert!(sizes_for_model("").is_none());
        assert!(sizes_for_model("stable-diffusion-xl").is_none());
    }

    // ── n clamping ────────────────────────────────────────────────────────

    #[test]
    fn n_is_clamped_and_dall_e_3_forced_to_one() {
        assert_eq!(clamp_n(1, "dall-e-3"), 1);
        assert_eq!(clamp_n(4, "dall-e-3"), 1);
        assert_eq!(clamp_n(0, "gpt-image-1"), 1);
        assert_eq!(clamp_n(1, "dall-e-3-2024"), 1); // relay alias
        assert_eq!(clamp_n(99, "gpt-image-1"), 4);
        assert_eq!(clamp_n(3, "dall-e-2"), 3);
        // Missing n (0 from unwrap_or(1) aside) can never reach the API as 0.
        assert_eq!(clamp_n(0, "stable-diffusion"), 1);
    }

    #[test]
    fn response_format_is_skipped_for_gpt_image() {
        assert!(wants_response_format("dall-e-3"));
        assert!(wants_response_format("DALL-E-2"));
        assert!(!wants_response_format("gpt-image-1"));
        assert!(!wants_response_format("openai/gpt-image-1-mini"));
    }

    // ── response parsing ──────────────────────────────────────────────────

    #[test]
    fn parse_b64_shape() {
        let v = serde_json::json!({"data": [{"b64_json": "aGk="}]});
        assert_eq!(
            parse_image_payloads(&v).unwrap(),
            vec![ImagePayload::B64("aGk=".into())]
        );
    }

    #[test]
    fn parse_url_shape_and_mixed() {
        let v = serde_json::json!({"data": [
            {"url": "https://cdn.example/a.png"},
            {"b64_json": "aGk="}
        ]});
        assert_eq!(
            parse_image_payloads(&v).unwrap(),
            vec![
                ImagePayload::Url("https://cdn.example/a.png".into()),
                ImagePayload::B64("aGk=".into()),
            ]
        );
    }

    #[test]
    fn parse_rejects_missing_and_empty_data() {
        let e = parse_image_payloads(&serde_json::json!({"created": 1})).unwrap_err();
        assert!(e.contains("data"), "{e}");
        let e = parse_image_payloads(&serde_json::json!({"data": []})).unwrap_err();
        assert!(e.contains("空数组"), "{e}");
        let e = parse_image_payloads(&serde_json::json!({"data": [{"revised_prompt": "x"}]}))
            .unwrap_err();
        assert!(e.contains("b64_json"), "{e}");
        // A non-array `data` must not panic.
        assert!(parse_image_payloads(&serde_json::json!({"data": "oops"})).is_err());
    }

    #[test]
    fn error_body_message_is_extracted() {
        assert_eq!(
            extract_error_message(r#"{"error":{"message":"Incorrect API key provided"}}"#),
            "Incorrect API key provided"
        );
        assert_eq!(
            extract_error_message(r#"{"error":"model not found"}"#),
            "model not found"
        );
        assert_eq!(
            extract_error_message(r#"{"message":"no such model"}"#),
            "no such model"
        );
        assert_eq!(
            extract_error_message(r#"{"detail":[{"msg":"field required"}]}"#),
            "field required"
        );
        assert_eq!(extract_error_message(""), "（响应体为空）");
        // Non-JSON (proxy HTML page) degrades to flattened text, not a panic.
        let html = "<html>\n  <body>502 Bad Gateway</body>\n</html>";
        assert_eq!(extract_error_message(html), "<html> <body>502 Bad Gateway</body> </html>");
    }

    #[test]
    fn revised_prompt_is_read_when_present() {
        let v = serde_json::json!({"data": [{"b64_json": "aGk=", "revised_prompt": "a red cat"}]});
        assert_eq!(revised_prompt(&v).as_deref(), Some("a red cat"));
        assert_eq!(revised_prompt(&serde_json::json!({"data": [{"b64_json": "aGk="}]})), None);
        assert_eq!(revised_prompt(&serde_json::json!({})), None);
    }

    #[test]
    fn http_hints_cover_the_common_failures() {
        assert!(http_error_hint(401).contains("api_key"));
        assert!(http_error_hint(404).contains("base_url"));
        assert!(http_error_hint(429).contains("额度"));
        assert_eq!(http_error_hint(200), "");
    }

    // ── bytes handling ────────────────────────────────────────────────────

    #[test]
    fn b64_decoding_tolerates_relay_quirks() {
        assert_eq!(decode_b64_payload("aGk=").unwrap(), b"hi".to_vec());
        assert_eq!(decode_b64_payload("  aGk=  ").unwrap(), b"hi".to_vec());
        assert_eq!(decode_b64_payload("aGk").unwrap(), b"hi".to_vec()); // padding stripped
        assert_eq!(
            decode_b64_payload("data:image/png;base64,aGk=").unwrap(),
            b"hi".to_vec()
        );
        assert_eq!(decode_b64_payload("aG\nk=").unwrap(), b"hi".to_vec()); // wrapped lines
        assert!(decode_b64_payload("").is_err());
        assert!(decode_b64_payload("not base64 !!").is_err());
    }

    #[test]
    fn format_is_sniffed_from_magic_bytes() {
        assert_eq!(sniff_ext(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]), Some("png"));
        assert_eq!(sniff_ext(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("jpg"));
        assert_eq!(
            sniff_ext(b"RIFF\x24\x00\x00\x00WEBPVP8 "),
            Some("webp")
        );
        assert_eq!(sniff_ext(b"RIFF\x24\x00\x00\x00WAVEfmt "), None);
        assert_eq!(sniff_ext(b"<html>nope"), None);
        let empty: &[u8] = &[];
        assert_eq!(sniff_ext(empty), None);
    }

    #[test]
    fn oversized_payload_is_rejected_before_writing() {
        assert!(ensure_image_bytes(Vec::new()).is_err());
        assert!(ensure_image_bytes(vec![1, 2, 3]).is_ok());
        let too_big = vec![0u8; MAX_IMAGE_BYTES + 1];
        assert!(ensure_image_bytes(too_big).is_err());
    }

    #[test]
    fn oversized_b64_is_refused_before_decoding() {
        // The pre-check must never reject a legal ≤32MB image (4 chars / 3 bytes).
        assert!(MAX_B64_CHARS as u64 >= (MAX_IMAGE_BYTES as u64) * 4 / 3 + 4);
        // One char past the cap errors out without allocating a decoded image.
        let huge = "A".repeat(MAX_B64_CHARS + 1);
        let err = decode_b64_image(&huge).unwrap_err();
        assert!(err.contains("上限"), "{err}");
        // A normal payload still goes through the decoder.
        assert_eq!(decode_b64_image("aGk=").unwrap(), b"hi".to_vec());
    }
}
