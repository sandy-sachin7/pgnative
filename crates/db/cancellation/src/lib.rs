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

/// TLS mode for cancellation — mirrors `pgnative_db_connection::SslMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SslMode {
    Disable,
    #[default]
    Prefer,
    Require,
    VerifyCa,
    VerifyFull,
}

/// `tokio_postgres::CancelToken`-backed canceller (used in production).
/// The token is obtained at `Client::connect` via `client.cancel_token()`.
pub struct TokenCanceller {
    #[cfg(feature = "postgres")]
    token: tokio_postgres::CancelToken,
    ssl_mode: SslMode,
    ssl_root_cert: Option<String>,
}

#[cfg(feature = "postgres")]
impl TokenCanceller {
    #[must_use]
    pub fn new(token: tokio_postgres::CancelToken) -> Self {
        Self {
            token,
            ssl_mode: SslMode::Disable,
            ssl_root_cert: None,
        }
    }

    #[must_use]
    pub fn with_tls(
        token: tokio_postgres::CancelToken,
        ssl_mode: SslMode,
        ssl_root_cert: Option<String>,
    ) -> Self {
        Self {
            token,
            ssl_mode,
            ssl_root_cert,
        }
    }
}

#[cfg(feature = "postgres")]
fn build_rustls_config(
    ssl_mode: SslMode,
    root_cert_pem: Option<&str>,
) -> Result<rustls::ClientConfig, String> {
    let mut roots = rustls::RootCertStore::empty();
    if let Some(pem) = root_cert_pem {
        let mut added = 0usize;
        for chunk in pem.split("-----BEGIN CERTIFICATE-----") {
            if let Some(end) = chunk.find("-----END CERTIFICATE-----") {
                let b64 = chunk[..end]
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .collect::<String>();
                if let Some(der) = decode_base64(&b64) {
                    let cert = rustls::pki_types::CertificateDer::from(der);
                    if roots.add(cert).is_ok() {
                        added += 1;
                    }
                }
            }
        }
        if added == 0 && !pem.as_bytes().is_empty() {
            return Err("invalid PEM: no certificates found".to_string());
        }
    } else {
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    }
    let builder = rustls::ClientConfig::builder();
    let config = match ssl_mode {
        SslMode::VerifyFull | SslMode::VerifyCa | SslMode::Require | SslMode::Prefer => {
            builder.with_root_certificates(roots).with_no_client_auth()
        }
        SslMode::Disable => unreachable!("Disable must use NoTls"),
    };
    Ok(config)
}

#[cfg(feature = "postgres")]
fn decode_base64(s: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(s.as_bytes())
        .ok()
}

#[cfg(feature = "postgres")]
#[async_trait::async_trait]
impl Canceller for TokenCanceller {
    async fn cancel(&self) -> Result<CancelOutcome, CancelError> {
        // `cancel_query` opens a short-lived connection to `host:port`
        // and sends `CancelRequest { pid, secret_key }` using the same TLS
        // mode as the original session — never fallback to NoTls on config
        // error (C1: would leak pid/secret in plaintext).
        if self.ssl_mode == SslMode::Disable {
            return match tokio::time::timeout(
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
                    Err(CancelError::Failed(e.to_string()))
                }
                Err(_) => Err(CancelError::Timeout(Duration::from_secs(3))),
            };
        }
        let cfg = build_rustls_config(self.ssl_mode, self.ssl_root_cert.as_deref())
            .map_err(CancelError::Failed)?;
        let tls = tokio_postgres_rustls::MakeRustlsConnect::new(cfg);
        match tokio::time::timeout(Duration::from_secs(3), self.token.cancel_query(tls)).await {
            Ok(Ok(())) => {
                debug!("CancelRequest sent (TLS)");
                Ok(CancelOutcome::Sent)
            }
            Ok(Err(e)) => {
                warn!(error = %e, "CancelRequest failed (TLS)");
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
