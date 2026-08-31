//! PG connection + state machine + CancelToken.
//!
//! Implements AGENTS.md §9 (connection), §22 (transactions), §26 (SSL)
//! per ADR-0008. UI never imports `tokio-postgres` types directly —
//! this crate re-exports the application-owned identifiers.

use std::fmt;
use std::time::Instant;

use secrecy::{ExposeSecret, SecretString};
use thiserror::Error;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Identifiers
// ---------------------------------------------------------------------------

/// Application-owned identifier for a saved connection (`ConnectionId` per §10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConnectionId(pub Uuid);

impl ConnectionId {
    /// Generate a new random id.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for ConnectionId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for ConnectionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Identifier for a single query execution (`QueryId` per §10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct QueryId(pub Uuid);

impl QueryId {
    /// Generate a new random id.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl fmt::Display for QueryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

// ---------------------------------------------------------------------------
// SSL / SSH
// ---------------------------------------------------------------------------

/// PostgreSQL SSL mode — maps 1:1 to `tokio-postgres` (§26). Default `Prefer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SslMode {
    Disable,
    #[default]
    Prefer,
    Require,
    VerifyCa,
    VerifyFull,
}

impl fmt::Display for SslMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Disable => "disable",
            Self::Prefer => "prefer",
            Self::Require => "require",
            Self::VerifyCa => "verify-ca",
            Self::VerifyFull => "verify-full",
        };
        f.write_str(s)
    }
}

/// Optional SSH tunnel — kept separate from PG config per §25.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshTunnelConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    /// Private key path — not logged.
    pub private_key_path: Option<String>,
}

// ---------------------------------------------------------------------------
// Connection configuration (non-secret vs secret split per §24)
// ---------------------------------------------------------------------------

/// Non-secret part persisted in SQLite; `password` lives in OS keychain.
#[derive(Clone)]
pub struct ConnectionConfig {
    pub id: ConnectionId,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub dbname: String,
    pub username: String,
    pub ssl_mode: SslMode,
    pub ssl_root_cert: Option<String>,
    pub ssh_tunnel: Option<SshTunnelConfig>,
}

impl ConnectionConfig {
    /// Return a `Debug`-safe URL with password redacted via `url::Url`.
    #[must_use]
    pub fn sanitized_url(&self, password: Option<&SecretString>) -> String {
        // Build then sanitize — never interpolate password directly.
        let pw_present = password.is_some();
        let raw = if pw_present {
            format!(
                "postgres://{}:{}@{}:{}/{}?sslmode={}",
                self.username,
                password.unwrap().expose_secret(),
                self.host,
                self.port,
                self.dbname,
                self.ssl_mode
            )
        } else {
            format!(
                "postgres://{}@{}:{}/{}?sslmode={}",
                self.username, self.host, self.port, self.dbname, self.ssl_mode
            )
        };
        sanitize_url(&raw)
    }
}

// Manual Debug that never prints password (password not even stored here).
impl fmt::Debug for ConnectionConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectionConfig")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("dbname", &self.dbname)
            .field("username", &self.username)
            .field("ssl_mode", &self.ssl_mode)
            .field("ssl_root_cert", &self.ssl_root_cert)
            .field("ssh_tunnel", &self.ssh_tunnel)
            .finish()
    }
}

/// Redact `password` query param or `user:pass@` authority from any URL.
#[must_use]
pub fn sanitize_url(raw: &str) -> String {
    // Try `url::Url` first; fallback to simple replace if parsing fails.
    if let Ok(mut url) = url::Url::parse(raw) {
        if url.password().is_some() {
            let _ = url.set_password(Some("***"));
        }
        // Also strip `password=` query param if present (JDBC style).
        let redacted_query = url
            .query_pairs()
            .map(|(k, v)| {
                if k == "password" {
                    format!("{k}=***")
                } else {
                    format!("{k}={v}")
                }
            })
            .collect::<Vec<_>>()
            .join("&");
        if url.query().is_some() {
            url.set_query(if redacted_query.is_empty() {
                None
            } else {
                Some(&redacted_query)
            });
        }
        return url.to_string();
    }
    // Fallback: replace `password=...` substring.
    let mut out = raw.to_string();
    if let Some(idx) = out.to_lowercase().find("password=") {
        let start = idx + "password=".len();
        if let Some(end) = out[start..].find(['&', ' ']) {
            out.replace_range(start..start + end, "***");
        } else {
            out.truncate(start);
            out.push_str("***");
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Errors (§33, §35)
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum ConnectionErrorKind {
    #[error("connection failed: {message} (sanitized url: {sanitized_url})")]
    ConnectionFailed {
        message: String,
        sanitized_url: String,
    },
    #[error("authentication failed")]
    AuthFailed,
    #[error("TLS failed: {0}")]
    TlsFailed(String),
    #[error("query failed: {0}")]
    QueryFailed(#[from] PgError),
    #[error("cancel failed: {0}")]
    CancelFailed(String),
    #[error("transaction poisoned — reconnect required")]
    TransactionPoisoned,
    #[error("not connected")]
    NotConnected,
}

/// Wire PG error preserved per §33.
#[derive(Debug, Clone, Error)]
#[error("[{sqlstate:?}] {message}")]
pub struct PgError {
    pub sqlstate: Option<String>,
    pub message: String,
    pub detail: Option<String>,
    pub hint: Option<String>,
    pub position: Option<u32>,
    pub is_cancel: bool,
}

impl PgError {
    #[must_use]
    pub fn is_cancel(&self) -> bool {
        self.is_cancel || self.sqlstate.as_deref() == Some("57014")
    }
}

// ---------------------------------------------------------------------------
// Transaction / session health (§22, §11)
// ---------------------------------------------------------------------------

/// Mirrors PG `ReadyForQuery` status byte `I/T/E`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TxState {
    #[default]
    Idle,
    InTransaction {
        since: Option<Instant>,
        readonly: bool,
    },
    InFailedTransaction,
}

impl TxState {
    #[must_use]
    pub fn is_active(&self) -> bool {
        !matches!(self, Self::Idle)
    }
}

/// Whether the underlying `tokio-postgres` session can be reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SessionHealth {
    #[default]
    Ready,
    NeedsReset,
    Poisoned,
}

// ---------------------------------------------------------------------------
// Connection state machine (§53 — explicit enum, not bool soup)
// ---------------------------------------------------------------------------

/// Connection lifecycle — see `docs/decisions/ADR-0008-connection-model.md`.
#[derive(Debug, Clone)]
pub enum ConnectionState {
    Disconnected,
    Connecting {
        since: Instant,
    },
    Connected {
        id: ConnectionId,
        tx: TxState,
        health: SessionHealth,
    },
    Executing {
        id: ConnectionId,
        query_id: QueryId,
        tx: TxState,
    },
    Cancelling {
        id: ConnectionId,
        query_id: QueryId,
        sent_at: Instant,
    },
    Error {
        id: Option<ConnectionId>,
        kind: String,
        retryable: bool,
    },
}

impl ConnectionState {
    #[must_use]
    pub fn is_connected(&self) -> bool {
        matches!(
            self,
            Self::Connected { .. } | Self::Executing { .. } | Self::Cancelling { .. }
        )
    }

    #[must_use]
    pub fn tx_state(&self) -> Option<TxState> {
        match self {
            Self::Connected { tx, .. } | Self::Executing { tx, .. } => Some(*tx),
            Self::Cancelling { .. } => None,
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// ManagedSession — long-lived `tokio-postgres` session per §9
// ---------------------------------------------------------------------------

/// Handle for a single `tokio-postgres` session (query or meta).
/// The actual `Client`/`Connection` + `CancelToken` live inside `db::execution`
/// and are spawned on Tokio; this struct keeps the health + tx view.
#[derive(Debug)]
pub struct ManagedSession {
    pub id: ConnectionId,
    pub health: SessionHealth,
    pub tx: TxState,
}

impl ManagedSession {
    #[must_use]
    pub fn new(id: ConnectionId) -> Self {
        Self {
            id,
            health: SessionHealth::Ready,
            tx: TxState::Idle,
        }
    }

    pub fn mark_poisoned(&mut self) {
        self.health = SessionHealth::Poisoned;
    }

    pub fn mark_needs_reset(&mut self) {
        if self.health != SessionHealth::Poisoned {
            self.health = SessionHealth::NeedsReset;
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Classify optimistic TxState transition from SQL text (case/whitespace tolerant).
/// Authoritative correction comes from `ReadyForQuery` byte; this is optimistic.
#[must_use]
pub fn classify_tx(sql: &str) -> Option<TxState> {
    let s = sql.trim().to_ascii_lowercase();
    if s.starts_with("begin") || s.starts_with("start transaction") {
        Some(TxState::InTransaction {
            since: None,
            readonly: false,
        })
    } else if s.starts_with("commit") || s.starts_with("rollback") {
        Some(TxState::Idle)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_password_in_url() {
        let raw = "postgres://bob:s3cret@localhost:5432/mydb?sslmode=prefer&password=s3cret";
        let sanitized = sanitize_url(raw);
        assert!(!sanitized.contains("s3cret"));
        assert!(sanitized.contains("***"));
    }

    #[test]
    fn connection_config_debug_redacts() {
        let cfg = ConnectionConfig {
            id: ConnectionId::new(),
            name: "local".into(),
            host: "localhost".into(),
            port: 5432,
            dbname: "test".into(),
            username: "bob".into(),
            ssl_mode: SslMode::Prefer,
            ssl_root_cert: None,
            ssh_tunnel: None,
        };
        let dbg = format!("{cfg:?}");
        assert!(!dbg.contains("s3cret"));
    }

    #[test]
    fn classify_tx_begin_commit() {
        assert!(matches!(
            classify_tx("BEGIN"),
            Some(TxState::InTransaction { .. })
        ));
        assert_eq!(classify_tx("COMMIT"), Some(TxState::Idle));
        assert_eq!(classify_tx("SELECT 1"), None);
    }

    #[test]
    fn tx_state_is_active() {
        assert!(!TxState::Idle.is_active());
        assert!(TxState::InFailedTransaction.is_active());
    }
}
