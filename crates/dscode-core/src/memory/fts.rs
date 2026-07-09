//! FTS helpers for memory search (SQLite FTS5 when available).

use rusqlite::Connection;

/// Ensure the FTS virtual table exists.
pub fn ensure_fts(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts USING fts5(
            subject, predicate, object, content, tokenize='porter'
        );",
    )?;
    Ok(())
}

/// Full-text search over memory facts. Returns (subject, predicate, object, score).
pub fn search_memory(
    conn: &Connection,
    query: &str,
    limit: usize,
) -> Result<Vec<(String, String, String, f64)>, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "SELECT subject, predicate, object, bm25(memory_fts) as score
         FROM memory_fts
         WHERE memory_fts MATCH ?1
         ORDER BY score
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![query, limit as i64], |row| {
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
