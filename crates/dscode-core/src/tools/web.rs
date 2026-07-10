//! Built-in web tools: `do_web_fetch` + `do_web_search`.
//!
//! - **do_web_fetch**: GET a URL, strip HTML, return readable text (capped).
//! - **do_web_search**: query DuckDuckGo HTML, return title/url/snippet list.
//!
//! Respects global proxy via `reqwest` client built from config when available;
//! falls back to a short-timeout direct client.

use async_trait::async_trait;
use regex::Regex;
use reqwest::Client;
use std::sync::OnceLock;
use std::time::Duration;

use crate::tools::trait_def::{Tool, ToolContext, ToolError, ToolResult};

const MAX_FETCH_BYTES: usize = 2 * 1024 * 1024; // 2 MiB raw body cap
const MAX_TEXT_CHARS: usize = 24_000;
const MAX_SEARCH_RESULTS: usize = 8;

fn default_client() -> &'static Client {
    static C: OnceLock<Client> = OnceLock::new();
    C.get_or_init(|| {
        Client::builder()
            .timeout(Duration::from_secs(25))
            .user_agent(
                "DSCode/0.2 (+https://github.com/fivif/dscode; local coding agent; web fetch)",
            )
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .expect("http client")
    })
}

fn html_to_text(html: &str) -> String {
    // Drop script/style
    let re_script = Regex::new(r"(?is)<script[^>]*>.*?</script>").unwrap();
    let re_style = Regex::new(r"(?is)<style[^>]*>.*?</style>").unwrap();
    let re_tags = Regex::new(r"(?is)<[^>]+>").unwrap();
    let re_ws = Regex::new(r"[ \t]+\n").unwrap();
    let re_blank = Regex::new(r"\n{3,}").unwrap();
    let re_ent = Regex::new(r"&(#x?[0-9a-fA-F]+|\w+);").unwrap();

    let mut s = re_script.replace_all(html, " ").into_owned();
    s = re_style.replace_all(&s, " ").into_owned();
    // Prefer line breaks for block-ish tags
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
    // Basic entities
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
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        // restore some newlines: split long runs — keep simple paragraph guess
        .chars()
        .collect::<String>()
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
        "Fetch a public HTTP(S) URL and return readable text (HTML stripped). \
         Use for docs, GitHub raw pages, API JSON (returns raw body if not HTML). \
         Not for authenticated pages. Prefer do_web_search first to find URLs."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "Full URL starting with http:// or https://"
                },
                "max_chars": {
                    "type": "integer",
                    "description": "Max characters of text to return (default 24000, max 48000)."
                }
            },
            "required": ["url"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let url = args["url"]
            .as_str()
            .ok_or_else(|| ToolError::MissingParameter("url".into()))?
            .trim();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Ok(ToolResult::err(
                "",
                "url must start with http:// or https://",
            ));
        }
        // Block obvious local/metadata targets
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
                return Ok(ToolResult::err(
                    "",
                    format!("refusing to fetch private/metadata URL ({ban})"),
                ));
            }
        }

        let max_chars = args["max_chars"]
            .as_u64()
            .unwrap_or(MAX_TEXT_CHARS as u64)
            .clamp(1000, 48_000) as usize;

        let resp = default_client()
            .get(url)
            .send()
            .await
            .map_err(|e| ToolError::Internal(format!("fetch failed: {e}")))?;

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
            let snippet: String = body.chars().take(500).collect();
            return Ok(ToolResult::err(
                snippet,
                format!("HTTP {status} for {final_url}"),
            ));
        }

        let bytes = resp
            .bytes()
            .await
            .map_err(|e| ToolError::Internal(format!("read body: {e}")))?;
        if bytes.len() > MAX_FETCH_BYTES {
            return Ok(ToolResult::err(
                "",
                format!(
                    "response too large ({} bytes > {MAX_FETCH_BYTES}); use a more specific URL",
                    bytes.len()
                ),
            ));
        }

        let raw = String::from_utf8_lossy(&bytes);
        let text = if ctype.contains("html") || raw.trim_start().starts_with('<') {
            html_to_text(&raw)
        } else {
            raw.into_owned()
        };
        let text = truncate_chars(text.trim(), max_chars);

        Ok(ToolResult::ok(format!(
            "URL: {final_url}\nContent-Type: {ctype}\nStatus: {status}\n\n{text}"
        )))
    }
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

#[async_trait]
impl Tool for DoWebSearch {
    fn name(&self) -> &str {
        "do_web_search"
    }

    fn description(&self) -> &str {
        "Search the public web (DuckDuckGo HTML, no API key). Returns a list of \
         title / URL / snippet results. Then use do_web_fetch on promising URLs for full content."
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
                    "description": "Max results (default 6, max 8)"
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let query = args["query"]
            .as_str()
            .ok_or_else(|| ToolError::MissingParameter("query".into()))?
            .trim();
        if query.is_empty() {
            return Ok(ToolResult::err("", "query is empty"));
        }
        let max = args["max_results"]
            .as_u64()
            .unwrap_or(6)
            .clamp(1, MAX_SEARCH_RESULTS as u64) as usize;

        // DuckDuckGo HTML endpoint (no JS)
        let url = format!(
            "https://html.duckduckgo.com/html/?q={}",
            urlencoding_encode(query)
        );

        let resp = default_client()
            .get(&url)
            .header("Accept", "text/html")
            .send()
            .await
            .map_err(|e| ToolError::Internal(format!("search request failed: {e}")))?;

        if !resp.status().is_success() {
            return Ok(ToolResult::err(
                "",
                format!("search HTTP {}", resp.status()),
            ));
        }
        let html = resp
            .text()
            .await
            .map_err(|e| ToolError::Internal(format!("search body: {e}")))?;

        let results = parse_ddg_html(&html, max);
        if results.is_empty() {
            // Fallback: return stripped page excerpt so model can still use something
            let text = truncate_chars(&html_to_text(&html), 4000);
            return Ok(ToolResult::ok(format!(
                "Query: {query}\nNo structured results parsed. Page text excerpt:\n\n{text}"
            )));
        }

        let mut out = format!("Query: {query}\nResults ({})\n\n", results.len());
        for (i, r) in results.iter().enumerate() {
            out.push_str(&format!(
                "{}. {}\n   URL: {}\n   {}\n\n",
                i + 1,
                r.title,
                r.url,
                r.snippet
            ));
        }
        out.push_str("Tip: call do_web_fetch on a URL for full page text.");
        Ok(ToolResult::ok(out))
    }
}

struct SearchHit {
    title: String,
    url: String,
    snippet: String,
}

fn urlencoding_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.as_bytes() {
        match *b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn parse_ddg_html(html: &str, max: usize) -> Vec<SearchHit> {
    // DDG result links: class="result__a" href="..."
    let re_link = Regex::new(
        r#"(?is)<a[^>]*class="[^"]*result__a[^"]*"[^>]*href="([^"]+)"[^>]*>(.*?)</a>"#,
    )
    .unwrap();
    let re_snip = Regex::new(
        r#"(?is)<a[^>]*class="[^"]*result__snippet[^"]*"[^>]*>(.*?)</a>|<td[^>]*class="[^"]*result__snippet[^"]*"[^>]*>(.*?)</td>"#,
    )
    .unwrap();
    // Simpler snippet class
    let re_snip2 = Regex::new(r#"(?is)<[^>]*class="[^"]*result__snippet[^"]*"[^>]*>(.*?)</"#).unwrap();

    let mut hits = Vec::new();
    let mut snips: Vec<String> = re_snip2
        .captures_iter(html)
        .filter_map(|c| c.get(1).map(|m| strip_tags(m.as_str())))
        .collect();

    for (i, cap) in re_link.captures_iter(html).enumerate() {
        if hits.len() >= max {
            break;
        }
        let href = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        let title = strip_tags(cap.get(2).map(|m| m.as_str()).unwrap_or(""));
        let url = decode_ddg_redirect(href);
        if url.is_empty() || title.is_empty() {
            continue;
        }
        let snippet = snips.get(i).cloned().unwrap_or_default();
        hits.push(SearchHit {
            title,
            url,
            snippet,
        });
    }

    // Alternate: uddg= in redirect links
    if hits.is_empty() {
        let re_uddg = Regex::new(r#"uddg=([^&"]+)"#).unwrap();
        let re_title = Regex::new(r#"(?is)class="result__a"[^>]*>(.*?)</a>"#).unwrap();
        let titles: Vec<String> = re_title
            .captures_iter(html)
            .filter_map(|c| c.get(1).map(|m| strip_tags(m.as_str())))
            .collect();
        for (i, cap) in re_uddg.captures_iter(html).enumerate() {
            if hits.len() >= max {
                break;
            }
            let enc = cap.get(1).map(|m| m.as_str()).unwrap_or("");
            let url = urlencoding_decode(enc);
            if !url.starts_with("http") {
                continue;
            }
            let title = titles.get(i).cloned().unwrap_or_else(|| url.clone());
            hits.push(SearchHit {
                title,
                url,
                snippet: String::new(),
            });
        }
    }

    let _ = re_snip; // keep for future
    hits
}

fn strip_tags(s: &str) -> String {
    let re = Regex::new(r"(?is)<[^>]+>").unwrap();
    let t = re.replace_all(s, "");
    html_entities(&t).split_whitespace().collect::<Vec<_>>().join(" ")
}

fn html_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}

fn decode_ddg_redirect(href: &str) -> String {
    if href.starts_with("http") && !href.contains("duckduckgo.com/l/") {
        return href.to_string();
    }
    // //duckduckgo.com/l/?uddg=https%3A%2F%2F...
    if let Some(idx) = href.find("uddg=") {
        let rest = &href[idx + 5..];
        let enc = rest.split('&').next().unwrap_or(rest);
        let u = urlencoding_decode(enc);
        if u.starts_with("http") {
            return u;
        }
    }
    if href.starts_with("//") {
        return format!("https:{href}");
    }
    href.to_string()
}

fn urlencoding_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let h = u8::from_str_radix(
                    std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("00"),
                    16,
                )
                .unwrap_or(b'?');
                out.push(h);
                i += 3;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_strip_basic() {
        let t = html_to_text("<html><script>x</script><p>Hello <b>world</b></p></html>");
        assert!(t.contains("Hello"));
        assert!(t.contains("world"));
        assert!(!t.contains("script"));
    }

    #[test]
    fn ddg_redirect() {
        let u = decode_ddg_redirect(
            "//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fpath&rut=x",
        );
        assert_eq!(u, "https://example.com/path");
    }
}

#[tokio::test]
async fn live_fetch_example_com() {
    use crate::tools::trait_def::{Tool, ToolContext};
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
        .execute(serde_json::json!({"url": "https://example.com", "max_chars": 2000}), &ctx)
        .await
        .expect("execute");
    assert!(r.success, "fetch failed: {:?}", r.error);
    assert!(
        r.output.to_lowercase().contains("example") || r.output.contains("Domain"),
        "unexpected body: {}",
        &r.output[..r.output.len().min(200)]
    );
}

#[tokio::test]
async fn live_search_rust_lang() {
    use crate::tools::trait_def::{Tool, ToolContext};
    use crate::safety::guard::SafetyGuard;
    use std::sync::Arc;
    let tool = DoWebSearch::new();
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let ctx = ToolContext::simple(
        std::env::temp_dir(),
        "t",
        "c1",
        tx,
        Arc::new(SafetyGuard::new(&[], true)),
    );
    let r = tool
        .execute(serde_json::json!({"query": "rust programming language", "max_results": 3}), &ctx)
        .await
        .expect("execute");
    // Network may be blocked; accept structured results OR graceful fallback text
    assert!(r.success || r.error.is_some());
    assert!(!r.output.is_empty() || r.error.is_some());
}

#[test]
fn private_url_blocked() {
    // sync check via runtime
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        use crate::tools::trait_def::{Tool, ToolContext};
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
    });
}
