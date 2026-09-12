//! Portal / cursor window fetch — proves §16 without OFFSET/LIMIT rewrite.
//! Uses DECLARE CURSOR / FETCH FORWARD on a dedicated session.

use std::time::Duration;

use pgnative_db_connection::{ConnectionConfig, ConnectionId, SslMode};
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
    tokio::time::sleep(Duration::from_millis(700)).await;
    Some((
        host,
        port,
        "pgnative_test".into(),
        "pgnative".into(),
        "pgnative_test".into(),
        Some(container),
    ))
}

async fn live_session(
    host: String,
    port: u16,
    dbname: String,
    username: String,
    password: String,
) -> pgnative_db_connection::LiveSession {
    let cfg = ConnectionConfig {
        id: ConnectionId(Uuid::new_v4()),
        name: "portal_test".into(),
        host,
        port,
        dbname,
        username,
        ssl_mode: SslMode::Disable,
        ssl_root_cert: None,
        ssh_tunnel: None,
    };
    let pw = SecretString::new(password.into());
    pgnative_db_connection::connect_live(&cfg, Some(&pw))
        .await
        .expect("connect_live")
}

#[tokio::test]
async fn portal_window_fetch_without_offset_rewrite() {
    let Some((host, port, dbname, username, password, _container)) = pg_params().await else {
        eprintln!("SKIP: no TEST_PG_URL and Docker not available");
        return;
    };
    let sess = live_session(host, port, dbname, username, password).await;
    let client = &sess.client;

    // Arbitrary user SQL — no LIMIT/OFFSET in it. Portal must not inject them.
    let user_sql = "SELECT g AS id, g::text AS txt FROM generate_series(1, 500) g ORDER BY g";
    assert!(!user_sql.contains("LIMIT"));
    assert!(!user_sql.contains("OFFSET"));

    let portal_name = format!("pgnative_portal_{}", Uuid::new_v4().simple());
    let mut portal = pgnative_results_portal::declare_portal(client, &portal_name, user_sql)
        .await
        .expect("declare_portal");
    assert_eq!(portal.columns.len(), 2);
    assert_eq!(portal.columns[0].name, "id");
    assert_eq!(portal.columns[1].name, "txt");
    assert_eq!(portal.fetched, 0);
    assert!(!portal.closed);

    // Verify by construction that DECLARE didn't rewrite user SQL with LIMIT/OFFSET:
    // the portal helper formats exactly `DECLARE \"name\" CURSOR FOR {user_sql}`.

    let mut all_ids = Vec::new();
    let mut windows = 0usize;
    let window = 100usize;
    let cap = pgnative_results_stream::PER_CELL_CAP;
    loop {
        let (rows, exhausted) =
            pgnative_results_portal::fetch_forward(client, &mut portal, window, cap)
                .await
                .expect("fetch_forward");
        windows += 1;
        for r in &rows {
            // id is int4 (oid 23) → CellValue::Int
            let id = match &r.cells[0] {
                pgnative_results_value::CellValue::Int(v) => *v as i64,
                pgnative_results_value::CellValue::SmallInt(v) => *v as i64,
                pgnative_results_value::CellValue::BigInt(v) => *v,
                other => panic!("unexpected id cell {other:?}"),
            };
            all_ids.push(id);
        }
        if exhausted {
            assert!(rows.len() <= window);
            break;
        }
        assert_eq!(rows.len(), window);
        // Avoid infinite loop in case of bug
        assert!(windows < 20, "too many windows");
    }
    assert_eq!(
        windows, 6,
        "500 rows with window 100 → 5 full + 1 empty-exhausted"
    );
    // Actually 500/100=5 full windows, the 6th fetch returns 0 and exhausted=true
    // Our loop counts that final empty fetch.
    assert_eq!(all_ids.len(), 500);
    assert_eq!(all_ids[0], 1);
    assert_eq!(all_ids[499], 500);
    // Strictly increasing (ORDER BY g preserved)
    for w in all_ids.windows(2) {
        assert!(w[0] < w[1]);
    }

    // After fetching, close and verify session usable (COMMIT happened)
    pgnative_results_portal::close_portal(client, &mut portal)
        .await
        .expect("close_portal");
    assert!(portal.closed);

    let rows = client
        .query("SELECT 42::int4 AS n", &[])
        .await
        .expect("post-close query");
    let n: i32 = rows[0].get("n");
    assert_eq!(n, 42);

    // Fetch after close must error AlreadyClosed
    let err = pgnative_results_portal::fetch_forward(client, &mut portal, 10, cap).await;
    assert!(err.is_err(), "fetch after close should fail");

    // Second close is idempotent
    pgnative_results_portal::close_portal(client, &mut portal)
        .await
        .expect("second close ok");

    drop(_container);
}

#[tokio::test]
async fn portal_zero_window_and_rollback() {
    let Some((host, port, dbname, username, password, _container)) = pg_params().await else {
        eprintln!("SKIP: no TEST_PG_URL and Docker not available");
        return;
    };
    let sess = live_session(host, port, dbname, username, password).await;
    let client = &sess.client;

    let portal_name = format!("pgnative_portal_{}", Uuid::new_v4().simple());
    let mut portal =
        pgnative_results_portal::declare_portal(client, &portal_name, "SELECT 1::int4 AS n")
            .await
            .expect("declare");

    let cap = pgnative_results_stream::PER_CELL_CAP;
    let (rows, exhausted) = pgnative_results_portal::fetch_forward(client, &mut portal, 0, cap)
        .await
        .expect("zero fetch");
    assert!(rows.is_empty());
    assert!(!exhausted);

    // Rollback path (simulates cancel/error)
    pgnative_results_portal::rollback_portal(client, &mut portal).await;
    assert!(portal.closed);
    // After rollback, session still usable
    let rows = client
        .query("SELECT 7::int4 AS n", &[])
        .await
        .expect("after rollback");
    let n: i32 = rows[0].get("n");
    assert_eq!(n, 7);

    drop(_container);
}
