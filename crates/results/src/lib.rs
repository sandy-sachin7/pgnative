//! pgNative result engine: stream → bounded store → viewport.
//!
//! Results are never `Vec<Row>`; the pipeline applies backpressure, per-cell
//! caps, and eviction budgets so 500k+ rows stay responsive.

/// Safe inline-edit SQL generation (parameterized, PK-gated).
pub mod edit;
/// CSV/JSON/SQL-INSERT encoders over buffered rows.
pub mod export;
/// Server-side cursor (`DECLARE`/`FETCH`) windows without `LIMIT` rewrites.
pub mod portal;
/// Bounded in-memory row window with row + byte budgets.
pub mod store;
/// Async row stream with backpressure and per-cell truncation.
pub mod stream;
/// Purpose-built table-browsing queries (parameterized, paginated).
pub mod table_browser;
/// PostgreSQL value representation (NULL-aware, type-preserving).
pub mod value;
/// Scroll-window math over the bounded store.
pub mod viewport;
