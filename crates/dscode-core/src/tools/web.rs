//! Built-in web tools:
//! - **do_web_fetch** — GET a public URL, strip HTML, return text
//! - **do_web_search** — Bing web search (title/URL/snippet)
//! - **do_deep_search** — concurrent multi-query research with full-text fetch
//!
//! When a proxy URL is configured, the agent may set `use_proxy` per call
//! (Settings `web_use_proxy` / `global` are the default when omitted).

use async_trait::async_trait;
use base64::Engine;
use futures::stream::{self, StreamExt};
use regex::Regex;
use reqwest::Client;
use std::net::{IpAddr, ToSocketAddrs};
use std::time::Duration;

use crate::agent::stream::StreamEvent;
use crate::config::settings::Config;
use crate::tools::trait_def::{Tool, ToolContext, ToolError, ToolResult};

pub(crate) const MAX_FETCH_BYTES: usize = 2 * 1024 * 1024;
/// Cap for the error/preview body kept from a non-2xx response.
const MAX_ERROR_BODY_BYTES: usize = 8 * 1024;
const MAX_TEXT_CHARS: usize = 24_000;
const MAX_SEARCH_RESULTS: usize = 10;
/// A single Bing organic result block is a few KB; anything far beyond that is
/// an unterminated block (markup change / captured page tail), not a result.
const MAX_BING_BLOCK_BYTES: usize = 24_000;

// ── proxy helpers ──────────────────────────────────────────────────────────

pub(crate) fn proxy_configured_url() -> Option<String> {
    Config::load().ok().and_then(|c| {
        if c.proxy.is_configured() {
            Some(c.proxy.url.trim().to_string())
        } else {
            None
        }
    })
}

fn settings_prefer_web_proxy() -> bool {
    Config::load()
        .map(|c| c.proxy.is_configured() && (c.proxy.global || c.proxy.web_use_proxy))
        .unwrap_or(false)
}

fn resolve_use_proxy(args: &serde_json::Value) -> (bool, Option<String>) {
    let url = proxy_configured_url();
    let Some(url) = url else {
        return (false, None);
    };
    let want = match args.get("use_proxy").and_then(|v| v.as_bool()) {
        Some(b) => b,
        None => settings_prefer_web_proxy(),
    };
    if want {
        (true, Some(url))
    } else {
        (false, None)
    }
}

pub(crate) fn web_client_for_args(args: &serde_json::Value) -> Result<(Client, Option<String>), ToolError> {
    let (_want, proxy) = resolve_use_proxy(args);
    let mut builder = Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(30))
        .user_agent(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
        )
        .redirect(redirect_policy(proxy.is_some()));
    if let Some(ref url) = proxy {
        let p = reqwest::Proxy::all(url.as_str())
            .map_err(|e| ToolError::Internal(format!("无效代理 URL: {e}")))?;
        builder = builder.proxy(p);
    } else {
        builder = builder.no_proxy();
    }
    let client = builder
        .build()
        .map_err(|e| ToolError::Internal(format!("HTTP client: {e}")))?;
    Ok((client, proxy))
}

pub(crate) fn proxy_note(proxy: &Option<String>, explicit: Option<bool>) -> String {
    match proxy {
        Some(u) => {
            let display = u.split('@').last().unwrap_or(u);
            match explicit {
                Some(true) => format!("proxy={display} · agent chose use_proxy=true"),
                Some(false) => format!("proxy={display}"),
                None => format!("proxy={display} · settings default"),
            }
        }
        None => match explicit {
            Some(true) => {
                "proxy=off · agent wanted use_proxy=true but no proxy URL configured".into()
            }
            Some(false) => "proxy=off · agent chose direct".into(),
            None => "proxy=off (direct)".into(),
        },
    }
}

pub(crate) fn use_proxy_param_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "boolean",
        "description": "If true, use the user-configured proxy (Settings). If false, direct. \
            Omit for Settings default. Only works when a proxy URL is set. \
            Prefer true when direct search/fetch fails (e.g. network restrictions)."
    })
}

// ── URL policy (shared by every network tool) ──────────────────────────────
//
// The policy lives here, next to the client builder, so every tool that dials
// through `web_client_for_args` inherits it — feeds and github included.
// Two layers:
//   1. `check_url_shallow` — URL parse + literal-IP / local-name checks, no DNS.
//      Used for every redirect hop and before any lookup.
//   2. `validate_target` — shallow check plus DNS resolution, so a hostname
//      that actually points into loopback/private/link-local space is refused
//      (`localtest.me`, `10-0-0-5.nip.io`, decimal/octal IP forms, …).

/// Hostnames that denote a local-only endpoint without an IP literal. DNS
/// resolution catches the rest; these are refused before/without resolving.
const LOCAL_HOST_SUFFIXES: [&str; 4] = [".localhost", ".local", ".internal", ".home.arpa"];

fn is_blocked_host_name(host: &str) -> Option<String> {
    let h = host.trim_matches('.').to_ascii_lowercase();
    if h == "localhost" || h.ends_with(".localhost") {
        return Some("localhost".into());
    }
    for suffix in LOCAL_HOST_SUFFIXES {
        if h.ends_with(suffix) {
            return Some(format!("internal-only hostname ({suffix})"));
        }
    }
    None
}

/// Loopback / private / link-local / ULA / reserved address ranges.
fn is_blocked_ip(ip: IpAddr) -> Option<&'static str> {
    match ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            let (a, b) = (octets[0], octets[1]);
            if v4.is_loopback() {
                Some("loopback")
            } else if v4.is_private() {
                Some("private")
            } else if v4.is_link_local() {
                Some("link-local")
            } else if v4.is_unspecified() {
                Some("unspecified")
            } else if v4.is_broadcast() {
                Some("broadcast")
            } else if a == 0 {
                Some("this-network")
            } else if a == 100 && (64..=127).contains(&b) {
                Some("carrier-grade NAT")
            } else if a >= 224 {
                Some("multicast/reserved")
            } else {
                None
            }
        }
        IpAddr::V6(v6) => {
            if v6.is_loopback() {
                Some("loopback")
            } else if v6.is_unspecified() {
                Some("unspecified")
            } else if let Some(v4) = v6.to_ipv4_mapped() {
                is_blocked_ip(IpAddr::V4(v4))
            } else if v6.segments()[0] & 0xffc0 == 0xfe80 {
                Some("link-local")
            } else if v6.segments()[0] & 0xfe00 == 0xfc00 {
                Some("unique-local")
            } else if v6.segments()[0] & 0xff00 == 0xff00 {
                Some("multicast")
            } else {
                None
            }
        }
    }
}

/// Parse + structural checks only. `Url` normalises decimal/octal/hex IPv4
/// forms (`http://2130706433/`, `http://0x7f000001/`, `http://127.000.000.001/`)
/// and bracketed IPv6, so the literal check sees the real address. Path and
/// query are never inspected, so `…/wiki/169.254.169.254` is not a false hit.
fn check_url_shallow(url: &str) -> Result<reqwest::Url, String> {
    let parsed = reqwest::Url::parse(url).map_err(|e| format!("invalid URL: {e}"))?;
    match parsed.scheme() {
        "http" | "https" => {}
        other => return Err(format!("scheme '{other}' is not http(s)")),
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| "URL has no host".to_string())?;
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = bare.parse::<IpAddr>() {
        if let Some(ban) = is_blocked_ip(ip) {
            return Err(format!("{ban} address {ip}"));
        }
    } else if let Some(ban) = is_blocked_host_name(bare) {
        return Err(ban);
    }
    Ok(parsed)
}

/// Full target validation for tool entry points. `use_proxy` skips the DNS
/// step: a configured proxy dials (and resolves) the host itself, so a local
/// lookup would reject hosts it can legitimately reach. Literal-IP and
/// local-name bans still apply in that case.
pub(crate) async fn validate_target(url: &str, use_proxy: bool) -> Result<(), String> {
    let parsed = check_url_shallow(url)?;
    if use_proxy {
        return Ok(());
    }
    let host = parsed.host_str().unwrap_or_default();
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if bare.parse::<IpAddr>().is_ok() {
        return Ok(()); // literal already vetted above
    }
    let port = parsed.port_or_known_default().unwrap_or(443);
    let addrs = tokio::net::lookup_host((bare, port))
        .await
        .map_err(|e| format!("cannot resolve host '{bare}': {e}"))?;
    let mut seen = 0usize;
    for sa in addrs {
        seen += 1;
        if let Some(ban) = is_blocked_ip(sa.ip()) {
            return Err(format!(
                "host '{bare}' resolves to a {ban} address ({})",
                sa.ip()
            ));
        }
    }
    if seen == 0 {
        return Err(format!("host '{bare}' did not resolve to any address"));
    }
    Ok(())
}

/// Redirect policy that re-validates *every* hop — the initial-URL check alone
/// is defeated by any open redirector (shorteners, `google/url?q=`). DNS is
/// resolved per hop only when no proxy is configured; with a proxy the hop is
/// resolved by the proxy, not on this machine.
fn redirect_policy(use_proxy: bool) -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() >= 5 {
            return attempt.error("too many redirects");
        }
        let url = attempt.url().clone();
        if let Err(reason) = check_url_shallow(url.as_str()) {
            return attempt.error(format!("redirect to blocked URL ({reason})"));
        }
        if !use_proxy {
            if let Some(host) = url.host_str() {
                let bare = host
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .to_string();
                if bare.parse::<IpAddr>().is_err() {
                    let port = url.port_or_known_default().unwrap_or(443);
                    match (bare.as_str(), port).to_socket_addrs() {
                        Ok(addrs) => {
                            for sa in addrs {
                                if let Some(ban) = is_blocked_ip(sa.ip()) {
                                    return attempt.error(format!(
                                        "redirect to {ban} address ({})",
                                        sa.ip()
                                    ));
                                }
                            }
                        }
                        Err(e) => {
                            return attempt.error(format!(
                                "redirect host '{bare}' did not resolve: {e}"
                            ))
                        }
                    }
                }
            }
        }
        attempt.follow()
    })
}

// ── body reading (bounded) ─────────────────────────────────────────────────

/// Read at most `max` bytes of a response, streaming instead of buffering the
/// whole body and checking afterwards. Returns `(bytes, truncated)`; a declared
/// `Content-Length` over the cap marks the result truncated up front and the
/// buffer is never pre-allocated beyond `max`.
pub(crate) async fn read_body_capped(
    mut resp: reqwest::Response,
    max: usize,
) -> Result<(Vec<u8>, bool), String> {
    let declared = resp.content_length();
    let mut truncated = declared.map_or(false, |n| n > max as u64);
    let capacity = declared.map_or(8192, |n| n.min(max as u64) as usize);
    let mut buf: Vec<u8> = Vec::with_capacity(capacity);
    loop {
        if buf.len() >= max {
            truncated = true;
            break;
        }
        let chunk = match resp.chunk().await {
            Ok(Some(c)) => c,
            Ok(None) => break,
            Err(e) => return Err(e.to_string()),
        };
        let room = max - buf.len();
        if chunk.len() > room {
            buf.extend_from_slice(&chunk[..room]);
            truncated = true;
            break;
        }
        buf.extend_from_slice(&chunk);
    }
    Ok((buf, truncated))
}

/// `charset=` from a Content-Type header, e.g. `text/html; charset=gbk`.
fn charset_from_content_type(ctype: &str) -> Option<String> {
    let lower = ctype.to_ascii_lowercase();
    let idx = lower.find("charset=")?;
    let rest = &ctype[idx + "charset=".len()..];
    let end = rest
        .find(|c: char| c == ';' || c == '"' || c == '\'' || c.is_whitespace())
        .unwrap_or(rest.len());
    let cs = rest[..end].trim().trim_matches('"').trim_matches('\'');
    (!cs.is_empty()).then(|| cs.to_string())
}

/// Sniff a charset from the document head: `<meta charset=…>`,
/// `content="…; charset=…"` or an XML declaration's `encoding="…"`.
fn charset_from_body(bytes: &[u8]) -> Option<String> {
    let head_end = bytes.len().min(4096);
    let head = String::from_utf8_lossy(&bytes[..head_end]).to_ascii_lowercase();
    let (needle, skip) = if let Some(i) = head.find("charset=") {
        (i, "charset=".len())
    } else if head.trim_start().starts_with("<?xml") {
        match head.find("encoding=") {
            Some(i) => (i, "encoding=".len()),
            None => return None,
        }
    } else {
        return None;
    };
    let rest = &head[needle + skip..];
    let end = rest
        .find(|c: char| c == '"' || c == '\'' || c == ';' || c == '>' || c.is_whitespace())
        .unwrap_or(rest.len());
    let cs = rest[..end].trim().trim_matches('"').trim_matches('\'');
    (!cs.is_empty()).then(|| cs.to_string())
}

/// Decode a body using the declared charset, then a `<meta>`/XML sniff, then
/// UTF-8 lossily. Without this, GBK/Big5/Shift_JIS pages come back as mojibake
/// even though the charset was reported right above the text.
pub(crate) fn decode_body(bytes: &[u8], ctype: &str) -> String {
    let label = charset_from_content_type(ctype).or_else(|| charset_from_body(bytes));
    if let Some(label) = label {
        if let Some(enc) = encoding_rs::Encoding::for_label(label.as_bytes()) {
            return enc.decode(bytes).0.into_owned();
        }
    }
    String::from_utf8_lossy(bytes).into_owned()
}

// ── html helpers ───────────────────────────────────────────────────────────

fn html_to_text(html: &str) -> String {
    let re_script = Regex::new(r"(?is)<script[^>]*>.*?</script>").unwrap();
    let re_style = Regex::new(r"(?is)<style[^>]*>.*?</style>").unwrap();
    let re_tags = Regex::new(r"(?is)<[^>]+>").unwrap();

    let mut s = re_script.replace_all(html, " ").into_owned();
    s = re_style.replace_all(&s, " ").into_owned();
    s = s
        .replace("<br>", "\n")
        .replace("<br/>", "\n")
        .replace("<br />", "\n")
        .replace("</p>", "\n\n")
        .replace("</div>", "\n")
        .replace("</li>", "\n")
        .replace("</h1>", "\n\n")
        .replace("</h2>", "\n\n")
        .replace("</h3>", "\n\n")
        .replace("</tr>", "\n");
    s = re_tags.replace_all(&s, " ").into_owned();
    s = decode_entities(&s);
    s = s.replace('\u{a0}', " ");
    collapse_lines(&s)
}

/// Decode one HTML entity name/number to its character. Unknown entities
/// return `None` and are left verbatim — the old catch-all sweep turned
/// `&#8217;`/`&#8220;`/`&#8212;` into spaces, splitting words ("don t").
fn entity_to_char(ent: &str) -> Option<char> {
    if let Some(num) = ent.strip_prefix('#') {
        let cp = if let Some(hex) = num.strip_prefix('x').or_else(|| num.strip_prefix('X')) {
            u32::from_str_radix(hex, 16).ok()?
        } else {
            num.parse::<u32>().ok()?
        };
        return char::from_u32(cp);
    }
    Some(match ent {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => ' ',
        // Spacing entities. Bing separates a result's date from its snippet with
        // `&ensp;·&ensp;`, so without these the user reads the raw entity in
        // every dated result — a defect no synthetic-SERP unit test caught,
        // because none of them emitted this entity.
        "ensp" => ' ',
        "emsp" => ' ',
        "thinsp" => ' ',
        "zwnj" => '\u{200c}',
        "zwj" => '\u{200d}',
        "shy" => '\u{00ad}',
        "mdash" => '—',
        "ndash" => '–',
        "hellip" => '…',
        "lsquo" => '\u{2018}',
        "rsquo" => '\u{2019}',
        "ldquo" => '\u{201c}',
        "rdquo" => '\u{201d}',
        "sbquo" => '\u{201a}',
        "bdquo" => '\u{201e}',
        "laquo" => '«',
        "raquo" => '»',
        "bull" => '•',
        "middot" => '·',
        "dagger" => '†',
        "Dagger" => '‡',
        "permil" => '‰',
        "prime" => '′',
        "Prime" => '″',
        "minus" => '−',
        "lowast" => '∗',
        "frasl" => '⁄',
        "oline" => '‾',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "deg" => '°',
        "plusmn" => '±',
        "times" => '×',
        "divide" => '÷',
        "euro" => '€',
        "pound" => '£',
        "yen" => '¥',
        "cent" => '¢',
        "curren" => '¤',
        "brvbar" => '¦',
        "sect" => '§',
        "para" => '¶',
        "uml" => '¨',
        "acute" => '´',
        "cedil" => '¸',
        "macr" => '¯',
        "not" => '¬',
        "iexcl" => '¡',
        "iquest" => '¿',
        "ordf" => 'ª',
        "ordm" => 'º',
        "sup1" => '¹',
        "sup2" => '²',
        "sup3" => '³',
        "frac12" => '½',
        "frac14" => '¼',
        "frac34" => '¾',
        "micro" => 'µ',
        _ => return None,
    })
}

/// Single left-to-right entity decode: named and numeric, unknown left intact.
fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        // An entity name is short; an unmatched '&' is literal text.
        // (char_indices — a byte slice here could split a multi-byte char.)
        let semi = rest
            .char_indices()
            .take(16)
            .find(|(_, c)| *c == ';')
            .map(|(p, _)| p);
        let semi = match semi {
            Some(p) => p,
            None => {
                out.push('&');
                rest = &rest[1..];
                continue;
            }
        };
        match entity_to_char(&rest[1..semi]) {
            Some(ch) => {
                out.push(ch);
                rest = &rest[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Collapse runs of spaces/tabs *within* each line and 3+ newlines to a blank
/// line. The previous `split_whitespace().join(" ")` also ate every newline,
/// so a whole page came back as one multi-thousand-character line.
fn collapse_lines(s: &str) -> String {
    let re_sp = Regex::new(r"[ \t]{2,}").unwrap();
    let re_blank = Regex::new(r"\n{3,}").unwrap();
    let joined = s
        .split('\n')
        .map(|line| re_sp.replace_all(line.trim(), " ").into_owned())
        .collect::<Vec<_>>()
        .join("\n");
    re_blank.replace_all(&joined, "\n\n").trim().to_string()
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max).collect();
    format!("{head}\n\n…[truncated, {max} chars]")
}

// ── do_web_fetch ───────────────────────────────────────────────────────────

pub struct DoWebFetch;

impl DoWebFetch {
    pub fn new() -> Self {
        Self
    }
}

impl Default for DoWebFetch {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for DoWebFetch {
    fn name(&self) -> &str {
        "do_web_fetch"
    }

    fn description(&self) -> &str {
        "Fetch public HTTP(S) page(s) as readable text (HTML stripped). \
         Accepts one `url` or up to 4 concurrent `urls`. HTML responses include a \
         \"Candidate links\" list (same-site / docs preferred) so you can deep-fetch \
         related pages for the user task. Not for authenticated pages. Optional use_proxy."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "Single URL (http/https). Use `urls` for concurrent multi-fetch."
                },
                "urls": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Up to 4 URLs to fetch concurrently (preferred when you already have several good links)."
                },
                "max_chars": {
                    "type": "integer",
                    "description": "Max body chars per page (default 24000, max 48000). Shared budget if multiple URLs."
                },
                "max_links": {
                    "type": "integer",
                    "description": "Max candidate links to list from each HTML page (default 20, max 40)."
                },
                "use_proxy": use_proxy_param_schema()
            }
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let mut targets: Vec<String> = Vec::new();
        if let Some(arr) = args.get("urls").and_then(|v| v.as_array()) {
            for v in arr {
                if let Some(s) = v.as_str() {
                    let t = s.trim();
                    if !t.is_empty() {
                        targets.push(t.to_string());
                    }
                }
            }
        }
        if let Some(u) = args.get("url").and_then(|v| v.as_str()) {
            let t = u.trim();
            if !t.is_empty() && !targets.iter().any(|x| x == t) {
                targets.insert(0, t.to_string());
            }
        }
        targets.truncate(4);
        if targets.is_empty() {
            return Err(ToolError::MissingParameter("url or urls".into()));
        }

        let max_chars = args["max_chars"]
            .as_u64()
            .unwrap_or(MAX_TEXT_CHARS as u64)
            .clamp(1000, 48_000) as usize;
        // Split budget across concurrent pages
        let per_page = (max_chars / targets.len().max(1)).clamp(2000, MAX_TEXT_CHARS);
        let max_links = args["max_links"]
            .as_u64()
            .unwrap_or(20)
            .clamp(5, 40) as usize;

        let explicit = args.get("use_proxy").and_then(|v| v.as_bool());
        let (client, proxy) = web_client_for_args(&args)?;
        let net = proxy_note(&proxy, explicit);
        let client = std::sync::Arc::new(client);

        // URL policy before anything is dialed: scheme + literal-IP/local-name
        // checks and DNS resolution. Redirect hops are re-checked by the
        // client's redirect policy.
        for url in &targets {
            if let Err(reason) = validate_target(url, proxy.is_some()).await {
                return Ok(ToolResult::err("", format!("refusing to fetch {url}: {reason}")));
            }
        }

        if targets.len() > 1 {
            emit_progress(
                ctx,
                format!("⟳ concurrent fetch · {} URLs · {net}\n", targets.len()),
            );
        }

        let n = targets.len();
        let mut futs = Vec::new();
        for (i, url) in targets.into_iter().enumerate() {
            let c = client.clone();
            let net = net.clone();
            futs.push(async move {
                let label = format!("[{}/{}]", i + 1, n);
                fetch_one_page(&c, &url, per_page, max_links, &net, &label).await
            });
        }

        let parts = futures::future::join_all(futs).await;
        let mut out = String::new();
        let mut any_ok = false;
        for (i, part) in parts.into_iter().enumerate() {
            if i > 0 {
                out.push_str("\n\n────────\n\n");
            }
            match part {
                Ok(s) => {
                    any_ok = true;
                    out.push_str(&s);
                }
                Err(e) => out.push_str(&format!("Fetch error: {e}")),
            }
        }
        if any_ok {
            out.push_str(
                "\n\n── Follow-up ──\n\
                 If the user task needs more detail, pick the best Candidate links above \
                 (prefer official docs / same-site API pages / next chapter) and call \
                 do_web_fetch again (you may pass multiple urls for concurrent fetch).",
            );
            Ok(ToolResult::ok(out))
        } else {
            Ok(ToolResult::err(out.clone(), out))
        }
    }
}

/// Extract http(s) links from HTML, resolve against base, rank same-site / docs higher.
fn extract_candidate_links(html: &str, base_url: &str, max: usize) -> Vec<(String, String)> {
    let re = Regex::new(r#"(?is)<a\b[^>]*\bhref\s*=\s*["']([^"']+)["'][^>]*>(.*?)</a>"#).unwrap();
    let base = match reqwest::Url::parse(base_url) {
        Ok(u) => u,
        Err(_) => return vec![],
    };
    let base_host = base.host_str().unwrap_or("").to_ascii_lowercase();

    let mut scored: Vec<(i32, String, String)> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for cap in re.captures_iter(html) {
        let href = cap.get(1).map(|m| m.as_str().trim()).unwrap_or("");
        if href.is_empty()
            || href.starts_with('#')
            || href.starts_with("javascript:")
            || href.starts_with("mailto:")
            || href.starts_with("data:")
            || href.starts_with("tel:")
        {
            continue;
        }
        let abs = match base.join(href) {
            Ok(u) => u,
            Err(_) => continue,
        };
        if abs.scheme() != "http" && abs.scheme() != "https" {
            continue;
        }
        let mut abs_s = abs.to_string();
        // drop fragments
        if let Some(i) = abs_s.find('#') {
            abs_s.truncate(i);
        }
        if check_url_shallow(&abs_s).is_err() {
            continue;
        }
        let key = abs_s.trim_end_matches('/').to_ascii_lowercase();
        if key == base_url.trim_end_matches('/').to_ascii_lowercase() {
            continue;
        }
        if !seen.insert(key) {
            continue;
        }

        let label_raw = cap.get(2).map(|m| m.as_str()).unwrap_or("");
        let mut label = {
            let re_tags = Regex::new(r"(?is)<[^>]+>").unwrap();
            decode_entities(&re_tags.replace_all(label_raw, " "))
                .split_whitespace()                .collect::<Vec<_>>()
                .join(" ")
        };
        if label.is_empty() {
            label = abs.path().to_string();
        }
        if label.chars().count() > 80 {
            label = label.chars().take(80).collect::<String>() + "…";
        }

        let host = abs.host_str().unwrap_or("").to_ascii_lowercase();
        let path = abs.path().to_ascii_lowercase();
        let mut score = 10i32;
        if !base_host.is_empty() && host == base_host {
            score += 40; // same site
        }
        for d in [
            "docs.rs",
            "doc.rust-lang.org",
            "developer.mozilla.org",
            "github.com",
            "readthedocs.io",
            "readthedocs.org",
            "wikipedia.org",
            "stackoverflow.com",
        ] {
            if host.contains(d) {
                score += 35;
                break;
            }
        }
        for kw in [
            "/docs",
            "/doc/",
            "/api/",
            "/reference",
            "/guide",
            "/tutorial",
            "/manual",
            "/book/",
            "readme",
            "/wiki/",
        ] {
            if path.contains(kw) {
                score += 15;
                break;
            }
        }
        // deprioritize junk
        for bad in ["login", "signup", "register", "cart", "share", "facebook", "twitter"] {
            if path.contains(bad) || host.contains(bad) {
                score -= 30;
            }
        }

        scored.push((score, label, abs_s));
    }

    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.2.cmp(&b.2)));
    scored
        .into_iter()
        .filter(|(s, _, _)| *s >= 5)
        .take(max)
        .map(|(_, label, url)| (label, url))
        .collect()
}

async fn fetch_one_page(
    client: &Client,
    url: &str,
    max_chars: usize,
    max_links: usize,
    net: &str,
    label: &str,
) -> Result<String, String> {
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("{label} {url}: {e} ({net})"))?;

    let status = resp.status();
    let final_url = resp.url().to_string();
    let ctype = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    if !status.is_success() {
        let (bytes, _) = read_body_capped(resp, MAX_ERROR_BODY_BYTES)
            .await
            .map_err(|e| format!("{label} read error body: {e}"))?;
        let body = decode_body(&bytes, &ctype);
        let snippet: String = body.chars().take(300).collect();
        return Err(format!(
            "{label} HTTP {status} for {final_url} ({net})\n{snippet}"
        ));
    }

    let (bytes, truncated) = read_body_capped(resp, MAX_FETCH_BYTES)
        .await
        .map_err(|e| format!("{label} read body: {e}"))?;

    let raw = decode_body(&bytes, &ctype);
    let is_html = ctype.contains("html") || raw.trim_start().starts_with('<');
    let links = if is_html {
        extract_candidate_links(&raw, &final_url, max_links)
    } else {
        vec![]
    };
    let text = if is_html { html_to_text(&raw) } else { raw };
    let mut text = truncate_chars(text.trim(), max_chars);
    if truncated {
        text.push_str(&format!(
            "\n\n…[truncated: body exceeded {} bytes]",
            MAX_FETCH_BYTES
        ));
    }

    let mut out = format!(
        "{label}\nURL: {final_url}\nContent-Type: {ctype}\nStatus: {status}\nNetwork: {net}\n\n{text}"
    );
    if !links.is_empty() {
        out.push_str("\n\n── Candidate links (for task-driven deep fetch) ──\n");
        out.push_str(
            "Pick links relevant to the user task (same-site docs / API / next chapter). \
             Call do_web_fetch with url or urls=[...].\n",
        );
        for (i, (label, href)) in links.iter().enumerate() {
            out.push_str(&format!("{}. {} — {}\n", i + 1, label, href));
        }
    }
    Ok(out)
}

// ── do_web_search ──────────────────────────────────────────────────────────

pub struct DoWebSearch;

impl DoWebSearch {
    pub fn new() -> Self {
        Self
    }
}

impl Default for DoWebSearch {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
struct SearchHit {
    title: String,
    url: String,
    snippet: String,
    source: String,
}

fn emit_progress(ctx: &ToolContext, chunk: impl Into<String>) {
    let _ = ctx.sender.send(StreamEvent::ToolProgress {
        id: ctx.tool_call_id.clone(),
        chunk: chunk.into(),
    });
}

// ── Bing web search (free, no key, operator-aware) ────────────────────────
//
// Scrapes the public Bing SERP (https://www.bing.com/search). Precision comes
// from passing the query verbatim so Bing advanced operators work
// (site: filetype: intitle: -exclude "phrase" OR), locking market/language to
// the *query's* script (a Chinese query must not be served the US market),
// parsing only real organic <li class="b_algo"> results inside Bing's
// <ol id="b_results"> container, dropping result words that do not match the
// query, and decoding Bing's /ck/a redirect links back to the destination URL.

const BING_SEARCH_URL: &str = "https://www.bing.com/search";
const BING_TIMEOUT: Duration = Duration::from_secs(20);
const BING_MAX_RETRIES: u32 = 2;
const BING_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36";

/// Bing market/language for one query. Single decision point so a future
/// config override (settings → web_search_market) only has to replace
/// [`bing_locale_for_query`].
#[derive(Debug, Clone, Copy)]
struct BingLocale {
    mkt: &'static str,
    setlang: &'static str,
    cc: &'static str,
    accept_language: &'static str,
}

const BING_LOCALE_EN: BingLocale = BingLocale {
    mkt: "en-US",
    setlang: "en-US",
    cc: "US",
    accept_language: "en-US,en;q=0.9",
};
const BING_LOCALE_ZH: BingLocale = BingLocale {
    mkt: "zh-CN",
    setlang: "zh-Hans",
    cc: "CN",
    accept_language: "zh-CN,zh;q=0.9,en;q=0.5",
};
const BING_LOCALE_JA: BingLocale = BingLocale {
    mkt: "ja-JP",
    setlang: "ja",
    cc: "JP",
    accept_language: "ja,en;q=0.5",
};
const BING_LOCALE_KO: BingLocale = BingLocale {
    mkt: "ko-KR",
    setlang: "ko",
    cc: "KR",
    accept_language: "ko,en;q=0.5",
};

/// Pick the market from the query's script. ASCII-only queries keep the US
/// market; CJK queries get the matching one (kana → ja, hangul → ko, Han →
/// zh-CN) instead of being forced into en-US results.
fn bing_locale_for_query(query: &str) -> BingLocale {
    let mut han = false;
    for c in query.chars() {
        let cp = c as u32;
        if (0x3040..=0x30FF).contains(&cp) {
            return BING_LOCALE_JA; // kana
        }
        if (0x1100..=0x11FF).contains(&cp) || (0xAC00..=0xD7AF).contains(&cp) {
            return BING_LOCALE_KO; // hangul
        }
        if (0x3400..=0x4DBF).contains(&cp)
            || (0x4E00..=0x9FFF).contains(&cp)
            || (0xF900..=0xFAFF).contains(&cp)
            || (0x20000..=0x2FA1F).contains(&cp)
        {
            han = true;
        }
    }
    if han {
        BING_LOCALE_ZH
    } else {
        BING_LOCALE_EN
    }
}

/// Decode a Bing `/ck/a?...&u=a1<base64url>` redirect into the real URL.
/// Non-redirect http(s) links pass through unchanged.
fn bing_decode_url(href: &str) -> Option<String> {
    if !href.contains("/ck/a") {
        return (href.starts_with("http://") || href.starts_with("https://"))
            .then(|| href.to_string());
    }
    let u = href.split("u=").nth(1)?.split('&').next()?;
    // Observed payload is `a1<url-safe base64, no padding>` — the leading `a1`
    // is a redirect marker, not part of the encoded URL.
    let payload = u.strip_prefix("a1").unwrap_or(u);
    let pad = "=".repeat((4 - payload.len() % 4) % 4);
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(&format!("{payload}{pad}"))
        .ok()?;
    String::from_utf8(bytes).ok()
}

/// Strip HTML tags + decode entities from a Bing result fragment.
fn bing_clean(s: &str, tag_re: &Regex) -> String {
    decode_entities(&tag_re.replace_all(s, "")).trim().to_string()
}

/// Slice out Bing's organic results container (`<ol id="b_results">…</ol>`),
/// counting `<ol>` depth so nested lists (sitelinks) do not end it early.
/// Falls back to the whole input when the container cannot be located.
fn bing_results_scope(html: &str) -> &str {
    let re_open = Regex::new(r#"(?i)<ol[^>]*\bid\s*=\s*["']?b_results\b"#).unwrap();
    let Some(open) = re_open.find(html) else {
        return html;
    };
    let start = open.start();
    let re_ol = Regex::new(r"(?i)<ol\b|</ol\s*>").unwrap();
    let mut depth = 0i32;
    for m in re_ol.find_iter(&html[start..]) {
        if m.as_str().starts_with("</") {
            depth -= 1;
            if depth == 0 {
                return &html[start..start + m.end()];
            }
        } else {
            depth += 1;
        }
    }
    &html[start..]
}

/// Lower-case query terms, operator prefixes removed. An empty result means
/// the query was operators/stop words only, and relevance filtering is skipped.
fn query_terms(query: &str) -> Vec<String> {
    let mut terms = Vec::new();
    for raw in query.split_whitespace() {
        let lower = raw.to_ascii_lowercase();
        let mut term = lower.as_str();
        for op in [
            "site:", "filetype:", "intitle:", "inurl:", "inbody:", "loc:", "language:", "lang:",
        ] {
            if let Some(rest) = term.strip_prefix(op) {
                term = rest;
                break;
            }
        }
        if term == "or" || term == "and" || term == "not" {
            continue;
        }
        let term = term.trim_start_matches('-').trim_matches('"').trim_matches('\'');
        if !term.is_empty() {
            terms.push(term.to_string());
        }
    }
    terms
}

/// Word-level match for plain ASCII terms (`rust` ≠ `trustworthy`), substring
/// match for CJK, phrases and symbol terms (`c++`, `.net`, `node.js`).
fn contains_term(hay: &str, term: &str) -> bool {
    if term.is_empty() {
        return true;
    }
    if !term.chars().all(|c| c.is_ascii_alphanumeric()) {
        return hay.contains(term);
    }
    hay.split(|c: char| !c.is_alphanumeric()).any(|w| w == term)
}

/// A hit is relevant when at least one query term appears in its title,
/// snippet or URL. Blocks Bing's fallback to unrelated "popular" results when
/// the real answer is missing.
fn hit_is_relevant(query: &str, title: &str, snippet: &str, url: &str) -> bool {
    let terms = query_terms(query);
    if terms.is_empty() {
        return true;
    }
    let hay = format!(
        "{}\n{}\n{}",
        title.to_lowercase(),
        snippet.to_lowercase(),
        url.to_lowercase()
    );
    terms.iter().any(|t| contains_term(&hay, t))
}

/// Parse Bing's SERP HTML into clean organic [`SearchHit`]s.
/// Returns `(hits, rejected)` where `rejected` counts blocks that were dropped
/// as non-organic, malformed or irrelevant.
fn bing_parse_html(html: &str, query: &str, limit: usize) -> (Vec<SearchHit>, usize) {
    let title_re = Regex::new(r#"<h2[^>]*>.*?<a[^>]*href="([^"]+)"[^>]*>(.*?)</a>"#).unwrap();
    let snip_re = Regex::new(r#"<p[^>]*>(.*?)</p>"#).unwrap();
    let tag_re = Regex::new(r"<[^>]+>").unwrap();

    // Only parse inside Bing's organic container, so the page tail (related
    // searches, FAQ, footer) can never be read as a result.
    let scope = bing_results_scope(html);

    // Each `<li class="b_algo …">` starts a result; the block ends at the next
    // result, the next non-organic module (answer/ad/pagination) or the end of
    // the results list — whichever comes first.
    let re_algo = Regex::new(r#"(?is)<li[^>]*\bclass\s*=\s*"[^"]*\bb_algo\b[^"]*""#).unwrap();
    let re_other =
        Regex::new(r#"(?is)<li[^>]*\bclass\s*=\s*"[^"]*\bb_(?:ans|ad|pag)\b[^"]*""#).unwrap();

    let algo_starts: Vec<usize> = re_algo.find_iter(scope).map(|m| m.start()).collect();
    let other_starts: Vec<usize> = re_other.find_iter(scope).map(|m| m.start()).collect();

    let mut hits = Vec::new();
    let mut rejected = 0usize;
    for (i, &start) in algo_starts.iter().enumerate() {
        if hits.len() >= limit {
            break;
        }
        let next_algo = algo_starts.get(i + 1).copied().unwrap_or(scope.len());
        let next_other = other_starts
            .iter()
            .copied()
            .find(|&p| p > start)
            .unwrap_or(scope.len());
        let end = next_algo.min(next_other).min(scope.len());
        let mut block = &scope[start..end];
        // A block may contain a nested `<ol>` (sitelinks); the organic list's
        // own `</ol>` is the *last* one in the block, not the first.
        if let Some(pos) = block.rfind("</ol>") {
            block = &block[..pos];
        }

        if re_other.is_match(block) || block.len() > MAX_BING_BLOCK_BYTES {
            rejected += 1;
            continue;
        }
        let Some(caps) = title_re.captures(block) else {
            rejected += 1;
            continue;
        };
        let href = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        let Some(url) = bing_decode_url(href) else {
            rejected += 1;
            continue;
        };
        // Drop Bing's own pages, non-http(s) payloads and anything pointing at
        // a private/local address.
        if url.contains("bing.com") || check_url_shallow(&url).is_err() {
            rejected += 1;
            continue;
        }
        let title = bing_clean(caps.get(2).map(|m| m.as_str()).unwrap_or(""), &tag_re);
        if title.is_empty() {
            rejected += 1;
            continue;
        }
        let snippet = snip_re
            .captures(block)
            .map(|c| bing_clean(c.get(1).map(|m| m.as_str()).unwrap_or(""), &tag_re))
            .unwrap_or_default();
        if !hit_is_relevant(query, &title, &snippet, &url) {
            rejected += 1;
            continue;
        }

        hits.push(SearchHit {
            title,
            url,
            snippet,
            source: "bing".into(),
        });
    }
    (hits, rejected)
}

/// One Bing search request (no retry — see [`bing_search`]).
async fn bing_search_once(
    client: &Client,
    query: &str,
    limit: usize,
) -> Result<Vec<SearchHit>, String> {
    let locale = bing_locale_for_query(query);
    let count = limit.min(10).to_string();
    let resp = client
        .get(BING_SEARCH_URL)
        .query(&[
            ("q", query),
            ("count", count.as_str()),
            ("setlang", locale.setlang),
            ("mkt", locale.mkt),
            ("cc", locale.cc),
        ])
        .header("User-Agent", BING_UA)
        .header("Accept-Language", locale.accept_language)
        .timeout(BING_TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("Bing HTTP: {e}"))?;

    let status = resp.status();
    let ctype = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let (bytes, _) = read_body_capped(resp, MAX_FETCH_BYTES)
        .await
        .map_err(|e| format!("Bing body: {e}"))?;
    let html = decode_body(&bytes, &ctype);
    if !status.is_success() {
        let snippet: String = html
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(200)
            .collect();
        return Err(format!("Bing HTTP {status}: {snippet}"));
    }
    let (hits, rejected) = bing_parse_html(&html, query, limit);
    if hits.is_empty() {
        if rejected > 0 {
            Err(format!(
                "Bing: {rejected} organic results, none matched the query terms"
            ))
        } else {
            Err(format!(
                "Bing: no organic results (no b_algo blocks in {} bytes — \
                 consent wall, captcha or changed SERP markup)",
                html.len()
            ))
        }
    } else {
        Ok(hits)
    }
}

/// Retry-aware Bing search — a transient network/proxy blip never kills it.
async fn bing_search(
    client: &Client,
    query: &str,
    limit: usize,
) -> Result<Vec<SearchHit>, String> {
    let mut last_err = String::new();
    for attempt in 0..BING_MAX_RETRIES {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(250 * u64::from(attempt))).await;
        }
        match bing_search_once(client, query, limit).await {
            Ok(hits) => return Ok(hits),
            Err(e) => last_err = e,
        }
    }
    Err(format!(
        "Bing search failed after {BING_MAX_RETRIES} tries: {last_err}"
    ))
}

fn dedupe_hits(hits: Vec<SearchHit>, max: usize) -> Vec<SearchHit> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for h in hits {
        let key = h.url.trim_end_matches('/').to_ascii_lowercase();
        if key.is_empty() || !seen.insert(key) {
            continue;
        }
        out.push(h);
        if out.len() >= max {
            break;
        }
    }
    out
}

async fn public_web_search(
    client: &Client,
    query: &str,
    limit: usize,
    ctx: &ToolContext,
) -> Result<(Vec<SearchHit>, Vec<String>), String> {
    let mut logs = Vec::new();

    // Bing organic search — primary (free, no key, operator-aware)
    emit_progress(ctx, "  ▸ Bing 搜索 …\n");
    let hits = match bing_search(client, query, limit).await {
        Ok(hits) => {
            emit_progress(ctx, format!("  ✓ Bing · {} 条\n", hits.len()));
            logs.push(format!("✓ Bing: {}", hits.len()));
            hits
        }
        Err(e) => {
            emit_progress(
                ctx,
                format!("  ✗ Bing · {}\n", e.chars().take(120).collect::<String>()),
            );
            logs.push(format!("✗ Bing: {e}"));
            return Err(e);
        }
    };

    let merged = dedupe_hits(hits, limit);
    if merged.is_empty() {
        Err("Bing: no results after dedupe".into())
    } else {
        Ok((merged, logs))
    }
}

#[async_trait]
impl Tool for DoWebSearch {
    fn name(&self) -> &str {
        "do_web_search"
    }

    fn description(&self) -> &str {
        "Web search powered by Bing (free, no API key). \
         Returns title/URL/snippet from Bing's organic results. \
         Supports Bing operators: site:, filetype:, intitle:, -exclude, \"phrase\", OR. \
         Use when you need links and have no URL yet; then do_web_fetch the best URL. \
         Optional use_proxy — prefer true if direct access fails (same proxy as do_web_fetch)."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query"
                },
                "max_results": {
                    "type": "integer",
                    "description": "Max results (default 8, max 10)"
                },
                "use_proxy": use_proxy_param_schema()
            },
            "required": ["query"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let query = args["query"]
            .as_str()
            .ok_or_else(|| ToolError::MissingParameter("query".into()))?
            .trim()
            .to_string();
        if query.is_empty() {
            return Ok(ToolResult::err("", "query is empty"));
        }
        let max = args["max_results"]
            .as_u64()
            .unwrap_or(8)
            .clamp(1, MAX_SEARCH_RESULTS as u64) as usize;

        let explicit = args.get("use_proxy").and_then(|v| v.as_bool());
        let (client, proxy) = web_client_for_args(&args)?;
        let net = proxy_note(&proxy, explicit);

        emit_progress(
            ctx,
            format!("⟳ 公开网页搜索 · {net}\n  查询: {query}\n"),
        );

        match public_web_search(&client, &query, max, ctx).await {
            Ok((hits, logs)) => {
                emit_progress(ctx, format!("● 合并 {} 条（去重）\n", hits.len()));
                let mut out = format!(
                    "Query: {query}\nNetwork: {net}\nSources:\n{}\n\nResults ({}):\n\n",
                    logs.join("\n"),
                    hits.len()
                );
                for (i, h) in hits.iter().enumerate() {
                    out.push_str(&format!(
                        "{}. [{}] {}\n   {}\n   {}\n\n",
                        i + 1,
                        h.source,
                        h.title,
                        h.url,
                        if h.snippet.is_empty() {
                            "—"
                        } else {
                            h.snippet.as_str()
                        }
                    ));
                }
                out.push_str("Tip: call do_web_fetch on a promising URL for full page text.");
                Ok(ToolResult::ok(out))
            }
            Err(e) => {
                emit_progress(ctx, format!("  ✗ 搜索失败\n"));
                // Auto-hint: if direct failed and proxy exists, suggest use_proxy
                let hint = if proxy.is_none() && proxy_configured_url().is_some() {
                    "\nHint: a proxy is configured — retry with use_proxy=true."
                } else if proxy.is_some() {
                    "\nHint: try use_proxy=false, or a more specific query."
                } else {
                    "\nHint: configure Settings → proxy if your network blocks search endpoints."
                };
                // A total failure must not be reported as success — the agent
                // loop's retry/stall handling keys on `success`.
                let msg = format!(
                    "Query: {query}\nNetwork: {net}\nSearch failed: {e}{hint}\n\
                     If you already have a URL, use do_web_fetch."
                );
                Ok(ToolResult::err(msg.clone(), msg))
            }
        }
    }
}

// ── Deep concurrent search: multi-query × full-text (Bing + Jina) ───────
//
// One Bing search per query (Jina Reader for full text),
// all queries run concurrently (buffer_unordered). This is safe against
// shared-session races and lets the free tier breathe.

#[derive(Debug, Clone)]
struct DeepItem {
    title: String,
    url: String,
    snippet: String,
    full: String,
}

/// Read a URL as clean markdown via Jina Reader (https://r.jina.ai, free, no key).
/// Strips the leading metadata block; returns Err on any failure so callers can
/// fall back to another channel (multi-backend routing, Agent-Reach style).
async fn jina_fetch(client: &Client, url: &str, max_chars: usize) -> Result<String, String> {
    // A target URL from a SERP is attacker-influenced and is disclosed to a
    // third party (r.jina.ai) — never hand it a private/local address.
    if let Err(reason) = check_url_shallow(url) {
        return Err(format!("Jina Reader target refused: {reason}"));
    }
    let jina_url = format!("https://r.jina.ai/{url}");
    let resp = client
        .get(&jina_url)
        .header(
            "User-Agent",
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36",
        )
        .header("Accept", "text/plain")
        .timeout(Duration::from_secs(25))
        .send()
        .await
        .map_err(|e| format!("Jina Reader HTTP: {e}"))?;
    let status = resp.status();
    let ctype = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    // Bounded read: at most 4 bytes per requested char (UTF-8 worst case),
    // floored so metadata + the head of the article always fit.
    let cap = max_chars.saturating_mul(4).clamp(64 * 1024, MAX_FETCH_BYTES);
    let (bytes, _) = read_body_capped(resp, cap)
        .await
        .map_err(|e| format!("Jina Reader body: {e}"))?;
    let text = decode_body(&bytes, &ctype);
    if !status.is_success() {
        let snippet: String = text.chars().take(160).collect();
        return Err(format!("Jina Reader HTTP {status}: {snippet}"));
    }
    // Jina prepends metadata (Title / URL Source / Published Time / Markdown Content:)
    let body = match text.find("Markdown Content:") {
        Some(idx) => text[idx + "Markdown Content:".len()..].trim().to_string(),
        None => text,
    };
    if body.trim().is_empty() {
        return Err("Jina Reader: empty content".into());
    }
    Ok(body.chars().take(max_chars).collect())
}

/// One Bing query: search + fetch top-N full texts (Jina Reader).
async fn bing_deep_query(
    client: &Client,
    query: &str,
    per_query: usize,
    depth: usize,
    max_chars: usize,
) -> Result<(Vec<DeepItem>, usize), String> {
    // search
    let hits = bing_search(client, query, per_query).await?;

    // full text: Jina Reader (free, no session, one GET per URL)
    let urls: Vec<String> = hits.iter().take(depth).map(|h| h.url.clone()).collect();
    let mut fetches: Vec<(String, String)> = Vec::new();
    if !urls.is_empty() {
        let jina_results: Vec<(String, Option<String>)> = stream::iter(urls.clone())
            .map(|u| {
                let client = client.clone();
                async move {
                    let r = jina_fetch(&client, &u, max_chars).await;
                    (u, r.ok())
                }
            })
            .buffer_unordered(depth.min(4))
            .collect()
            .await;
        for (u, t) in jina_results {
            if let Some(t) = t {
                fetches.push((u, t));
            }
        }
    }

    let items = hits
        .into_iter()
        .map(|h| {
            let full = fetches
                .iter()
                .find(|(u, _)| {
                    u.trim_end_matches('/')
                        .eq_ignore_ascii_case(h.url.trim_end_matches('/'))
                })
                .map(|(_, t)| t.clone())
                .unwrap_or_default();
            DeepItem {
                title: h.title,
                url: h.url,
                snippet: h.snippet,
                full,
            }
        })
        .collect();
    Ok((items, fetches.len()))
}

/// Deep concurrent research: run many queries in parallel, fetch full page
/// text for the top results of each, and merge everything into one report.
pub struct DoDeepSearch;

impl DoDeepSearch {
    pub fn new() -> Self {
        Self
    }
}

impl Default for DoDeepSearch {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for DoDeepSearch {
    fn name(&self) -> &str {
        "do_deep_search"
    }

    fn description(&self) -> &str {
        "Deep concurrent web research (Bing + Jina): run multiple search queries in \
         parallel, then fetch full page text for the top results of each query. \
         Use when you need a thorough multi-angle answer backed by real page \
         content, not just snippets. Split the topic into several specific \
         `queries`. Optional use_proxy — prefer true if direct access fails."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "queries": {
                    "type": "array",
                    "items": { "type": "string" },
                    "minItems": 1,
                    "description": "One or more specific search queries (split the topic into angles)"
                },
                "per_query": {
                    "type": "integer",
                    "description": "Results per query (default 5, max 10)"
                },
                "depth": {
                    "type": "integer",
                    "description": "How many top results per query get full-text fetch (default 2, max 3)"
                },
                "concurrency": {
                    "type": "integer",
                    "description": "Max parallel queries (default 4, max 8)"
                },
                "max_chars": {
                    "type": "integer",
                    "description": "Max full-text chars per page (default 3000)"
                },
                "use_proxy": use_proxy_param_schema()
            },
            "required": ["queries"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let queries: Vec<String> = args["queries"]
            .as_array()
            .ok_or_else(|| ToolError::MissingParameter("queries".into()))?
            .iter()
            .filter_map(|q| q.as_str().map(|s| s.trim().to_string()))
            .filter(|s| !s.is_empty())
            .collect();
        if queries.is_empty() {
            return Ok(ToolResult::err("", "queries is empty"));
        }
        let per_query = args["per_query"].as_u64().unwrap_or(5).clamp(1, 10) as usize;
        let depth = args["depth"].as_u64().unwrap_or(2).clamp(1, 3) as usize;
        let concurrency = args["concurrency"].as_u64().unwrap_or(4).clamp(1, 8) as usize;
        let max_chars = args["max_chars"].as_u64().unwrap_or(3000).clamp(500, 8000) as usize;

        let explicit = args.get("use_proxy").and_then(|v| v.as_bool());
        let (client, proxy) = web_client_for_args(&args)?;
        let net = proxy_note(&proxy, explicit);

        emit_progress(
            ctx,
            format!(
                "⟳ 深度并发搜索 · {net}\n  {} 个查询 · 并发 {} · 每查询 {} 条 / 深挖 {} 篇全文\n",
                queries.len(),
                concurrency,
                per_query,
                depth
            ),
        );

        let client2 = client.clone();
        let results: Vec<(usize, String, Result<(Vec<DeepItem>, usize), String>)> =
            stream::iter(queries.iter().cloned().enumerate())
                .map(move |(i, q)| {
                    let client = client2.clone();
                    async move {
                        let r = bing_deep_query(&client, &q, per_query, depth, max_chars).await;
                        (i, q, r)
                    }
                })
                .buffer_unordered(concurrency)
                .collect()
                .await;

        // re-order by original query index
        let mut by_idx: Vec<Option<(String, Result<(Vec<DeepItem>, usize), String>)>> =
            vec![None; queries.len()];
        for (i, q, r) in results {
            by_idx[i] = Some((q, r));
        }

        let mut out = String::from("Deep Search\n");
        out.push_str(&format!("Network: {net}\nQueries ({}):\n", queries.len()));
        for (i, q) in queries.iter().enumerate() {
            out.push_str(&format!("  {}. \"{q}\"\n", i + 1));
        }

        let mut ok_count = 0usize;
        let mut item_count = 0usize;
        let mut full_count = 0usize;
        for (i, q) in queries.iter().enumerate() {
            match &by_idx[i] {
                Some((_, Ok((items, n_full)))) => {
                    ok_count += 1;
                    item_count += items.len();
                    full_count += n_full;
                    out.push_str(&format!("\n── Query {}: \"{q}\" — {} results, {n_full} full texts\n", i + 1, items.len()));
                    for (j, it) in items.iter().enumerate() {
                        out.push_str(&format!("[{}] {}\n    {}\n    {}\n", j + 1, it.title, it.url, it.snippet));
                        if !it.full.is_empty() {
                            let head: String = it.full.chars().take(600).collect();
                            out.push_str(&format!("    📄 {}", head.replace('\n', "\n    ")));
                            out.push('\n');
                        }
                    }
                }
                Some((_, Err(e))) => {
                    out.push_str(&format!("\n── Query {}: \"{q}\" ❌ {e}\n", i + 1));
                }
                None => {}
            }
        }
        out.push_str(&format!(
            "\nSummary: {ok_count}/{} queries OK · {item_count} results · {full_count} full texts\n\
             Tip: follow up with do_web_fetch for any single page.",
            queries.len()
        ));
        Ok(ToolResult::ok(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_strip_basic() {
        let t = html_to_text("<html><script>x</script><p>Hello <b>world</b></p></html>");
        assert!(t.contains("Hello"));
        assert!(t.contains("world"));
    }

    #[test]
    fn html_to_text_keeps_line_structure() {
        let t = html_to_text("<p>One</p><p>Two</p>");
        assert!(t.contains('\n'), "expected line breaks, got {t:?}");
        assert!(t.contains("One") && t.contains("Two"));
    }

    #[test]
    fn entities_are_decoded_not_deleted() {
        assert_eq!(decode_entities("don&#8217;t"), "don\u{2019}t");
        assert_eq!(decode_entities("it&#x27;s"), "it's");
        assert_eq!(decode_entities("a &amp; b"), "a & b");
        assert_eq!(decode_entities("&unknown; x"), "&unknown; x");
    }

    #[test]
    fn blocked_urls_cover_obfuscated_forms() {
        for u in [
            "http://127.0.0.1/",
            "http://2130706433/",
            "http://0x7f000001/",
            "http://127.000.000.001/",
            "http://[::1]/",
            "http://[::ffff:127.0.0.1]/",
            "http://169.254.169.254/latest/meta-data/",
            "http://10.0.0.5/",
            "http://192.168.1.1/",
            "ftp://example.com/",
        ] {
            assert!(check_url_shallow(u).is_err(), "should be blocked: {u}");
        }
        // Path/query mentions must not be false positives.
        assert!(check_url_shallow("https://en.wikipedia.org/wiki/169.254.169.254").is_ok());
        assert!(check_url_shallow("https://example.com/?q=127.0.0.1").is_ok());
    }

    #[test]
    fn bing_locale_follows_query_script() {
        assert_eq!(bing_locale_for_query("rust async runtime").mkt, "en-US");
        assert_eq!(bing_locale_for_query("Rust 异步编程").mkt, "zh-CN");
        assert_eq!(bing_locale_for_query("東京 ラーメン").mkt, "ja-JP");
        assert_eq!(bing_locale_for_query("서울 맛집").mkt, "ko-KR");
    }

    #[test]
    fn relevance_filter_drops_unrelated_hits() {
        assert!(hit_is_relevant(
            "rust async",
            "Tokio — async runtime for Rust",
            "",
            "https://tokio.rs/"
        ));
        assert!(!hit_is_relevant(
            "rust async",
            "Cooking recipes",
            "how to bake bread",
            "https://food.example/"
        ));
        assert!(hit_is_relevant("异步编程", "Rust 异步编程入门", "", "https://example.com/a"));
        assert!(hit_is_relevant("site:example.com", "Some page", "", "https://example.com/x"));
        // Operator-only query: filtering is skipped, not everything dropped.
        assert!(hit_is_relevant("OR", "anything at all", "", "https://x.example/"));
    }

    #[test]
    fn bing_parse_ignores_page_tail() {
        let html = r#"
<html><body>
<ol id="b_results">
<li class="b_algo"><h2><a href="https://doc.rust-lang.org/book/">The Rust Book</a></h2>
<p>Learn Rust with the official book.</p></li>
<li class="b_algo"><h2><a href="https://tokio.rs/">Tokio</a></h2>
<p>An async runtime for Rust.</p><ol><li><a href="https://tokio.rs/x">sitelink</a></li></ol></li>
<li class="b_ans"><h2><a href="https://faq.example/">People also ask</a></h2><p>Unrelated FAQ answer.</p></li>
</ol>
<div id="b_context"><h2><a href="https://footer.example/">Related searches</a></h2><p>Footer junk.</p></div>
</body></html>"#;
        let (hits, _) = bing_parse_html(html, "rust book", 10);
        assert_eq!(hits.len(), 2, "{hits:?}");
        assert!(hits.iter().all(|h| !h.url.contains("faq.example")));
        assert!(hits.iter().all(|h| !h.url.contains("footer.example")));
        assert!(hits.iter().all(|h| !h.snippet.contains("Footer junk")));
        assert!(hits.iter().any(|h| h.title.contains("Tokio")));
    }

    #[test]
    fn resolve_proxy_without_config() {
        // When no proxy in env config, use_proxy true still cannot enable
        let args = serde_json::json!({"use_proxy": false});
        let (want, url) = resolve_use_proxy(&args);
        if proxy_configured_url().is_none() {
            assert!(!want);
            assert!(url.is_none());
        }
    }

    #[test]
    fn extract_links_prefers_same_site_docs() {
        let html = r##"
        <a href="/docs/api">API Reference</a>
        <a href="https://docs.rs/tokio">Tokio docs</a>
        <a href="https://evil.com/login">Login</a>
        <a href="#frag">Top</a>
        <a href="https://example.com/guide/intro">Guide</a>
        "##;
        let links = extract_candidate_links(html, "https://example.com/page", 10);
        assert!(links.iter().any(|(_, u)| u.contains("example.com/docs/api")));
        assert!(links.iter().any(|(_, u)| u.contains("docs.rs")));
        assert!(!links.iter().any(|(_, u)| u.contains("#frag")));
        assert!(
            links[0].1.contains("example.com") || links[0].1.contains("docs.rs"),
            "top link: {:?}",
            links[0]
        );
    }
}

#[tokio::test]
async fn private_url_blocked() {
    use crate::safety::guard::SafetyGuard;
    use std::sync::Arc;
    let tool = DoWebFetch::new();
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let ctx = ToolContext::simple(
        std::env::temp_dir(),
        "t",
        "c1",
        tx,
        Arc::new(SafetyGuard::new(&[], true)),
    );
    let r = tool
        .execute(serde_json::json!({"url": "http://127.0.0.1/"}), &ctx)
        .await
        .unwrap();
    assert!(!r.success);
}

/// Live smoke: Bing search via proxy if configured, else direct.
#[tokio::test]
async fn live_search_bing_smoke() {
    use crate::safety::guard::SafetyGuard;
    use std::sync::Arc;

    let tool = DoWebSearch::new();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let ctx = ToolContext::simple(
        std::env::temp_dir(),
        "t",
        "search",
        tx,
        Arc::new(SafetyGuard::new(&[], true)),
    );

    // Prefer proxy when available (user env often needs it)
    let use_proxy = proxy_configured_url().is_some();
    let r = tool
        .execute(
            serde_json::json!({
                "query": "OpenAI latest news",
                "max_results": 5,
                "use_proxy": use_proxy
            }),
            &ctx,
        )
        .await
        .expect("exec");

    while let Ok(_) = rx.try_recv() {}

    println!(
        "search use_proxy={use_proxy} success={} head=\n{}",
        r.success,
        &r.output.chars().take(800).collect::<String>()
    );

    assert!(r.output.contains("Query:"));
    // Must get real hits or a structured failure — never panic
    assert!(
        r.output.contains("https://") || r.output.contains("Search failed"),
        "unexpected: {}",
        &r.output.chars().take(300).collect::<String>()
    );
    if r.output.contains("https://") {
        assert!(
            r.output.contains("[bing]"),
            "expected bing source tag"
        );
    }
}

/// Live: single fetch + candidate links + multi-url concurrent.
#[tokio::test]
async fn live_fetch_links_and_multi() {
    use crate::safety::guard::SafetyGuard;
    use crate::tools::trait_def::{Tool, ToolContext};
    use std::sync::Arc;

    let tool = DoWebFetch::new();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let ctx = ToolContext::simple(
        std::env::temp_dir(),
        "live",
        "f1",
        tx,
        Arc::new(SafetyGuard::new(&[], true)),
    );

    let use_proxy = proxy_configured_url().is_some();
    println!("use_proxy={use_proxy}");

    // 1) Single page with links (example.com is minimal; use rust-lang.org or wikipedia)
    let r1 = tool
        .execute(
            serde_json::json!({
                "url": "https://www.rust-lang.org/",
                "max_chars": 4000,
                "max_links": 15,
                "use_proxy": use_proxy
            }),
            &ctx,
        )
        .await
        .expect("exec single");
    while let Ok(_) = rx.try_recv() {}

    println!("=== SINGLE success={} ===", r1.success);
    println!("{}", &r1.output.chars().take(1200).collect::<String>());
    assert!(r1.success, "single fetch failed: {:?}", r1.error);
    assert!(
        r1.output.contains("Candidate links") || r1.output.contains("https://"),
        "expected body and/or links"
    );
    let has_links = r1.output.contains("Candidate links");
    println!("has_candidate_links={has_links}");

    // 2) Concurrent multi-url
    let r2 = tool
        .execute(
            serde_json::json!({
                "urls": [
                    "https://example.com/",
                    "https://www.rust-lang.org/learn"
                ],
                "max_chars": 6000,
                "max_links": 10,
                "use_proxy": use_proxy
            }),
            &ctx,
        )
        .await
        .expect("exec multi");
    let mut prog = String::new();
    while let Ok(ev) = rx.try_recv() {
        if let crate::agent::stream::StreamEvent::ToolProgress { chunk, .. } = ev {
            prog.push_str(&chunk);
        }
    }
    println!("=== MULTI progress ===\n{prog}");
    println!("=== MULTI success={} ===", r2.success);
    println!("{}", &r2.output.chars().take(1500).collect::<String>());
    assert!(r2.success, "multi fetch failed: {:?}", r2.error);
    assert!(
        r2.output.contains("[1/2]") && r2.output.contains("[2/2]"),
        "expected concurrent labels"
    );
    assert!(
        r2.output.contains("example") || r2.output.contains("Example"),
        "expected example.com content"
    );
    assert!(
        r2.output.contains("Follow-up") || r2.output.contains("Candidate links"),
        "expected follow-up guidance or links"
    );
}


/// Effect demo: print full candidate links + multi-fetch quality.
#[tokio::test]
async fn live_fetch_effect_demo() {
    use crate::safety::guard::SafetyGuard;
    use crate::tools::trait_def::{Tool, ToolContext};
    use std::sync::Arc;
    use std::time::Instant;

    let tool = DoWebFetch::new();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let ctx = ToolContext::simple(
        std::env::temp_dir(),
        "fx",
        "demo",
        tx,
        Arc::new(SafetyGuard::new(&[], true)),
    );
    let use_proxy = proxy_configured_url().is_some();
    println!("=== ENV use_proxy={use_proxy} ===\n");

    // A) Task-like page with many docs links
    let t0 = Instant::now();
    let r = tool
        .execute(
            serde_json::json!({
                "url": "https://doc.rust-lang.org/book/",
                "max_chars": 3500,
                "max_links": 20,
                "use_proxy": use_proxy
            }),
            &ctx,
        )
        .await
        .expect("book");
    let ms = t0.elapsed().as_millis();
    while let Ok(_) = rx.try_recv() {}
    println!("=== A) rust book single fetch  {ms}ms  success={} ===", r.success);
    // Print only meta + candidate links section
    if let Some(i) = r.output.find("── Candidate links") {
        let head = &r.output[..r.output.find("\n\n").unwrap_or(200).min(400)];
        println!("{head}\n");
        println!("{}", &r.output[i..]);
    } else {
        println!("(no candidate links section)\n{}", &r.output.chars().take(600).collect::<String>());
    }
    assert!(r.success);

    // B) Concurrent: search-like multi page
    let t1 = Instant::now();
    let r2 = tool
        .execute(
            serde_json::json!({
                "urls": [
                    "https://docs.rs/tokio/latest/tokio/macro.select.html",
                    "https://tokio.rs/tokio/tutorial/select",
                    "https://example.com/"
                ],
                "max_chars": 9000,
                "max_links": 12,
                "use_proxy": use_proxy
            }),
            &ctx,
        )
        .await
        .expect("multi");
    let ms2 = t1.elapsed().as_millis();
    let mut prog = String::new();
    while let Ok(ev) = rx.try_recv() {
        if let crate::agent::stream::StreamEvent::ToolProgress { chunk, .. } = ev {
            prog.push_str(&chunk);
        }
    }
    println!("\n=== B) concurrent 3-URL fetch  {ms2}ms  success={} ===", r2.success);
    println!("progress: {prog}");
    // Summarize each page: first line URL + whether has candidates + body keyword
    for part in r2.output.split("────────") {
        let url_line = part.lines().find(|l| l.starts_with("URL:")).unwrap_or("?");
        let has = part.contains("Candidate links");
        let nlinks = part.matches(" — https://").count() + part.matches(" — http://").count();
        let ok = part.contains("Status: 200");
        println!("  {url_line} | 200={ok} | candidates={has} (~{nlinks} listed)");
        if has {
            // print first 5 candidate lines
            if let Some(i) = part.find("── Candidate links") {
                for line in part[i..].lines().skip(2).take(6) {
                    if line.starts_with(|c: char| c.is_ascii_digit()) {
                        println!("    {line}");
                    }
                }
            }
        }
    }
    assert!(r2.success);
    assert!(r2.output.contains("[1/3]") && r2.output.contains("[3/3]"));

    // C) Deep-fetch: take a candidate from book page and fetch it
    let mut deep_url = None;
    if let Some(i) = r.output.find("── Candidate links") {
        for line in r.output[i..].lines() {
            // Prefer installation chapter for a realistic "task deep fetch"
            if line.contains("https://doc.rust-lang.org/book/ch01-01-installation.html") {
                if let Some(pos) = line.rfind("https://") {
                    deep_url = Some(line[pos..].trim().to_string());
                    break;
                }
            }
        }
        if deep_url.is_none() {
            for line in r.output[i..].lines() {
                if let Some(pos) = line.rfind("https://doc.rust-lang.org/") {
                    deep_url = Some(line[pos..].trim().to_string());
                    break;
                }
            }
        }
    }
    if let Some(u) = deep_url {
        let t2 = Instant::now();
        let r3 = tool
            .execute(serde_json::json!({"url": u, "max_chars": 2500, "max_links": 8, "use_proxy": use_proxy}), &ctx)
            .await
            .expect("deep");
        println!("\n=== C) deep-fetch from candidate  {}ms ===", t2.elapsed().as_millis());
        println!("fetched: {u}");
        println!("success={} body_preview:\n{}", r3.success, &r3.output.chars().take(500).collect::<String>());
        assert!(r3.success);
    } else {
        println!("\n=== C) skipped deep-fetch (no same-site candidate) ===");
    }
}

