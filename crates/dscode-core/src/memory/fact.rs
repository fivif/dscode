//! Fact tier — structured (subject, predicate, object) triples.

use serde::{Deserialize, Serialize};

/// A structured knowledge triple extracted from conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fact {
    pub id: String,
    pub session_id: String,
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub confidence: f64,
    pub created_at: i64,
}

impl Fact {
    pub fn new(
        session_id: impl Into<String>,
        subject: impl Into<String>,
        predicate: impl Into<String>,
        object: impl Into<String>,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: session_id.into(),
            subject: subject.into(),
            predicate: predicate.into(),
            object: object.into(),
            confidence: 0.7,
            created_at: chrono::Utc::now().timestamp(),
        }
    }
}

/// True when `kw` occurs in `haystack` as a whole word.
///
/// The keyword side is ASCII, but any non-ASCII character counts as a
/// boundary: `用python写` does mention python, while `rusty`, `reactor` and
/// `pythonic` do not mention Rust, React or Python.
fn contains_word(haystack: &str, kw: &str) -> bool {
    let mut from = 0;
    while let Some(pos) = haystack[from..].find(kw) {
        let start = from + pos;
        let end = start + kw.len();
        let is_word_char = |c: char| c.is_ascii_alphanumeric() || c == '_';
        let before_ok = haystack[..start].chars().next_back().map_or(true, |c| !is_word_char(c));
        let after_ok = haystack[end..].chars().next().map_or(true, |c| !is_word_char(c));
        if before_ok && after_ok {
            return true;
        }
        from = end;
        if from >= haystack.len() {
            break;
        }
    }
    false
}

/// Strip fenced code blocks — a language name inside a code sample or an error
/// dump is not evidence about the project.
fn strip_code_blocks(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_fence = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if !in_fence {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Lightweight heuristic fact extractor (no extra LLM call — token efficient).
pub fn extract_facts(session_id: &str, text: &str) -> Vec<Fact> {
    let mut facts = Vec::new();
    // Language names inside a fenced block are a sample or an error dump, not
    // a statement about the project.
    let prose = strip_code_blocks(text);
    let lower = prose.to_lowercase();

    // Decisions and rules first: `truncate(12)` below used to keep the six
    // language signals ahead of them, so a fact-rich turn dropped the real
    // content and kept the boilerplate.
    for line in prose.lines() {
        let l = line.trim();
        if l.len() < 12 || l.len() > 200 {
            continue;
        }
        let ll = l.to_lowercase();
        if ll.contains("decided") || ll.contains("chose") || ll.starts_with("we'll use") {
            facts.push(Fact::new(session_id, "decision", "states", l));
        }
        if ll.contains("must ") || ll.contains("always ") || ll.contains("never ") {
            facts.push(Fact::new(session_id, "rule", "states", l));
        }
    }

    // Language / framework signals — whole words only, so a question *about*
    // python no longer claims the project uses python.
    for (kw, obj) in [
        ("rust", "language:rust"),
        ("typescript", "language:typescript"),
        ("python", "language:python"),
        ("react", "framework:react"),
        ("tokio", "framework:tokio"),
        ("tauri", "framework:tauri"),
    ] {
        if contains_word(&lower, kw) {
            facts.push(Fact::new(session_id, "project", "uses", obj));
        }
    }

    facts.truncate(12);
    facts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_signal_requires_a_word_boundary() {
        // Substrings of other words must not claim a language.
        assert!(extract_facts("s", "this is a rusty reactor, very pythonic").is_empty());
        // A whole word does.
        let f = extract_facts("s", "we should use rust for this service");
        assert!(f.iter().any(|x| x.object == "language:rust"));
        // CJK-adjacent still counts as a boundary.
        let f = extract_facts("s", "这段用python写的代码有问题");
        assert!(f.iter().any(|x| x.object == "language:python"));
    }

    #[test]
    fn code_blocks_do_not_produce_language_facts() {
        let f = extract_facts("s", "look at this:\n```rust\nfn main() {}\n```\n");
        assert!(f.is_empty());
    }

    #[test]
    fn decisions_are_kept_ahead_of_language_signals_when_truncating() {
        let mut text = String::new();
        for i in 0..12 {
            text.push_str(&format!("we decided to keep subsystem number {i} incremental\n"));
        }
        text.push_str("rust python react tokio tauri typescript\n");
        let f = extract_facts("s", &text);
        assert_eq!(f.len(), 12);
        assert!(
            f.iter().all(|x| x.subject == "decision"),
            "language signals crowded out the decisions: {f:?}"
        );
    }
}
