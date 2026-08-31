//! Editor tabs persistence — per product decision.
//! Cursor/selection history: LRU cap = 1000.
//! Persisted editor buffers: OFF by default, explicit opt-in (never saves
//! arbitrary SQL containing secrets by default).
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// LRU cap for cursor/selection history per product decision.
pub const CURSOR_HISTORY_CAP: usize = 1000;

/// Whether persisted editor buffers are enabled. OFF by default — explicit opt-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistedBuffers {
    Off,
    On,
}

impl Default for PersistedBuffers {
    fn default() -> Self {
        Self::Off
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditorTab {
    pub tab_id: String,
    pub connection_id: Option<String>,
    pub content: String,
    pub cursor: usize,
    /// Selection range (start, end) for LRU cursor history.
    pub selection: Option<(usize, usize)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CursorHistoryEntry {
    pub tab_id: String,
    pub cursor: usize,
    pub selection: Option<(usize, usize)>,
    pub at_ms: i64,
}

#[derive(Debug, Error)]
pub enum EditorError {
    #[error("rusqlite: {0}")]
    Rusqlite(#[from] rusqlite::Error),
}

pub fn init(conn: &Connection) -> Result<(), EditorError> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS editor_state (tab_id TEXT PRIMARY KEY, connection_id TEXT, content TEXT NOT NULL, cursor INTEGER NOT NULL, selection_start INTEGER, selection_end INTEGER)",
        [],
    )?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS cursor_history (tab_id TEXT NOT NULL, cursor INTEGER NOT NULL, sel_start INTEGER, sel_end INTEGER, at_ms INTEGER NOT NULL, PRIMARY KEY (tab_id, at_ms))",
        [],
    )?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS editor_prefs (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
        [],
    )?;
    // Default: persisted buffers OFF
    conn.execute(
        "INSERT OR IGNORE INTO editor_prefs(key, value) VALUES ('persisted_buffers', 'off')",
        [],
    )?;
    Ok(())
}

pub fn persisted_buffers_enabled(conn: &Connection) -> Result<bool, EditorError> {
    let mut stmt = conn.prepare("SELECT value FROM editor_prefs WHERE key='persisted_buffers'")?;
    let val: Option<String> = stmt.query_row([], |r| r.get(0)).ok();
    Ok(val.as_deref() == Some("on"))
}

pub fn set_persisted_buffers(conn: &Connection, enabled: bool) -> Result<(), EditorError> {
    conn.execute(
        "INSERT INTO editor_prefs(key, value) VALUES ('persisted_buffers', ?1) ON CONFLICT(key) DO UPDATE SET value=?1",
        params![if enabled { "on" } else { "off" }],
    )?;
    Ok(())
}

pub fn upsert(conn: &Connection, tab: &EditorTab) -> Result<(), EditorError> {
    // Respect product decision: OFF by default — never persist arbitrary SQL containing secrets unless explicit opt-in.
    if !persisted_buffers_enabled(conn).unwrap_or(false) {
        return Ok(());
    }
    let (sel_start, sel_end) = tab.selection.unzip();
    conn.execute(
        "INSERT INTO editor_state(tab_id, connection_id, content, cursor, selection_start, selection_end) VALUES (?1,?2,?3,?4,?5,?6) ON CONFLICT(tab_id) DO UPDATE SET connection_id=?2, content=?3, cursor=?4, selection_start=?5, selection_end=?6",
        params![
            tab.tab_id,
            tab.connection_id,
            tab.content,
            tab.cursor as i64,
            sel_start.map(|v| v as i64),
            sel_end.map(|v| v as i64)
        ],
    )?;
    Ok(())
}

/// Record cursor/selection with LRU cap 1000.
pub fn record_cursor(
    conn: &Connection,
    tab_id: &str,
    cursor: usize,
    selection: Option<(usize, usize)>,
    at_ms: i64,
) -> Result<(), EditorError> {
    conn.execute(
        "INSERT INTO cursor_history(tab_id, cursor, sel_start, sel_end, at_ms) VALUES (?1,?2,?3,?4,?5)",
        params![
            tab_id,
            cursor as i64,
            selection.map(|(s, _)| s as i64),
            selection.map(|(_, e)| e as i64),
            at_ms
        ],
    )?;
    // Enforce LRU cap 1000
    conn.execute(
        "DELETE FROM cursor_history WHERE rowid NOT IN (SELECT rowid FROM cursor_history ORDER BY at_ms DESC LIMIT ?1)",
        params![CURSOR_HISTORY_CAP as i64],
    )?;
    Ok(())
}
