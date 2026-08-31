//! History with FTS5 — per AGENTS §23, §27.
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub id: Uuid,
    pub connection_id: String,
    pub query_text: String,
    pub executed_at: DateTime<Utc>,
    pub duration_ms: Option<u64>,
    pub rows_affected: Option<i64>,
    pub success: bool,
    pub error_code: Option<String>,
}

#[derive(Debug, Error)]
pub enum HistoryError {
    #[error("rusqlite: {0}")]
    Rusqlite(#[from] rusqlite::Error),
}

pub fn init(conn: &Connection) -> Result<(), HistoryError> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS history (
            id TEXT PRIMARY KEY,
            connection_id TEXT NOT NULL,
            query_text TEXT NOT NULL,
            executed_at INTEGER NOT NULL,
            duration_ms INTEGER,
            rows_affected INTEGER,
            success INTEGER NOT NULL,
            error_code TEXT
        )",
        [],
    )?;
    conn.execute(
        "CREATE VIRTUAL TABLE IF NOT EXISTS history_fts USING fts5(query_text, content='history', content_rowid='rowid', tokenize='porter unicode61')",
        [],
    )?;
    conn.execute(
        "CREATE TRIGGER IF NOT EXISTS history_ai AFTER INSERT ON history BEGIN
            INSERT INTO history_fts(rowid, query_text) VALUES (new.rowid, new.query_text);
         END",
        [],
    )?;
    conn.execute(
        "CREATE TRIGGER IF NOT EXISTS history_ad AFTER DELETE ON history BEGIN
            INSERT INTO history_fts(history_fts, rowid, query_text) VALUES('delete', old.rowid, old.query_text);
         END",
        [],
    )?;
    conn.execute(
        "CREATE TRIGGER IF NOT EXISTS history_au AFTER UPDATE ON history BEGIN
            INSERT INTO history_fts(history_fts, rowid, query_text) VALUES('delete', old.rowid, old.query_text);
            INSERT INTO history_fts(rowid, query_text) VALUES (new.rowid, new.query_text);
         END",
        [],
    )?;
    Ok(())
}

/// Redact history text that likely contains secrets per §35.
/// Heuristic: if lowercased query contains password|passwd|pwd|secret|token
/// or api_key|apikey|private_key|aws_secret|client_secret store a placeholder.
#[must_use]
pub fn sanitize_for_history(sql: &str) -> String {
    let low = sql.to_ascii_lowercase();
    if low.contains("password")
        || low.contains("passwd")
        || low.contains("secret")
        || low.contains("token")
        || low.contains("pwd")
        || low.contains("api_key")
        || low.contains("apikey")
        || low.contains("private_key")
        || low.contains("aws_secret")
        || low.contains("client_secret")
    {
        return "[REDACTED sensitive query]".to_string();
    }
    sql.to_string()
}

pub fn insert(conn: &Connection, e: &HistoryEntry) -> Result<(), HistoryError> {
    const CAP: usize = 64 * 1024;
    let sanitized = sanitize_for_history(&e.query_text);
    let truncated = if sanitized.len() > CAP {
        // UTF-8 safe truncation at char boundary
        let mut end = CAP;
        while !sanitized.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        &sanitized[..end]
    } else {
        &sanitized
    };
    conn.execute(
        "INSERT INTO history(id, connection_id, query_text, executed_at, duration_ms, rows_affected, success, error_code)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        params![e.id.to_string(), e.connection_id, truncated, e.executed_at.timestamp_millis(), e.duration_ms.map(|v| v as i64), e.rows_affected, if e.success {1} else {0}, e.error_code],
    )?;
    Ok(())
}

pub fn search(conn: &Connection, q: &str) -> Result<Vec<HistoryEntry>, HistoryError> {
    // §24: cap + sanitize FTS5 query — raw user input with NEAR/OR/\" can DoS
    let q = {
        let mut s = q.chars().take(200).collect::<String>();
        // Escape double-quotes for FTS5 phrase; if syntax still invalid, caller
        // gets empty result via Error handling rather than crash.
        s = s.replace('"', "\"\"");
        if s.trim().is_empty() {
            return Ok(vec![]);
        }
        s
    };
    let mut stmt = conn.prepare(
        "SELECT h.id, h.connection_id, h.query_text, h.executed_at, h.duration_ms, h.rows_affected, h.success, h.error_code
         FROM history_fts JOIN history h ON h.rowid = history_fts.rowid
         WHERE history_fts MATCH ?1 ORDER BY rank LIMIT 50"
    )?;
    let rows = stmt.query_map(params![q], |row| {
        let id_str: String = row.get(0)?;
        let id = Uuid::parse_str(&id_str).unwrap_or_else(|_| Uuid::nil());
        Ok(HistoryEntry {
            id,
            connection_id: row.get(1)?,
            query_text: row.get(2)?,
            executed_at: DateTime::from_timestamp_millis(row.get::<_, i64>(3)?)
                .unwrap_or_else(Utc::now),
            duration_ms: row.get::<_, Option<i64>>(4)?.map(|v| v as u64),
            rows_affected: row.get(5)?,
            success: row.get::<_, i64>(6)? != 0,
            error_code: row.get(7)?,
        })
    })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fts_roundtrip() {
        let conn = Connection::open_in_memory().unwrap();
        init(&conn).unwrap();
        let e = HistoryEntry {
            id: Uuid::new_v4(),
            connection_id: "c1".into(),
            query_text: "SELECT * FROM users WHERE id=1".into(),
            executed_at: Utc::now(),
            duration_ms: Some(12),
            rows_affected: Some(1),
            success: true,
            error_code: None,
        };
        insert(&conn, &e).unwrap();
        let results = search(&conn, "users").unwrap();
        assert_eq!(results.len(), 1);
    }
}
