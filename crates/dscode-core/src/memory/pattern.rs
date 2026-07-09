//! Pattern tier — cross-session generalizations.

use serde::{Deserialize, Serialize};

/// A recurring coding pattern observed across sessions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pattern {
    pub id: String,
    pub name: String,
    pub description: String,
    pub occurrence_count: u32,
    pub last_seen_at: i64,
    pub tags: Vec<String>,
}

impl Pattern {
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.into(),
            description: description.into(),
            occurrence_count: 1,
            last_seen_at: chrono::Utc::now().timestamp(),
            tags: vec![],
        }
    }
}

/// Promote repeated facts into patterns.
pub fn promote_patterns(facts: &[(String, String, String)]) -> Vec<Pattern> {
    use std::collections::HashMap;
    let mut counts: HashMap<(String, String, String), u32> = HashMap::new();
    for (s, p, o) in facts {
        *counts
            .entry((s.clone(), p.clone(), o.clone()))
            .or_insert(0) += 1;
    }
    counts
        .into_iter()
        .filter(|(_, n)| *n >= 2)
        .map(|((s, p, o), n)| {
            let mut pat = Pattern::new(
                format!("{s}_{p}"),
                format!("{s} {p} {o} (seen {n} times)"),
            );
            pat.occurrence_count = n;
            pat.tags = vec![p];
            pat
        })
        .collect()
}
