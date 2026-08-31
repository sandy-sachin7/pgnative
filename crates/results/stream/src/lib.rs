//! PG row stream → CellValue — bounded, back-pressured, egui-independent.
//! Implements AGENTS.md §15 (stream part) per plan C1-C2, C8.

use bytes::Bytes;
use pgnative_results_value::{CellValue, Row};
use thiserror::Error;

// ---------------------------------------------------------------------------
// Config / events
// ---------------------------------------------------------------------------

/// Per-cell byte cap (§19) — large text/json/bytea truncated at stream.
/// Budget: 64 KiB per cell vs 64 MiB store (§15) and channel_cap=16 (§C2)
/// mitigates worst-case memory; theoretical 10×64 KiB×10 cols ≈ 6.4 MiB per
/// batch is bounded by store eviction.
pub const PER_CELL_CAP: usize = 64 * 1024;

/// Render truncation cap (viewport shows affordance beyond this).
pub const RENDER_CAP: usize = 2 * 1024;

#[inline]
fn is_utf8_boundary(bytes: &[u8], idx: usize) -> bool {
    if idx == 0 || idx >= bytes.len() {
        return true;
    }
    // Continuation bytes 10xxxxxx are not boundaries
    (bytes[idx] & 0b1100_0000) != 0b1000_0000
}

#[derive(Debug, Clone)]
pub struct StreamConfig {
    pub per_cell_cap: usize,
    pub batch_size: usize,
    pub channel_cap: usize,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            per_cell_cap: PER_CELL_CAP,
            batch_size: 64,
            channel_cap: 16,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ColumnMeta {
    pub name: String,
    pub pg_type_oid: u32,
    pub type_name: String,
    pub nullable: bool,
}

#[derive(Debug)]
pub enum StreamEvent {
    Meta(Vec<ColumnMeta>),
    Batch(Vec<Row>),
    Error(StreamError),
    Complete { rows: u64, elapsed_ms: u64 },
    Cancelled,
}

#[derive(Debug, Error)]
pub enum StreamError {
    #[error("pg error: {0}")]
    Pg(String),
    #[error("decode error: {0}")]
    Decode(String),
}

// ---------------------------------------------------------------------------
// Channel helpers — backpressure is `tokio::sync::mpsc::bounded(channel_cap)`.
// Producer `send().await` blocks when consumer is slow (plan C2).
// ---------------------------------------------------------------------------

/// Create a bounded channel for `StreamEvent` with backpressure.
#[must_use]
pub fn channel(
    cfg: &StreamConfig,
) -> (
    tokio::sync::mpsc::Sender<StreamEvent>,
    tokio::sync::mpsc::Receiver<StreamEvent>,
) {
    tokio::sync::mpsc::channel(cfg.channel_cap)
}

// ---------------------------------------------------------------------------
// Value mapping — oid → CellValue (text protocol v1, per CellValue enum)
// ---------------------------------------------------------------------------

/// Map raw text bytes + oid to `CellValue` with per-cell cap.
///
/// Handles OIDs: 16 bool, 21 int2, 23 int4, 20 int8, 700 float4, 701 float8,
/// 1700 numeric, 1082 date, 1083 time, 1114 timestamp, 1184 timestamptz,
/// 2950 uuid, 114 json, 3802 jsonb, 17 bytea, 25/1043/1042 text/varchar/bpchar,
/// arrays (1007 etc.) and falls back to `Text`/`Other`.
#[must_use]
pub fn decode_cell(raw: Option<&[u8]>, oid: u32) -> CellValue {
    decode_cell_with_cap(raw, oid, PER_CELL_CAP)
}

/// Same as `decode_cell` but with explicit cap (for testing / config).
#[must_use]
pub fn decode_cell_with_cap(raw: Option<&[u8]>, oid: u32, cap: usize) -> CellValue {
    let Some(bytes) = raw else {
        return CellValue::Null;
    };
    // Truncate large values at stream — UTF-8 safe at char boundary (C8).
    // TODO(§19): truncated jsonb/bytea coerced to Text; preserve variant
    // after truncation if downstream needs type fidelity.
    if bytes.len() > cap {
        let mut end = cap;
        while end > 0 && !is_utf8_boundary(bytes, end) {
            end -= 1;
        }
        if end == 0 {
            // cap landed inside first char (cap=1 with multi-byte); return empty
            // rather than reintroducing invalid UTF-8 via max(1) (crates/results/stream/src/lib.rs:107)
            return CellValue::Text(Bytes::new());
        }
        let truncated = &bytes[..end.min(bytes.len())];
        return CellValue::Text(Bytes::copy_from_slice(truncated));
    }
    match oid {
        16 => match bytes {
            b"t" | b"true" | b"True" | b"TRUE" => CellValue::Bool(true),
            b"f" | b"false" | b"False" | b"FALSE" => CellValue::Bool(false),
            _ => CellValue::Bool(false),
        },
        21 => String::from_utf8_lossy(bytes)
            .parse::<i16>()
            .map(CellValue::SmallInt)
            .unwrap_or_else(|_| CellValue::Text(Bytes::copy_from_slice(bytes))),
        23 => String::from_utf8_lossy(bytes)
            .parse::<i32>()
            .map(CellValue::Int)
            .unwrap_or_else(|_| CellValue::Text(Bytes::copy_from_slice(bytes))),
        20 => String::from_utf8_lossy(bytes)
            .parse::<i64>()
            .map(CellValue::BigInt)
            .unwrap_or_else(|_| CellValue::Text(Bytes::copy_from_slice(bytes))),
        700 => String::from_utf8_lossy(bytes)
            .parse::<f32>()
            .map(CellValue::Float)
            .unwrap_or_else(|_| CellValue::Text(Bytes::copy_from_slice(bytes))),
        701 => String::from_utf8_lossy(bytes)
            .parse::<f64>()
            .map(CellValue::Double)
            .unwrap_or_else(|_| CellValue::Text(Bytes::copy_from_slice(bytes))),
        1700 => {
            // numeric — preserve as Numeric via BigDecimal; fallback to Text
            let s = String::from_utf8_lossy(bytes);
            s.parse::<bigdecimal::BigDecimal>()
                .map(CellValue::Numeric)
                .unwrap_or_else(|_| CellValue::Text(Bytes::copy_from_slice(bytes)))
        }
        1082 => {
            let s = String::from_utf8_lossy(bytes);
            s.parse::<chrono::NaiveDate>()
                .map(CellValue::Date)
                .unwrap_or_else(|_| CellValue::Text(Bytes::copy_from_slice(bytes)))
        }
        1083 => {
            let s = String::from_utf8_lossy(bytes);
            s.parse::<chrono::NaiveTime>()
                .map(CellValue::Time)
                .unwrap_or_else(|_| CellValue::Text(Bytes::copy_from_slice(bytes)))
        }
        1114 => {
            let s = String::from_utf8_lossy(bytes);
            // PG timestamp without tz: "2024-01-02 03:04:05" or ISO
            s.parse::<chrono::NaiveDateTime>()
                .map(CellValue::Timestamp)
                .unwrap_or_else(|_| CellValue::Text(Bytes::copy_from_slice(bytes)))
        }
        1184 => {
            let s = String::from_utf8_lossy(bytes);
            // timestamptz — parse as RFC3339 or "YYYY-MM-DD HH:MM:SS+TZ"
            if let Ok(dt) = s.parse::<chrono::DateTime<chrono::Utc>>() {
                CellValue::TimestampTz(dt)
            } else if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&s) {
                CellValue::TimestampTz(dt.with_timezone(&chrono::Utc))
            } else {
                // Try "2024-01-02 03:04:05+00" form
                s.parse::<chrono::NaiveDateTime>()
                    .map(|nd| nd.and_utc())
                    .map(CellValue::TimestampTz)
                    .unwrap_or_else(|_| CellValue::Text(Bytes::copy_from_slice(bytes)))
            }
        }
        114 => CellValue::Json(Bytes::copy_from_slice(bytes)),
        3802 => CellValue::Jsonb(Bytes::copy_from_slice(bytes)),
        2950 => String::from_utf8_lossy(bytes)
            .parse::<uuid::Uuid>()
            .map(CellValue::Uuid)
            .unwrap_or_else(|_| CellValue::Text(Bytes::copy_from_slice(bytes))),
        17 => {
            // bytea text format is \xDEADBEEF hex — keep as Bytea bytes
            if bytes.starts_with(b"\\x") {
                // hex decode
                let hex = &bytes[2..];
                let decoded = hex_decode(hex);
                CellValue::Bytea(Bytes::from(decoded))
            } else {
                CellValue::Bytea(Bytes::copy_from_slice(bytes))
            }
        }
        // Text-like
        25 | 1043 | 1042 | 18 | 19 | 26 | 28 => CellValue::Text(Bytes::copy_from_slice(bytes)),
        // Arrays — keep rendered text form
        1007 | 1009 | 1016 | 1000 | 1001 | 1005 | 1021 | 1022 | 1231 | 2951 | 199 | 3807 => {
            CellValue::Array(Bytes::copy_from_slice(bytes))
        }
        _ => CellValue::Text(Bytes::copy_from_slice(bytes)),
    }
}

fn hex_decode(hex: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(hex.len() / 2);
    for chunk in hex.chunks(2) {
        if chunk.len() == 2 {
            let hi = hex_val(chunk[0]);
            let lo = hex_val(chunk[1]);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h << 4) | l);
            }
        }
    }
    out
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Decode a row from parallel slices of raw bytes + oids into a `Row`.
#[must_use]
pub fn decode_row(raw_cells: Vec<Option<Vec<u8>>>, oids: &[u32], cap: usize) -> Row {
    let cells = raw_cells
        .into_iter()
        .zip(oids.iter().copied().chain(std::iter::repeat(25)))
        .map(|(raw, oid)| decode_cell_with_cap(raw.as_deref(), oid, cap))
        .collect();
    Row::new(cells)
}

// ---------------------------------------------------------------------------
// Streaming driver — `tokio-postgres::query_raw` → `StreamEvent` with backpressure
// ---------------------------------------------------------------------------

/// Drive a `tokio_postgres` row stream into a bounded channel.
///
/// Sends `Meta` first, then `Batch` chunks of `config.batch_size` rows.
/// Each `send().await` applies backpressure when `channel_cap` is full.
/// Emits `Complete` on success, `Error` on PG error, respects cancellation
/// by dropping the stream (caller drops `JoinHandle` / cancels token).
///
/// This is the canonical `query_raw → CellValue` wiring (C1-C2).
pub async fn drive_stream<S>(
    mut row_stream: S,
    columns: Vec<ColumnMeta>,
    config: StreamConfig,
    tx: tokio::sync::mpsc::Sender<StreamEvent>,
) -> Result<(), StreamError>
where
    S: futures::Stream<Item = Result<tokio_postgres::Row, tokio_postgres::Error>> + Unpin,
{
    let start = std::time::Instant::now();
    // Send meta — if receiver closed, stop early (backpressure / cancel)
    if tx.send(StreamEvent::Meta(columns.clone())).await.is_err() {
        return Ok(());
    }
    let oids: Vec<u32> = columns.iter().map(|c| c.pg_type_oid).collect();
    let mut batch = Vec::with_capacity(config.batch_size);
    let mut rows: u64 = 0;

    use futures::StreamExt as _;
    while let Some(res) = row_stream.next().await {
        match res {
            Ok(pg_row) => {
                let mut cells = Vec::with_capacity(oids.len());
                for (i, oid) in oids.iter().enumerate() {
                    // PG may send binary for scalar types (int2/int4/int8/float/bool)
                    // when using `query_raw` with prepared statement — `get::<&str>`
                    // then panics with "error deserializing column".
                    // Try typed binary decode first, fall back to text `&str`.
                    let cv = match oid {
                        16 => match pg_row.try_get::<usize, Option<bool>>(i) {
                            Ok(v) => v.map_or(CellValue::Null, CellValue::Bool),
                            Err(_) => {
                                let raw = pg_row
                                    .try_get::<usize, Option<&str>>(i)
                                    .ok()
                                    .flatten()
                                    .map(|s| s.as_bytes());
                                decode_cell_with_cap(raw, *oid, config.per_cell_cap)
                            }
                        },
                        21 => match pg_row.try_get::<usize, Option<i16>>(i) {
                            Ok(v) => v.map_or(CellValue::Null, CellValue::SmallInt),
                            Err(_) => {
                                let raw = pg_row
                                    .try_get::<usize, Option<&str>>(i)
                                    .ok()
                                    .flatten()
                                    .map(|s| s.as_bytes());
                                decode_cell_with_cap(raw, *oid, config.per_cell_cap)
                            }
                        },
                        23 => match pg_row.try_get::<usize, Option<i32>>(i) {
                            Ok(v) => v.map_or(CellValue::Null, CellValue::Int),
                            Err(_) => {
                                let raw = pg_row
                                    .try_get::<usize, Option<&str>>(i)
                                    .ok()
                                    .flatten()
                                    .map(|s| s.as_bytes());
                                decode_cell_with_cap(raw, *oid, config.per_cell_cap)
                            }
                        },
                        20 => match pg_row.try_get::<usize, Option<i64>>(i) {
                            Ok(v) => v.map_or(CellValue::Null, CellValue::BigInt),
                            Err(_) => {
                                let raw = pg_row
                                    .try_get::<usize, Option<&str>>(i)
                                    .ok()
                                    .flatten()
                                    .map(|s| s.as_bytes());
                                decode_cell_with_cap(raw, *oid, config.per_cell_cap)
                            }
                        },
                        700 => match pg_row.try_get::<usize, Option<f32>>(i) {
                            Ok(v) => v.map_or(CellValue::Null, CellValue::Float),
                            Err(_) => {
                                let raw = pg_row
                                    .try_get::<usize, Option<&str>>(i)
                                    .ok()
                                    .flatten()
                                    .map(|s| s.as_bytes());
                                decode_cell_with_cap(raw, *oid, config.per_cell_cap)
                            }
                        },
                        701 => match pg_row.try_get::<usize, Option<f64>>(i) {
                            Ok(v) => v.map_or(CellValue::Null, CellValue::Double),
                            Err(_) => {
                                let raw = pg_row
                                    .try_get::<usize, Option<&str>>(i)
                                    .ok()
                                    .flatten()
                                    .map(|s| s.as_bytes());
                                decode_cell_with_cap(raw, *oid, config.per_cell_cap)
                            }
                        },
                        17 => {
                            if let Ok(Some(b)) = pg_row.try_get::<usize, Option<&[u8]>>(i) {
                                CellValue::Bytea(Bytes::copy_from_slice(b))
                            } else {
                                let raw = pg_row
                                    .try_get::<usize, Option<&str>>(i)
                                    .ok()
                                    .flatten()
                                    .map(|s| s.as_bytes());
                                decode_cell_with_cap(raw, *oid, config.per_cell_cap)
                            }
                        }
                        _ => {
                            let raw = pg_row
                                .try_get::<usize, Option<&str>>(i)
                                .ok()
                                .flatten()
                                .map(|s| s.as_bytes())
                                .or_else(|| {
                                    pg_row.try_get::<usize, Option<&[u8]>>(i).ok().flatten()
                                });
                            decode_cell_with_cap(raw, *oid, config.per_cell_cap)
                        }
                    };
                    cells.push(cv);
                }
                batch.push(Row::new(cells));
                rows += 1;
                if batch.len() >= config.batch_size {
                    let to_send =
                        std::mem::replace(&mut batch, Vec::with_capacity(config.batch_size));
                    if tx.send(StreamEvent::Batch(to_send)).await.is_err() {
                        // Receiver dropped — cancellation / viewport closed
                        return Ok(());
                    }
                }
            }
            Err(e) => {
                let _ = tx
                    .send(StreamEvent::Error(StreamError::Pg(e.to_string())))
                    .await;
                return Ok(());
            }
        }
    }
    // Flush remainder
    if !batch.is_empty() {
        let _ = tx.send(StreamEvent::Batch(batch)).await;
    }
    let elapsed_ms = start.elapsed().as_millis() as u64;
    let _ = tx.send(StreamEvent::Complete { rows, elapsed_ms }).await;
    Ok(())
}

/// Convenience: spawn `drive_stream` as a Tokio task with cancellation.
///
/// Returns `JoinHandle` — droppping/aborting it stops the stream and the
/// bounded channel will be closed (C2). Caller should also trigger
/// `db::cancellation::cancel` for native PG cancellation separately.
pub fn spawn_drive<S>(
    row_stream: S,
    columns: Vec<ColumnMeta>,
    config: StreamConfig,
    tx: tokio::sync::mpsc::Sender<StreamEvent>,
) -> tokio::task::JoinHandle<Result<(), StreamError>>
where
    S: futures::Stream<Item = Result<tokio_postgres::Row, tokio_postgres::Error>>
        + Send
        + Unpin
        + 'static,
{
    tokio::spawn(drive_stream(row_stream, columns, config, tx))
}

/// Helper to build `ColumnMeta` from `tokio_postgres::Column`.
#[must_use]
pub fn column_meta_from_pg(col: &tokio_postgres::Column) -> ColumnMeta {
    ColumnMeta {
        name: col.name().to_owned(),
        pg_type_oid: col.type_().oid(),
        type_name: col.type_().name().to_owned(),
        nullable: true, // PG text protocol doesn't expose nullability here; schema model does
    }
}

// ---------------------------------------------------------------------------
// Testing helper: drive from in-memory rows without a live PG connection.
// Validates backpressure + batching + truncation without testcontainers.
// ---------------------------------------------------------------------------

/// Drive an iterator of raw rows (for unit tests) into the channel with
/// the same batching/backpressure semantics as the live PG path.
pub async fn drive_iter(
    rows: impl IntoIterator<Item = Vec<Option<Vec<u8>>>>,
    columns: Vec<ColumnMeta>,
    config: StreamConfig,
    tx: tokio::sync::mpsc::Sender<StreamEvent>,
) {
    let start = std::time::Instant::now();
    if tx.send(StreamEvent::Meta(columns.clone())).await.is_err() {
        return;
    }
    let oids: Vec<u32> = columns.iter().map(|c| c.pg_type_oid).collect();
    let mut batch = Vec::with_capacity(config.batch_size);
    let mut count: u64 = 0;
    for raw_row in rows {
        let row = decode_row(raw_row, &oids, config.per_cell_cap);
        batch.push(row);
        count += 1;
        if batch.len() >= config.batch_size {
            let to_send = std::mem::replace(&mut batch, Vec::with_capacity(config.batch_size));
            if tx.send(StreamEvent::Batch(to_send)).await.is_err() {
                return;
            }
        }
    }
    if !batch.is_empty() {
        let _ = tx.send(StreamEvent::Batch(batch)).await;
    }
    let elapsed_ms = start.elapsed().as_millis() as u64;
    let _ = tx
        .send(StreamEvent::Complete {
            rows: count,
            elapsed_ms,
        })
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_null() {
        assert_eq!(decode_cell(None, 25), CellValue::Null);
    }

    #[test]
    fn decode_bool() {
        assert_eq!(decode_cell(Some(b"t"), 16), CellValue::Bool(true));
        assert_eq!(decode_cell(Some(b"f"), 16), CellValue::Bool(false));
    }

    #[test]
    fn decode_int() {
        assert_eq!(decode_cell(Some(b"42"), 23), CellValue::Int(42));
    }

    #[test]
    fn decode_truncates_large() {
        let big = vec![b'a'; PER_CELL_CAP + 100];
        let v = decode_cell(Some(&big), 25);
        match v {
            CellValue::Text(b) => assert_eq!(b.len(), PER_CELL_CAP),
            _ => panic!("expected Text"),
        }
    }

    #[test]
    fn decode_numeric() {
        let v = decode_cell(Some(b"1234.567"), 1700);
        assert!(matches!(v, CellValue::Numeric(_)));
    }

    #[test]
    fn decode_uuid() {
        let v = decode_cell(Some(b"550e8400-e29b-41d4-a716-446655440000"), 2950);
        assert!(matches!(v, CellValue::Uuid(_)));
    }

    #[test]
    fn decode_json() {
        let v = decode_cell(Some(b"{\"a\":1}"), 114);
        assert!(matches!(v, CellValue::Json(_)));
        let v2 = decode_cell(Some(b"{\"a\":1}"), 3802);
        assert!(matches!(v2, CellValue::Jsonb(_)));
    }

    #[test]
    fn decode_date() {
        let v = decode_cell(Some(b"2024-01-15"), 1082);
        assert!(matches!(v, CellValue::Date(_)));
    }

    #[test]
    fn decode_byte_truncation_uses_cap_param() {
        let big = vec![b'x'; 5000];
        let v = decode_cell_with_cap(Some(&big), 25, 100);
        match v {
            CellValue::Text(b) => assert_eq!(b.len(), 100),
            _ => panic!("expected Text"),
        }
    }

    #[tokio::test]
    async fn drive_iter_batches_and_backpressure() {
        let cols = vec![
            ColumnMeta {
                name: "id".into(),
                pg_type_oid: 23,
                type_name: "int4".into(),
                nullable: false,
            },
            ColumnMeta {
                name: "name".into(),
                pg_type_oid: 25,
                type_name: "text".into(),
                nullable: true,
            },
        ];
        let cfg = StreamConfig {
            per_cell_cap: 1024,
            batch_size: 2,
            channel_cap: 2,
        };
        let (tx, mut rx) = channel(&cfg);
        let rows = (0..5).map(|i| vec![Some(format!("{i}").into_bytes()), Some(b"hello".to_vec())]);
        // Run producer concurrently: with channel_cap=2 the 3rd send would block
        // forever if we awaited drive_iter before draining rx (deadlock).
        let producer = tokio::spawn(drive_iter(rows, cols, cfg, tx));
        // Should receive Meta, then batches of 2,2,1 then Complete
        let mut batches = 0;
        let mut total_rows = 0;
        let mut saw_meta = false;
        let mut saw_complete = false;
        while let Some(ev) = rx.recv().await {
            match ev {
                StreamEvent::Meta(_) => saw_meta = true,
                StreamEvent::Batch(b) => {
                    batches += 1;
                    total_rows += b.len();
                }
                StreamEvent::Complete { rows, .. } => {
                    saw_complete = true;
                    assert_eq!(rows, 5);
                }
                _ => {}
            }
        }
        assert!(saw_meta);
        assert!(saw_complete);
        assert_eq!(total_rows, 5);
        assert_eq!(batches, 3);
        producer.await.unwrap();
    }

    #[tokio::test]
    async fn backpressure_blocks_sender_when_full() {
        let cfg = StreamConfig {
            per_cell_cap: 1024,
            batch_size: 1,
            channel_cap: 1,
        };
        let (tx, mut rx) = channel(&cfg);
        // Fill channel (cap=1) — first send succeeds, second would block
        tx.send(StreamEvent::Meta(vec![])).await.unwrap();
        // Channel is now full; try_send should fail
        assert!(tx.try_send(StreamEvent::Batch(vec![])).is_err());
        // Drain one
        rx.recv().await.unwrap();
        // Now try_send should succeed
        assert!(tx.try_send(StreamEvent::Batch(vec![])).is_ok());
    }
}
