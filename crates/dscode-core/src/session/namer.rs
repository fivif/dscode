//! LLM-backed session naming.
//!
//! Asked once per session, right after the first user message, for a short
//! sidebar title. It never owns the session's name: the deterministic title
//! from [`crate::session::manager::SessionManager::derive_title_from_message`]
//! is written first and stays if anything here fails, so a provider that is
//! slow, unreachable, or chatty can only ever leave the session with the title
//! it already had.
//!
//! Naming is deliberately a separate, cheaper model call (the router model) —
//! one line of output must not cost the same as the turn it names.

use std::time::Duration;

use tracing::{debug, warn};

use crate::providers::trait_def::{LlmProvider, Message, MessageContent};

/// Longest title we accept from the model. Sidebar rows are a single line; a
/// "title" much longer than this is a summary, and a summary is what the
/// deterministic namer already produces.
const MAX_TITLE_CHARS: usize = 24;

/// How much of the first message the model sees: enough to name the task, not
/// enough to turn the request into a summarisation job.
const MAX_SOURCE_CHARS: usize = 1200;

/// Wall-clock ceiling for the naming call. The namer runs beside the turn, but
/// a hung provider still occupies a connection and a DB read until it returns.
const NAMER_TIMEOUT: Duration = Duration::from_secs(15);

/// Prompt asking the model for a one-line session title.
pub fn title_prompt(user_message: &str) -> String {
    let excerpt: String = user_message.trim().chars().take(MAX_SOURCE_CHARS).collect();
    format!(
        "Give this chat session a short title.\n\
         Rules:\n\
         - Reply with the title only. No quotes, no label, no explanation.\n\
         - One line, at most {MAX_TITLE_CHARS} characters.\n\
         - Write it in the same language as the message.\n\
         - Name the concrete task, not the conversation itself.\n\n\
         Message:\n{excerpt}"
    )
}

/// Ask `provider` for a title derived from `user_message`.
///
/// `None` on every failure path — provider error, timeout, unusable output —
/// so the caller keeps the deterministic title. One attempt only: a namer that
/// needs a retry is slower than the turn whose session it is naming.
pub async fn suggest_title(provider: &dyn LlmProvider, user_message: &str) -> Option<String> {
    let request = vec![Message {
        content: MessageContent::Text(title_prompt(user_message)),
        ..Default::default()
    }];

    let response = match tokio::time::timeout(NAMER_TIMEOUT, provider.chat(request, Vec::new())).await
    {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            warn!(error = %e, "session namer: provider call failed, keeping derived title");
            return None;
        }
        Err(_) => {
            warn!("session namer: timed out, keeping derived title");
            return None;
        }
    };

    let title = sanitize_title(&response.content);
    if title.is_none() {
        debug!(reply = %response.content, "session namer: unusable reply, keeping derived title");
    }
    title
}

/// Turn raw model output into a usable title, or `None` when it is junk.
///
/// `None` is not an error to paper over: it sends the caller to the
/// deterministic title. A model that answers a naming request with a paragraph
/// or a preamble ("Certainly! Here's a title:") must not become a session
/// name, so anything that does not look like a bare title is refused rather
/// than trimmed into one.
pub fn sanitize_title(raw: &str) -> Option<String> {
    // Models wrap the answer, add a label, or say a sentence before it. Pick
    // the first line that is not one of those things.
    let candidate = raw
        .lines()
        .map(strip_decorations)
        .find(|line| !line.is_empty() && !looks_like_preamble(line))?;

    let collapsed: String = candidate.split_whitespace().collect::<Vec<_>>().join(" ");
    // More than one sentence is a reply about the title, not the title.
    if sentence_breaks(&collapsed) > 1 {
        return None;
    }
    let collapsed = collapsed
        .trim_end_matches(|c| matches!(c, '。' | '！' | '？' | '.' | '!' | '?' | '，' | ',' | '；' | ';'))
        .trim();
    if collapsed.is_empty() {
        return None;
    }
    // A paragraph on one line is still a paragraph.
    if collapsed.chars().count() > MAX_TITLE_CHARS * 3 {
        return None;
    }

    let truncated = collapsed.chars().count() > MAX_TITLE_CHARS;
    let mut title: String = collapsed.chars().take(MAX_TITLE_CHARS).collect();
    if truncated {
        title.push('…');
    }
    // Punctuation, an emoji or a stray label on its own carries no information.
    if !title.chars().any(char::is_alphanumeric) {
        return None;
    }
    Some(title)
}

/// Strip the wrappers a model puts around a one-line answer: code fences,
/// a surrounding quote pair, a leading `标题：` / `Title:` label, a list marker.
fn strip_decorations(line: &str) -> String {
    let mut text = line.trim().trim_matches('`').trim();
    text = text
        .trim_matches(|c| matches!(c, '"' | '\'' | '“' | '”' | '‘' | '’' | '「' | '」'))
        .trim();

    for prefix in [
        "标题：",
        "标题:",
        "会话标题：",
        "会话标题:",
        "Title:",
        "title:",
        "Session title:",
        "Session Title:",
    ] {
        if let Some(rest) = text.strip_prefix(prefix) {
            text = rest.trim();
            break;
        }
    }

    // "- Fix login" / "* Fix login" / "1. Fix login" / "# Fix login".
    let unmarked = text
        .strip_prefix("- ")
        .or_else(|| text.strip_prefix("* "))
        .or_else(|| text.strip_prefix("# "))
        .or_else(|| {
            let (head, rest) = text.split_once(". ")?;
            head.chars().all(|c| c.is_ascii_digit()).then_some(rest)
        });
    match unmarked {
        Some(rest) => rest.trim().to_string(),
        None => text.to_string(),
    }
}

/// Whether a line is chatter ("Certainly!", "Here's a title:") rather than the
/// title itself.
fn looks_like_preamble(line: &str) -> bool {
    let lower = line.trim().to_lowercase();
    // A line ending in a colon is a label; the answer, if there is one, is on
    // the next line.
    if lower.ends_with(':') || lower.ends_with('：') {
        return true;
    }
    const OPENERS: [&str; 13] = [
        "certainly",
        "sure,",
        "sure!",
        "sure thing",
        "of course",
        "here's",
        "here is",
        "here are",
        "the title",
        "i'd suggest",
        "i would suggest",
        "based on",
        "标题",
    ];
    OPENERS.iter().any(|o| lower.starts_with(o))
}

/// Sentence terminators, ignoring a trailing ellipsis (which is punctuation on
/// one title, not three sentences).
fn sentence_breaks(text: &str) -> usize {
    let trimmed = text.trim_end_matches(|c| c == '.' || c == '…');
    trimmed
        .chars()
        .filter(|c| matches!(c, '。' | '！' | '？' | '.' | '!' | '?'))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_a_plain_title() {
        assert_eq!(sanitize_title("Fix token expiry"), Some("Fix token expiry".into()));
    }

    #[test]
    fn strips_quotes_fences_and_labels() {
        assert_eq!(sanitize_title("\"Fix token expiry\""), Some("Fix token expiry".into()));
        assert_eq!(sanitize_title("```Fix token expiry```"), Some("Fix token expiry".into()));
        assert_eq!(sanitize_title("Title: Fix token expiry"), Some("Fix token expiry".into()));
        assert_eq!(sanitize_title("标题：修复登录"), Some("修复登录".into()));
        assert_eq!(sanitize_title("- 修复登录"), Some("修复登录".into()));
    }

    #[test]
    fn takes_the_answer_under_a_preamble() {
        let raw = "Certainly! Here's a title:\n修复 token 过期";
        assert_eq!(sanitize_title(raw), Some("修复 token 过期".into()));
        // …but a reply that is nothing but preamble is refused.
        assert_eq!(sanitize_title("Certainly! Here's a title:"), None);
    }

    #[test]
    fn refuses_a_paragraph() {
        let raw = "The user wants to fix the login flow. They mention token expiry, \
                   which is probably in the auth middleware. I would suggest starting there.";
        assert_eq!(sanitize_title(raw), None);
    }

    #[test]
    fn caps_a_long_single_line() {
        let raw = "Implement the user registration flow with email verification";
        let title = sanitize_title(raw).unwrap();
        assert!(title.chars().count() <= MAX_TITLE_CHARS + 1);
        assert!(title.ends_with('…'));
    }

    #[test]
    fn refuses_empty_and_punctuation_only() {
        assert_eq!(sanitize_title(""), None);
        assert_eq!(sanitize_title("   "), None);
        assert_eq!(sanitize_title("?!..."), None);
    }

    #[test]
    fn keeps_a_trailing_ellipsis_title() {
        assert_eq!(sanitize_title("Waiting for input..."), Some("Waiting for input".into()));
    }

    #[test]
    fn prompt_carries_the_message_and_the_limit() {
        let p = title_prompt("修复登录");
        assert!(p.contains("修复登录"));
        assert!(p.contains(&MAX_TITLE_CHARS.to_string()));
    }
}
