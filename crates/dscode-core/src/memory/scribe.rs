//! Scribe — orchestrates raw → fact → pattern memory pipeline.

use tracing::{debug, warn};

use super::fact::{extract_facts, Fact};
use super::pattern::promote_patterns;
use super::raw::RawMessage;
use super::store::MemoryStore;

/// The memory pipeline orchestrator.
///
/// The store is optional: memory is an enhancement, so a machine where
/// `memory.db` cannot be opened degrades to a scribe that recalls nothing and
/// stores nothing instead of failing the chat turn.
pub struct Scribe {
    store: Option<MemoryStore>,
    /// When set, `recall` only returns facts ingested from this conversation.
    session_id: Option<String>,
}

impl Scribe {
    pub fn new() -> Result<Self, String> {
        Ok(Self {
            store: Some(MemoryStore::open_default()?),
            session_id: None,
        })
    }

    /// Scribe scoped to one conversation.
    ///
    /// Recall will only surface facts that were ingested from `session_id`, so
    /// one session's conversation content can never be injected into another
    /// session's system prompt. Call sites that render recall into a prompt
    /// should use this constructor rather than [`Scribe::new`].
    pub fn for_session(session_id: impl Into<String>) -> Result<Self, String> {
        Ok(Self {
            store: Some(MemoryStore::open_default()?),
            session_id: Some(session_id.into()),
        })
    }

    pub fn with_store(store: MemoryStore) -> Self {
        Self {
            store: Some(store),
            session_id: None,
        }
    }

    /// A scribe with no backing store: every operation is a no-op.
    pub fn disabled() -> Self {
        Self {
            store: None,
            session_id: None,
        }
    }

    /// Ingest a conversation turn: store raw messages and extract facts.
    pub fn ingest_turn(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
    ) -> Result<Vec<Fact>, String> {
        let Some(store) = self.store.as_ref() else {
            debug!("memory store unavailable; ingest skipped");
            return Ok(Vec::new());
        };

        let raw = RawMessage::new(session_id, role, content);
        store
            .insert_raw(&raw)
            .map_err(|e| format!("raw insert: {e}"))?;

        let facts = extract_facts(session_id, content);
        for f in &facts {
            if let Err(e) = store.insert_fact(f) {
                warn!(%e, "fact insert failed");
            }
        }

        // Pattern promotion from recent session facts. A failed read used to
        // be swallowed by `if let Ok(..)`, which skipped promotion silently.
        match store.list_facts(session_id, 50) {
            Ok(recent) => {
                let triples: Vec<_> = recent
                    .iter()
                    .map(|f| (f.subject.clone(), f.predicate.clone(), f.object.clone()))
                    .collect();
                for pat in promote_patterns(&triples) {
                    if let Err(e) = store.insert_pattern(&pat) {
                        warn!(%e, pattern = %pat.name, "pattern upsert failed");
                    }
                }
            }
            Err(e) => warn!(%e, "pattern promotion skipped: could not list facts"),
        }

        debug!(session = %session_id, facts = facts.len(), "scribe ingested turn");
        Ok(facts)
    }

    /// Search memory for context injection (token-efficient).
    ///
    /// Scoped to the session this scribe was built for. A scribe built with
    /// [`Scribe::new`] has no session and therefore recalls nothing: the index
    /// is global and there is no way to tell whose facts belong in the current
    /// prompt, so returning them would inject one conversation's content into
    /// another's system prompt. Use [`Scribe::for_session`] at call sites that
    /// render recall into a prompt, or [`Scribe::recall_global`] to opt in to a
    /// deliberately unscoped search.
    pub fn recall(&self, query: &str, limit: usize) -> Vec<String> {
        match self.session_id.as_deref() {
            Some(session_id) => self.recall_scoped(query, Some(session_id), limit),
            None => {
                warn!(
                    "memory recall skipped: this Scribe has no session scope; \
                     construct it with Scribe::for_session(&session_id)"
                );
                Vec::new()
            }
        }
    }

    /// Recall restricted to `session_id`, regardless of how this scribe was
    /// constructed.
    pub fn recall_for_session(&self, session_id: &str, query: &str, limit: usize) -> Vec<String> {
        self.recall_scoped(query, Some(session_id), limit)
    }

    /// Deliberately unscoped recall across every session. Only for callers that
    /// are not building a per-session prompt.
    pub fn recall_global(&self, query: &str, limit: usize) -> Vec<String> {
        self.recall_scoped(query, None, limit)
    }

    fn recall_scoped(&self, query: &str, session_id: Option<&str>, limit: usize) -> Vec<String> {
        let Some(store) = self.store.as_ref() else {
            return Vec::new();
        };
        match store.search(query, session_id, limit) {
            Ok(hits) => Self::render(hits),
            Err(e) => {
                // This branch used to be `Err(_) => vec![]` behind a comment
                // promising a keyword fallback that did not exist. The
                // fallback below is real.
                warn!(error = %e, "memory FTS recall failed; falling back to keyword scan");
                match store.search_lexical(query, session_id, limit) {
                    Ok(hits) => Self::render(hits),
                    Err(e) => {
                        warn!(error = %e, "memory keyword fallback failed");
                        Vec::new()
                    }
                }
            }
        }
    }

    fn render(hits: Vec<(String, String, String, f64)>) -> Vec<String> {
        hits.into_iter()
            .map(|(s, p, o, score)| format!("[{score:.2}] {s} — {p} — {o}"))
            .collect()
    }

    /// The backing store, or `None` for a disabled scribe.
    pub fn store(&self) -> Option<&MemoryStore> {
        self.store.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One fact, ingested from session "A".
    fn seeded() -> Scribe {
        let store = MemoryStore::open_in_memory().unwrap();
        store
            .insert_fact(&Fact::new("A", "project", "uses", "language:rust"))
            .unwrap();
        Scribe::with_store(store)
    }

    #[test]
    fn scoped_recall_returns_this_sessions_facts() {
        let hits = seeded().recall_for_session("A", "rust", 6);
        assert_eq!(hits.len(), 1);
        assert!(hits[0].contains("language:rust"), "{hits:?}");
    }

    #[test]
    fn another_session_never_sees_them() {
        assert!(seeded().recall_for_session("B", "rust", 6).is_empty());
    }

    #[test]
    fn a_scribe_without_a_session_fails_closed() {
        let scribe = seeded();
        // No session scope → no recall, so nothing can leak into a prompt...
        assert!(scribe.recall("rust", 6).is_empty());
        // ...but an explicit opt-in still works.
        assert_eq!(scribe.recall_global("rust", 6).len(), 1);
    }
}

impl Default for Scribe {
    /// Never panics.
    ///
    /// `Default` cannot report a failure, so an unusable on-disk store
    /// degrades to a temp-file store, then to an in-memory one, and finally to
    /// a disabled scribe whose operations are no-ops. The previous
    /// implementation `.expect()`-ed on the *fallback* path, so an unavailable
    /// temp directory aborted the process.
    fn default() -> Self {
        match Self::new() {
            Ok(scribe) => scribe,
            Err(err) => {
                warn!(error = %err, "memory.db unavailable; falling back to the temp directory");
                match MemoryStore::new(std::env::temp_dir().join("dscode-memory.db")) {
                    Ok(store) => Self::with_store(store),
                    Err(err) => {
                        warn!(error = %err, "temp memory store unavailable; trying in-memory");
                        match MemoryStore::open_in_memory() {
                            Ok(store) => Self::with_store(store),
                            Err(err) => {
                                warn!(error = %err, "memory disabled for this process");
                                Self::disabled()
                            }
                        }
                    }
                }
            }
        }
    }
}
