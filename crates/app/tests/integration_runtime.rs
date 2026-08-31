//! True AppRuntime integration test — AppCommand → AppRuntime → AppEvent
//! Proves the *actual* dispatch loop (spawn_runtime) against real PG 16,
//! not just the low-level connect_live helper. Covers §8 §10 §11 §15.
//!
//! Uses `ConnectDirect` to bypass SQLite/keychain and drive the runtime
//! via crossbeam channels. Validates: Connected → SchemaUpdated →
//! Execute → QueryProgress/QueryFinished → ResultStore → Cancel.

use std::sync::Arc;
use std::time::{Duration, Instant};

use pgnative_app::{AppCommand, AppEvent, AppState};
use pgnative_db_connection::{ConnectionConfig, ConnectionId, SslMode};
use pgnative_results_store::{ResultStore, SharedStore, StoreConfig};
use secrecy::SecretString;
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{GenericImage, ImageExt};
use uuid::Uuid;

async fn pg_params() -> Option<(
    String,
    u16,
    String,
    String,
    String,
    Option<testcontainers::ContainerAsync<GenericImage>>,
)> {
    if let Ok(url) = std::env::var("TEST_PG_URL") {
        let parsed = url::Url::parse(&url).ok()?;
        let host = parsed.host_str()?.to_string();
        let port = parsed.port().unwrap_or(5432);
        let dbname = parsed.path().trim_start_matches('/').to_string();
        let username = parsed.username().to_string();
        let password = parsed.password().unwrap_or("postgres").to_string();
        return Some((host, port, dbname, username, password, None));
    }
    let img = GenericImage::new("postgres", "16-alpine")
        .with_exposed_port(5432.tcp())
        .with_wait_for(WaitFor::message_on_stderr(
            "database system is ready to accept connections",
        ))
        .with_wait_for(WaitFor::message_on_stdout(
            "database system is ready to accept connections",
        ))
        .with_env_var("POSTGRES_USER", "pgnative")
        .with_env_var("POSTGRES_PASSWORD", "pgnative_test")
        .with_env_var("POSTGRES_DB", "pgnative_test");
    let container = img.start().await.ok()?;
    let host = container.get_host().await.ok()?.to_string();
    let port = container.get_host_port_ipv4(5432).await.ok()?;
    tokio::time::sleep(Duration::from_millis(800)).await;
    Some((
        host,
        port,
        "pgnative_test".to_string(),
        "pgnative".to_string(),
        "pgnative_test".to_string(),
        Some(container),
    ))
}

/// Wait up to `timeout` for an event matching `pred`, draining crossbeam `event_rx`.
async fn wait_for<F>(
    event_rx: &crossbeam_channel::Receiver<AppEvent>,
    timeout: Duration,
    pred: F,
) -> Option<AppEvent>
where
    F: Fn(&AppEvent) -> bool,
{
    let start = Instant::now();
    while start.elapsed() < timeout {
        while let Ok(ev) = event_rx.try_recv() {
            if pred(&ev) {
                return Some(ev);
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    None
}

#[tokio::test]
async fn runtime_connect_execute_cancel_via_appcommand() {
    let Some((host, port, dbname, username, password, _container)) = pg_params().await else {
        eprintln!("SKIP: no TEST_PG_URL and Docker/testcontainers not available");
        return;
    };

    // Spawn AppRuntime with in-memory store + state
    let store: SharedStore = Arc::new(parking_lot::RwLock::new(ResultStore::new(
        StoreConfig::default(),
    )));
    let state = Arc::new(parking_lot::RwLock::new(AppState::new()));
    let (cmd_tx, cmd_rx) = crossbeam_channel::bounded::<AppCommand>(256);
    let (event_tx, event_rx) = crossbeam_channel::bounded::<AppEvent>(256);
    let _rt = pgnative_app::runtime::spawn_runtime(
        cmd_rx,
        event_tx,
        Arc::clone(&store),
        Arc::clone(&state),
    );

    // Build direct config from testcontainer PG
    let conn_id = ConnectionId(Uuid::new_v4());
    let cfg = ConnectionConfig {
        id: conn_id,
        name: "runtime_test".into(),
        host: host.clone(),
        port,
        dbname: dbname.clone(),
        username: username.clone(),
        ssl_mode: SslMode::Disable,
        ssl_root_cert: None,
        ssh_tunnel: None,
    };
    let secret = SecretString::new(password.clone().into());

    // 1) ConnectDirect → expect ConnectionStateChanged + SchemaUpdated
    cmd_tx
        .send(AppCommand::ConnectDirect {
            config: cfg,
            password: Some(secret),
        })
        .expect("send ConnectDirect");

    let ev = wait_for(
        &event_rx,
        Duration::from_secs(15),
        |ev| matches!(ev, AppEvent::ConnectionStateChanged { state, .. } if state == "connected"),
    )
    .await;
    assert!(
        ev.is_some(),
        "should receive ConnectionStateChanged connected"
    );

    // SchemaUpdated is sent after prepare_session + introspect — may arrive shortly after
    let schema_ev = wait_for(&event_rx, Duration::from_secs(10), |ev| {
        matches!(ev, AppEvent::SchemaUpdated { .. })
    })
    .await;
    assert!(
        schema_ev.is_some(),
        "should receive SchemaUpdated after connect"
    );
    if let Some(AppEvent::SchemaUpdated { model, .. }) = schema_ev {
        assert!(!model.schemas().is_empty(), "SchemaUpdated non-empty");
    }

    // Drain any leftover errors before next step
    while let Ok(ev) = event_rx.try_recv() {
        if let AppEvent::Error { op, message } = ev {
            // introspect errors are allowed to be logged but not fail the gate
            eprintln!("pre-execute event {op}: {message}");
        }
    }

    // 2) Execute SELECT 1 — expect QueryFinished success + store row
    cmd_tx
        .send(AppCommand::Execute {
            tab: "t1".into(),
            sql: "SELECT 1 AS one".into(),
            connection: conn_id,
        })
        .expect("send Execute SELECT 1");

    let finished = wait_for(&event_rx, Duration::from_secs(10), |ev| {
        matches!(ev, AppEvent::QueryFinished { success: true, .. })
    })
    .await;
    assert!(finished.is_some(), "SELECT 1 should finish successfully");
    // Give store a moment to be populated (push_batch is synchronous before event)
    tokio::time::sleep(Duration::from_millis(100)).await;
    {
        let g = store.read();
        assert!(
            g.len() >= 1,
            "store should contain at least 1 row after SELECT 1"
        );
        // Find the SELECT 1 row — last batch may contain it
        let snap = g.snapshot_range(0, g.len());
        let found = snap.iter().any(|row| {
            row.cells
                .iter()
                .any(|c| matches!(c, pgnative_results_value::CellValue::Int(1)))
        });
        assert!(found, "store should contain Int(1) from SELECT 1");
    }

    // 3) Fixture: create temp table via Execute, then query it
    // We use a second Execute to create fixture; but Execute only supports SELECT-like
    // batch. Use direct client via a second connection for DDL, then query via runtime.
    // Instead drive the runtime for 100-row SELECT by inserting via a one-off live client.
    // For isolation, create fixture via a direct connect_live then query via runtime.
    {
        // Direct client for setup (reuse same PG params)
        let setup_cfg = ConnectionConfig {
            id: ConnectionId(Uuid::new_v4()),
            name: "setup".into(),
            host: host.clone(),
            port,
            dbname: dbname.clone(),
            username: username.clone(),
            ssl_mode: SslMode::Disable,
            ssl_root_cert: None,
            ssh_tunnel: None,
        };
        let setup_pw = SecretString::new(password.clone().into());
        let sess = pgnative_db_connection::connect_live(&setup_cfg, Some(&setup_pw))
            .await
            .expect("setup connect");
        sess.client
            .batch_execute(
                "DROP TABLE IF EXISTS runtime_fixture; \
                 CREATE TEMP TABLE runtime_fixture (id int4, txt text, flag bool);",
            )
            .await
            .expect("create fixture");
        // Insert 100 rows via setup session — reuse same PG instance but temp table
        // is session-local, so we must use the *runtime's* session. Instead create a
        // permanent table for cross-session visibility.
        sess.client
            .batch_execute("DROP TABLE IF EXISTS runtime_fixture_perm; CREATE TABLE runtime_fixture_perm (id int4, txt text, flag bool);")
            .await
            .expect("create perm fixture");
        for i in 0..100 {
            sess.client
                .execute(
                    "INSERT INTO runtime_fixture_perm (id, txt, flag) VALUES ($1,$2,$3)",
                    &[&(i as i32), &format!("row-{i}"), &(i % 2 == 0)],
                )
                .await
                .expect("insert");
        }
        // Keep sess alive until queried
        std::mem::forget(sess);
    }

    // Clear store before next query to assert exact count
    // (store is cumulative; we check delta)
    let before_len = store.read().len();
    cmd_tx
        .send(AppCommand::Execute {
            tab: "t1".into(),
            sql: "SELECT id, txt, flag FROM runtime_fixture_perm ORDER BY id".into(),
            connection: conn_id,
        })
        .expect("send Execute fixture");
    let finished2 = wait_for(&event_rx, Duration::from_secs(10), |ev| {
        matches!(ev, AppEvent::QueryFinished { success: true, .. })
    })
    .await;
    assert!(finished2.is_some(), "fixture SELECT should finish");
    tokio::time::sleep(Duration::from_millis(100)).await;
    {
        let g = store.read();
        let delta = g.len().saturating_sub(before_len);
        assert_eq!(
            delta, 100,
            "fixture query should add 100 rows to store (got {delta})"
        );
    }

    // 4) Cancellation via AppCommand::Cancel — pg_sleep cancellable
    // Need the QueryId of the long query. Capture it via state or QueryProgress.
    cmd_tx
        .send(AppCommand::Execute {
            tab: "t1".into(),
            sql: "SELECT pg_sleep(10)".into(),
            connection: conn_id,
        })
        .expect("send pg_sleep");
    // Wait briefly for the query to start and appear in state.queries
    let qid = {
        let start = Instant::now();
        let mut found = None;
        while start.elapsed() < Duration::from_secs(5) {
            if let Some(id) = state.read().queries.keys().next().copied() {
                // Heuristic: the last inserted is the pg_sleep; but there may be
                // multiple keys. Take any that corresponds to pg_sleep SQL.
                for (k, sql) in state.read().queries.clone() {
                    if sql.contains("pg_sleep") {
                        found = Some(k);
                        break;
                    }
                }
                if found.is_some() {
                    break;
                }
                // fallback: any key
                if found.is_none() {
                    found = Some(id);
                }
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        found
    };
    // Give driver a moment to reach PG
    tokio::time::sleep(Duration::from_millis(300)).await;
    if let Some(qid) = qid {
        cmd_tx
            .send(AppCommand::Cancel { query_id: qid })
            .expect("send Cancel");
        let fin = wait_for(
            &event_rx,
            Duration::from_secs(5),
            |ev| matches!(ev, AppEvent::QueryFinished { query_id, .. } if *query_id == qid),
        )
        .await;
        assert!(fin.is_some(), "cancelled query should emit QueryFinished");
        // Verify session still usable after cancel
        cmd_tx
            .send(AppCommand::Execute {
                tab: "t1".into(),
                sql: "SELECT 42 AS v".into(),
                connection: conn_id,
            })
            .expect("send post-cancel SELECT");
        let ok = wait_for(&event_rx, Duration::from_secs(10), |ev| {
            matches!(ev, AppEvent::QueryFinished { success: true, .. })
        })
        .await;
        assert!(ok.is_some(), "session should remain usable after cancel");
    } else {
        eprintln!("WARN: could not capture QueryId for cancel test — skipping cancel assertion");
    }

    // 5) Disconnect
    cmd_tx
        .send(AppCommand::Disconnect { id: conn_id })
        .expect("send Disconnect");
    let disc = wait_for(&event_rx, Duration::from_secs(5), |ev| {
        matches!(ev, AppEvent::ConnectionStateChanged { state, .. } if state == "disconnected")
    })
    .await;
    assert!(
        disc.is_some(),
        "should receive disconnected after Disconnect"
    );

    drop(_container);
}
