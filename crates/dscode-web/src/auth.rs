//! API authentication for the web shell.
//!
//! Every `/api/*` route is gated by a per-startup bearer token (see
//! `main.rs::api_router`). Without it, any web page the user's browser visits
//! could `fetch('http://127.0.0.1:8080/api/invoke')` and drive the agent —
//! including reading `perm_…` ids off `/api/events` and approving its own
//! dangerous-command prompt (see the CRITICAL finding in the shells review).
//!
//! Two transports are accepted because `EventSource` cannot set headers:
//! - `Authorization: Bearer <token>` (normal `fetch`)
//! - `?token=<token>` (SSE; also handy when pasting a URL)
//!
//! The query form leaks into browser history / server logs, so it is a
//! fallback, not the preferred form.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

/// Shared secret for the `/api/*` surface.
#[derive(Clone)]
pub struct WebAuth {
    token: Arc<str>,
}

impl WebAuth {
    pub fn new(token: String) -> Self {
        Self {
            token: Arc::from(token.as_str()),
        }
    }
}

/// Middleware: reject any request that does not carry the token.
pub async fn require_token(State(auth): State<WebAuth>, req: Request, next: Next) -> Response {
    match provided_token(&req) {
        Some(t) if constant_time_eq(t.as_bytes(), auth.token.as_bytes()) => next.run(req).await,
        Some(_) => unauthorized("invalid token"),
        None => unauthorized("missing token"),
    }
}

fn unauthorized(reason: &str) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({
            "error": reason,
            "hint": "pass the DSCODE_WEB_TOKEN value as `Authorization: Bearer <token>` or `?token=<token>`",
        })),
    )
        .into_response()
}

/// Extract the token from the `Authorization` header, falling back to the
/// `token` query parameter (needed for `EventSource`).
fn provided_token(req: &Request) -> Option<String> {
    if let Some(t) = bearer_token(req) {
        return Some(t);
    }
    query_param(req.uri().query()?, "token")
}

fn bearer_token(req: &Request) -> Option<String> {
    let raw = req.headers().get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, value) = raw.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn query_param(query: &str, key: &str) -> Option<String> {
    for pair in query.split('&') {
        let Some((k, v)) = pair.split_once('=') else {
            continue;
        };
        if k == key {
            return Some(percent_decode(v));
        }
    }
    None
}

/// Minimal percent-decoder for query values (the `url` crate is not a
/// dependency of this shell). `+` is treated as a space, matching form encoding.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                match (hi, lo) {
                    (Some(h), Some(l)) => {
                        out.push((h * 16 + l) as u8);
                        i += 3;
                    }
                    _ => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Compare without an early exit on the first differing byte. Not a formal
/// constant-time guarantee (the length is compared first, and the compiler is
/// not constrained), but it removes the trivial byte-by-byte timing signal.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Whether a `host:port` bind address stays on this machine.
///
/// Anything that cannot be parsed as a loopback IP (or `localhost`) counts as
/// exposed — fail closed, so an unparsable hostname requires an explicit token.
pub fn is_loopback_addr(addr: &str) -> bool {
    let host = match addr.rsplit_once(':') {
        Some((h, _)) => h,
        None => addr,
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<std::net::IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_detection() {
        assert!(is_loopback_addr("127.0.0.1:8080"));
        assert!(is_loopback_addr("localhost:8080"));
        assert!(is_loopback_addr("[::1]:8080"));
        assert!(!is_loopback_addr("0.0.0.0:8080"));
        assert!(!is_loopback_addr("192.168.1.5:8080"));
        assert!(!is_loopback_addr("example.com:8080"));
    }

    #[test]
    fn query_token_decoding() {
        assert_eq!(query_param("a=1&token=abc-123", "token").as_deref(), Some("abc-123"));
        assert_eq!(query_param("token=a%2Bb", "token").as_deref(), Some("a+b"));
        assert_eq!(query_param("token=a+b", "token").as_deref(), Some("a b"));
        assert_eq!(query_param("other=1", "token"), None);
    }

    #[test]
    fn constant_time_compare() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }
}
