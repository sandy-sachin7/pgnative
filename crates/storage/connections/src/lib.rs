//! SQLite connections (non-secret) — per AGENTS §27.
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedConnection {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub dbname: String,
    pub username: String,
    pub ssl_mode: String,
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("rusqlite: {0}")]
    Rusqlite(#[from] rusqlite::Error),
}

pub fn init(conn: &Connection) -> Result<(), StoreError> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS connections (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            host TEXT NOT NULL,
            port INTEGER NOT NULL,
            dbname TEXT NOT NULL,
            username TEXT NOT NULL,
            ssl_mode TEXT NOT NULL
        )",
        [],
    )?;
    Ok(())
}

pub fn upsert(conn: &Connection, c: &SavedConnection) -> Result<(), StoreError> {
    conn.execute(
        "INSERT INTO connections(id,name,host,port,dbname,username,ssl_mode)
         VALUES (?1,?2,?3,?4,?5,?6,?7)
         ON CONFLICT(id) DO UPDATE SET name=?2, host=?3, port=?4, dbname=?5, username=?6, ssl_mode=?7",
        params![c.id, c.name, c.host, c.port, c.dbname, c.username, c.ssl_mode],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip() {
        let conn = Connection::open_in_memory().unwrap();
        init(&conn).unwrap();
        let c = SavedConnection {
            id: "1".into(),
            name: "local".into(),
            host: "localhost".into(),
            port: 5432,
            dbname: "test".into(),
            username: "bob".into(),
            ssl_mode: "prefer".into(),
        };
        upsert(&conn, &c).unwrap();
    }
}
