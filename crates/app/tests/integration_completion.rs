//! Live completion test — real introspected schema feeds the completion engine.
//! Proves the exact inputs the UI now wires per keystroke:
//! `extract_aliases_with_model` + `CompletionEngine::complete` with dot-target.

use std::time::Duration;

use pgnative_db::connection::{ConnectionConfig, ConnectionId, SslMode};
use pgnative_schema::completion::{extract_aliases_with_model, CompletionEngine};
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

#[tokio::test]
async fn completion_alias_columns_against_live_schema() {
    let Some((host, port, dbname, username, password, _container)) = pg_params().await else {
        eprintln!("SKIP: no TEST_PG_URL and Docker/testcontainers not available");
        return;
    };
    let cfg = ConnectionConfig {
        id: ConnectionId(Uuid::new_v4()),
        name: "completion".into(),
        host,
        port,
        dbname,
        username,
        ssl_mode: SslMode::Disable,
        ssl_root_cert: None,
        ssh_tunnel: None,
    };
    let secret = SecretString::new(password.into());
    let mut sess = None;
    for _ in 0..5 {
        match pgnative_db::connection::connect_live(&cfg, Some(&secret)).await {
            Ok(s) => {
                sess = Some(s);
                break;
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(500)).await,
        }
    }
    let sess = sess.expect("connect_live should succeed");
    sess.client
        .batch_execute(
            "DROP TABLE IF EXISTS compl_users;
             CREATE TABLE compl_users (id integer PRIMARY KEY, email text NOT NULL)",
        )
        .await
        .expect("fixture DDL");
    pgnative_db::introspection::prepare_session(&sess.client)
        .await
        .expect("prepare_session");
    let model = pgnative_db::introspection::introspect(&sess.client)
        .await
        .expect("introspect should succeed");

    let engine = CompletionEngine::new(&model);

    // `SELECT t. FROM compl_users t` with cursor at end — the UI's dot path.
    let sql = "SELECT t. FROM compl_users t";
    let aliases = extract_aliases_with_model(sql, sql.len(), &model);
    assert!(
        aliases.contains_key("t"),
        "alias 't' should resolve, got keys: {:?}",
        aliases.keys().collect::<Vec<_>>()
    );
    let items = engine.complete("", &aliases, Some("t"));
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(
        labels.contains(&"id") && labels.contains(&"email"),
        "dot completion should list id+email, got {labels:?}"
    );

    // Plain table-prefix path still works against the live schema.
    let tables = engine.complete("compl_us", &aliases, None);
    assert!(
        tables.iter().any(|i| i.label == "compl_users"),
        "prefix should match compl_users"
    );

    sess.client
        .batch_execute("DROP TABLE compl_users")
        .await
        .expect("fixture cleanup");
}
