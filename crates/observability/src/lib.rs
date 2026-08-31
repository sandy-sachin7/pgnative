//! Local diagnostics — tracing spans per AGENTS §55 (debug-only, no telemetry).
use tracing::{info_span, Span};

#[must_use]
pub fn span_startup() -> Span {
    info_span!("startup")
}

#[must_use]
pub fn span_introspection(conn: &str) -> Span {
    info_span!("introspection", connection = conn)
}

#[must_use]
pub fn span_query(query_id: &str) -> Span {
    info_span!("query", query_id = query_id.to_string())
}

pub fn record_rows_received(rows: u64) {
    tracing::debug!(rows, "rows received");
}

pub fn record_cancel_latency(ms: u64) {
    tracing::debug!(cancel_latency_ms = ms, "cancel");
}
