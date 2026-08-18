//! GitHub search via the public REST API — no token needed, rate-limited.
//! Agent-Reach style "installed & usable" platform tool.

use super::trait_def::{Tool, ToolContext, ToolError, ToolResult};
use super::web::{proxy_note, proxy_configured_url, web_client_for_args};
use crate::agent::stream::StreamEvent;
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct DoGithubSearch;

impl DoGithubSearch {
    pub fn new() -> Self {
        Self
    }
}

impl Default for DoGithubSearch {
    fn default() -> Self {
        Self::new()
    }
}

/// Percent-encode a GitHub search query (spaces -> '+', keep safe chars).
fn encode_q(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn progress(ctx: &ToolContext, chunk: String) {
    let _ = ctx.sender.send(StreamEvent::ToolProgress {
        id: ctx.tool_call_id.clone(),
        chunk,
    });
}

#[async_trait]
impl Tool for DoGithubSearch {
    fn name(&self) -> &str {
        "do_github_search"
    }

    fn description(&self) -> &str {
        "Search GitHub public repositories, issues or users via the public REST API (no token needed). Returns name, URL, description, stars and language for repos; title/state for issues. Use for code, projects and open-source discovery."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "GitHub search query, e.g. 'rust async runtime', or qualified like 'language:rust stars:>1000'"
                },
                "type": {
                    "type": "string",
                    "enum": ["repos", "issues", "users"],
                    "default": "repos",
                    "description": "What to search: repositories, issues or users"
                },
                "per_page": {
                    "type": "integer",
                    "default": 8,
                    "maximum": 20,
                    "description": "Number of results"
                },
                "use_proxy": {
                    "type": "boolean",
                    "default": false,
                    "description": "Route through the local proxy if direct access fails"
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolResult, ToolError> {
        let query = args
            .get("query")
            .and_then(|q| q.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if query.is_empty() {
            return Err(ToolError::MissingParameter("query".into()));
        }
        let kind = args
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("repos")
            .to_string();
        let per_page = args
            .get("per_page")
            .and_then(|p| p.as_u64())
            .unwrap_or(8)
            .min(20) as usize;
        let use_proxy = args
            .get("use_proxy")
            .and_then(|p| p.as_bool())
            .unwrap_or_else(|| proxy_configured_url().is_some());

        let (client, proxy) = web_client_for_args(&args)?;
        progress(ctx, format!("  ▸ GitHub {kind} 搜索 …\n"));

        let api = match kind.as_str() {
            "issues" => "issues",
            "users" => "users",
            _ => "repositories",
        };
        let url = format!(
            "https://api.github.com/search/{api}?q={}&per_page={per_page}&sort=stars&order=desc",
            encode_q(&query)
        );

        let resp = client
            .get(&url)
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "dscode")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .map_err(|e| ToolError::Internal(format!("GitHub API HTTP: {e}")))?;

        let status = resp.status();
        let v: Value = resp
            .json()
            .await
            .map_err(|e| ToolError::Internal(format!("GitHub API parse: {e}")))?;

        if !status.is_success() {
            let msg = v
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown");
            let hint = if status.as_u16() == 403 {
                " (rate limited — unauthenticated GitHub allows ~10 req/min)"
            } else {
                ""
            };
            progress(ctx, format!("  ✗ GitHub HTTP {status}: {msg}{hint}\n"));
            return Ok(ToolResult::err(
                format!("GitHub HTTP {status}: {msg}{hint}"),
                format!("GitHub HTTP {status}: {msg}{hint}"),
            ));
        }

        let items = v
            .get("items")
            .and_then(|i| i.as_array())
            .cloned()
            .unwrap_or_default();
        let total = v.get("total_count").and_then(|t| t.as_u64()).unwrap_or(0);

        let mut lines: Vec<String> = Vec::new();
        match kind.as_str() {
            "issues" => {
                for it in items.iter().take(per_page) {
                    let title = it
                        .get("title")
                        .and_then(|t| t.as_str())
                        .unwrap_or("")
                        .trim();
                    let url = it
                        .get("html_url")
                        .and_then(|t| t.as_str())
                        .unwrap_or("")
                        .trim();
                    if title.is_empty() {
                        continue;
                    }
                    let state = it.get("state").and_then(|t| t.as_str()).unwrap_or("");
                    let comments = it.get("comments").and_then(|t| t.as_u64()).unwrap_or(0);
                    lines.push(format!(
                        "- [{state}] {title}\n  {url}\n  💬 {comments} comments\n"
                    ));
                }
            }
            "users" => {
                for it in items.iter().take(per_page) {
                    let login = it
                        .get("login")
                        .and_then(|t| t.as_str())
                        .unwrap_or("")
                        .trim();
                    let url = it
                        .get("html_url")
                        .and_then(|t| t.as_str())
                        .unwrap_or("")
                        .trim();
                    if login.is_empty() {
                        continue;
                    }
                    lines.push(format!("- {login}\n  {url}\n"));
                }
            }
            _ => {
                for it in items.iter().take(per_page) {
                    let full = it
                        .get("full_name")
                        .and_then(|t| t.as_str())
                        .unwrap_or("")
                        .trim();
                    let url = it
                        .get("html_url")
                        .and_then(|t| t.as_str())
                        .unwrap_or("")
                        .trim();
                    if full.is_empty() {
                        continue;
                    }
                    let desc = it
                        .get("description")
                        .and_then(|t| t.as_str())
                        .unwrap_or("")
                        .trim();
                    let stars = it
                        .get("stargazers_count")
                        .and_then(|t| t.as_u64())
                        .unwrap_or(0);
                    let lang = it.get("language").and_then(|t| t.as_str()).unwrap_or("");
                    let mut line = format!("- ⭐ {stars} · {full}\n  {url}");
                    if !lang.is_empty() {
                        line.push_str(&format!("\n  🗣 {lang}"));
                    }
                    if !desc.is_empty() {
                        line.push_str(&format!(
                            "\n  {}",
                            desc.chars().take(140).collect::<String>()
                        ));
                    }
                    lines.push(line + "\n");
                }
            }
        }

        progress(ctx, format!("  ✓ GitHub {kind} · {} 条\n", lines.len()));
        let out = format!(
            "Query: {query}\nNetwork: {}\nSources:\n✓ GitHub {kind}: {} (total {total})\nResults ({}):\n{}\n[github/{kind}]",
            proxy_note(&proxy, args.get("use_proxy").and_then(|p| p.as_bool())),
            lines.len(),
            lines.len(),
            lines.join("\n")
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
    fn encode_q_encodes_spaces_and_symbols() {
        assert_eq!(encode_q("rust async"), "rust+async");
        assert_eq!(encode_q("language:rust stars:>1000"), "language%3Arust+stars%3A%3E1000");
    }

    #[tokio::test]
    #[ignore = "live network"]
    async fn live_github_search_smoke() {
        let tool = DoGithubSearch::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = ToolContext::simple(
            std::env::temp_dir(),
            "t",
            "github",
            tx,
            Arc::new(SafetyGuard::new(&[], true)),
        );
        let use_proxy = proxy_configured_url().is_some();
        let r = tool
            .execute(
                serde_json::json!({
                    "query": "rust async runtime",
                    "per_page": 3,
                    "use_proxy": use_proxy
                }),
                &ctx,
            )
            .await
            .expect("exec");
        println!(
            "github success={} head={}",
            r.success,
            r.output.chars().take(400).collect::<String>()
        );
        assert!(r.success, "github search failed: {}", r.error.unwrap_or_default());
        assert!(r.output.contains("[github/repos]"));
        assert!(r.output.contains("http"));
    }
}
