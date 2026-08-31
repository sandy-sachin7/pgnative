//! Native PG cancellation — real `CancelRequest` on a separate connection.
//! Implements AGENTS.md §11: cancellation means `CancelToken.cancel_query`, not drop.

use std::time::Duration;
use thiserror::Error;
use tracing::{debug, warn};

#[derive(Debug, Error)]
pub enum CancelError {
    #[error("cancel token not available (not connected)")]
    NoToken,
    #[error("cancel failed: {0}")]
    Failed(String),
    #[error("cancel timed out after {0:?}")]
    Timeout(Duration),
}

/// Outcome of a cancel attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelOutcome {
    Sent,
    AlreadyFinished,
    FailedRequiresReconnect,
    NotConnected,
}

/// Abstraction over `tokio_postgres::CancelToken` so execution can cancel
/// without depending on driver types in tests.
#[async_trait::async_trait]
pub trait Canceller: Send + Sync {
    async fn cancel(&self) -> Result<CancelOutcome, CancelError>;
}

/// `tokio_postgres::CancelToken`-backed canceller (used in production).
/// The token is obtained at `Client::connect` via `client.cancel_token()`.
pub struct TokenCanceller {
    // Stored as opaque `tokio_postgres::CancelToken` — we box it to avoid
    // exposing the type in the public API of this crate when compiled
    // without the `tokio-postgres` feature in tests.
    #[cfg(feature = "postgres")]
    token: tokio_postgres::CancelToken,
}

#[cfg(feature = "postgres")]
impl TokenCanceller {
    #[must_use]
    pub fn new(token: tokio_postgres::CancelToken) -> Self {
        Self { token }
    }
}

#[cfg(feature = "postgres")]
#[async_trait::async_trait]
impl Canceller for TokenCanceller {
    async fn cancel(&self) -> Result<CancelOutcome, CancelError> {
        // `cancel_query` opens a short-lived connection to `host:port`
        // and sends `CancelRequest { pid, secret_key }`.
        // On success PG replies `57014 query_canceled` to the target backend.
        match tokio::time::timeout(
            Duration::from_secs(3),
            self.token.cancel_query(tokio_postgres::NoTls),
        )
        .await
        {
            Ok(Ok(())) => {
                debug!("CancelRequest sent");
                Ok(CancelOutcome::Sent)
            }
            Ok(Err(e)) => {
                warn!(error = %e, "CancelRequest failed");
                // Map network/auth errors to `FailedRequiresReconnect`.
                Err(CancelError::Failed(e.to_string()))
            }
            Err(_) => Err(CancelError::Timeout(Duration::from_secs(3))),
        }
    }
}

/// No-op canceller for tests / already-finished queries.
pub struct NoopCanceller;

#[async_trait::async_trait]
impl Canceller for NoopCanceller {
    async fn cancel(&self) -> Result<CancelOutcome, CancelError> {
        Ok(CancelOutcome::AlreadyFinished)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn noop_returns_already_finished() {
        let c = NoopCanceller;
        assert_eq!(c.cancel().await.unwrap(), CancelOutcome::AlreadyFinished);
    }
}
