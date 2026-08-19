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
use std::time::Duration;

use crate::agent::stream::StreamEvent;
use crate::config::settings::Config;
use crate::tools::feeds::DoRssRead;
use crate::tools::github::DoGithubSearch;
use crate::tools::trait_def::{Tool, ToolContext, ToolError, ToolResult};

const MAX_FETCH_BYTES: usize = 2 * 1024 * 1024;
const MAX_TEXT_CHARS: usize = 24_000;
const MAX_SEARCH_RESULTS: usize = 10;

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
        .redirect(reqwest::redirect::Policy::limited(5));
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

// ── html helpers ───────────────────────────────────────────────────────────

fn html_to_text(html: &str) -> String {
    let re_script = Regex::new(r"(?is)<script[^>]*>.*?</script>").unwrap();
    let re_style = Regex::new(r"(?is)<style[^>]*>.*?</style>").unwrap();
    let re_tags = Regex::new(r"(?is)<[^>]+>").unwrap();
    let re_ws = Regex::new(r"[ \t]+\n").unwrap();
    let re_blank = Regex::new(r"\n{3,}").unwrap();
    let re_ent = Regex::new(r"&(#x?[0-9a-fA-F]+|\w+);").unwrap();

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
    s = s
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");
    s = re_ent.replace_all(&s, " ").into_owned();
    s = re_ws.replace_all(&s, "\n").into_owned();
    s = re_blank.replace_all(&s, "\n\n").into_owned();
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max).collect();
    format!("{head}\n\n…[truncated, {max} chars]")
}

fn html_entities_basic(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&nbsp;", " ")
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

        for url in &targets {
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                return Ok(ToolResult::err(
                    "",
                    format!("url must start with http:// or https://: {url}"),
                ));
            }
            if let Some(ban) = is_blocked_url(url) {
                return Ok(ToolResult::err(
                    "",
                    format!("refusing to fetch private/metadata URL ({ban}): {url}"),
                ));
            }
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

fn is_blocked_url(url: &str) -> Option<&'static str> {
    let lower = url.to_ascii_lowercase();
    for ban in [
        "://127.",
        "://localhost",
        "://0.0.0.0",
        "://[::1]",
        "://10.",
        "://192.168.",
        "://169.254.",
        "metadata.google",
        "169.254.169.254",
    ] {
        if lower.contains(ban) {
            return Some(ban);
        }
    }
    None
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
        if is_blocked_url(&abs_s).is_some() {
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
            html_entities_basic(&re_tags.replace_all(label_raw, " "))
                .split_whitespace()
                .collect::<Vec<_>>()
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
        let body = resp.text().await.unwrap_or_default();
        let snippet: String = body.chars().take(300).collect();
        return Err(format!(
            "{label} HTTP {status} for {final_url} ({net})\n{snippet}"
        ));
    }

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("{label} read body: {e}"))?;
    if bytes.len() > MAX_FETCH_BYTES {
        return Err(format!(
            "{label} response too large ({} bytes)",
            bytes.len()
        ));
    }

    let raw = String::from_utf8_lossy(&bytes);
    let is_html = ctype.contains("html") || raw.trim_start().starts_with('<');
    let links = if is_html {
        extract_candidate_links(&raw, &final_url, max_links)
    } else {
        vec![]
    };
    let text = if is_html {
        html_to_text(&raw)
    } else {
        raw.into_owned()
    };
    let text = truncate_chars(text.trim(), max_chars);

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
// (site: filetype: intitle: -exclude "phrase" OR), locking market/language via
// mkt/setlang/cc, and parsing only real organic <li class="b_algo"> results —
// decoding Bing's /ck/a redirect links back to the real destination URL.

const BING_SEARCH_URL: &str = "https://www.bing.com/search";
const BING_TIMEOUT: Duration = Duration::from_secs(20);
const BING_MAX_RETRIES: u32 = 2;
const BING_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36";

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

/// Strip HTML tags + decode basic entities from a Bing result fragment.
fn bing_clean(s: &str, tag_re: &Regex) -> String {
    html_entities_basic(&tag_re.replace_all(s, "")).trim().to_string()
}

/// Parse Bing's SERP HTML into clean organic [`SearchHit`]s.
fn bing_parse_html(html: &str, limit: usize) -> Vec<SearchHit> {
    let title_re = Regex::new(r#"<h2[^>]*>.*?<a[^>]*href="([^"]+)"[^>]*>(.*?)</a>"#).unwrap();
    let snip_re = Regex::new(r#"<p[^>]*>(.*?)</p>"#).unwrap();
    let tag_re = Regex::new(r"<[^>]+>").unwrap();

    let mut hits = Vec::new();
    // Split on the organic-result marker. The segment before the first
    // `<li class="b_algo"` is boilerplate; every later segment begins a result
    // block. (Rust's regex crate has no look-ahead, so split instead.)
    let mut blocks = html.split(r#"<li class="b_algo""#);
    blocks.next(); // drop leading boilerplate

    for block in blocks {
        if hits.len() >= limit {
            break;
        }
        // A block runs until the next `<li class="b_algo"` or the closing
        // `</ol>` of the results list — whichever comes first.
        let b = block.split("</ol>").next().unwrap_or(block);

        let Some(caps) = title_re.captures(b) else {
            continue;
        };
        let href = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        let Some(url) = bing_decode_url(href) else {
            continue;
        };
        // Drop Bing's own pages and anything without a real title.
        if url.contains("bing.com") {
            continue;
        }
        let title = bing_clean(caps.get(2).map(|m| m.as_str()).unwrap_or(""), &tag_re);
        if title.is_empty() {
            continue;
        }
        let snippet = snip_re
            .captures(b)
            .map(|c| bing_clean(c.get(1).map(|m| m.as_str()).unwrap_or(""), &tag_re))
            .unwrap_or_default();

        hits.push(SearchHit {
            title,
            url,
            snippet,
            source: "bing".into(),
        });
    }
    hits
}

/// One Bing search request (no retry — see [`bing_search`]).
async fn bing_search_once(
    client: &Client,
    query: &str,
    limit: usize,
) -> Result<Vec<SearchHit>, String> {
    let count = limit.min(10).to_string();
    let resp = client
        .get(BING_SEARCH_URL)
        .query(&[
            ("q", query),
            ("count", count.as_str()),
            ("setlang", "en-US"),
            ("mkt", "en-US"),
            ("cc", "US"),
        ])
        .header("User-Agent", BING_UA)
        .header("Accept-Language", "en-US,en;q=0.9")
        .timeout(BING_TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("Bing HTTP: {e}"))?;

    let status = resp.status();
    let html = resp.text().await.map_err(|e| format!("Bing body: {e}"))?;
    if !status.is_success() {
        return Err(format!("Bing HTTP {status}"));
    }
    let hits = bing_parse_html(&html, limit);
    if hits.is_empty() {
        Err("Bing: no organic results".into())
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
                Ok(ToolResult::ok(format!(
                    "Query: {query}\nNetwork: {net}\nSearch failed: {e}{hint}\n\
                     If you already have a URL, use do_web_fetch."
                )))
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
    let text = resp
        .text()
        .await
        .map_err(|e| format!("Jina Reader body: {e}"))?;
    if !status.is_success() {
        return Err(format!("Jina Reader HTTP {status}"));
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

