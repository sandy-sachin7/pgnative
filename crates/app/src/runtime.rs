//! AppRuntime — single Tokio task draining AppCommand, spawning per-query tasks.
use std::collections::HashMap;
use std::sync::Arc;

use pgnative_db_connection::{connect_live, LiveSession};
use pgnative_db_connection::{ConnectionConfig, ConnectionId, QueryId, SslMode};
use pgnative_results_store::SharedStore;

use crate::{AppCommand, AppEvent, AppState};

type SessionMap = HashMap<ConnectionId, LiveSession>;

struct QueryEntry {
    id: QueryId,
    connection: ConnectionId,
    cancel: tokio_postgres::CancelToken,
    handle: tokio::task::JoinHandle<()>,
}

pub fn spawn_runtime(
    cmd_rx: crossbeam_channel::Receiver<AppCommand>,
    event_tx: crossbeam_channel::Sender<AppEvent>,
    store: SharedStore,
    state: Arc<parking_lot::RwLock<AppState>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut sessions: SessionMap = HashMap::new();
        let mut queries: HashMap<QueryId, QueryEntry> = HashMap::new();

        loop {
            let cmd = {
                let rx = cmd_rx.clone();
                match tokio::task::spawn_blocking(move || rx.recv()).await {
                    Ok(Ok(c)) => Some(c),
                    Ok(Err(_)) => break, // channel disconnected
                    Err(_) => break,
                }
            };
            let Some(cmd) = cmd else {
                break;
            };

            match cmd {
                AppCommand::Connect { id } => {
                    let ev_tx = event_tx.clone();
                    let Some(cfg) = load_connection_config(id) else {
                        let _ = ev_tx.send(AppEvent::Error {
                            op: "connect".into(),
                            message: format!("unknown connection {id}"),
                        });
                        continue;
                    };
                    let pw = crate::resolve_password(id);
                    match connect_live(&cfg, pw.as_ref()).await {
                        Ok(sess) => {
                            let conn_id = sess.id;
                            {
                                let mut s = state.write();
                                s.connections.insert(conn_id, sess.state());
                            }
                            let _ = ev_tx.send(AppEvent::ConnectionStateChanged {
                                id: conn_id,
                                state: "connected".into(),
                            });
                            let client_ref = &sess.client;
                            let _ = pgnative_db_introspection::prepare_session(client_ref).await;
                            match pgnative_db_introspection::introspect(client_ref).await {
                                Ok(model) => {
                                    let arc = Arc::new(model);
                                    state.write().set_schema((*arc).clone());
                                    let _ = ev_tx.send(AppEvent::SchemaUpdated {
                                        connection: conn_id,
                                        model: arc,
                                    });
                                }
                                Err(e) => {
                                    let _ = ev_tx.send(AppEvent::Error {
                                        op: "introspect".into(),
                                        message: e.to_string(),
                                    });
                                }
                            }
                            sessions.insert(conn_id, sess);
                        }
                        Err(e) => {
                            let _ = ev_tx.send(AppEvent::Error {
                                op: "connect".into(),
                                message: e.to_string(),
                            });
                            state.write().connections.insert(
                                id,
                                pgnative_db_connection::ConnectionState::Error {
                                    id: Some(id),
                                    kind: e.to_string(),
                                    retryable: true,
                                },
                            );
                        }
                    }
                }
                AppCommand::Disconnect { id } => {
                    sessions.remove(&id);
                    state.write().connections.remove(&id);
                    let _ = event_tx.send(AppEvent::ConnectionStateChanged {
                        id,
                        state: "disconnected".into(),
                    });
                }
                AppCommand::Execute {
                    tab: _,
                    sql,
                    connection,
                } => {
                    let Some(sess) = sessions.get(&connection) else {
                        let _ = event_tx.send(AppEvent::Error {
                            op: "execute".into(),
                            message: "not connected".into(),
                        });
                        continue;
                    };
                    let qid = QueryId::new();
                    state.write().queries.insert(qid, sql.clone());
                    let cancel = sess.cancel_token();
                    let client = std::sync::Arc::clone(&sess.client);
                    let ev_tx = event_tx.clone();
                    let store_clone = store.clone();
                    let sql_for_history = sql.clone();
                    let conn_for_history = connection;
                    let sql_limited = if is_select_without_limit(&sql) {
                        format!("{} LIMIT 100", sql.trim_end().trim_end_matches(';'))
                    } else {
                        sql.clone()
                    };
                    let handle = tokio::spawn(async move {
                        let start = std::time::Instant::now();
                        // Prepare first to get column metadata, then stream with empty params.
                        let stmt_res = client.prepare(&sql_limited).await;
                        let (stmt_metas, stream_res) = match stmt_res {
                            Ok(s) => {
                                let metas: Vec<pgnative_results_stream::ColumnMeta> = s
                                    .columns()
                                    .iter()
                                    .map(pgnative_results_stream::column_meta_from_pg)
                                    .collect();
                                let empty: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = vec![];
                                let r = client.query_raw(&s, empty).await;
                                (metas, r)
                            }
                            Err(e) => {
                                let _ = ev_tx.send(AppEvent::Error {
                                    op: "query".into(),
                                    message: e.to_string(),
                                });
                                let _ = ev_tx.send(AppEvent::QueryFinished {
                                    query_id: qid,
                                    success: false,
                                });
                                return;
                            }
                        };
                        match stream_res {
                            Ok(stream) => {
                                let cols = stmt_metas;
                                let (tx, mut rx) = pgnative_results_stream::channel(
                                    &pgnative_results_stream::StreamConfig::default(),
                                );
                                // RowStream contains PhantomPinned (!Unpin); pin before drive.
                                let stream = Box::pin(stream);
                                let drive = pgnative_results_stream::spawn_drive(
                                    stream,
                                    cols,
                                    pgnative_results_stream::StreamConfig::default(),
                                    tx,
                                );
                                let mut total: u64 = 0;
                                while let Some(ev) = rx.recv().await {
                                    match ev {
                                        pgnative_results_stream::StreamEvent::Batch(batch) => {
                                            let n = batch.len() as u64;
                                            total += n;
                                            {
                                                store_clone.write().push_batch(batch);
                                            }
                                            let _ = ev_tx.send(AppEvent::QueryProgress {
                                                query_id: qid,
                                                rows: total,
                                            });
                                        }
                                        pgnative_results_stream::StreamEvent::Complete {
                                            rows,
                                            ..
                                        } => {
                                            store_clone.write().complete();
                                            let _ = ev_tx.send(AppEvent::QueryFinished {
                                                query_id: qid,
                                                success: true,
                                            });
                                            let hist_sql = sql_for_history.clone();
                                            tokio::task::spawn_blocking(move || {
                                                if let Ok(conn) =
                                                    crate::open_app_db(&crate::app_db_path())
                                                {
                                                    let entry =
                                                        pgnative_storage_history::HistoryEntry {
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
                                                    let _ = pgnative_storage_history::insert(
                                                        &conn, &entry,
                                                    );
                                                }
                                            });
                                            break;
                                        }
                                        pgnative_results_stream::StreamEvent::Error(e) => {
                                            store_clone.write().cancel();
                                            let _ = ev_tx.send(AppEvent::Error {
                                                op: "query".into(),
                                                message: e.to_string(),
                                            });
                                            let _ = ev_tx.send(AppEvent::QueryFinished {
                                                query_id: qid,
                                                success: false,
                                            });
                                            break;
                                        }
                                        _ => {}
                                    }
                                }
                                let _ = drive.await;
                            }
                            Err(e) => {
                                let _ = ev_tx.send(AppEvent::Error {
                                    op: "query".into(),
                                    message: e.to_string(),
                                });
                                let _ = ev_tx.send(AppEvent::QueryFinished {
                                    query_id: qid,
                                    success: false,
                                });
                            }
                        }
                    });
                    queries.insert(
                        qid,
                        QueryEntry {
                            id: qid,
                            connection,
                            cancel,
                            handle,
                        },
                    );
                }
                AppCommand::Cancel { query_id } => {
                    if let Some(entry) = queries.remove(&query_id) {
                        let _ = entry.cancel.cancel_query(tokio_postgres::NoTls).await;
                        entry.handle.abort();
                        let _ = event_tx.send(AppEvent::QueryFinished {
                            query_id,
                            success: false,
                        });
                    }
                }
                AppCommand::RefreshSchema { connection } => {
                    if let Some(sess) = sessions.get(&connection) {
                        let ev_tx = event_tx.clone();
                        let state_clone = state.clone();
                        let client = std::sync::Arc::clone(&sess.client);
                        tokio::spawn(async move {
                            match pgnative_db_introspection::introspect(&client).await {
                                Ok(model) => {
                                    let arc = Arc::new(model);
                                    state_clone.write().set_schema((*arc).clone());
                                    let _ = ev_tx.send(AppEvent::SchemaUpdated {
                                        connection,
                                        model: arc,
                                    });
                                }
                                Err(e) => {
                                    let _ = ev_tx.send(AppEvent::Error {
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
                            let res =
                                pgnative_storage_history::search(&conn, &query).unwrap_or_default();
                            let joined = res
                                .into_iter()
                                .map(|e| e.query_text)
                                .collect::<Vec<_>>()
                                .join("\n---\n");
                            let _ = ev_tx.send(AppEvent::Error {
                                op: "history".into(),
                                message: joined,
                            });
                        }
                    });
                }
                AppCommand::Export { query_id, format } => {
                    let _ = event_tx.send(AppEvent::Error {
                        op: "export".into(),
                        message: format!("export {query_id} {format:?} not yet implemented"),
                    });
                }
            }
        }
    })
}

fn load_connection_config(id: ConnectionId) -> Option<ConnectionConfig> {
    let path = crate::app_db_path();
    let conn = crate::open_app_db(&path).ok()?;
    let sc = load_saved(&conn, &id.0.to_string())?;
    let ssl_mode = match sc.ssl_mode.as_str() {
        "disable" => SslMode::Disable,
        "require" => SslMode::Require,
        "verify-ca" => SslMode::VerifyCa,
        "verify-full" => SslMode::VerifyFull,
        _ => SslMode::Prefer,
    };
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
) -> Option<pgnative_storage_connections::SavedConnection> {
    let mut stmt = conn
        .prepare("SELECT id,name,host,port,dbname,username,ssl_mode FROM connections WHERE id=?1")
        .ok()?;
    let mut rows = stmt.query([id]).ok()?;
    let row = rows.next().ok()??;
    Some(pgnative_storage_connections::SavedConnection {
        id: row.get(0).ok()?,
        name: row.get(1).ok()?,
        host: row.get(2).ok()?,
        port: row.get(3).ok()?,
        dbname: row.get(4).ok()?,
        username: row.get(5).ok()?,
        ssl_mode: row.get(6).ok()?,
    })
}

fn is_select_without_limit(sql: &str) -> bool {
    let s = sql.trim().to_ascii_lowercase();
    if !s.starts_with("select") {
        return false;
    }
    !s.contains("limit")
}
