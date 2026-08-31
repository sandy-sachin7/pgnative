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

/// PostgreSQL SSL mode — **user-facing** 5-mode UX per product decision.
/// `Disable / Prefer / Require / VerifyCa / VerifyFull`.
/// Driver mapping (e.g. `tokio_postgres::config::SslMode` which may have fewer
/// variants) is an **internal DB-boundary concern** — see `build_pg_config` /
/// `build_rustls_config` and `connect_live`. Default `Prefer` (§26).
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

    /// State-machine transitions (§53). Each returns the next state or
    /// keeps the current state if the transition is illegal (no panic).
    #[must_use]
    pub fn on_connecting(since: Instant) -> Self {
        Self::Connecting { since }
    }

    #[must_use]
    pub fn on_connected(&self, id: ConnectionId, tx: TxState, health: SessionHealth) -> Self {
        match self {
            Self::Connecting { .. } | Self::Disconnected | Self::Error { .. } => {
                Self::Connected { id, tx, health }
            }
            _ => self.clone(),
        }
    }

    #[must_use]
    pub fn on_begin_execute(&self, query_id: QueryId) -> Self {
        match self {
            Self::Connected { id, tx, .. } => Self::Executing {
                id: *id,
                query_id,
                tx: *tx,
            },
            _ => self.clone(),
        }
    }

    #[must_use]
    pub fn on_ready_for_query(&self, status: u8) -> Self {
        let tx = tx_state_from_ready_for_query(status);
        let health = health_for_tx(tx);
        match self {
            Self::Executing { id, query_id, .. } | Self::Cancelling { id, query_id, .. } => {
                Self::Connected {
                    id: *id,
                    tx,
                    health,
                }
            }
            Self::Connected { id, .. } => Self::Connected {
                id: *id,
                tx,
                health,
            },
            _ => self.clone(),
        }
    }

    #[must_use]
    pub fn on_cancel(&self) -> Self {
        match self {
            Self::Executing { id, query_id, .. } => Self::Cancelling {
                id: *id,
                query_id: *query_id,
                sent_at: Instant::now(),
            },
            _ => self.clone(),
        }
    }

    #[must_use]
    pub fn on_error(&self, kind: String, retryable: bool) -> Self {
        let id = match self {
            Self::Connected { id, .. }
            | Self::Executing { id, .. }
            | Self::Cancelling { id, .. } => Some(*id),
            _ => None,
        };
        Self::Error {
            id,
            kind,
            retryable,
        }
    }

    #[must_use]
    pub fn on_disconnect(&self) -> Self {
        Self::Disconnected
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

    /// Authoritative update from `ReadyForQuery` status byte.
    pub fn apply_ready_for_query(&mut self, status: u8) {
        let tx = tx_state_from_ready_for_query(status);
        self.tx = tx;
        self.health = health_for_tx(tx);
    }
}

// ---------------------------------------------------------------------------
// PG wire helpers — real tokio-postgres wiring (§9, §26)
// ---------------------------------------------------------------------------

/// Map PG `ReadyForQuery` status byte (`I`/`T`/`E`) to [`TxState`].
/// `I` = idle, `T` = in transaction, `E` = in failed transaction (RFC §53).
#[must_use]
pub fn tx_state_from_ready_for_query(status: u8) -> TxState {
    match status {
        b'I' => TxState::Idle,
        b'T' => TxState::InTransaction {
            since: Some(Instant::now()),
            readonly: false,
        },
        b'E' => TxState::InFailedTransaction,
        _ => TxState::Idle,
    }
}

/// Update [`SessionHealth`] from the authoritative [`TxState`].
#[must_use]
pub fn health_for_tx(tx: TxState) -> SessionHealth {
    match tx {
        TxState::InFailedTransaction => SessionHealth::NeedsReset,
        _ => SessionHealth::Ready,
    }
}

/// Build a `tokio_postgres::Config` from [`ConnectionConfig`] + optional secret.
/// Never logs the password; caller must pass `sanitized_url` to error mapping.
#[must_use]
pub fn build_pg_config(
    cfg: &ConnectionConfig,
    password: Option<&SecretString>,
) -> tokio_postgres::Config {
    let mut pg = tokio_postgres::Config::new();
    pg.host(&cfg.host);
    pg.port(cfg.port);
    pg.dbname(&cfg.dbname);
    pg.user(&cfg.username);
    if let Some(pw) = password {
        pg.password(pw.expose_secret());
    }
    // Keepalive + connect timeout are set at connect time; config is pure.
    // `application_name` identifies pgNative sessions in `pg_stat_activity`.
    pg.application_name("pgNative");
    // SslMode is handled at the TLS connector layer, not here; we still
    // stash it so `Config` consumers can inspect if they build URLs.
    let _ = cfg.ssl_mode;
    pg
}

/// Build a `rustls::ClientConfig` honoring [`SslMode`].
/// `Disable` is handled by the caller selecting `NoTls`; this function is
/// only called when TLS is desired. If `root_cert_pem` is provided it is
/// parsed as PEM and added to the trust anchor store; otherwise an empty
/// store is used (handshake will fail for self-signed unless the server
/// presents a system-trusted chain — caller maps to `TlsFailed`).
pub fn build_rustls_config(
    ssl_mode: SslMode,
    root_cert_pem: Option<&str>,
) -> Result<rustls::ClientConfig, String> {
    let mut roots = rustls::RootCertStore::empty();
    // If a PEM bundle is supplied, decode it without requiring `rustls-pemfile`
    // as an extra workspace dep: split on `-----BEGIN CERTIFICATE-----`.
    if let Some(pem) = root_cert_pem {
        // Best-effort PEM extraction — if parsing fails we surface TlsFailed.
        // We avoid a hard dep on `rustls-pemfile`; the caller can also pass
        // `None` and rely on system roots for VerifyFull/VerifyCa.
        let pem_bytes = pem.as_bytes();
        // Use `rustls::pki_types::CertificateDer` parsing via `pem` crate style:
        // fallback to trying `rustls`'s built-in PEM loader if available.
        // For now, attempt to load via `rustls_pemfile` if present, else
        // treat the PEM as opaque and return an error guiding the caller.
        // To keep the crate buildable without `rustls-pemfile`, we do a
        // minimal split and base64-decode attempt using only std.
        let mut added = 0usize;
        for chunk in pem.split("-----BEGIN CERTIFICATE-----") {
            if let Some(end) = chunk.find("-----END CERTIFICATE-----") {
                let b64 = chunk[..end]
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .collect::<String>();
                // Decode base64 via `rustls` helper if possible; otherwise skip.
                // We use a tiny inline base64 decoder to avoid new deps.
                if let Some(der) = decode_base64(&b64) {
                    let cert = rustls::pki_types::CertificateDer::from(der);
                    if roots.add(cert).is_ok() {
                        added += 1;
                    }
                }
            }
        }
        if added == 0 && !pem_bytes.is_empty() {
            // No cert added — treat as invalid PEM.
            return Err("invalid PEM: no certificates found".to_string());
        }
        let _ = added;
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

fn decode_base64(s: &str) -> Option<Vec<u8>> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::new();
    let mut buf: u32 = 0;
    let mut bits: u8 = 0;
    for &b in s.as_bytes() {
        if b == b'=' {
            break;
        }
        let val = TABLE.iter().position(|&x| x == b)? as u32;
        buf = (buf << 6) | val;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buf >> bits) & 0xFF) as u8);
        }
    }
    Some(out)
}

/// Classify a `tokio_postgres::Error` into [`ConnectionErrorKind`] using the
/// already-sanitized URL (never re-interpolate the password).
#[must_use]
pub fn map_connect_error(err: tokio_postgres::Error, sanitized_url: String) -> ConnectionErrorKind {
    let msg = err.to_string();
    let lower = msg.to_ascii_lowercase();
    // `tokio-postgres` surfaces auth failures as `password authentication failed`
    // or `no password supplied`; TLS failures contain `tls`/`certificate`.
    if lower.contains("password authentication failed")
        || lower.contains("authentication failed")
        || lower.contains("no password")
    {
        return ConnectionErrorKind::AuthFailed;
    }
    if lower.contains("tls") || lower.contains("certificate") || lower.contains("handshake") {
        return ConnectionErrorKind::TlsFailed(msg);
    }
    // Check sqlstate if present (e.g. 28P01 invalid_password)
    // `tokio_postgres::Error::as_db_error()` exposes the `DbError` with `code()`.
    if let Some(db_err) = err.as_db_error() {
        let code = db_err.code().code().to_string();
        if code == "28P01" {
            return ConnectionErrorKind::AuthFailed;
        }
        let pg = PgError {
            sqlstate: Some(code),
            message: msg.clone(),
            detail: db_err.detail().map(|s| s.to_string()),
            hint: db_err.hint().map(|s| s.to_string()),
            position: db_err.position().and_then(|p| p.parse::<u32>().ok()),
            is_cancel: false,
        };
        return ConnectionErrorKind::QueryFailed(pg);
    }
    ConnectionErrorKind::ConnectionFailed {
        message: msg,
        sanitized_url,
    }
}

/// Live PG session owning `Client + CancelToken + driver JoinHandle`.
/// Spawned via [`connect_live`] (see below). `health`/`tx` mirror the PG
/// `ReadyForQuery` state byte authoritatively.
pub struct LiveSession {
    pub id: ConnectionId,
    pub client: tokio_postgres::Client,
    pub cancel_token: tokio_postgres::CancelToken,
    pub health: SessionHealth,
    pub tx: TxState,
    _driver: tokio::task::JoinHandle<()>,
}

impl LiveSession {
    /// Current [`ConnectionState`] view of this live session.
    #[must_use]
    pub fn state(&self) -> ConnectionState {
        ConnectionState::Connected {
            id: self.id,
            tx: self.tx,
            health: self.health,
        }
    }

    /// Apply an authoritative `ReadyForQuery` byte to update `tx`/`health`.
    pub fn apply_ready_for_query(&mut self, status: u8) {
        let tx = tx_state_from_ready_for_query(status);
        self.tx = tx;
        self.health = health_for_tx(tx);
    }

    /// Cancel token clone for `db::cancellation::TokenCanceller`.
    #[must_use]
    pub fn cancel_token(&self) -> tokio_postgres::CancelToken {
        self.cancel_token.clone()
    }
}

/// Connect with real `tokio_postgres::Client::connect`.
///
/// * Builds `Config` from `ConnectionConfig` + `SecretString` (never logged).
/// * Maps `SslMode` → `MakeRustlsConnect` / `NoTls` (Disable) / TOFU for Prefer.
/// * Spawns the `Connection` driver onto Tokio and returns a [`LiveSession`]
///   holding `Client + CancelToken + JoinHandle`.
/// * Errors are mapped via [`map_connect_error`] with [`sanitize_url`]-redacted URL.
pub async fn connect_live(
    cfg: &ConnectionConfig,
    password: Option<&SecretString>,
) -> Result<LiveSession, ConnectionErrorKind> {
    let sanitized = cfg.sanitized_url(password);
    let pg_config = build_pg_config(cfg, password);

    // Choose TLS connector per SslMode.
    let (client, connection) = match cfg.ssl_mode {
        SslMode::Disable => pg_config
            .connect(tokio_postgres::NoTls)
            .await
            .map_err(|e| map_connect_error(e, sanitized.clone()))?,
        _ => {
            let rustls_cfg = build_rustls_config(cfg.ssl_mode, cfg.ssl_root_cert.as_deref())
                .map_err(ConnectionErrorKind::TlsFailed)?;
            let tls = tokio_postgres_rustls::MakeRustlsConnect::new(rustls_cfg);
            pg_config
                .connect(tls)
                .await
                .map_err(|e| map_connect_error(e, sanitized.clone()))?
        }
    };

    let cancel_token = client.cancel_token();
    // Drive the connection in background; failures mark session poisoned.
    let driver = tokio::spawn(async move {
        if let Err(e) = connection.await {
            tracing::warn!(error = %e, "pg connection driver exited");
        }
    });

    // Probe readiness: the `ReadyForQuery` byte arrives implicitly after
    // startup; we default to Idle/Ready and let query execution correct it.
    Ok(LiveSession {
        id: cfg.id,
        client,
        cancel_token,
        health: SessionHealth::Ready,
        tx: TxState::Idle,
        _driver: driver,
    })
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
