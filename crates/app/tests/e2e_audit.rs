//! End-to-end audit driver: AppCommand → AppRuntime → real PostgreSQL.
//!
//! Gated on `E2E_PG_URL` (e.g. `postgres://postgres@/auditdb?host=/tmp/pgaudit`);
//! skips (like the other live tests) when absent so CI stays green.
//!
//! This exercises the REAL dispatch loop (`spawn_runtime`) against a REAL
//! seeded database: connect → introspect → query every edge table → CRUD →
//! 120k-row stream → cancel → error recovery → history → CSV export →
//! tx-decision dialog flow → refresh. egui rendering itself is verified
//! separately via screenshots of the real binary.
use std::sync::Arc;
use std::time::{Duration, Instant};

use pgnative_app::{AppCommand, AppEvent, AppState, ExportFormat};
use pgnative_db::connection::{ConnectionConfig, ConnectionId, QueryId, SslMode};
use pgnative_results::store::{ResultStore, SharedStore, StoreConfig};

fn e2e_params() -> Option<(String, u16, String, String, Option<String>)> {
    // Direct vars first (unix-socket URLs have an empty host that `url` rejects).
    if std::env::var("E2E_PG_URL").is_ok() || std::env::var("E2E_PG_HOST").is_ok() {
        let host = std::env::var("E2E_PG_HOST").unwrap_or("/tmp/pgaudit".into());
        let port = std::env::var("E2E_PG_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(5432);
        let dbname = std::env::var("E2E_PG_DB").unwrap_or("auditdb".into());
        let username = std::env::var("E2E_PG_USER").unwrap_or("postgres".into());
        let password = std::env::var("E2E_PG_PASSWORD").ok();
        if let Ok(url) = std::env::var("E2E_PG_URL") {
            if let Ok(parsed) = url::Url::parse(&url) {
                let h = parsed
                    .query_pairs()
                    .find(|(k, _)| k == "host")
                    .map(|(_, v)| v.to_string())
                    .or_else(|| parsed.host_str().map(str::to_string))
                    .unwrap_or(host);
                let p = parsed.port().unwrap_or(port);
                let db = parsed.path().trim_start_matches('/').to_string();
                let db = if db.is_empty() { dbname } else { db };
                let u = parsed.username().to_string();
                let u = if u.is_empty() { username } else { u };
                let pw = parsed.password().map(str::to_string).or(password);
                return Some((h, p, db, u, pw));
            }
        }
        return Some((host, port, dbname, username, password));
    }
    None
}

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

fn is_finished(ev: &AppEvent) -> bool {
    matches!(ev, AppEvent::QueryFinished { .. })
}

struct Driver {
    cmd_tx: crossbeam_channel::Sender<AppCommand>,
    event_rx: crossbeam_channel::Receiver<AppEvent>,
    store: SharedStore,
    state: Arc<parking_lot::RwLock<AppState>>,
    conn: ConnectionId,
    stage: usize,
}

impl Driver {
    fn log(&mut self, msg: &str) {
        self.stage += 1;
        eprintln!("E2E [{}] {msg}", self.stage);
    }

    /// Execute `sql`, wait for finish, return (success, qid).
    async fn exec(&mut self, sql: &str, timeout: Duration) -> (bool, QueryId) {
        self.cmd_tx
            .send(AppCommand::Execute {
                tab: "e2e".into(),
                sql: sql.into(),
                connection: self.conn,
            })
            .expect("send Execute");
        let ev = wait_for(&self.event_rx, timeout, is_finished)
            .await
            .unwrap_or_else(|| panic!("timeout waiting for finish: {sql}"));
        match ev {
            AppEvent::QueryFinished { query_id, success } => (success, query_id),
            _ => unreachable!(),
        }
    }

    async fn exec_ok(&mut self, sql: &str) -> QueryId {
        let (ok, qid) = self.exec(sql, Duration::from_secs(30)).await;
        assert!(ok, "query failed: {sql}");
        qid
    }

    fn store_cols(&self) -> Vec<String> {
        self.store.read().columns()
    }

    fn store_cell(&self, row: usize, col: usize) -> String {
        self.store.read().snapshot_range(row, 1)[0].cells[col].to_display_string()
    }

    fn store_len(&self) -> usize {
        self.store.read().len()
    }
}

#[tokio::test]
async fn e2e_full_audit() {
    let Some((host, port, dbname, username, password)) = e2e_params() else {
        eprintln!("SKIP: set E2E_PG_URL to run the audit driver");
        return;
    };

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

    let conn_id = ConnectionId(uuid::Uuid::new_v4());
    let cfg = ConnectionConfig {
        id: conn_id,
        name: "e2e".into(),
        host: host.clone(),
        port,
        dbname: dbname.clone(),
        username: username.clone(),
        ssl_mode: SslMode::Disable,
        ssl_root_cert: None,
        ssh_tunnel: None,
    };
    let pw = password.map(|p| secrecy::SecretString::new(p.into()));
    let mut d = Driver {
        cmd_tx: cmd_tx.clone(),
        event_rx: event_rx.clone(),
        store: Arc::clone(&store),
        state: Arc::clone(&state),
        conn: conn_id,
        stage: 0,
    };

    // -- S1: connect + introspect -------------------------------------------
    d.log(&format!("connect to {dbname} as {username}"));
    d.cmd_tx
        .send(AppCommand::ConnectDirect {
            config: cfg,
            password: pw,
        })
        .expect("send ConnectDirect");
    let ev = wait_for(
        &d.event_rx,
        Duration::from_secs(20),
        |e| matches!(e, AppEvent::ConnectionStateChanged { state, .. } if state == "connected"),
    )
    .await;
    assert!(ev.is_some(), "never connected");
    let ev = wait_for(&d.event_rx, Duration::from_secs(20), |e| {
        matches!(e, AppEvent::SchemaUpdated { .. })
    })
    .await;
    let AppEvent::SchemaUpdated { model, .. } = ev.expect("no SchemaUpdated") else {
        unreachable!()
    };
    for want in [
        "users",
        "orders",
        "edge_alltypes",
        "weird table",
        "active_users",
    ] {
        assert!(
            model.relations().iter().any(|r| r.name == want),
            "schema missing {want}"
        );
    }
    d.log("connected + schema has all edge relations");

    // -- S2: trivial query ----------------------------------------------------
    d.exec_ok("SELECT 1 AS one").await;
    assert_eq!(d.store_len(), 1);
    assert_eq!(d.store_cols(), vec!["one".to_string()]);
    assert_eq!(d.store_cell(0, 0), "1");
    d.log("SELECT 1 ok");

    // -- S3: all-types matrix (25 cols, full + NULL-heavy rows) ---------------
    d.exec_ok("SELECT * FROM edge_alltypes ORDER BY i4").await;
    assert_eq!(d.store_len(), 2);
    let cols = d.store_cols();
    assert_eq!(cols.len(), 25, "expected 25 columns, got {cols:?}");
    for want in ["i4", "vc", "b", "m", "jb", "ia", "by", "ip", "z"] {
        assert!(cols.contains(&want.to_string()), "missing col {want}");
    }
    assert_eq!(
        d.store_cell(0, cols.iter().position(|c| c == "vc").unwrap()),
        "twenty-chars-here!"
    );
    assert_eq!(
        d.store_cell(1, cols.iter().position(|c| c == "i4").unwrap()),
        "2"
    );
    d.log("all-types matrix ok (incl NULL-heavy row)");

    // -- S4: empty table -------------------------------------------------------
    d.exec_ok("SELECT * FROM edge_empty").await;
    assert_eq!(d.store_len(), 0);
    assert_eq!(d.store_cols(), vec!["id".to_string(), "v".to_string()]);
    d.log("empty table ok");

    // -- S5: weird identifiers -------------------------------------------------
    d.exec_ok("SELECT \"Id\", \"odd column\", \"UPPER\" FROM \"weird table\"")
        .await;
    assert_eq!(d.store_len(), 1);
    assert_eq!(d.store_cell(0, 1), "sp ace");
    d.log("quoted identifiers ok");

    // -- S6: generated / identity ----------------------------------------------
    d.exec_ok("SELECT total FROM edge_generated ORDER BY id")
        .await;
    assert_eq!(d.store_cell(0, 0), "7");
    d.log("generated column ok");

    // -- S7: large values -------------------------------------------------------
    d.exec_ok("SELECT length(payload) AS n FROM edge_large")
        .await;
    assert_eq!(d.store_cell(0, 0), "108000");
    d.log("108KB text cell ok");

    // -- S8: CRUD ---------------------------------------------------------------
    d.exec_ok("DELETE FROM crud_check").await;
    d.exec_ok("INSERT INTO crud_check(id, v) VALUES (7, 'seven'), (8, 'eight')")
        .await;
    d.exec_ok("SELECT count(*) AS c FROM crud_check").await;
    assert_eq!(d.store_cell(0, 0), "2");
    d.exec_ok("UPDATE crud_check SET v='seven-upd' WHERE id=7")
        .await;
    d.exec_ok("SELECT v FROM crud_check WHERE id=7").await;
    assert_eq!(d.store_cell(0, 0), "seven-upd");
    d.exec_ok("DELETE FROM crud_check WHERE id=8").await;
    d.exec_ok("SELECT count(*) AS c FROM crud_check").await;
    assert_eq!(d.store_cell(0, 0), "1");
    d.exec_ok("DELETE FROM crud_check").await;
    d.log("CRUD walk ok");

    // -- S9: 120k-row stream, bounded store --------------------------------------
    let (ok, _) = d
        .exec("SELECT * FROM big_series", Duration::from_secs(180))
        .await;
    assert!(ok, "big_series failed");
    assert_eq!(store.read().total_pushed(), 120_000);
    assert!(
        d.store_len() <= 50_000,
        "eviction did not fire: {}",
        d.store_len()
    );
    assert!(store.read().byte_used() <= 64 * 1024 * 1024);
    d.log(&format!(
        "120k rows streamed, store capped at {}",
        d.store_len()
    ));

    // -- S10: cancel a long query, session stays usable ---------------------------
    d.cmd_tx
        .send(AppCommand::Execute {
            tab: "e2e".into(),
            sql: "SELECT pg_sleep(30)".into(),
            connection: d.conn,
        })
        .expect("send sleep");
    // pg_sleep emits no batches (single row at the very end), so no
    // QueryProgress ever arrives — take the qid from tracked state instead
    // (also proves the runtime registers the query before it completes).
    let reg_start = Instant::now();
    let query_id = loop {
        let keys: Vec<_> = d.state.read().queries.keys().copied().collect();
        if let [only] = keys.as_slice() {
            break *only;
        }
        assert!(
            reg_start.elapsed() < Duration::from_secs(10),
            "query never registered"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    d.cmd_tx
        .send(AppCommand::Cancel { query_id })
        .expect("send Cancel");
    let ev = wait_for(&d.event_rx, Duration::from_secs(15), is_finished).await;
    assert!(matches!(
        ev,
        Some(AppEvent::QueryFinished { success: false, .. })
    ));
    d.exec_ok("SELECT 1 AS alive").await;
    assert_eq!(d.store_cell(0, 0), "1");
    d.log("cancel works, session usable after");

    // -- S11: error recovery ------------------------------------------------------
    let (ok, _) = d
        .exec("SELECT * FROM no_such_table_xyz", Duration::from_secs(30))
        .await;
    assert!(!ok, "bad SQL unexpectedly succeeded");
    d.exec_ok("SELECT 1 AS recovered").await;
    assert_eq!(d.store_cell(0, 0), "1");
    d.log("error recovery ok");

    // -- S12: history --------------------------------------------------------------
    // History insert is fire-and-forget; poll until visible.
    let start = Instant::now();
    let found = loop {
        d.cmd_tx
            .send(AppCommand::HistorySearch {
                query: "edge_alltypes".into(),
            })
            .expect("send HistorySearch");
        let hit = wait_for(
            &d.event_rx,
            Duration::from_secs(5),
            |e| matches!(e, AppEvent::HistoryResults { results } if results.iter().any(|r| r.contains("edge_alltypes"))),
        )
        .await
        .is_some();
        if hit || start.elapsed() > Duration::from_secs(20) {
            break hit;
        }
    };
    assert!(found, "history never surfaced edge_alltypes query");
    d.log("history search ok");

    // -- S13: CSV export --------------------------------------------------------------
    let qid = d
        .exec_ok("SELECT i4, vc FROM edge_alltypes ORDER BY i4")
        .await;
    d.cmd_tx
        .send(AppCommand::Export {
            query_id: qid,
            format: ExportFormat::Csv,
        })
        .expect("send Export");
    let ev = wait_for(&d.event_rx, Duration::from_secs(15), |e| {
        matches!(e, AppEvent::ExportProgress { .. })
    })
    .await;
    let AppEvent::ExportProgress { written, path, .. } = ev.expect("no ExportProgress") else {
        unreachable!()
    };
    assert_eq!(written, 2);
    let body = std::fs::read_to_string(&path).expect("read export file");
    let mut lines = body.lines();
    assert_eq!(lines.next().unwrap(), "i4,vc");
    assert_eq!(body.lines().count(), 3);
    std::fs::remove_file(&path).ok();
    d.log(&format!("CSV export ok ({written} rows -> {path})"));

    // -- S14: tx decision dialog flow --------------------------------------------------
    d.exec_ok("BEGIN").await;
    d.exec_ok("INSERT INTO crud_check(id, v) VALUES (99, 'tx-row')")
        .await;
    assert!(
        d.state.read().tx_state(d.conn).is_active(),
        "tx not tracked as active"
    );
    d.cmd_tx
        .send(AppCommand::Disconnect { id: d.conn })
        .expect("send Disconnect");
    let ev = wait_for(&d.event_rx, Duration::from_secs(10), |e| {
        matches!(e, AppEvent::DisconnectRequiresDecision { .. })
    })
    .await;
    assert!(ev.is_some(), "no DisconnectRequiresDecision");
    // Session must still be alive (no disconnected event yet).
    assert!(
        wait_for(
            &d.event_rx,
            Duration::from_secs(2),
            |e| matches!(e, AppEvent::ConnectionStateChanged { state, .. } if state == "disconnected"),
        )
        .await
        .is_none(),
        "teardown happened before decision"
    );
    d.cmd_tx
        .send(AppCommand::ResolveTransaction {
            id: d.conn,
            commit: true,
        })
        .expect("send ResolveTransaction");
    let ev = wait_for(
        &d.event_rx,
        Duration::from_secs(10),
        |e| matches!(e, AppEvent::ConnectionStateChanged { state, .. } if state == "disconnected"),
    )
    .await;
    assert!(ev.is_some(), "no disconnected after resolve");
    // Reconnect and prove the committed row survived.
    let cfg2 = ConnectionConfig {
        id: ConnectionId(uuid::Uuid::new_v4()),
        name: "e2e-2".into(),
        host: host.clone(),
        port,
        dbname: dbname.clone(),
        username: username.clone(),
        ssl_mode: SslMode::Disable,
        ssl_root_cert: None,
        ssh_tunnel: None,
    };
    d.conn = cfg2.id;
    d.cmd_tx
        .send(AppCommand::ConnectDirect {
            config: cfg2,
            password: None,
        })
        .expect("reconnect");
    wait_for(
        &d.event_rx,
        Duration::from_secs(20),
        |e| matches!(e, AppEvent::ConnectionStateChanged { state, .. } if state == "connected"),
    )
    .await
    .expect("reconnect failed");
    d.exec_ok("SELECT v FROM crud_check WHERE id=99").await;
    assert_eq!(d.store_cell(0, 0), "tx-row");
    d.exec_ok("DELETE FROM crud_check WHERE id=99").await;
    d.log("tx dialog flow ok (commit survived reconnect)");

    // -- S15: refresh schema -----------------------------------------------------------
    d.cmd_tx
        .send(AppCommand::RefreshSchema { connection: d.conn })
        .expect("send RefreshSchema");
    wait_for(&d.event_rx, Duration::from_secs(20), |e| {
        matches!(e, AppEvent::SchemaUpdated { .. })
    })
    .await
    .expect("no SchemaUpdated after refresh");
    d.log("schema refresh ok");

    eprintln!("E2E ALL STAGES GREEN");
}
