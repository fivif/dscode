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

/// Lightweight heuristic fact extractor (no extra LLM call — token efficient).
pub fn extract_facts(session_id: &str, text: &str) -> Vec<Fact> {
    let mut facts = Vec::new();
    let lower = text.to_lowercase();

    // Language / framework signals
    for (kw, obj) in [
        ("rust", "language:rust"),
        ("typescript", "language:typescript"),
        ("python", "language:python"),
        ("react", "framework:react"),
        ("tokio", "framework:tokio"),
        ("tauri", "framework:tauri"),
    ] {
        if lower.contains(kw) {
            facts.push(Fact::new(session_id, "project", "uses", obj));
        }
    }

    // Decision patterns: "we decided X" / "chose X"
    for line in text.lines() {
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

    facts.truncate(12);
    facts
}
