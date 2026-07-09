//! Scribe — orchestrates raw → fact → pattern memory pipeline.

use tracing::{debug, warn};

use super::fact::{extract_facts, Fact};
use super::pattern::promote_patterns;
use super::raw::RawMessage;
use super::store::MemoryStore;

/// The memory pipeline orchestrator.
pub struct Scribe {
    store: MemoryStore,
}

impl Scribe {
    pub fn new() -> Result<Self, String> {
        Ok(Self {
            store: MemoryStore::open_default()?,
        })
    }

    pub fn with_store(store: MemoryStore) -> Self {
        Self { store }
    }

    /// Ingest a conversation turn: store raw messages and extract facts.
    pub fn ingest_turn(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
    ) -> Result<Vec<Fact>, String> {
        let raw = RawMessage::new(session_id, role, content);
        self.store
            .insert_raw(&raw)
            .map_err(|e| format!("raw insert: {e}"))?;

        let facts = extract_facts(session_id, content);
        for f in &facts {
            if let Err(e) = self.store.insert_fact(f) {
                warn!(%e, "fact insert failed");
            }
        }

        // Pattern promotion from recent session facts
        if let Ok(recent) = self.store.list_facts(session_id, 50) {
            let triples: Vec<_> = recent
                .iter()
                .map(|f| (f.subject.clone(), f.predicate.clone(), f.object.clone()))
                .collect();
            for pat in promote_patterns(&triples) {
                let _ = self.store.insert_pattern(&pat);
            }
        }

        debug!(session = %session_id, facts = facts.len(), "scribe ingested turn");
        Ok(facts)
    }

    /// Search memory for context injection (token-efficient).
    pub fn recall(&self, query: &str, limit: usize) -> Vec<String> {
        match self.store.search(query, limit) {
            Ok(hits) => hits
                .into_iter()
                .map(|(s, p, o, score)| format!("[{score:.2}] {s} — {p} — {o}"))
                .collect(),
            Err(_) => {
                // Fallback: list recent facts matching keywords
                vec![]
            }
        }
    }

    pub fn store(&self) -> &MemoryStore {
        &self.store
    }
}

impl Default for Scribe {
    fn default() -> Self {
        Self::new().unwrap_or_else(|_| {
            // In-memory fallback if home dir unavailable
            let store = MemoryStore::new(std::env::temp_dir().join("dscode-memory.db"))
                .expect("temp memory store");
            Self { store }
        })
    }
}
