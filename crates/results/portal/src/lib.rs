//! Portal / server-side cursor window fetch for arbitrary SQL.
//! Implements AGENTS.md §16 window fetch (DECLARE/FETCH) without OFFSET rewrite.
//! Per ADR-0009: portal lives inside a transaction on a dedicated session.
//! Never injects LIMIT/OFFSET into user SQL; user_sql is passed verbatim to
//! `DECLARE ... CURSOR FOR <user_sql>`. Caller must use a session not already
//! in a transaction (or portal will error on BEGIN). Close via CLOSE+COMMIT.

use bytes::Bytes;
use pgnative_results_stream::{column_meta_from_pg, ColumnMeta};
use pgnative_results_value::{CellValue, Row};
use thiserror::Error;

/// Errors from portal operations, preserving PG sqlstate where available.
#[derive(Debug, Error)]
pub enum PortalError {
    #[error("pg error: {0}")]
    Pg(#[from] tokio_postgres::Error),
    #[error("portal already closed")]
    AlreadyClosed,
    #[error("portal not declared: {0}")]
    NotDeclared(String),
    #[error("decode error: {0}")]
    Decode(String),
}

/// State of a declared portal.
#[derive(Debug)]
pub struct Portal {
    /// Server-side cursor name (quoted when sent).
    pub name: String,
    /// Column metadata captured at DECLARE time via PREPARE.
    pub columns: Vec<ColumnMeta>,
    /// Total rows fetched via FETCH so far.
    pub fetched: u64,
    /// Whether CLOSE/COMMIT has been issued.
    pub closed: bool,
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Declare a portal for `user_sql` on `client`.
///
/// Runs `BEGIN READ ONLY` (if not already in transaction) then
/// `DECLARE <name> CURSOR FOR <user_sql>` with user_sql unmodified.
/// Returns `Portal` with column metadata from `PREPARE` of the same sql.
/// Caller must eventually call `close_portal` to release the transaction.
///
/// `name` should be unique per session (e.g. `pgnative_portal_<uuid>`).
pub async fn declare_portal(
    client: &tokio_postgres::Client,
    name: &str,
    user_sql: &str,
) -> Result<Portal, PortalError> {
    // Capture column metadata without modifying user_sql.
    let stmt = client.prepare(user_sql).await?;
    let columns = stmt
        .columns()
        .iter()
        .map(column_meta_from_pg)
        .collect::<Vec<_>>();

    // BEGIN — portal requires a transaction. READ ONLY avoids accidental writes.
    // If already in transaction this will error; caller should use a dedicated session.
    client.batch_execute("BEGIN READ ONLY").await?;

    let qname = quote_ident(name);
    // DECLARE does not accept parameters; embed user_sql verbatim (no LIMIT/OFFSET injection).
    let declare_sql = format!("DECLARE {qname} CURSOR FOR {user_sql}");
    client.batch_execute(&declare_sql).await?;

    Ok(Portal {
        name: name.to_owned(),
        columns,
        fetched: 0,
        closed: false,
    })
}

/// Fetch next `count` rows from portal.
///
/// Executes `FETCH FORWARD count FROM <name>` and decodes rows via
/// `pgnative_results_stream::decode_cell_with_cap` reusing the same OID mapping
/// as the streaming path. Returns `(rows, exhausted)` where exhausted=true when
/// fewer than `count` rows were returned (portal at end, but still open until close).
pub async fn fetch_forward(
    client: &tokio_postgres::Client,
    portal: &mut Portal,
    count: usize,
    per_cell_cap: usize,
) -> Result<(Vec<Row>, bool), PortalError> {
    if portal.closed {
        return Err(PortalError::AlreadyClosed);
    }
    if count == 0 {
        return Ok((vec![], false));
    }
    let qname = quote_ident(&portal.name);
    let sql = format!("FETCH FORWARD {count} FROM {qname}");
    let stmt = client.prepare(&sql).await?;
    let empty: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = vec![];
    let stream = client.query_raw(&stmt, empty).await?;
    let oids: Vec<u32> = portal.columns.iter().map(|c| c.pg_type_oid).collect();

    use futures::StreamExt as _;
    let mut stream = Box::pin(stream);
    let mut rows = Vec::with_capacity(count.min(1024));
    while let Some(res) = stream.next().await {
        let pg_row = res?;
        let mut cells = Vec::with_capacity(oids.len());
        for (i, oid) in oids.iter().enumerate() {
            // Reuse same binary-first decoding as stream::drive_stream.
            let cv = decode_cell_for_fetch(&pg_row, i, *oid, per_cell_cap);
            cells.push(cv);
        }
        rows.push(Row::new(cells));
    }
    let n = rows.len() as u64;
    portal.fetched += n;
    let exhausted = (n as usize) < count;
    Ok((rows, exhausted))
}

/// Close portal and commit transaction.
pub async fn close_portal(
    client: &tokio_postgres::Client,
    portal: &mut Portal,
) -> Result<(), PortalError> {
    if portal.closed {
        return Ok(());
    }
    let qname = quote_ident(&portal.name);
    // CLOSE is optional if we COMMIT, but explicit CLOSE makes intent clear.
    // Ignore error if portal already exhausted/closed by PG.
    let _ = client.batch_execute(&format!("CLOSE {qname}")).await;
    // End the READ ONLY transaction started at DECLARE.
    client.batch_execute("COMMIT").await?;
    portal.closed = true;
    Ok(())
}

/// Rollback portal transaction (on error/cancel). Idempotent.
pub async fn rollback_portal(client: &tokio_postgres::Client, portal: &mut Portal) {
    if portal.closed {
        return;
    }
    let _ = client.batch_execute("ROLLBACK").await;
    portal.closed = true;
}

fn decode_cell_for_fetch(
    pg_row: &tokio_postgres::Row,
    idx: usize,
    oid: u32,
    cap: usize,
) -> CellValue {
    use pgnative_results_stream::decode_cell_with_cap;
    // Mirror stream::drive_stream typed branches (binary first, text fallback).
    match oid {
        16 => match pg_row.try_get::<usize, Option<bool>>(idx) {
            Ok(v) => v.map_or(CellValue::Null, CellValue::Bool),
            Err(_) => {
                let raw = pg_row
                    .try_get::<usize, Option<&str>>(idx)
                    .ok()
                    .flatten()
                    .map(|s| s.as_bytes());
                decode_cell_with_cap(raw, oid, cap)
            }
        },
        2950 => match pg_row.try_get::<usize, Option<uuid::Uuid>>(idx) {
            Ok(v) => v.map_or(CellValue::Null, CellValue::Uuid),
            Err(_) => {
                let raw = pg_row
                    .try_get::<usize, Option<&str>>(idx)
                    .ok()
                    .flatten()
                    .map(|s| s.as_bytes())
                    .or_else(|| pg_row.try_get::<usize, Option<&[u8]>>(idx).ok().flatten());
                decode_cell_with_cap(raw, oid, cap)
            }
        },
        21 => match pg_row.try_get::<usize, Option<i16>>(idx) {
            Ok(v) => v.map_or(CellValue::Null, CellValue::SmallInt),
            Err(_) => {
                let raw = pg_row
                    .try_get::<usize, Option<&str>>(idx)
                    .ok()
                    .flatten()
                    .map(|s| s.as_bytes());
                decode_cell_with_cap(raw, oid, cap)
            }
        },
        23 => match pg_row.try_get::<usize, Option<i32>>(idx) {
            Ok(v) => v.map_or(CellValue::Null, CellValue::Int),
            Err(_) => {
                let raw = pg_row
                    .try_get::<usize, Option<&str>>(idx)
                    .ok()
                    .flatten()
                    .map(|s| s.as_bytes());
                decode_cell_with_cap(raw, oid, cap)
            }
        },
        20 => match pg_row.try_get::<usize, Option<i64>>(idx) {
            Ok(v) => v.map_or(CellValue::Null, CellValue::BigInt),
            Err(_) => {
                let raw = pg_row
                    .try_get::<usize, Option<&str>>(idx)
                    .ok()
                    .flatten()
                    .map(|s| s.as_bytes());
                decode_cell_with_cap(raw, oid, cap)
            }
        },
        700 => match pg_row.try_get::<usize, Option<f32>>(idx) {
            Ok(v) => v.map_or(CellValue::Null, CellValue::Float),
            Err(_) => {
                let raw = pg_row
                    .try_get::<usize, Option<&str>>(idx)
                    .ok()
                    .flatten()
                    .map(|s| s.as_bytes());
                decode_cell_with_cap(raw, oid, cap)
            }
        },
        701 => match pg_row.try_get::<usize, Option<f64>>(idx) {
            Ok(v) => v.map_or(CellValue::Null, CellValue::Double),
            Err(_) => {
                let raw = pg_row
                    .try_get::<usize, Option<&str>>(idx)
                    .ok()
                    .flatten()
                    .map(|s| s.as_bytes());
                decode_cell_with_cap(raw, oid, cap)
            }
        },
        17 => {
            if let Ok(Some(b)) = pg_row.try_get::<usize, Option<&[u8]>>(idx) {
                CellValue::Bytea(Bytes::copy_from_slice(b))
            } else {
                let raw = pg_row
                    .try_get::<usize, Option<&str>>(idx)
                    .ok()
                    .flatten()
                    .map(|s| s.as_bytes());
                decode_cell_with_cap(raw, oid, cap)
            }
        }
        1082 => match pg_row.try_get::<usize, Option<chrono::NaiveDate>>(idx) {
            Ok(v) => v.map_or(CellValue::Null, CellValue::Date),
            Err(_) => {
                let raw = pg_row
                    .try_get::<usize, Option<&str>>(idx)
                    .ok()
                    .flatten()
                    .map(|s| s.as_bytes());
                decode_cell_with_cap(raw, oid, cap)
            }
        },
        1083 => match pg_row.try_get::<usize, Option<chrono::NaiveTime>>(idx) {
            Ok(v) => v.map_or(CellValue::Null, CellValue::Time),
            Err(_) => {
                let raw = pg_row
                    .try_get::<usize, Option<&str>>(idx)
                    .ok()
                    .flatten()
                    .map(|s| s.as_bytes());
                decode_cell_with_cap(raw, oid, cap)
            }
        },
        1114 => match pg_row.try_get::<usize, Option<chrono::NaiveDateTime>>(idx) {
            Ok(v) => v.map_or(CellValue::Null, CellValue::Timestamp),
            Err(_) => {
                let raw = pg_row
                    .try_get::<usize, Option<&str>>(idx)
                    .ok()
                    .flatten()
                    .map(|s| s.as_bytes());
                decode_cell_with_cap(raw, oid, cap)
            }
        },
        1184 => {
            if let Ok(Some(dt)) =
                pg_row.try_get::<usize, Option<chrono::DateTime<chrono::Utc>>>(idx)
            {
                CellValue::TimestampTz(dt)
            } else if let Ok(Some(s)) = pg_row.try_get::<usize, Option<&str>>(idx) {
                decode_cell_with_cap(Some(s.as_bytes()), oid, cap)
            } else {
                CellValue::Null
            }
        }
        114 => {
            if let Ok(Some(v)) = pg_row.try_get::<usize, Option<serde_json::Value>>(idx) {
                CellValue::Json(Bytes::copy_from_slice(v.to_string().as_bytes()))
            } else if let Ok(Some(s)) = pg_row.try_get::<usize, Option<&str>>(idx) {
                CellValue::Json(Bytes::copy_from_slice(s.as_bytes()))
            } else {
                CellValue::Null
            }
        }
        3802 => {
            if let Ok(Some(v)) = pg_row.try_get::<usize, Option<serde_json::Value>>(idx) {
                CellValue::Jsonb(Bytes::copy_from_slice(v.to_string().as_bytes()))
            } else if let Ok(Some(s)) = pg_row.try_get::<usize, Option<&str>>(idx) {
                CellValue::Jsonb(Bytes::copy_from_slice(s.as_bytes()))
            } else {
                CellValue::Null
            }
        }
        _ => {
            let raw = pg_row
                .try_get::<usize, Option<&str>>(idx)
                .ok()
                .flatten()
                .map(|s| s.as_bytes())
                .or_else(|| pg_row.try_get::<usize, Option<&[u8]>>(idx).ok().flatten());
            let cv = if let Some(b) = raw {
                decode_cell_with_cap(Some(b), oid, cap)
            } else if let Ok(Some(s)) = pg_row.try_get::<usize, Option<String>>(idx) {
                CellValue::Text(Bytes::copy_from_slice(s.as_bytes()))
            } else {
                match pg_row.try_get::<usize, Option<&str>>(idx) {
                    Ok(None) => CellValue::Null,
                    _ => CellValue::Null,
                }
            };
            match oid {
                1007 | 1009 | 1016 | 1000 | 1001 | 1005 | 1021 | 1022 | 1231 | 2951 | 199
                | 3807 => match cv {
                    CellValue::Text(b) => CellValue::Array(b),
                    other => other,
                },
                _ => cv,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_ident_escapes() {
        assert_eq!(quote_ident("foo"), "\"foo\"");
        assert_eq!(quote_ident("a\"b"), "\"a\"\"b\"");
    }

    #[test]
    fn declare_does_not_rewrite_sql() {
        // Sanity: user SQL must not be mutated with LIMIT/OFFSET by the portal helper.
        // This is a compile-time property — the format string is `DECLARE ... CURSOR FOR {user_sql}`.
        let user_sql = "SELECT * FROM t WHERE x > 5 ORDER BY y";
        let qname = quote_ident("my_cur");
        let declare = format!("DECLARE {qname} CURSOR FOR {user_sql}");
        assert!(declare.contains(user_sql));
        assert!(!declare.contains("LIMIT"));
        assert!(!declare.contains("OFFSET"));
    }
}
