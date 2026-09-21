//! FTS helpers for memory search (SQLite FTS5 when available).

use rusqlite::Connection;

/// Cap on how many terms of a user query are turned into a MATCH expression.
/// A pasted document would otherwise produce a multi-kilobyte query.
const MAX_QUERY_TOKENS: usize = 32;

/// Ensure the FTS virtual table exists.
pub fn ensure_fts(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts USING fts5(
            subject, predicate, object, content, tokenize='porter'
        );",
    )?;
    Ok(())
}

/// Split arbitrary user text into literal FTS terms.
///
/// Everything that is not alphanumeric is a separator (Unicode-aware, so CJK
/// text survives as tokens). This is the one character class FTS5's query
/// grammar assigns no meaning to — `:`, `"`, `*`, `-`, `+`, `(`, `)`, `^`, and
/// the bare words `AND`/`OR`/`NOT` are all operators, which is why binding raw
/// user text to `MATCH` fails with "syntax error" or "no such column".
pub fn query_tokens(query: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for ch in query.chars() {
        if ch.is_alphanumeric() {
            current.push(ch);
        } else if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
            if tokens.len() >= MAX_QUERY_TOKENS {
                return tokens;
            }
        }
    }
    if !current.is_empty() && tokens.len() < MAX_QUERY_TOKENS {
        tokens.push(current);
    }
    tokens
}

/// Build a safe FTS5 `MATCH` expression from raw user text.
///
/// Returns `None` when the text contains no indexable term — an empty `MATCH`
/// is itself a syntax error, so the caller must skip the query entirely.
///
/// Each term is double-quoted, which makes it a literal in the FTS5 grammar
/// (so a term that happens to spell `AND` can never be read as an operator),
/// and the terms are OR-ed: conversational input (`fix the panic in
/// parser.rs:212`) would never match a stored triple as a whole phrase, but
/// one of its terms (`panic`, `parser`) often does.
pub fn build_match_query(query: &str) -> Option<String> {
    let tokens = query_tokens(query);
    if tokens.is_empty() {
        return None;
    }
    Some(
        tokens
            .iter()
            .map(|t| format!("\"{t}\""))
            .collect::<Vec<_>>()
            .join(" OR "),
    )
}

/// Full-text search over memory facts. Returns (subject, predicate, object, score).
///
/// `session_id` scopes the search to one conversation. `memory_fts` is a single
/// global index with no session column — FTS5 virtual tables cannot
/// `ALTER TABLE ADD COLUMN` — so the scope is applied against the `facts` row
/// that owns the hit, which *is* keyed by `session_id`. Pass `None` for a
/// global search.
pub fn search_memory(
    conn: &Connection,
    query: &str,
    session_id: Option<&str>,
    limit: usize,
) -> Result<Vec<(String, String, String, f64)>, rusqlite::Error> {
    let Some(match_expr) = build_match_query(query) else {
        return Ok(Vec::new());
    };
    let mut stmt = conn.prepare(
        "SELECT subject, predicate, object, bm25(memory_fts) as score
         FROM memory_fts
         WHERE memory_fts MATCH ?1
           AND (?2 IS NULL OR EXISTS (
                 SELECT 1 FROM facts f
                 WHERE f.subject    = memory_fts.subject
                   AND f.predicate  = memory_fts.predicate
                   AND f.object     = memory_fts.object
                   AND f.session_id = ?2))
         ORDER BY score
         LIMIT ?3",
    )?;
    let rows = stmt.query_map(rusqlite::params![match_expr, session_id, limit as i64], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, f64>(3).unwrap_or(0.0).abs(),
        ))
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// `search_memory`'s session scope joins back to `facts`, so the tests need
    /// both tables. Production creates them in `MemoryStore::init`.
    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS facts (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                subject TEXT NOT NULL,
                predicate TEXT NOT NULL,
                object TEXT NOT NULL,
                confidence REAL NOT NULL DEFAULT 0.7,
                created_at INTEGER NOT NULL
            );",
        )
        .unwrap();
        ensure_fts(&conn).unwrap();
        conn
    }

    fn insert_fact_row(conn: &Connection, id: &str, session: &str, s: &str, p: &str, o: &str) {
        conn.execute(
            "INSERT INTO facts (id, session_id, subject, predicate, object, confidence, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 0.7, 1)",
            rusqlite::params![id, session, s, p, o],
        )
        .unwrap();
    }

    /// The index is keyed by SPO and shared between sessions, exactly like
    /// `insert_fact` writes it (it deletes by SPO before inserting).
    fn index(conn: &Connection, s: &str, p: &str, o: &str) {
        conn.execute(
            "INSERT INTO memory_fts (subject, predicate, object, content) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![s, p, o, format!("{s} {p} {o}")],
        )
        .unwrap();
    }

    fn fact(conn: &Connection, id: &str, session: &str, s: &str, p: &str, o: &str) {
        insert_fact_row(conn, id, session, s, p, o);
        index(conn, s, p, o);
    }

    fn seeded() -> Connection {
        let conn = test_conn();
        // Shaped like what the pipeline actually stores: extracted language
        // signals plus verbatim decision/rule lines from the conversation.
        fact(&conn, "f1", "A", "project", "uses", "language:rust");
        fact(&conn, "f2", "A", "project", "uses", "framework:tokio");
        fact(&conn, "f3", "A", "rule", "states", "never panic in the parser");
        fact(&conn, "f4", "A", "decision", "states", "we decided to add tests for TODO handling");
        fact(&conn, "f5", "A", "rule", "states", "read src/main.rs before editing");
        fact(&conn, "f6", "A", "rule", "states", "use node for the build tooling");
        conn
    }

    /// The five verbatim messages from the bug report. Each one used to come
    /// back as `Err` on the first `sqlite3_step` ("syntax error near ...",
    /// "no such column: ..."), which `recall` turned into an empty result.
    #[test]
    fn real_world_messages_no_longer_error() {
        let conn = seeded();
        for q in [
            "fix the panic in parser.rs:212",
            "TODO: add tests",
            "what about C++ and node?",
            "read src/main.rs and tell me",
            "how do I use tokio::spawn",
        ] {
            let hits = search_memory(&conn, q, None, 6).expect(q);
            assert!(!hits.is_empty(), "expected hits for {q:?}");
        }
    }

    #[test]
    fn a_query_with_no_indexed_term_is_a_clean_empty_result() {
        let conn = seeded();
        assert!(search_memory(&conn, "   ... ??? ", None, 6).unwrap().is_empty());
        assert!(build_match_query("   ... ??? ").is_none());
    }

    #[test]
    fn every_term_is_quoted_so_operators_cannot_leak_through() {
        let m = build_match_query("title:foo AND OR NOT NEAR").unwrap();
        // `title:foo` must be two literal terms, never a column filter, and no
        // bare operator keyword may survive as an operator.
        assert_eq!(m, "\"title\" OR \"foo\" OR \"AND\" OR \"OR\" OR \"NOT\" OR \"NEAR\"");
        for term in m.split(" OR ") {
            assert!(term.starts_with('"') && term.ends_with('"'), "{term}");
        }
        assert!(!m.contains(':'));
    }

    /// The index is global, so a hit must be filtered through the owning
    /// `facts` row — otherwise session A's conversation content is injected
    /// into session B's system prompt.
    #[test]
    fn session_scope_excludes_other_sessions() {
        let conn = seeded();
        assert_eq!(search_memory(&conn, "rust", Some("A"), 6).unwrap().len(), 1);
        assert!(search_memory(&conn, "rust", Some("B"), 6).unwrap().is_empty());
        assert_eq!(search_memory(&conn, "rust", None, 6).unwrap().len(), 1);
    }

    /// Two sessions holding the same triple must not multiply rows through the
    /// join back to `facts`.
    #[test]
    fn shared_spo_across_sessions_does_not_duplicate_hits() {
        let conn = test_conn();
        insert_fact_row(&conn, "f1", "A", "project", "uses", "language:rust");
        insert_fact_row(&conn, "f2", "B", "project", "uses", "language:rust");
        index(&conn, "project", "uses", "language:rust");
        assert_eq!(search_memory(&conn, "rust", Some("A"), 6).unwrap().len(), 1);
        assert_eq!(search_memory(&conn, "rust", Some("B"), 6).unwrap().len(), 1);
        assert_eq!(search_memory(&conn, "rust", None, 6).unwrap().len(), 1);
    }
}
