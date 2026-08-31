//! Async query execution + streaming — app-owned types, never expose `tokio-postgres` Row.
//! Implements AGENTS.md §10, §11.

use std::time::{Duration, Instant};

use pgnative_db_connection::{PgError, QueryId, SessionHealth, TxState};
use pgnative_results_value::{CellValue, Row};
use thiserror::Error;
use tracing::{debug, warn};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Public types (§10)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct QueryRequest {
    pub query_id: QueryId,
    pub sql: String,
}

impl QueryRequest {
    #[must_use]
    pub fn new(sql: impl Into<String>) -> Self {
        Self {
            query_id: QueryId(Uuid::new_v4()),
            sql: sql.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum ExecutionState {
    Queued,
    Executing {
        start: Instant,
    },
    Streaming {
        start: Instant,
        first_row: Instant,
        rows: u64,
    },
    Cancelling,
    Completed {
        rows: u64,
        elapsed: Duration,
    },
    Failed(PgError),
    Cancelled,
}

#[derive(Debug, Clone)]
pub struct Execution {
    pub query_id: QueryId,
    pub state: ExecutionState,
    pub tx: TxState,
    pub health: SessionHealth,
}

#[derive(Debug, Error)]
pub enum ExecutionError {
    #[error("not connected")]
    NotConnected,
    #[error("query failed: {0}")]
    Pg(#[from] PgError),
    #[error("cancel failed: {0}")]
    Cancel(String),
}

// ---------------------------------------------------------------------------
// ExecutionHandle — caller controls the stream + cancellation
// ---------------------------------------------------------------------------

/// Handle returned to `crates/app` orchestration after `execute()`.
pub struct ExecutionHandle {
    pub query_id: QueryId,
    pub start: Instant,
    // In production this holds `tokio::task::JoinHandle<()>` + `mpsc::Receiver<Row>` + `CancelToken`.
    // For WU2 we keep a minimal in-memory handle that tests can drive.
}

impl ExecutionHandle {
    #[must_use]
    pub fn new(query_id: QueryId) -> Self {
        Self {
            query_id,
            start: Instant::now(),
        }
    }
}

// ---------------------------------------------------------------------------
// Executor trait (§8 channel model)
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
pub trait Executor: Send + Sync {
    async fn execute(&self, req: QueryRequest) -> Result<ExecutionHandle, ExecutionError>;
    async fn cancel(&self, query_id: QueryId) -> Result<(), ExecutionError>;
}

/// In-memory executor for tests / offline — does not touch PG.
pub struct InMemoryExecutor;

#[async_trait::async_trait]
impl Executor for InMemoryExecutor {
    async fn execute(&self, req: QueryRequest) -> Result<ExecutionHandle, ExecutionError> {
        // §35: never log raw SQL (may contain INSERT secrets); only log id/len
        debug!(
            query_id = %req.query_id,
            sql_len = req.sql.len(),
            "execute (in-memory)"
        );
        Ok(ExecutionHandle::new(req.query_id))
    }

    async fn cancel(&self, _query_id: QueryId) -> Result<(), ExecutionError> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers: map `tokio_postgres::Error` → `PgError` (real impl in `db::connection`)
// ---------------------------------------------------------------------------

#[must_use]
pub fn map_pg_error(msg: String, sqlstate: Option<String>) -> PgError {
    let is_cancel = sqlstate.as_deref() == Some("57014");
    PgError {
        sqlstate,
        message: msg,
        detail: None,
        hint: None,
        position: None,
        is_cancel,
    }
}

/// Decode a `tokio_postgres::Row` into `Row` — placeholder; real impl
/// iterates `row.columns()` and matches `type_oid` → `CellValue` via
/// `postgres-types::FromSql`. WU3 (`results/stream`) owns the full mapper.
#[must_use]
pub fn decode_row_mock(cells: Vec<CellValue>) -> Row {
    Row::new(cells)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_cancel_sqlstate() {
        let e = map_pg_error("canceling statement".into(), Some("57014".into()));
        assert!(e.is_cancel());
    }

    #[tokio::test]
    async fn in_memory_executor_roundtrip() {
        let exec = InMemoryExecutor;
        let req = QueryRequest::new("SELECT 1");
        let h = exec.execute(req).await.unwrap();
        assert!(!h.query_id.0.is_nil());
    }
}
