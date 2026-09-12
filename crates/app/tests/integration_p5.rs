//! P5 hardening live tests — edit roundtrip + tx disconnect (§20 §21 §22).
//! Drives the real AppRuntime against PG16 (testcontainers or TEST_PG_URL).

use std::sync::Arc;
use std::time::{Duration, Instant};

use pgnative_app::{AppCommand, AppEvent, AppState};
use pgnative_db::connection::{ConnectionConfig, ConnectionId, SslMode};
use pgnative_results::edit::{update_sql_optimistic, update_sql_with_pk, ColumnDiff};
use pgnative_results::store::{ResultStore, SharedStore, StoreConfig};
use pgnative_schema::model::column::Column;
use pgnative_schema::model::relation::{PrimaryKey, Relation};
use pgnative_schema::model::types::{Id, Nullability, Oid, RelationKind, ValueSource};
use secrecy::SecretString;
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{GenericImage, ImageExt};
use uuid::Uuid;

type PgParts = (
    String,
    u16,
    String,
    String,
    String,
    Option<testcontainers::ContainerAsync<GenericImage>>,
);

async fn pg_params() -> Option<PgParts> {
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

struct Harness {
    cmd_tx: crossbeam_channel::Sender<AppCommand>,
    event_rx: crossbeam_channel::Receiver<AppEvent>,
    store: SharedStore,
    state: Arc<parking_lot::RwLock<AppState>>,
    conn_id: ConnectionId,
    #[allow(dead_code)]
    rt: tokio::task::JoinHandle<()>,
}

async fn launch(parts: &PgParts) -> Harness {
    let (host, port, dbname, username, password, _) = parts;
    let store: SharedStore = Arc::new(parking_lot::RwLock::new(ResultStore::new(
        StoreConfig::default(),
    )));
    let state = Arc::new(parking_lot::RwLock::new(AppState::new()));
    let (cmd_tx, cmd_rx) = crossbeam_channel::bounded::<AppCommand>(256);
    let (event_tx, event_rx) = crossbeam_channel::bounded::<AppEvent>(256);
    let rt = pgnative_app::runtime::spawn_runtime(
        cmd_rx,
        event_tx,
        Arc::clone(&store),
        Arc::clone(&state),
    );
    let conn_id = ConnectionId(Uuid::new_v4());
    let cfg = ConnectionConfig {
        id: conn_id,
        name: "p5_test".into(),
        host: host.clone(),
        port: *port,
        dbname: dbname.clone(),
        username: username.clone(),
        ssl_mode: SslMode::Disable,
        ssl_root_cert: None,
        ssh_tunnel: None,
    };
    cmd_tx
        .send(AppCommand::ConnectDirect {
            config: cfg,
            password: Some(SecretString::new(password.clone().into())),
        })
        .expect("send ConnectDirect");
    let ev = wait_for(
        &event_rx,
        Duration::from_secs(15),
        |ev| matches!(ev, AppEvent::ConnectionStateChanged { state, .. } if state == "connected"),
    )
    .await;
    assert!(ev.is_some(), "should connect");
    Harness {
        cmd_tx,
        event_rx,
        store,
        state,
        conn_id,
        rt,
    }
}

async fn setup_client(parts: &PgParts) -> pgnative_db::connection::LiveSession {
    let (host, port, dbname, username, password, _) = parts;
    let cfg = ConnectionConfig {
        id: ConnectionId(Uuid::new_v4()),
        name: "p5_setup".into(),
        host: host.clone(),
        port: *port,
        dbname: dbname.clone(),
        username: username.clone(),
        ssl_mode: SslMode::Disable,
        ssl_root_cert: None,
        ssh_tunnel: None,
    };
    pgnative_db::connection::connect_live(&cfg, Some(&SecretString::new(password.clone().into())))
        .await
        .expect("setup connect_live")
}

async fn exec_ok(h: &Harness, tab: &str, sql: &str) {
    h.cmd_tx
        .send(AppCommand::Execute {
            tab: tab.into(),
            sql: sql.into(),
            connection: h.conn_id,
        })
        .expect("send Execute");
    let fin = wait_for(&h.event_rx, Duration::from_secs(15), |ev| {
        matches!(ev, AppEvent::QueryFinished { success: true, .. })
    })
    .await;
    assert!(fin.is_some(), "Execute should finish: {sql}");
    tokio::time::sleep(Duration::from_millis(100)).await;
}

/// Substitute $1..$N with quoted text literals (test-only inlining so the
/// runtime's text-only Execute path runs exactly the generated SQL shape).
fn inline_params(sql: &str, params: &[String]) -> String {
    let mut out = sql.to_string();
    for (i, p) in params.iter().enumerate() {
        out = out.replacen(
            &format!("${}", i + 1),
            &format!("'{}'", p.replace('\'', "''")),
            1,
        );
    }
    assert!(!out.contains('$'), "all params must be inlined: {out}");
    out
}

fn sm_col(id: u32, name: &str, pos: u16) -> Column {
    Column {
        id: Id(id),
        owner: Id(0),
        name: name.into(),
        position: pos,
        ty: Id(0),
        nullability: Nullability::Nullable,
        has_default: false,
        default_expr: None,
        value_source: ValueSource::Stored,
    }
}

fn rel_single() -> Relation {
    Relation {
        id: Id(0),
        schema: Id(0),
        oid: Oid(1),
        name: "p5_edit_single".into(),
        kind: RelationKind::Table,
        columns: vec![sm_col(0, "id", 1), sm_col(1, "txt", 2)],
        primary_key: Some(PrimaryKey {
            columns: vec![Id(0)],
            name: None,
        }),
        unique_keys: vec![],
        foreign_keys_out: vec![],
        foreign_keys_in: vec![],
        comment: None,
    }
}

fn rel_comp() -> Relation {
    Relation {
        id: Id(0),
        schema: Id(0),
        oid: Oid(2),
        name: "p5_edit_comp".into(),
        kind: RelationKind::Table,
        columns: vec![sm_col(0, "a", 1), sm_col(1, "b", 2), sm_col(2, "txt", 3)],
        primary_key: Some(PrimaryKey {
            columns: vec![Id(0), Id(1)],
            name: None,
        }),
        unique_keys: vec![],
        foreign_keys_out: vec![],
        foreign_keys_in: vec![],
        comment: None,
    }
}

fn rel_nopk() -> Relation {
    Relation {
        id: Id(0),
        schema: Id(0),
        oid: Oid(3),
        name: "p5_edit_nopk".into(),
        kind: RelationKind::Table,
        columns: vec![sm_col(0, "id", 1), sm_col(1, "txt", 2)],
        primary_key: None,
        unique_keys: vec![],
        foreign_keys_out: vec![],
        foreign_keys_in: vec![],
        comment: None,
    }
}

fn txt_diff(old: &str, new: &str) -> Vec<ColumnDiff> {
    vec![ColumnDiff {
        col: "txt".into(),
        old: old.into(),
        new: new.into(),
    }]
}

#[tokio::test]
async fn edit_live_roundtrip() {
    let Some(parts) = pg_params().await else {
        eprintln!("SKIP: no TEST_PG_URL and Docker not available");
        return;
    };
    let setup = setup_client(&parts).await;
    setup
        .client
        .batch_execute(
            "DROP TABLE IF EXISTS p5_edit_single; \
             DROP TABLE IF EXISTS p5_edit_comp; \
             DROP TABLE IF EXISTS p5_edit_nopk; \
             CREATE TABLE p5_edit_single (id int4 PRIMARY KEY, txt text); \
             INSERT INTO p5_edit_single VALUES (1, 'a'); \
             CREATE TABLE p5_edit_comp (a int4, b int4, txt text, PRIMARY KEY (a, b)); \
             INSERT INTO p5_edit_comp VALUES (1, 2, 'a'); \
             CREATE TABLE p5_edit_nopk (id int4, txt text); \
             INSERT INTO p5_edit_nopk VALUES (1, 'a');",
        )
        .await
        .expect("create edit fixtures");
    drop(setup);

    let h = launch(&parts).await;

    // 1) Single-PK UPDATE through the real runtime (generated SQL shape).
    let (sql, params) = update_sql_with_pk(
        &rel_single(),
        &txt_diff("a", "b"),
        &[("id".into(), "1".into())],
    )
    .expect("gen single-pk update");
    exec_ok(&h, "edit1", &inline_params(&sql, &params)).await;
    exec_ok(&h, "edit1", "SELECT txt FROM p5_edit_single ORDER BY id").await;
    {
        let g = h.store.read();
        let snap = g.snapshot_range(0, g.len());
        let found = snap.iter().any(|row| {
            row.cells
                .iter()
                .any(|c| matches!(c, pgnative_results::value::CellValue::Text(t) if t == "b"))
        });
        assert!(found, "single-PK UPDATE must change exactly row 1 to 'b'");
    }

    // 2) No-PK table: generation refuses — never reaches the wire.
    let err = update_sql_with_pk(
        &rel_nopk(),
        &txt_diff("a", "b"),
        &[("id".into(), "1".into())],
    );
    assert!(err.is_err(), "no-PK edit must be refused");

    // 3) Composite PK + optimistic stale guard with TRUE $N param binding.
    // NOTE: the edit crate yields Vec<String> params; callers bind each value
    // with its column type (text stays String, int4 binds i32) — the runtime's
    // text-only Execute path inlines instead (see step 1).
    let live = setup_client(&parts).await;
    let (csql, cparams) = update_sql_with_pk(
        &rel_comp(),
        &txt_diff("a", "x"),
        &[("a".into(), "1".into()), ("b".into(), "2".into())],
    )
    .expect("gen composite update");
    assert_eq!(cparams.len(), 3, "SET-new + 2 PK params");
    let (x_new, a_pk, b_pk) = (cparams[0].clone(), cparams[1].clone(), cparams[2].clone());
    let (a_pk, b_pk): (i32, i32) = (a_pk.parse().unwrap(), b_pk.parse().unwrap());
    let rows = live
        .client
        .query(&csql, &[&x_new, &a_pk, &b_pk])
        .await
        .expect("composite update");
    assert_eq!(rows.len(), 1, "composite UPDATE...RETURNING * hits 1 row");
    let t: String = rows[0].get("txt");
    assert_eq!(t, "x");

    // Stale write: someone else moved txt to 'x', our old value 'a' is stale.
    let (ssql, sparams) = update_sql_optimistic(
        &rel_comp(),
        &txt_diff("a", "stale-overwrite"),
        &[("a".into(), "1".into()), ("b".into(), "2".into())],
    )
    .expect("gen optimistic update");
    let stale_new = sparams[0].clone();
    let stale_old = sparams[3].clone();
    let stale_rows = live
        .client
        .query(&ssql, &[&stale_new, &a_pk, &b_pk, &stale_old])
        .await
        .expect("stale optimistic update");
    assert!(
        stale_rows.is_empty(),
        "stale optimistic UPDATE must match 0 rows (conflict, not overwrite)"
    );
    let cur: String = live
        .client
        .query("SELECT txt FROM p5_edit_comp WHERE a = 1 AND b = 2", &[])
        .await
        .expect("re-select")[0]
        .get("txt");
    assert_eq!(cur, "x", "stale write must not silently overwrite");

    // Fresh optimistic write succeeds.
    let (fsql, fparams) = update_sql_optimistic(
        &rel_comp(),
        &txt_diff("x", "y"),
        &[("a".into(), "1".into()), ("b".into(), "2".into())],
    )
    .expect("gen fresh optimistic update");
    let fresh_new = fparams[0].clone();
    let fresh_old = fparams[3].clone();
    let fresh_rows = live
        .client
        .query(&fsql, &[&fresh_new, &a_pk, &b_pk, &fresh_old])
        .await
        .expect("fresh optimistic update");
    assert_eq!(fresh_rows.len(), 1);

    live.client
        .batch_execute(
            "DROP TABLE p5_edit_single; DROP TABLE p5_edit_comp; DROP TABLE p5_edit_nopk;",
        )
        .await
        .expect("drop edit fixtures");
    drop(live);
    drop(parts.5);
}

#[tokio::test]
async fn tx_disconnect_dialog_flow() {
    let Some(parts) = pg_params().await else {
        eprintln!("SKIP: no TEST_PG_URL and Docker not available");
        return;
    };
    let setup = setup_client(&parts).await;
    setup
        .client
        .batch_execute("DROP TABLE IF EXISTS p5_tx_probe; CREATE TABLE p5_tx_probe (id int4);")
        .await
        .expect("create tx probe");
    drop(setup);

    let h = launch(&parts).await;

    // BEGIN flips the tracked tx state (§22 runtime wiring).
    exec_ok(&h, "tx1", "BEGIN").await;
    assert!(
        h.state.read().tx_state(h.conn_id).is_active(),
        "tx must be active after BEGIN"
    );
    exec_ok(&h, "tx1", "INSERT INTO p5_tx_probe VALUES (1)").await;

    // Disconnect with an open txn must emit the decision hook and KEEP the
    // session — no silent commit, no silent rollback, no teardown.
    h.cmd_tx
        .send(AppCommand::Disconnect { id: h.conn_id })
        .expect("send Disconnect");
    let decision = wait_for(&h.event_rx, Duration::from_secs(5), |ev| {
        matches!(ev, AppEvent::DisconnectRequiresDecision { .. })
    })
    .await;
    assert!(
        decision.is_some(),
        "Disconnect with open txn must emit DisconnectRequiresDecision"
    );
    let torn_down = wait_for(&h.event_rx, Duration::from_secs(2), |ev| {
        matches!(
            ev,
            AppEvent::ConnectionStateChanged { state, .. } if state == "disconnected"
        )
    })
    .await;
    assert!(
        torn_down.is_none(),
        "session must stay alive until the dialog resolves"
    );

    // Session still answers queries while the dialog is pending.
    exec_ok(&h, "tx1", "SELECT 1").await;

    // Rollback + disconnect: teardown emits `disconnected`, clears tracked tx,
    // and the uncommitted row is gone (rolled back on close — never committed).
    h.cmd_tx
        .send(AppCommand::ResolveTransaction {
            id: h.conn_id,
            commit: false,
        })
        .expect("send ResolveTransaction");
    let down = wait_for(&h.event_rx, Duration::from_secs(5), |ev| {
        matches!(
            ev,
            AppEvent::ConnectionStateChanged { state, .. } if state == "disconnected"
        )
    })
    .await;
    assert!(down.is_some(), "rollback must disconnect");
    assert!(
        h.state.read().tx.get(&h.conn_id).is_none(),
        "teardown must clear tracked tx state"
    );

    let probe = setup_client(&parts).await;
    let n: i64 = probe
        .client
        .query("SELECT count(*) AS n FROM p5_tx_probe", &[])
        .await
        .expect("probe count")[0]
        .get("n");
    assert_eq!(
        n, 0,
        "open txn must not be silently committed on disconnect"
    );
    probe
        .client
        .batch_execute("DROP TABLE p5_tx_probe;")
        .await
        .expect("drop tx probe");
    drop(probe);
    drop(parts.5);
}

#[tokio::test]
async fn tx_disconnect_commit_path() {
    let Some(parts) = pg_params().await else {
        eprintln!("SKIP: no TEST_PG_URL and Docker not available");
        return;
    };
    let setup = setup_client(&parts).await;
    setup
        .client
        .batch_execute("DROP TABLE IF EXISTS p5_tx_commit; CREATE TABLE p5_tx_commit (id int4);")
        .await
        .expect("create tx table");
    drop(setup);

    let h = launch(&parts).await;
    exec_ok(&h, "txc", "BEGIN").await;
    exec_ok(&h, "txc", "INSERT INTO p5_tx_commit VALUES (42)").await;
    h.cmd_tx
        .send(AppCommand::Disconnect { id: h.conn_id })
        .expect("send Disconnect");
    let decision = wait_for(&h.event_rx, Duration::from_secs(5), |ev| {
        matches!(ev, AppEvent::DisconnectRequiresDecision { .. })
    })
    .await;
    assert!(decision.is_some(), "open txn must require a decision");

    // Commit + disconnect: row survives on a fresh session.
    h.cmd_tx
        .send(AppCommand::ResolveTransaction {
            id: h.conn_id,
            commit: true,
        })
        .expect("send ResolveTransaction");
    let down = wait_for(&h.event_rx, Duration::from_secs(5), |ev| {
        matches!(
            ev,
            AppEvent::ConnectionStateChanged { state, .. } if state == "disconnected"
        )
    })
    .await;
    assert!(down.is_some(), "commit must disconnect");

    let probe = setup_client(&parts).await;
    let n: i64 = probe
        .client
        .query("SELECT count(*) AS n FROM p5_tx_commit", &[])
        .await
        .expect("probe count")[0]
        .get("n");
    assert_eq!(n, 1, "committed row must be visible after dialog commit");
    probe
        .client
        .batch_execute("DROP TABLE p5_tx_commit;")
        .await
        .expect("drop tx table");
    drop(probe);
    drop(parts.5);
}
