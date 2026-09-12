//! Preferences (JSON in SQLite).
use rusqlite::{params, Connection};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PrefError {
    #[error("rusqlite: {0}")]
    Rusqlite(#[from] rusqlite::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

pub fn init(conn: &Connection) -> Result<(), PrefError> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS preferences (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
        [],
    )?;
    Ok(())
}

pub fn set(conn: &Connection, key: &str, value: &Value) -> Result<(), PrefError> {
    conn.execute("INSERT INTO preferences(key, value) VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value=?2", params![key, value.to_string()])?;
    Ok(())
}

pub fn get(conn: &Connection, key: &str) -> Result<Option<Value>, PrefError> {
    let mut stmt = conn.prepare("SELECT value FROM preferences WHERE key=?1")?;
    let mut rows = stmt.query(params![key])?;
    if let Some(row) = rows.next()? {
        let s: String = row.get(0)?;
        Ok(Some(serde_json::from_str(&s)?))
    } else {
        Ok(None)
    }
}
