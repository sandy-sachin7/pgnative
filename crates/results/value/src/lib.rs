//! Bounded PostgreSQL value and row representation.
//!
//! [`CellValue`] intentionally preserves type information rather than eagerly
//! converting every value to a `String` (see `AGENTS.md` §19). This crate is
//! independent of `egui` and of `tokio-postgres` so it can be unit-tested and
//! reused by export, editing, and rendering paths.

use bigdecimal::BigDecimal;
use bytes::Bytes;
use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime};

/// A single cell value from a query result.
///
/// Large text / JSON / bytea values are stored behind [`Bytes`] to avoid
/// needless duplication during rendering.
#[derive(Debug, Clone, PartialEq)]
pub enum CellValue {
    /// SQL `NULL`.
    Null,
    /// `bool`.
    Bool(bool),
    /// 2-byte integer.
    SmallInt(i16),
    /// 4-byte integer.
    Int(i32),
    /// 8-byte integer.
    BigInt(i64),
    /// `real` (4-byte float).
    Float(f32),
    /// `double precision` (8-byte float).
    Double(f64),
    /// `numeric` / `decimal`.
    Numeric(BigDecimal),
    /// Text-like types.
    Text(Bytes),
    /// `bytea`.
    Bytea(Bytes),
    /// `date`.
    Date(NaiveDate),
    /// `time` / `time without time zone`.
    Time(NaiveTime),
    /// `timestamp` / `timestamp without time zone`.
    Timestamp(NaiveDateTime),
    /// `timestamptz` / `timestamp with time zone`, stored as UTC.
    TimestampTz(DateTime<chrono::Utc>),
    /// `uuid`.
    Uuid(uuid::Uuid),
    /// `json` (raw text kept verbatim).
    Json(Bytes),
    /// `jsonb`.
    Jsonb(Bytes),
    /// Any array type; stored as a rendered text form for display.
    Array(Bytes),
    /// Enum values.
    Enum(Bytes),
    /// Any other / unknown type, kept as raw text bytes.
    Other(Bytes),
}

impl CellValue {
    /// A cheap boolean covering `NULL`.
    #[must_use]
    pub const fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    /// Render the value as a `String` for display and export.
    ///
    /// This is intentionally not called eagerly for every cell; rendering
    /// inspects [`CellValue`] directly. It exists for small / scalar contexts
    /// (tests, export passthrough) where a single string is acceptable.
    #[must_use]
    pub fn to_display_string(&self) -> String {
        match self {
            Self::Null => "NULL".to_owned(),
            Self::Bool(b) => b.to_string(),
            Self::SmallInt(v) => v.to_string(),
            Self::Int(v) => v.to_string(),
            Self::BigInt(v) => v.to_string(),
            Self::Float(v) => v.to_string(),
            Self::Double(v) => v.to_string(),
            Self::Numeric(v) => v.to_string(),
            Self::Text(b) | Self::Json(b) | Self::Jsonb(b) | Self::Array(b)
            | Self::Enum(b) | Self::Other(b) => String::from_utf8_lossy(b).into_owned(),
            Self::Bytea(b) => hex_encode(b),
            Self::Date(d) => d.to_string(),
            Self::Time(t) => t.to_string(),
            Self::Timestamp(t) => t.to_string(),
            Self::TimestampTz(t) => t.to_rfc3339(),
            Self::Uuid(u) => u.to_string(),
        }
    }

    /// Whether this value is textual (right-alignment / styling decisions).
    #[must_use]
    pub const fn is_textual(&self) -> bool {
        matches!(
            self,
            Self::Text(_) | Self::Json(_) | Self::Jsonb(_) | Self::Array(_) | Self::Enum(_)
                | Self::Other(_) | Self::Bytea(_)
        )
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// A single row of a query result.
///
/// Stores values aligned with the result's column metadata (kept separately by
/// the caller).
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// 0-based index of this row within the overall result stream, if known.
    pub index: Option<u64>,
    /// Cell values in column order.
    pub cells: Vec<CellValue>,
}

impl Row {
    /// Build a row without a known index.
    #[must_use]
    pub fn new(cells: Vec<CellValue>) -> Self {
        Self { index: None, cells }
    }
}
