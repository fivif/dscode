//! RSS / Atom reader — parse any feed URL into a clean item list.
//! Agent-Reach style "installed & usable" platform tool.

use super::trait_def::{Tool, ToolContext, ToolError, ToolResult};
use super::web::{proxy_note, proxy_configured_url, web_client_for_args};
use crate::agent::stream::StreamEvent;
use async_trait::async_trait;
use regex::Regex;
use serde_json::{json, Value};

pub struct DoRssRead;

impl DoRssRead {
    pub fn new() -> Self {
        Self
    }
}

impl Default for DoRssRead {
    fn default() -> Self {
        Self::new()
    }
}

fn strip_tags(s: &str) -> String {
    let re = Regex::new(r"(?is)<[^>]+>").unwrap();
    re.replace_all(s, " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parse a feed body. Returns (items, feed_title) where each item is
/// (title, link, date, summary). Handles Atom first, then RSS 2.0.
fn parse_feed(body: &str, limit: usize) -> (Vec<(String, String, String, String)>, String) {
    let re_title = Regex::new(r"(?is)<title[^>]*>(.*?)</title>").unwrap();
    let feed_title = re_title
        .captures(body)
        .and_then(|c| c.get(1))
        .map(|m| strip_tags(m.as_str()).chars().take(80).collect())
        .unwrap_or_default();

    // ── Atom entries (link carries href attribute) ──
    if body.contains("<entry") {
        let re_entry = Regex::new(r"(?is)<entry\b.*?</entry>").unwrap();
        let re_link = Regex::new(r#"(?is)<link[^>]*href\s*=\s*["']([^"']+)["']"#).unwrap();
        let re_pub = Regex::new(r"(?is)<(?:published|updated)[^>]*>(.*?)</(?:published|updated)>").unwrap();
        let re_sum = Regex::new(r"(?is)<(?:summary|content)[^>]*>(.*?)</(?:summary|content)>").unwrap();
        let mut items = Vec::new();
        for cap in re_entry.captures_iter(body) {
            if items.len() >= limit {
                break;
            }
            let b = cap.get(0).map(|m| m.as_str()).unwrap_or("");
            let title = re_title
                .captures(b)
                .and_then(|c| c.get(1))
                .map(|m| strip_tags(m.as_str()))
                .unwrap_or_default();
            let link = re_link
                .captures(b)
                .and_then(|c| c.get(1))
                .map(|m| m.as_str().to_string())
                .unwrap_or_default();
            let date = re_pub
                .captures(b)
                .and_then(|c| c.get(1))
                .map(|m| strip_tags(m.as_str()))
                .unwrap_or_default();
            let sum = re_sum
                .captures(b)
                .and_then(|c| c.get(1))
                .map(|m| strip_tags(m.as_str()))
                .unwrap_or_default();
            if title.is_empty() || link.is_empty() {
                continue;
            }
            items.push((title, link, date, sum));
        }
        if !items.is_empty() {
            return (items, feed_title);
        }
    }

    // ── RSS 2.0 items ──
    let re_item = Regex::new(r"(?is)<item\b.*?</item>").unwrap();
    let re_link = Regex::new(r"(?is)<link[^>]*>(.*?)</link>").unwrap();
    let re_pub = Regex::new(r"(?is)<pubDate[^>]*>(.*?)</pubDate>").unwrap();
    let re_desc = Regex::new(r"(?is)<description[^>]*>(.*?)</description>").unwrap();
    let mut items = Vec::new();
    for cap in re_item.captures_iter(body) {
        if items.len() >= limit {
            break;
        }
        let b = cap.get(0).map(|m| m.as_str()).unwrap_or("");
        let title = re_title
            .captures(b)
            .and_then(|c| c.get(1))
            .map(|m| strip_tags(m.as_str()))
            .unwrap_or_default();
        let link = re_link
            .captures(b)
            .and_then(|c| c.get(1))
            .map(|m| strip_tags(m.as_str()))
            .unwrap_or_default();
        let date = re_pub
            .captures(b)
            .and_then(|c| c.get(1))
            .map(|m| strip_tags(m.as_str()))
            .unwrap_or_default();
        let sum = re_desc
            .captures(b)
            .and_then(|c| c.get(1))
            .map(|m| strip_tags(m.as_str()))
            .unwrap_or_default();
        if title.is_empty() || link.is_empty() {
            continue;
        }
        items.push((title, link, date, sum));
    }
    (items, feed_title)
}

#[async_trait]
impl Tool for DoRssRead {
    fn name(&self) -> &str {
        "do_rss_read"
    }

    fn description(&self) -> &str {
        "Fetch any RSS or Atom feed URL and return its recent items (title, link, date, summary). Use for blogs, news sites, release notes and podcasts."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "Full RSS/Atom feed URL, e.g. https://example.com/feed.xml"
                },
                "max_items": {
                    "type": "integer",
                    "default": 10,
                    "maximum": 30,
                    "description": "Number of items to return"
                },
                "use_proxy": {
                    "type": "boolean",
                    "default": false,
                    "description": "Route through the local proxy if direct access fails"
                }
            },
            "required": ["url"]
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolResult, ToolError> {
        let url = args
            .get("url")
            .and_then(|u| u.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(ToolError::InvalidParameter {
                name: "url".into(),
                reason: "must be http(s) URL".into(),
            });
        }
        let max_items = args
            .get("max_items")
            .and_then(|m| m.as_u64())
            .unwrap_or(10)
            .min(30) as usize;

        let (client, proxy) = web_client_for_args(&args)?;
        let _ = ctx.sender.send(StreamEvent::ToolProgress {
            id: ctx.tool_call_id.clone(),
            chunk: format!("  ▸ RSS 抓取 {url} …\n"),
        });

        let resp = client
            .get(&url)
            .send()
            .await
            .map_err(|e| ToolError::Internal(format!("RSS fetch: {e}")))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| ToolError::Internal(format!("RSS body: {e}")))?;
        if !status.is_success() {
            return Ok(ToolResult::err(
                format!("RSS HTTP {status}"),
                format!("RSS HTTP {status}"),
            ));
        }
        if !body.contains("<item") && !body.contains("<entry") {
            return Ok(ToolResult::err(
                format!("No RSS/Atom items found at {url} (HTTP {status}). This may be an HTML page, not a feed."),
                format!("No RSS/Atom items found at {url} (HTTP {status}). This may be an HTML page, not a feed."),
            ));
        }

        let (items, feed_title) = parse_feed(&body, max_items);
        let mut lines = Vec::new();
        for (i, (t, l, d, s)) in items.iter().enumerate() {
            let mut line = format!("{}. {t}\n   {l}", i + 1);
            if !d.is_empty() {
                line.push_str(&format!("\n   📅 {d}"));
            }
            if !s.is_empty() {
                line.push_str(&format!("\n   {}", s.chars().take(140).collect::<String>()));
            }
            lines.push(line);
        }

        let head = if feed_title.is_empty() { url.clone() } else { feed_title };
        let out = format!(
            "Feed: {head}\nNetwork: {}\nSources:\n✓ RSS/Atom: {} items\nItems ({}):\n{}\n[rss]",
            proxy_note(&proxy, args.get("use_proxy").and_then(|p| p.as_bool())),
            lines.len(),
            lines.len(),
            lines.join("\n\n")
        );
        Ok(ToolResult::ok(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::safety::guard::SafetyGuard;
    use std::sync::Arc;

    #[test]
    fn parse_rss_items() {
        let body = r#"<?xml version="1.0"?>
<rss version="2.0"><channel>
<title>Test Feed</title>
<item><title>Hello Rust</title><link>https://example.com/a</link>
<pubDate>Mon, 01 Jan 2024 00:00:00 GMT</pubDate>
<description>An &lt;em&gt;intro&lt;/em&gt; post.</description></item>
<item><title>Second</title><link>https://example.com/b</link></item>
</channel></rss>"#;
        let (items, title) = parse_feed(body, 10);
        assert_eq!(title, "Test Feed");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].0, "Hello Rust");
        assert_eq!(items[0].1, "https://example.com/a");
        assert!(items[0].3.contains("intro"));
        assert_eq!(items[1].0, "Second");
    }

    #[test]
    fn parse_atom_entries() {
        let body = r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom">
<title>Atom Feed</title>
<entry><title>Post One</title>
<link href="https://example.com/1"/>
<published>2024-02-01T10:00:00Z</published>
<summary>Summary text here.</summary></entry>
<entry><title>Post Two</title>
<link href="https://example.com/2"/></entry>
</feed>"#;
        let (items, title) = parse_feed(body, 10);
        assert_eq!(title, "Atom Feed");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].1, "https://example.com/1");
        assert!(items[0].2.contains("2024"));
    }

    #[tokio::test]
    #[ignore = "live network"]
    async fn live_rss_read_smoke() {
        let tool = DoRssRead::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = ToolContext::simple(
            std::env::temp_dir(),
            "t",
            "rss",
            tx,
            Arc::new(SafetyGuard::new(&[], true)),
        );
        let use_proxy = proxy_configured_url().is_some();
        let r = tool
            .execute(
                serde_json::json!({
                    "url": "https://blog.rust-lang.org/feed.xml",
                    "max_items": 5,
                    "use_proxy": use_proxy
                }),
                &ctx,
            )
            .await
            .expect("exec");
        println!(
            "rss success={} head={}",
            r.success,
            r.output.chars().take(400).collect::<String>()
        );
        assert!(r.success, "rss read failed: {}", r.error.unwrap_or_default());
        assert!(r.output.contains("[rss]"));
    }
}
