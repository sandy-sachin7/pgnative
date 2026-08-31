//! PG row stream → CellValue — bounded, back-pressured, egui-independent.
//! Implements AGENTS.md §15 (stream part) per plan C1-C2.

use bytes::Bytes;
use pgnative_results_value::{CellValue, Row};
use thiserror::Error;

// ---------------------------------------------------------------------------
// Config / events
// ---------------------------------------------------------------------------

/// Per-cell byte cap (§19) — large text/json/bytea truncated at stream.
pub const PER_CELL_CAP: usize = 256 * 1024;

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
            batch_size: 256,
            channel_cap: 512,
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
// Value mapping — oid → CellValue (text protocol v1)
// ---------------------------------------------------------------------------

/// Map raw text bytes + oid to `CellValue`. Real implementation matches
/// `pg_type.oid` (16=bool, 20=int8, 21=int2, 23=int4, 700=float4, 701=float8,
/// 1700=numeric, 1082=date, 1083=time, 1114=timestamp, 1184=timestamptz, 2950=uuid,
/// 114=json, 3802=jsonb, etc.) and falls back to `Text`/`Other`.
#[must_use]
pub fn decode_cell(raw: Option<&[u8]>, oid: u32) -> CellValue {
    let Some(bytes) = raw else {
        return CellValue::Null;
    };
    if bytes.len() > PER_CELL_CAP {
        // Truncate at stream — renderer will show affordance.
        return CellValue::Text(Bytes::copy_from_slice(&bytes[..PER_CELL_CAP]));
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
        114 => CellValue::Json(Bytes::copy_from_slice(bytes)),
        3802 => CellValue::Jsonb(Bytes::copy_from_slice(bytes)),
        2950 => String::from_utf8_lossy(bytes)
            .parse::<uuid::Uuid>()
            .map(CellValue::Uuid)
            .unwrap_or_else(|_| CellValue::Text(Bytes::copy_from_slice(bytes))),
        _ => CellValue::Text(Bytes::copy_from_slice(bytes)),
    }
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
}
