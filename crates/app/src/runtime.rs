//! AppRuntime — single Tokio task draining AppCommand, spawning per-query tasks.
use std::collections::HashMap;
use std::sync::Arc;

use pgnative_db::connection::{connect_live, LiveSession};
use pgnative_db::connection::{ConnectionConfig, ConnectionId, QueryId, SslMode, TxState};
use pgnative_results::store::SharedStore;

use crate::{AppCommand, AppEvent, AppState};

type SessionMap = HashMap<ConnectionId, LiveSession>;

/// Reliable send for terminal/lifecycle events (`QueryFinished`,
/// `DisconnectRequiresDecision`).
///
/// E2E proved the race: a 120k-row query emits ~2k `QueryProgress` events in
/// one burst; the bounded (256) event channel fills and a trailing
/// `try_send(QueryFinished)` is silently dropped, so the UI (or driver)
/// waits forever for a finish that will never arrive. Retry briefly instead
/// of dropping — the UI drains continuously so this resolves in ms and never
/// blocks the executor (async sleep between attempts).
async fn send_reliable(tx: &crossbeam_channel::Sender<AppEvent>, mut ev: AppEvent) {
    for _ in 0..200 {
        match tx.try_send(ev) {
            Ok(()) => return,
            Err(crossbeam_channel::TrySendError::Full(ret)) => {
                ev = ret;
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            Err(crossbeam_channel::TrySendError::Disconnected(_)) => return,
        }
    }
    let _ = tx.try_send(ev);
}

struct QueryEntry {
    id: QueryId,
    connection: ConnectionId,
    cancel: tokio_postgres::CancelToken,
    ssl_mode: SslMode,
    ssl_root_cert: Option<String>,
    handle: tokio::task::JoinHandle<()>,
}

/// Shared teardown for `Disconnect` and `ResolveTransaction` (§22).
/// Aborts the driver, drops session + queries, clears tracked tx state,
/// and notifies the UI. Never commits: any open txn is rolled back by close.
fn teardown_connection(
    id: ConnectionId,
    sessions: &mut SessionMap,
    queries: &Arc<parking_lot::Mutex<HashMap<QueryId, QueryEntry>>>,
    state: &Arc<parking_lot::RwLock<AppState>>,
    event_tx: &crossbeam_channel::Sender<AppEvent>,
) {
    if let Some(mut sess) = sessions.remove(&id) {
        sess.abort_driver();
    }
    {
        let mut s = state.write();
        s.connections.remove(&id);
        s.tx.remove(&id);
    }
    let _ = event_tx.try_send(AppEvent::ConnectionStateChanged {
        id,
        state: "disconnected".into(),
    });
    // Abort and clean up any queries still tied to this connection
    {
        let mut qs = queries.lock();
        let to_abort: Vec<QueryId> = qs
            .iter()
            .filter(|(_, e)| e.connection == id)
            .map(|(k, _)| *k)
            .collect();
        for qid in to_abort {
            if let Some(e) = qs.remove(&qid) {
                e.handle.abort();
            }
        }
    }
}

pub fn spawn_runtime(
    cmd_rx: crossbeam_channel::Receiver<AppCommand>,
    event_tx: crossbeam_channel::Sender<AppEvent>,
    store: SharedStore,
    state: Arc<parking_lot::RwLock<AppState>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut sessions: SessionMap = HashMap::new();
        let queries: Arc<parking_lot::Mutex<HashMap<QueryId, QueryEntry>>> =
            Arc::new(parking_lot::Mutex::new(HashMap::new()));

        // Bridge crossbeam (sync, UI thread) -> tokio mpsc with a single blocking thread
        // instead of per-command spawn_blocking churn (§8).
        // Channel cap 256 prevents flood; try_send+retry avoids blocking_send deadlock
        // (crates/app/src/runtime.rs:38) where bridge thread could block forever if
        // receiver stalls. Abort on shutdown via bridge_handle.abort().
        let (bridge_tx, mut bridge_rx) = tokio::sync::mpsc::channel::<AppCommand>(256);
        // Single dedicated thread forwards commands; exits when crossbeam disconnects
        // or bridge is closed (runtime shutdown).
        let bridge_handle = tokio::task::spawn_blocking(move || {
            while let Ok(cmd) = cmd_rx.recv() {
                let mut pending: Option<AppCommand> = Some(cmd);
                while let Some(c) = pending.take() {
                    match bridge_tx.try_send(c) {
                        Ok(()) => break,
                        Err(tokio::sync::mpsc::error::TrySendError::Full(ret)) => {
                            if bridge_tx.is_closed() {
                                return;
                            }
                            // Channel full (256) — back off briefly then retry to avoid
                            // blocking forever (crates/app/src/runtime.rs:38)
                            std::thread::sleep(std::time::Duration::from_millis(1));
                            pending = Some(ret);
                        }
                        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => return,
                    }
                }
            }
        });

        loop {
            let Some(cmd) = bridge_rx.recv().await else {
                break;
            };

            match cmd {
                AppCommand::Connect { id } => {
                    let ev_tx = event_tx.clone();
                    // SQLite I/O must not block the Tokio worker (§7 fix).
                    let cfg_opt = tokio::task::spawn_blocking(move || load_connection_config(id))
                        .await
                        .ok()
                        .flatten();
                    let Some(cfg) = cfg_opt else {
                        let _ = ev_tx.try_send(AppEvent::Error {
                            op: "connect".into(),
                            message: format!("unknown connection {id}"),
                        });
                        continue;
                    };
                    // Keychain access may hit D-Bus/secret-service — also blocking.
                    let pw = tokio::task::spawn_blocking(move || crate::resolve_password(id))
                        .await
                        .ok()
                        .flatten();
                    match connect_live(&cfg, pw.as_ref()).await {
                        Ok(sess) => {
                            let conn_id = sess.id;
                            {
                                let mut s = state.write();
                                s.connections.insert(conn_id, sess.state());
                            }
                            let _ = ev_tx.try_send(AppEvent::ConnectionStateChanged {
                                id: conn_id,
                                state: "connected".into(),
                            });
                            let client_ref = &sess.client;
                            let _ = pgnative_db::introspection::prepare_session(client_ref).await;
                            match pgnative_db::introspection::introspect(client_ref).await {
                                Ok(model) => {
                                    let arc = Arc::new(model);
                                    state.write().set_schema((*arc).clone());
                                    let _ = ev_tx.try_send(AppEvent::SchemaUpdated {
                                        connection: conn_id,
                                        model: arc,
                                    });
                                }
                                Err(e) => {
                                    let _ = ev_tx.try_send(AppEvent::Error {
                                        op: "introspect".into(),
                                        message: e.to_string(),
                                    });
                                }
                            }
                            sessions.insert(conn_id, sess);
                        }
                        Err(e) => {
                            let _ = ev_tx.try_send(AppEvent::Error {
                                op: "connect".into(),
                                message: e.to_string(),
                            });
                            state.write().connections.insert(
                                id,
                                pgnative_db::connection::ConnectionState::Error {
                                    id: Some(id),
                                    kind: e.to_string(),
                                    retryable: true,
                                },
                            );
                        }
                    }
                }
                AppCommand::ConnectDirect { config, password } => {
                    let ev_tx = event_tx.clone();
                    match connect_live(&config, password.as_ref()).await {
                        Ok(sess) => {
                            let conn_id = sess.id;
                            {
                                let mut s = state.write();
                                s.connections.insert(conn_id, sess.state());
                            }
                            let _ = ev_tx.try_send(AppEvent::ConnectionStateChanged {
                                id: conn_id,
                                state: "connected".into(),
                            });
                            let client_ref = &sess.client;
                            let _ = pgnative_db::introspection::prepare_session(client_ref).await;
                            match pgnative_db::introspection::introspect(client_ref).await {
                                Ok(model) => {
                                    let arc = Arc::new(model);
                                    state.write().set_schema((*arc).clone());
                                    let _ = ev_tx.try_send(AppEvent::SchemaUpdated {
                                        connection: conn_id,
                                        model: arc,
                                    });
                                }
                                Err(e) => {
                                    let _ = ev_tx.try_send(AppEvent::Error {
                                        op: "introspect".into(),
                                        message: e.to_string(),
                                    });
                                }
                            }
                            sessions.insert(conn_id, sess);
                        }
                        Err(e) => {
                            let _ = ev_tx.try_send(AppEvent::Error {
                                op: "connect".into(),
                                message: e.to_string(),
                            });
                            state.write().connections.insert(
                                config.id,
                                pgnative_db::connection::ConnectionState::Error {
                                    id: Some(config.id),
                                    kind: e.to_string(),
                                    retryable: true,
                                },
                            );
                        }
                    }
                }
                AppCommand::Disconnect { id } => {
                    // §22: an open txn blocks teardown. Emit the decision hook
                    // and keep the session alive — the dialog answers via
                    // `ResolveTransaction`. PG would roll back on close, so we
                    // must not tear down before the user decides.
                    if state.read().disconnect_requires_decision(id) {
                        send_reliable(&event_tx, AppEvent::DisconnectRequiresDecision { id }).await;
                        continue;
                    }
                    teardown_connection(id, &mut sessions, &queries, &state, &event_tx);
                }
                AppCommand::ResolveTransaction { id, commit } => {
                    let verb = if commit { "COMMIT" } else { "ROLLBACK" };
                    let outcome = match sessions.get(&id) {
                        None => None,
                        Some(sess) => Some(sess.client.simple_query(verb).await.map(|_| ())),
                    };
                    match outcome {
                        // Session already gone: nothing to resolve.
                        None => {}
                        Some(Ok(())) => {
                            state.write().set_tx(id, TxState::Idle);
                            teardown_connection(id, &mut sessions, &queries, &state, &event_tx);
                        }
                        Some(Err(e)) if commit => {
                            // Failed COMMIT keeps the session: the txn may still
                            // be live and the user can retry or roll back.
                            let _ = event_tx.try_send(AppEvent::Error {
                                op: "commit".into(),
                                message: e.to_string(),
                            });
                        }
                        Some(Err(e)) => {
                            // Failed ROLLBACK: session state is suspect, so tear
                            // down (close rolls back server-side). Never commit.
                            let _ = event_tx.try_send(AppEvent::Error {
                                op: "rollback".into(),
                                message: e.to_string(),
                            });
                            teardown_connection(id, &mut sessions, &queries, &state, &event_tx);
                        }
                    }
                }
                AppCommand::Execute {
                    tab: _,
                    sql,
                    connection,
                } => {
                    let Some(sess) = sessions.get(&connection) else {
                        let _ = event_tx.try_send(AppEvent::Error {
                            op: "execute".into(),
                            message: "not connected".into(),
                        });
                        continue;
                    };
                    let qid = QueryId::new();
                    state.write().queries.insert(qid, sql.clone());
                    // New query owns the shared store: drop Q1 rows/headers so
                    // Q2 never renders stale data (§15).
                    store.write().clear();
                    let cancel = sess.cancel_token();
                    let ssl_mode = sess.ssl_mode;
                    let ssl_root_cert = sess.ssl_root_cert.clone();
                    let client = std::sync::Arc::clone(&sess.client);
                    let ev_tx = event_tx.clone();
                    let store_clone = store.clone();
                    let sql_for_history = sql.clone();
                    let conn_for_history = connection;
                    let queries_clone = Arc::clone(&queries);
                    let state_clone = Arc::clone(&state);
                    // §16: never silently rewrite user SQL with LIMIT/OFFSET; LIMIT belongs
                    // only in table-browser queries (crates/results/table_browser).
                    let sql_exec = sql.clone();
                    // Fix race (crates/app/src/runtime.rs:160): insert placeholder BEFORE spawn
                    // so Cancel arriving immediately after qid creation finds the entry.
                    // QGuard will remove entry on drop, ensuring no leak on fast prepare error.
                    let placeholder = tokio::spawn(std::future::pending::<()>());
                    queries.lock().insert(
                        qid,
                        QueryEntry {
                            id: qid,
                            connection,
                            cancel,
                            ssl_mode,
                            ssl_root_cert: ssl_root_cert.clone(),
                            handle: placeholder,
                        },
                    );
                    let handle = tokio::spawn(async move {
                        struct QGuard {
                            qid: QueryId,
                            queries: Arc<parking_lot::Mutex<HashMap<QueryId, QueryEntry>>>,
                            state: Arc<parking_lot::RwLock<AppState>>,
                        }
                        impl Drop for QGuard {
                            fn drop(&mut self) {
                                self.queries.lock().remove(&self.qid);
                                self.state.write().queries.remove(&self.qid);
                            }
                        }
                        // Separate handle for §22 tx tracking (QGuard owns state_clone).
                        let tx_state_handle = Arc::clone(&state_clone);
                        let _qguard = QGuard {
                            qid,
                            queries: queries_clone,
                            state: state_clone,
                        };
                        let start = std::time::Instant::now();
                        // Prepare first to get column metadata, then stream with empty params.
                        let stmt_res = client.prepare(&sql_exec).await;
                        let (stmt_metas, stream_res) = match stmt_res {
                            Ok(s) => {
                                let metas: Vec<pgnative_results::stream::ColumnMeta> = s
                                    .columns()
                                    .iter()
                                    .map(pgnative_results::stream::column_meta_from_pg)
                                    .collect();
                                let empty: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = vec![];
                                let r = client.query_raw(&s, empty).await;
                                (metas, r)
                            }
                            Err(e) => {
                                // NB: `e.to_string()` alone renders server
                                // errors as the literal "db error"
                                // (tokio_postgres `Kind::Db`); unwrap the
                                // DbError source for the real message.
                                store_clone.write().fail();
                                let _ = ev_tx.try_send(AppEvent::Error {
                                    op: "query".into(),
                                    message: pgnative_results::stream::pg_error_text(&e),
                                });
                                send_reliable(
                                    &ev_tx,
                                    AppEvent::QueryFinished {
                                        query_id: qid,
                                        success: false,
                                    },
                                )
                                .await;
                                return;
                            }
                        };
                        match stream_res {
                            Ok(stream) => {
                                let cols = stmt_metas;
                                let (tx, mut rx) = pgnative_results::stream::channel(
                                    &pgnative_results::stream::StreamConfig::default(),
                                );
                                // RowStream contains PhantomPinned (!Unpin); pin before drive.
                                let stream = Box::pin(stream);
                                let drive = pgnative_results::stream::spawn_drive(
                                    stream,
                                    cols,
                                    pgnative_results::stream::StreamConfig::default(),
                                    tx,
                                );
                                let mut total: u64 = 0;
                                // Throttle progress: a 120k-row burst would
                                // otherwise flood the 256-cap event channel and
                                // starve the terminal QueryFinished (E2E S9).
                                let mut last_progress = std::time::Instant::now()
                                    .checked_sub(std::time::Duration::from_secs(1))
                                    .unwrap_or_else(std::time::Instant::now);
                                while let Some(ev) = rx.recv().await {
                                    match ev {
                                        pgnative_results::stream::StreamEvent::Meta(metas) => {
                                            let names = metas
                                                .iter()
                                                .map(|m| m.name.clone())
                                                .collect::<Vec<_>>();
                                            store_clone.write().set_columns(names);
                                        }
                                        pgnative_results::stream::StreamEvent::Batch(batch) => {
                                            let n = batch.len() as u64;
                                            total += n;
                                            {
                                                store_clone.write().push_batch(batch);
                                            }
                                            if last_progress.elapsed()
                                                >= std::time::Duration::from_millis(200)
                                            {
                                                last_progress = std::time::Instant::now();
                                                let _ = ev_tx.try_send(AppEvent::QueryProgress {
                                                    query_id: qid,
                                                    rows: total,
                                                });
                                            }
                                        }
                                        pgnative_results::stream::StreamEvent::Complete {
                                            rows,
                                            ..
                                        } => {
                                            store_clone.write().complete();
                                            // §22 optimistic tx tracking: BEGIN/COMMIT/ROLLBACK
                                            // update the badge/decision state; authoritative
                                            // correction comes from ReadyForQuery when wired.
                                            if let Some(tx) =
                                                pgnative_db::connection::classify_tx(&sql_exec)
                                            {
                                                tx_state_handle
                                                    .write()
                                                    .set_tx(conn_for_history, tx);
                                            }
                                            send_reliable(
                                                &ev_tx,
                                                AppEvent::QueryFinished {
                                                    query_id: qid,
                                                    success: true,
                                                },
                                            )
                                            .await;
                                            let hist_sql = sql_for_history.clone();
                                            tokio::task::spawn_blocking(move || {
                                                if let Ok(conn) =
                                                    crate::open_app_db(&crate::app_db_path())
                                                {
                                                    let entry =
                                                        pgnative_storage::history::HistoryEntry {
                                                            id: uuid::Uuid::new_v4(),
                                                            connection_id: conn_for_history
                                                                .0
                                                                .to_string(),
                                                            query_text: hist_sql,
                                                            executed_at: chrono::Utc::now(),
                                                            duration_ms: Some(
                                                                start.elapsed().as_millis() as u64,
                                                            ),
                                                            rows_affected: Some(rows as i64),
                                                            success: true,
                                                            error_code: None,
                                                        };
                                                    let _ = pgnative_storage::history::insert(
                                                        &conn, &entry,
                                                    );
                                                }
                                            });
                                            break;
                                        }
                                        pgnative_results::stream::StreamEvent::Error(e) => {
                                            // Genuine stream failure, not a user cancel —
                                            // mark Error (StreamError::Pg already carries
                                            // the unwrapped server message).
                                            store_clone.write().fail();
                                            let _ = ev_tx.try_send(AppEvent::Error {
                                                op: "query".into(),
                                                message: e.to_string(),
                                            });
                                            send_reliable(
                                                &ev_tx,
                                                AppEvent::QueryFinished {
                                                    query_id: qid,
                                                    success: false,
                                                },
                                            )
                                            .await;
                                            break;
                                        }
                                        _ => {}
                                    }
                                }
                                let _ = drive.await;
                            }
                            Err(e) => {
                                store_clone.write().fail();
                                let _ = ev_tx.try_send(AppEvent::Error {
                                    op: "query".into(),
                                    message: pgnative_results::stream::pg_error_text(&e),
                                });
                                send_reliable(
                                    &ev_tx,
                                    AppEvent::QueryFinished {
                                        query_id: qid,
                                        success: false,
                                    },
                                )
                                .await;
                            }
                        }
                    });
                    // Swap placeholder with real handle; if Cancel raced and removed entry, abort new task
                    {
                        let mut qs = queries.lock();
                        if let Some(entry) = qs.get_mut(&qid) {
                            entry.handle.abort();
                            entry.handle = handle;
                        } else {
                            handle.abort();
                        }
                    }
                }
                AppCommand::Cancel { query_id } => {
                    let entry_opt = { queries.lock().remove(&query_id) };
                    if let Some(entry) = entry_opt {
                        let cancel_res: Result<(), tokio_postgres::Error> = match entry.ssl_mode {
                            SslMode::Disable => {
                                entry.cancel.cancel_query(tokio_postgres::NoTls).await
                            }
                            _ => {
                                match pgnative_db::connection::build_rustls_config(
                                    entry.ssl_mode,
                                    entry.ssl_root_cert.as_deref(),
                                ) {
                                    Ok(cfg) => {
                                        let tls =
                                            tokio_postgres_rustls::MakeRustlsConnect::new(cfg);
                                        entry.cancel.cancel_query(tls).await
                                    }
                                    Err(e) => {
                                        // Never fallback to NoTls — would leak pid/secret in plaintext
                                        // on hostssl servers (C1). Mark Poisoned directly.
                                        tracing::warn!(
                                            connection = %entry.connection,
                                            error = %e,
                                            "TLS config for cancel failed — not sending plaintext CancelRequest"
                                        );
                                        if let Some(sess) = sessions.get_mut(&entry.connection) {
                                            sess.health =
                                                pgnative_db::connection::SessionHealth::Poisoned;
                                        }
                                        // Treat as cancel failure — poisoned path below will also handle,
                                        // but we short-circuit to avoid fabricating tokio_postgres::Error.
                                        entry.handle.abort();
                                        send_reliable(
                                            &event_tx,
                                            AppEvent::QueryFinished {
                                                query_id,
                                                success: false,
                                            },
                                        )
                                        .await;
                                        continue;
                                    }
                                }
                            }
                        };
                        if cancel_res.is_err() {
                            if let Some(sess) = sessions.get_mut(&entry.connection) {
                                sess.health = pgnative_db::connection::SessionHealth::Poisoned;
                                tracing::warn!(
                                    connection = %entry.connection,
                                    "cancel failed — marking session Poisoned (needs reconnect)"
                                );
                            }
                        }
                        entry.handle.abort();
                        send_reliable(
                            &event_tx,
                            AppEvent::QueryFinished {
                                query_id,
                                success: false,
                            },
                        )
                        .await;
                    }
                }
                AppCommand::RefreshSchema { connection } => {
                    if let Some(sess) = sessions.get(&connection) {
                        let ev_tx = event_tx.clone();
                        let state_clone = state.clone();
                        let client = std::sync::Arc::clone(&sess.client);
                        tokio::spawn(async move {
                            match pgnative_db::introspection::introspect(&client).await {
                                Ok(model) => {
                                    let arc = Arc::new(model);
                                    state_clone.write().set_schema((*arc).clone());
                                    let _ = ev_tx.try_send(AppEvent::SchemaUpdated {
                                        connection,
                                        model: arc,
                                    });
                                }
                                Err(e) => {
                                    let _ = ev_tx.try_send(AppEvent::Error {
                                        op: "refresh_schema".into(),
                                        message: e.to_string(),
                                    });
                                }
                            }
                        });
                    }
                }
                AppCommand::HistorySearch { query } => {
                    let ev_tx = event_tx.clone();
                    tokio::task::spawn_blocking(move || {
                        if let Ok(conn) = crate::open_app_db(&crate::app_db_path()) {
                            let res = pgnative_storage::history::search(&conn, &query)
                                .unwrap_or_default();
                            let results = res.into_iter().map(|e| e.query_text).collect::<Vec<_>>();
                            // Prefer typed HistoryResults; keep Error fallback for old listeners
                            let _ = ev_tx.try_send(AppEvent::HistoryResults {
                                results: results.clone(),
                            });
                            // Back-compat fallback (removed once all listeners handle HistoryResults)
                            let joined = results.join("\n---\n");
                            let _ = ev_tx.try_send(AppEvent::Error {
                                op: "history".into(),
                                message: joined,
                            });
                        }
                    });
                }
                AppCommand::Export { query_id, format } => {
                    // Minimal CSV export (§28): snapshot buffered rows (bounded
                    // store, so this is the visible window — not the full
                    // server-side result) and write one file. Blocking fs I/O
                    // runs on spawn_blocking, off the dispatch loop.
                    if !matches!(format, crate::ExportFormat::Csv) {
                        let _ = event_tx.try_send(AppEvent::Error {
                            op: "export".into(),
                            message: "only CSV export is supported for now".into(),
                        });
                        return;
                    }
                    let ev_tx = event_tx.clone();
                    let store_clone = store.clone();
                    tokio::task::spawn_blocking(move || {
                        let (columns, rows) = {
                            let guard = store_clone.read();
                            (guard.columns(), guard.snapshot_range(0, guard.len()))
                        };
                        let csv = match pgnative_results::export::export_csv(&rows, &columns) {
                            Ok(csv) => csv,
                            Err(e) => {
                                let _ = ev_tx.try_send(AppEvent::Error {
                                    op: "export".into(),
                                    message: format!("csv encode failed: {e}"),
                                });
                                return;
                            }
                        };
                        let path =
                            std::env::temp_dir().join(format!("pgnative-export-{query_id}.csv"));
                        match std::fs::write(&path, csv) {
                            Ok(()) => {
                                let _ = ev_tx.try_send(AppEvent::ExportProgress {
                                    query_id,
                                    written: rows.len() as u64,
                                    path: path.display().to_string(),
                                });
                            }
                            Err(e) => {
                                let _ = ev_tx.try_send(AppEvent::Error {
                                    op: "export".into(),
                                    message: format!("write {} failed: {e}", path.display()),
                                });
                            }
                        }
                    });
                }
            }
        }
        // Ensure bridge thread exits on shutdown
        bridge_handle.abort();
    })
}

fn load_connection_config(id: ConnectionId) -> Option<ConnectionConfig> {
    let path = crate::app_db_path();
    let conn = crate::open_app_db(&path).ok()?;
    let sc = load_saved(&conn, &id.0.to_string())?;
    let ssl_mode = pgnative_db::connection::ssl_mode_from_str(&sc.ssl_mode);
    Some(ConnectionConfig {
        id,
        name: sc.name,
        host: sc.host,
        port: sc.port,
        dbname: sc.dbname,
        username: sc.username,
        ssl_mode,
        ssl_root_cert: None,
        ssh_tunnel: None,
    })
}

fn load_saved(
    conn: &rusqlite::Connection,
    id: &str,
) -> Option<pgnative_storage::connections::SavedConnection> {
    let mut stmt = conn
        .prepare("SELECT id,name,host,port,dbname,username,ssl_mode FROM connections WHERE id=?1")
        .ok()?;
    let mut rows = stmt.query([id]).ok()?;
    let row = rows.next().ok()??;
    Some(pgnative_storage::connections::SavedConnection {
        id: row.get(0).ok()?,
        name: row.get(1).ok()?,
        host: row.get(2).ok()?,
        port: row.get(3).ok()?,
        dbname: row.get(4).ok()?,
        username: row.get(5).ok()?,
        ssl_mode: row.get(6).ok()?,
    })
}
