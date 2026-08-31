# ADR 0008 — Connection Model

**Status:** Accepted — 2026-08-31  
**Context:** §9, §10, §11, §22

## Context
Desktop client has 1–3 concurrent connections, not web-scale pool. Affinity matters (portal, cursor, BEGIN, temp table, SET, LISTEN).

## Decision
- Explicit long-lived sessions: `ConnectionHandle { query_session: ManagedSession, meta_session: Option<ManagedSession> }` per `ConnectionId`.
- Each `ManagedSession` wraps `tokio_postgres::Client + Connection driver JoinHandle + CancelToken`.
- `meta_session` lazily created for introspection/table-browser.
- State machine enum (§53) not bool soup: `Disconnected | Connecting{since} | Connected{tx, health} | Executing{query_id} | Cancelling{sent_at} | Error{kind, retryable}`.
- `TxState { Idle, InTransaction{since,readonly}, InFailedTransaction }` mirrors `ReadyForQuery I/T/E`.
- Driver: `tokio-postgres 0.7 + tokio-postgres-rustls 0.14 + rustls ring` — exposes `CancelToken::cancel_query` on separate TcpStream, `ReadyForQuery` status, `query_raw` streaming.
- No `deadpool-postgres` unless measurement shows >5% wall-time win (requires ADR amendment).

## Alternatives
- Generic pool: breaks affinity, no latency win for pgNative.

## Consequences
- Cancellation = native `CancelRequest` via `CancelToken` on separate connection → `57014 query_canceled`.
- Tx badge always accurate; disconnect with `InTransaction` requires explicit `Commit|Rollback|KeepOpen`.
- Secrets sanitized via `url::Url` before `tracing`.

## Tradeoffs
- Two sessions per connection doubles TLS handshakes (once per saved connection lifetime, amortized).
