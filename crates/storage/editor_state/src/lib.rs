//! Editor tabs persistence.
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditorTab {
    pub tab_id: String,
    pub connection_id: Option<String>,
    pub content: String,
    pub cursor: usize,
}

#[derive(Debug, Error)]
pub enum EditorError {
    #[error("rusqlite: {0}")]
    Rusqlite(#[from] rusqlite::Error),
}

pub fn init(conn: &Connection) -> Result<(), EditorError> {
    conn.execute("CREATE TABLE IF NOT EXISTS editor_state (tab_id TEXT PRIMARY KEY, connection_id TEXT, content TEXT NOT NULL, cursor INTEGER NOT NULL)", [])?;
    Ok(())
}

pub fn upsert(conn: &Connection, tab: &EditorTab) -> Result<(), EditorError> {
    conn.execute("INSERT INTO editor_state(tab_id, connection_id, content, cursor) VALUES (?1,?2,?3,?4) ON CONFLICT(tab_id) DO UPDATE SET connection_id=?2, content=?3, cursor=?4", params![tab.tab_id, tab.connection_id, tab.content, tab.cursor as i64])?;
    Ok(())
}
