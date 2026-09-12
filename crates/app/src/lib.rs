//! Application orchestration — Command/Event, state machines, tx badge.
//! Implements AGENTS.md §8, §24, §27-29, §30, §54 per plan D1/D2 + Track D.
//!
//! Track D adds:
//! - `eframe::App` integration (`PgnativeApp`) wiring `ui::*` + `results::*`
//!   + `schema/completion` into a single render loop per §29/§30.
//! - SQLite file resolution + **versioned migrations** for connections/history/
//!   editor_state/preferences per §27/§62.
//! - OS keychain password resolution via `pgnative-storage-keychain` per §24.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender};
use parking_lot::RwLock;
use pgnative_db::connection::{
    parse_connection_url, ssl_mode_from_str, ConnectionConfig, ConnectionId, ConnectionState,
    QueryId, TxState,
};
use pgnative_schema::model::SchemaModel;
use secrecy::SecretString;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Commands / Events (unchanged API — extended with storage/keyring handling)
// ---------------------------------------------------------------------------

/// UI → App commands (§29).
#[derive(Debug, Clone)]
pub enum AppCommand {
    Connect {
        id: ConnectionId,
    },
    /// Direct connect with explicit config — test + programmatic path that
    /// bypasses SQLite/keychain (used by integration C gate).
    ConnectDirect {
        config: ConnectionConfig,
        password: Option<SecretString>,
    },
    Disconnect {
        id: ConnectionId,
    },
    /// Resolve an open transaction, then disconnect (§22).
    /// Sent from the disconnect-decision dialog: `commit` runs `COMMIT`,
    /// otherwise `ROLLBACK`. Both clear the tracked tx state; a failed
    /// `COMMIT` keeps the session alive so the user can retry.
    ResolveTransaction {
        id: ConnectionId,
        commit: bool,
    },
    Execute {
        tab: String,
        sql: String,
        connection: ConnectionId,
    },
    Cancel {
        query_id: QueryId,
    },
    RefreshSchema {
        connection: ConnectionId,
    },
    HistorySearch {
        query: String,
    },
    Export {
        query_id: QueryId,
        format: ExportFormat,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Csv,
    Json,
    SqlInsert,
}

/// App → UI events (§8 typed events, bounded).
#[derive(Debug, Clone)]
pub enum AppEvent {
    ConnectionStateChanged {
        id: ConnectionId,
        state: String,
    },
    QueryProgress {
        query_id: QueryId,
        rows: u64,
    },
    QueryFinished {
        query_id: QueryId,
        success: bool,
    },
    SchemaUpdated {
        connection: ConnectionId,
        model: Arc<SchemaModel>,
    },
    ExportProgress {
        /// Query whose buffered rows were written.
        query_id: QueryId,
        /// Number of rows written.
        written: u64,
        /// Destination file path.
        path: String,
    },
    Error {
        op: String,
        message: String,
    },
    DisconnectRequiresDecision {
        id: ConnectionId,
    },
    PreferencesRestored {
        ui_state: crate::ui::layout::UiState,
    },
    HistoryResults {
        results: Vec<String>,
    },
}

/// Domain state — AppState (§54), separate from UiState.
///
/// NOTE: `schema` duplicates `pgnative_schema::cache::SchemaCache` state.
/// `SchemaCache` is the canonical TTL/epoch store (hot, epoch increments on
/// every `set_ready*`). `AppState::schema` is kept in sync via
/// `SchemaUpdated` events and should eventually be replaced by a shared
/// `SchemaCache` instance to avoid divergence.
#[derive(Debug, Default)]
pub struct AppState {
    pub connections: HashMap<ConnectionId, ConnectionState>,
    pub queries: HashMap<QueryId, String>,
    /// Derived from `connections` (`ConnectionState::tx()` is canonical); kept for
    /// fast `disconnect_requires_decision` check and updated via `set_tx` / poll_events.
    pub tx: HashMap<ConnectionId, TxState>,
    pub schema: RwLock<Option<Arc<SchemaModel>>>,
}

impl AppState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_tx(&mut self, id: ConnectionId, tx: TxState) {
        self.tx.insert(id, tx);
    }

    #[must_use]
    pub fn tx_state(&self, id: ConnectionId) -> TxState {
        self.tx.get(&id).copied().unwrap_or(TxState::Idle)
    }

    pub fn set_schema(&self, model: SchemaModel) {
        *self.schema.write() = Some(Arc::new(model));
    }

    /// Check disconnect with active tx → requires explicit decision (§22).
    #[must_use]
    pub fn disconnect_requires_decision(&self, id: ConnectionId) -> bool {
        self.tx_state(id).is_active()
    }
}

pub mod runtime;
/// egui view layer (pure rendering, no SQL/I/O).
pub mod ui;

/// Controller — owns channels + optional Tokio JoinSet drain.
///
/// Channels are bounded (256) per §8 back-pressure contract.
pub struct AppController {
    pub cmd_tx: Sender<AppCommand>,
    pub cmd_rx: Receiver<AppCommand>,
    pub event_tx: Sender<AppEvent>,
    pub event_rx: Receiver<AppEvent>,
    pub state: Arc<RwLock<AppState>>,
}

impl AppController {
    #[must_use]
    pub fn new() -> Self {
        let (cmd_tx, cmd_rx) = crossbeam_channel::bounded(256);
        let (event_tx, event_rx) = crossbeam_channel::bounded(256);
        Self {
            cmd_tx,
            cmd_rx,
            event_tx,
            event_rx,
            state: Arc::new(RwLock::new(AppState::new())),
        }
    }

    pub fn send_command(&self, cmd: AppCommand) {
        // §30: never block UI thread; bounded 256.
        // NOTE: after PgnativeApp::new swaps cmd_rx with a dummy receiver,
        // self.cmd_rx is not paired with cmd_tx — do not try to drain it.
        if let Err(crossbeam_channel::TrySendError::Full(_)) = self.cmd_tx.try_send(cmd) {
            tracing::warn!("cmd channel full — dropping command");
        }
    }

    pub fn drain_events(&self) -> Vec<AppEvent> {
        let mut out = vec![];
        while let Ok(ev) = self.event_rx.try_recv() {
            out.push(ev);
        }
        out
    }

    /// Non-blocking try_recv for eframe poll.
    pub fn try_recv_command(&self) -> Option<AppCommand> {
        self.cmd_rx.try_recv().ok()
    }

    pub fn emit(&self, ev: AppEvent) {
        // Non-blocking; drop if UI not draining fast enough
        let _ = self.event_tx.try_send(ev);
    }
}

impl Default for AppController {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Storage: SQLite path + versioned migrations (§27, §62)
// ---------------------------------------------------------------------------

/// Current app DB schema version — bump on each breaking change to
/// local storage. See `migrate()` for history.
pub const APP_DB_VERSION: i32 = 2;

/// Resolve platform-appropriate app DB path via `directories`.
///
/// On failure falls back to temp dir so tests/CI never panic.
#[must_use]
pub fn app_db_path() -> PathBuf {
    if let Some(proj) = directories::ProjectDirs::from("com", "pgnative", "pgnative") {
        let dir = proj.data_dir().to_path_buf();
        // Ensure parent exists eagerly so callers can open directly.
        let _ = std::fs::create_dir_all(&dir);
        dir.join("pgnative.db")
    } else {
        std::env::temp_dir().join("pgnative.db")
    }
}

/// Open (or create) the app SQLite DB and run versioned migrations.
///
/// Idempotent and safe to call on every startup. Uses `PRAGMA user_version`
/// as the schema version marker (canonical SQLite pattern).
pub fn open_app_db(path: &std::path::Path) -> Result<rusqlite::Connection, rusqlite::Error> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let conn = rusqlite::Connection::open(path)?;
    migrate(&conn)?;
    Ok(conn)
}

/// Run versioned migrations in order until `APP_DB_VERSION`.
///
/// Each step is additive and never destroys user data per §62.
pub fn migrate(conn: &rusqlite::Connection) -> Result<(), rusqlite::Error> {
    // Cheap pragmas for app-local SQLite.
    conn.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;",
    )?;
    let version: i32 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap_or(0);

    if version < 1 {
        conn.execute("BEGIN", [])?;
        let res: Result<(), rusqlite::Error> = (|| {
            // v1: baseline tables from storage crates
            pgnative_storage::connections::init(conn).map_err(|e| match e {
                pgnative_storage::connections::StoreError::Rusqlite(inner) => inner,
            })?;
            pgnative_storage::history::init(conn).map_err(|e| match e {
                pgnative_storage::history::HistoryError::Rusqlite(inner) => inner,
            })?;
            pgnative_storage::editor_state::init(conn).map_err(|e| match e {
                pgnative_storage::editor_state::EditorError::Rusqlite(inner) => inner,
            })?;
            pgnative_storage::preferences::init(conn).map_err(|e| match e {
                pgnative_storage::preferences::PrefError::Rusqlite(inner) => inner,
                pgnative_storage::preferences::PrefError::Json(_) => {
                    rusqlite::Error::InvalidParameterName("json".into())
                }
            })?;
            conn.execute("PRAGMA user_version=1", [])?;
            Ok(())
        })();
        match res {
            Ok(()) => {
                conn.execute("COMMIT", [])?;
            }
            Err(e) => {
                let _ = conn.execute("ROLLBACK", []);
                return Err(e);
            }
        }
    }
    if version < 2 {
        conn.execute("BEGIN", [])?;
        let res: Result<(), rusqlite::Error> = (|| {
            // v2: add updated_at column to connections if missing (backwards-aware)
            // and create index on history.connection_id
            let has_updated: bool = conn
                .prepare("SELECT sql FROM sqlite_master WHERE type='table' AND name='connections'")
                .ok()
                .and_then(|mut s| {
                    s.query_row([], |r| r.get::<_, Option<String>>(0))
                        .ok()
                        .flatten()
                })
                .map(|sql| sql.contains("updated_at"))
                .unwrap_or(false);
            if !has_updated {
                // ALTER TABLE is idempotent via try; ignore if column exists
                let _ = conn.execute(
                    "ALTER TABLE connections ADD COLUMN updated_at INTEGER DEFAULT NULL",
                    [],
                );
            }
            let _ = conn.execute(
                "CREATE INDEX IF NOT EXISTS idx_history_connection ON history(connection_id)",
                [],
            );
            let _ = conn.execute(
                "CREATE INDEX IF NOT EXISTS idx_history_executed_at ON history(executed_at DESC)",
                [],
            );
            conn.execute("PRAGMA user_version=2", [])?;
            Ok(())
        })();
        match res {
            Ok(()) => {
                conn.execute("COMMIT", [])?;
            }
            Err(e) => {
                let _ = conn.execute("ROLLBACK", []);
                return Err(e);
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Keyring integration (§24) — non-secret in SQLite, secret in OS keychain
// ---------------------------------------------------------------------------

/// Build a [`ConnectionConfig`] + password from the connection form.
///
/// URL-first: when `form.url` is non-empty it is parsed via
/// [`parse_connection_url`]; an explicitly typed password overrides the one
/// embedded in the URL. Otherwise the individual host/port/db/user fields are
/// used with the typed password. Never logs secrets.
fn build_connection_from_form(
    form: &crate::ui::connections::ConnectionForm,
) -> Result<(ConnectionConfig, Option<SecretString>), String> {
    if !form.url.trim().is_empty() {
        let (mut cfg, url_pw) = parse_connection_url(&form.url)?;
        if !form.password.is_empty() {
            return Ok((cfg, Some(SecretString::new(form.password.clone().into()))));
        }
        if !form.name.trim().is_empty() {
            cfg.name = form.name.trim().to_string();
        }
        return Ok((cfg, url_pw));
    }
    if form.dbname.trim().is_empty() {
        return Err("database is required (or paste a connection URL)".to_string());
    }
    let name = if form.name.trim().is_empty() {
        if form.username.trim().is_empty() {
            format!("{}/{}", form.host.trim(), form.dbname.trim())
        } else {
            format!(
                "{}@{}/{}",
                form.username.trim(),
                form.host.trim(),
                form.dbname.trim()
            )
        }
    } else {
        form.name.trim().to_string()
    };
    let password = if form.password.is_empty() {
        None
    } else {
        Some(SecretString::new(form.password.clone().into()))
    };
    Ok((
        ConnectionConfig {
            id: ConnectionId(Uuid::new_v4()),
            name,
            host: form.host.trim().to_string(),
            port: form.port,
            dbname: form.dbname.trim().to_string(),
            username: form.username.trim().to_string(),
            ssl_mode: ssl_mode_from_str(&form.ssl_mode),
            ssl_root_cert: None,
            ssh_tunnel: None,
        },
        password,
    ))
}

/// Persist a connection's non-secret config to SQLite and its password to the
/// OS keychain (best-effort: keychain failures are logged, never fatal, and
/// never fall back to plaintext storage per §24).
fn persist_connection(cfg: &ConnectionConfig, password: Option<&SecretString>) {
    let saved = pgnative_storage::connections::SavedConnection {
        id: cfg.id.0.to_string(),
        name: cfg.name.clone(),
        host: cfg.host.clone(),
        port: cfg.port,
        dbname: cfg.dbname.clone(),
        username: cfg.username.clone(),
        ssl_mode: cfg.ssl_mode.to_string(),
    };
    match open_app_db(&app_db_path()) {
        Ok(conn) => {
            if let Err(e) = pgnative_storage::connections::upsert(&conn, &saved) {
                tracing::warn!("persist connection: {e}");
            }
        }
        Err(e) => tracing::warn!("persist connection (open db): {e}"),
    }
    if let Some(pw) = password {
        if let Err(e) = store_password(cfg.id, pw.clone()) {
            tracing::warn!("persist connection (keychain unavailable): {e}");
        }
    }
}
///
/// Resolve password for a connection from the OS keychain.
///
/// Returns `None` if absent (caller should prompt), never logs the secret.
/// Wraps `pgnative_storage::keychain::get_password` with sanitized error mapping.
#[must_use]
pub fn resolve_password(id: ConnectionId) -> Option<secrecy::SecretString> {
    pgnative_storage::keychain::get_password(id.0).ok()
}

/// Persist password to OS keychain.
pub fn store_password(
    id: ConnectionId,
    password: secrecy::SecretString,
) -> Result<(), pgnative_storage::keychain::KeychainError> {
    pgnative_storage::keychain::set_password(id.0, password)
}

/// Remove password from keychain on connection deletion.
pub fn delete_password(id: ConnectionId) -> Result<(), pgnative_storage::keychain::KeychainError> {
    pgnative_storage::keychain::delete_password(id.0)
}

// ---------------------------------------------------------------------------
// eframe integration — PgnativeApp (§30: render is pure, no SQL/FS blocking)
// ---------------------------------------------------------------------------

/// Top-level eframe app wiring explorer/editor/results/layout/theme/history.
///
/// All heavy work (DB, storage, keychain) happens via `AppCommand` dispatch
/// on a Tokio task or the controller channel — `update()` only drains events
/// and renders from snapshot state per §30.
pub struct PgnativeApp {
    pub controller: AppController,
    pub ui_state: crate::ui::layout::UiState,
    pub viewport: pgnative_results::viewport::ViewportState,
    pub theme: crate::ui::theme::Theme,
    pub schema: Option<Arc<SchemaModel>>,
    pub editor_tabs: HashMap<String, crate::ui::editor::EditorTab>,
    pub active_tab: Option<String>,
    pub history_query: String,
    pub history_results: Vec<String>,
    pub connection_form: crate::ui::connections::ConnectionForm,
    /// Last connection error, shown inline in the connections panel.
    pub connect_error: Option<String>,
    /// Connection that Run/Refresh target — set on successful connect, never
    /// guessed from `HashMap` iteration order.
    pub active_connection: Option<ConnectionId>,
    /// Display name snapshot for the status pill (form name or host/db),
    /// taken at Connect click time; cleared on disconnect.
    pub active_connection_name: Option<String>,
    /// Most recent streaming query — Esc/Cancel and Export target this.
    pub active_query: Option<QueryId>,
    /// Connection with an open txn awaiting a commit/rollback/keep-open
    /// decision (§22). Set by `DisconnectRequiresDecision`, cleared when the
    /// connection reports `disconnected` or the user keeps it open.
    pub pending_disconnect: Option<ConnectionId>,
    /// Last successfully finished query — Export stays available after the
    /// stream completes (store still holds its buffered rows until next Execute).
    pub last_completed_query: Option<QueryId>,
    /// Result of the last export (saved path or error), shown under results.
    pub export_status: Option<String>,
    /// Shared result store (populated by async execution layer).
    pub store: Arc<parking_lot::RwLock<pgnative_results::store::ResultStore>>,
    completion_cache: Option<Arc<pgnative_schema::completion::CompletionEngine>>,
    completion_schema_ptr: Option<*const pgnative_schema::model::SchemaModel>,
    runtime_handle: Option<tokio::task::JoinHandle<()>>,
    last_editor_persist: std::time::Instant,
}

impl PgnativeApp {
    #[must_use]
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // Apply theme eagerly (custom visuals + type scale, not stock)
        let theme = crate::ui::theme::Theme::dark();
        theme.apply(&cc.egui_ctx);

        // Restore UI state: default immediately; load persisted state off UI thread.
        let ui_state = crate::ui::layout::UiState::default();

        let store: Arc<parking_lot::RwLock<pgnative_results::store::ResultStore>> = Arc::new(
            parking_lot::RwLock::new(pgnative_results::store::ResultStore::new(
                pgnative_results::store::StoreConfig::default(),
            )),
        );
        let mut controller = AppController::new();
        let mut runtime_handle: Option<tokio::task::JoinHandle<()>> = None;
        // Spawn single AppRuntime dispatcher if a Tokio handle is available.
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let (_dummy_tx, dummy_rx) = crossbeam_channel::bounded::<AppCommand>(256);
            let cmd_rx = std::mem::replace(&mut controller.cmd_rx, dummy_rx);
            let event_tx = controller.event_tx.clone();
            let state = Arc::clone(&controller.state);
            let store_clone = Arc::clone(&store);
            let rt_handle =
                crate::runtime::spawn_runtime(cmd_rx, event_tx.clone(), store_clone, state);
            // Keep handle so Drop can abort; also forward completion
            runtime_handle = Some(handle.spawn(async move {
                let _ = rt_handle.await;
            }));
            // Load persisted UI state off the render thread (§30)
            let ev_tx = event_tx;
            handle.spawn_blocking(move || {
                if let Ok(conn) = open_app_db(&app_db_path()) {
                    if let Ok(Some(v)) = pgnative_storage::preferences::get(&conn, "ui_state") {
                        if let Ok(restored) =
                            serde_json::from_value::<crate::ui::layout::UiState>(v)
                        {
                            let _ = ev_tx
                                .try_send(AppEvent::PreferencesRestored { ui_state: restored });
                        }
                    }
                }
            });
        }

        // Populate history panel with recents on launch (empty query → recents).
        // Harmless if no runtime is running: the command just sits undrained.
        controller.send_command(AppCommand::HistorySearch {
            query: String::new(),
        });

        // First frame is usable immediately: one tab with a starter query
        // so the user can connect and hit Ctrl+Enter without setup.
        let mut editor_tabs = HashMap::new();
        let mut first_tab = crate::ui::editor::EditorTab::new("tab-1");
        first_tab.content = "-- Connect above, then Ctrl+Enter to run\nSELECT 1;".to_string();
        editor_tabs.insert("tab-1".to_string(), first_tab);

        Self {
            controller,
            ui_state,
            viewport: pgnative_results::viewport::ViewportState::default(),
            theme,
            schema: None,
            editor_tabs,
            active_tab: Some("tab-1".to_string()),
            history_query: String::new(),
            history_results: Vec::new(),
            connection_form: crate::ui::connections::ConnectionForm::default(),
            connect_error: None,
            active_connection: None,
            active_connection_name: None,
            active_query: None,
            pending_disconnect: None,
            last_completed_query: None,
            export_status: None,
            store,
            completion_cache: None,
            completion_schema_ptr: None,
            runtime_handle,
            last_editor_persist: std::time::Instant::now() - std::time::Duration::from_secs(1),
        }
    }

    fn poll_events(&mut self) {
        for ev in self.controller.drain_events() {
            match ev {
                AppEvent::SchemaUpdated { model, .. } => {
                    self.schema = Some(model);
                }
                AppEvent::ConnectionStateChanged { id, state } => {
                    if state == "connected" {
                        self.connect_error = None;
                        self.active_connection = Some(id);
                    }
                    if state == "disconnected" {
                        if self.active_connection == Some(id) {
                            self.active_connection = None;
                            self.active_connection_name = None;
                        }
                        if self.pending_disconnect == Some(id) {
                            self.pending_disconnect = None;
                        }
                    }
                    tracing::info!(state = %state, "connection state");
                }
                AppEvent::Error { op, message } => {
                    // Back-compat: history search still emits Error{op:"history"} until
                    // runtime migrates to HistoryResults (small append, no break).
                    if op == "history" {
                        if message.is_empty() {
                            self.history_results.clear();
                        } else {
                            self.history_results =
                                message.split("\n---\n").map(|s| s.to_string()).collect();
                        }
                    } else {
                        if op == "connect" {
                            self.connect_error = Some(message.clone());
                        }
                        if op == "export" {
                            self.export_status = Some(message.clone());
                        }
                        tracing::warn!(op = %op, message = %message, "app error");
                    }
                }
                AppEvent::DisconnectRequiresDecision { id } => {
                    self.pending_disconnect = Some(id);
                }
                AppEvent::PreferencesRestored { ui_state } => {
                    self.ui_state = ui_state;
                }
                AppEvent::HistoryResults { results } => {
                    self.history_results = results;
                }
                AppEvent::QueryProgress { query_id, .. } => {
                    self.active_query = Some(query_id);
                    // A new stream invalidates the previous finished result.
                    self.last_completed_query = None;
                }
                AppEvent::QueryFinished {
                    query_id, success, ..
                } => {
                    if self.active_query == Some(query_id) {
                        self.active_query = None;
                    }
                    if success {
                        self.last_completed_query = Some(query_id);
                    }
                }
                AppEvent::ExportProgress { written, path, .. } => {
                    self.export_status = Some(format!("exported {written} rows → {path}"));
                }
                _ => {}
            }
        }
    }

    /// Shortcuts: Ctrl+Enter execute, Esc cancel, F5 refresh (§32).
    /// Must be called from `ui()` after `ctx` is cloned; uses `ctx.input`.
    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        // Ctrl+Enter → execute active tab
        let exec = ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::Enter));
        if exec {
            if let Some(tab_id) = self.active_tab.clone() {
                if let Some(tab) = self.editor_tabs.get(&tab_id) {
                    if let Some(conn_id) = self.active_connection {
                        self.controller.send_command(AppCommand::Execute {
                            tab: tab.id.clone(),
                            sql: tab.content.clone(),
                            connection: conn_id,
                        });
                    }
                }
            }
        }
        // Esc → cancel last query
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            if let Some(qid) = self.active_query {
                self.controller
                    .send_command(AppCommand::Cancel { query_id: qid });
            }
        }
        // F5 → refresh schema
        if ctx.input(|i| i.key_pressed(egui::Key::F5)) {
            if let Some(conn_id) = self.active_connection {
                self.controller.send_command(AppCommand::RefreshSchema {
                    connection: conn_id,
                });
            }
        }
    }

    fn tx_badge_text(&self) -> Option<(String, egui::Color32)> {
        // Derive from canonical ConnectionState::tx() plus AppState::tx fallback.
        let state = self.controller.state.read();
        let in_tx = state.connections.values().any(|cs| {
            matches!(
                cs.tx_state(),
                Some(TxState::InTransaction { .. }) | Some(TxState::InFailedTransaction)
            )
        }) || state.tx.values().any(|tx| {
            matches!(
                tx,
                TxState::InTransaction { .. } | TxState::InFailedTransaction
            )
        });
        drop(state);
        if !in_tx {
            return None;
        }
        // Distinguish failed transactions (red) from clean ones (green).
        let failed = self
            .controller
            .state
            .read()
            .connections
            .values()
            .any(|cs| matches!(cs.tx_state(), Some(TxState::InFailedTransaction)))
            || self
                .controller
                .state
                .read()
                .tx
                .values()
                .any(|tx| matches!(tx, TxState::InFailedTransaction));
        if failed {
            Some(("TX ERR".to_string(), self.theme.danger))
        } else {
            Some(("TX".to_string(), self.theme.success))
        }
    }
}

impl eframe::App for PgnativeApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Poll controller events (non-blocking) before render
        self.poll_events();
        let ctx = ui.ctx().clone();
        // Keyboard shortcuts (§32): Ctrl+Enter execute, Esc cancel, F5 refresh
        self.handle_shortcuts(&ctx);

        // Top bar: brand + connection status pill + Tx badge + actions + theme toggle
        egui::Panel::top("top_bar").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("pgNative").strong().size(15.0));
                // Connection status pill — green dot + name when live,
                // muted dot + "Not connected" otherwise.
                let dot = if self.active_connection.is_some() {
                    self.theme.success
                } else {
                    self.theme.text_faint
                };
                let name = self
                    .active_connection_name
                    .clone()
                    .unwrap_or_else(|| "Not connected".to_string());
                self.theme
                    .band()
                    .corner_radius(egui::CornerRadius::same(10))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 5.0;
                            let (rect, _) =
                                ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                            ui.painter().circle_filled(rect.center(), 4.0, dot);
                            ui.label(
                                egui::RichText::new(name)
                                    .small()
                                    .color(self.theme.text_secondary),
                            );
                        });
                    });
                // Tx badge (§22) — visible when any connection is in transaction
                if let Some((label, color)) = self.tx_badge_text() {
                    self.theme
                        .band()
                        .corner_radius(egui::CornerRadius::same(10))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 5.0;
                                let (rect, _) = ui.allocate_exact_size(
                                    egui::vec2(8.0, 8.0),
                                    egui::Sense::hover(),
                                );
                                ui.painter().circle_filled(rect.center(), 4.0, color);
                                ui.label(egui::RichText::new(label).small().strong().color(color));
                            });
                        });
                }
                if ui.button("New Tab").clicked() {
                    let id = format!("tab-{}", self.editor_tabs.len() + 1);
                    self.editor_tabs
                        .insert(id.clone(), crate::ui::editor::EditorTab::new(id.clone()));
                    self.active_tab = Some(id);
                    self.controller.send_command(AppCommand::HistorySearch {
                        query: String::new(),
                    });
                }
                if ui.button("Refresh Schema").clicked() {
                    if let Some(id) = self.active_connection {
                        self.controller
                            .send_command(AppCommand::RefreshSchema { connection: id });
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let label = if self.theme.is_dark { "Light" } else { "Dark" };
                    if ui.button(label).clicked() {
                        self.theme = if self.theme.is_dark {
                            crate::ui::theme::Theme::light()
                        } else {
                            crate::ui::theme::Theme::dark()
                        };
                        self.theme.apply(&ctx);
                    }
                });
            });
        });

        // Left: explorer — reads Arc<SchemaModel> snapshot, filterable
        let schema_clone = self.schema.clone();
        egui::Panel::left("explorer")
            .resizable(true)
            .default_size(260.0)
            .show(ui, |ui| {
                ui.label(self.theme.section_label("Explorer"));
                ui.text_edit_singleline(&mut self.ui_state.search);
                let model_ref = schema_clone.as_deref();
                crate::ui::explorer::show_explorer(ui, model_ref, &self.ui_state.search);
            });

        // Right: history panel (FTS) — driven by HistorySearch command.
        // Clicking an entry loads it into the editor and re-runs it when connected.
        let picked = egui::Panel::right("history")
            .resizable(true)
            .default_size(280.0)
            .show(ui, |ui| {
                ui.label(self.theme.section_label("History"));
                let resp = ui.text_edit_singleline(&mut self.history_query);
                if resp.changed() {
                    self.controller.send_command(AppCommand::HistorySearch {
                        query: self.history_query.clone(),
                    });
                }
                crate::ui::history_panel::show_history(
                    ui,
                    &self.history_query,
                    &self.history_results,
                )
            })
            .inner;
        if let Some(sql) = picked {
            let tab_id = match self.active_tab.clone() {
                Some(id) => id,
                None => {
                    let id = format!("tab-{}", self.editor_tabs.len() + 1);
                    self.editor_tabs
                        .insert(id.clone(), crate::ui::editor::EditorTab::new(id.clone()));
                    self.active_tab = Some(id.clone());
                    id
                }
            };
            if let Some(tab) = self.editor_tabs.get_mut(&tab_id) {
                tab.content = sql.clone();
            }
            if let Some(conn_id) = self.active_connection {
                self.controller.send_command(AppCommand::Execute {
                    tab: tab_id,
                    sql,
                    connection: conn_id,
                });
            }
        }

        // Central: editor tabs + virtualized results grid
        egui::CentralPanel::default().show(ui, |ui| {
            // Editor
            ui.horizontal(|ui| {
                for tab_id in self.editor_tabs.keys().cloned().collect::<Vec<_>>() {
                    let selected = self.active_tab.as_deref() == Some(&tab_id);
                    if ui.selectable_label(selected, &tab_id).clicked() {
                        self.active_tab = Some(tab_id.clone());
                    }
                }
            });
            if let Some(tab_id) = self.active_tab.clone() {
                if let Some(tab) = self.editor_tabs.get_mut(&tab_id) {
                    let mut content = tab.content.clone();
                    let editor_id = egui::Id::new(("sql-editor", tab.id.clone()));
                    let resp = self.theme.card().show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(&mut content)
                                .id(editor_id)
                                .font(egui::TextStyle::Monospace)
                                .desired_rows(12)
                                .desired_width(f32::INFINITY)
                                .frame(egui::Frame::NONE)
                                .hint_text("SELECT * FROM …"),
                        )
                    }).inner;
                    if resp.changed() {
                        tab.content = content;
                        tab.cursor = tab.content.len();
                        // Persist off UI thread per §30 — debounced to avoid per-keystroke thread explosion.
                        let now = std::time::Instant::now();
                        let debounce = std::time::Duration::from_millis(350);
                        if now.duration_since(self.last_editor_persist) >= debounce {
                            self.last_editor_persist = now;
                            let tab_id_clone = tab.id.clone();
                            let content_clone = tab.content.clone();
                            let cursor_clone = tab.cursor;
                            let persist = move || {
                                if let Ok(conn) = open_app_db(&app_db_path()) {
                                    let _ = pgnative_storage::editor_state::upsert(
                                        &conn,
                                        &pgnative_storage::editor_state::EditorTab {
                                            tab_id: tab_id_clone,
                                            connection_id: None,
                                            content: content_clone,
                                            cursor: cursor_clone,
                                            selection: None,
                                        },
                                    );
                                }
                            };
                            if let Ok(h) = tokio::runtime::Handle::try_current() {
                                h.spawn_blocking(persist);
                            } else {
                                std::thread::spawn(persist);
                            }
                        }
                    }
                    // Completion — cursor-aware: real caret (1-frame lag) with
                    // end-of-text fallback; aliases + dot-target from text
                    // before the cursor; cache engine per schema Arc ptr (§30).
                    if let Some(schema) = &self.schema {
                        let ptr = Arc::as_ptr(schema);
                        let engine: Arc<pgnative_schema::completion::CompletionEngine> =
                            if self.completion_schema_ptr == Some(ptr) {
                                if let Some(cached) = self.completion_cache.as_ref() {
                                    Arc::clone(cached)
                                } else {
                                    let e = Arc::new(
                                        pgnative_schema::completion::CompletionEngine::new(schema),
                                    );
                                    self.completion_cache = Some(Arc::clone(&e));
                                    self.completion_schema_ptr = Some(ptr);
                                    e
                                }
                            } else {
                                let e = Arc::new(
                                    pgnative_schema::completion::CompletionEngine::new(schema),
                                );
                                self.completion_cache = Some(Arc::clone(&e));
                                self.completion_schema_ptr = Some(ptr);
                                e
                            };
                        let cursor_bytes = egui::TextEdit::load_state(ui.ctx(), editor_id)
                            .and_then(|s| s.cursor.char_range())
                            .map(|r| {
                                crate::ui::editor::char_to_byte(&tab.content, r.primary.index.0)
                            })
                            .unwrap_or(tab.content.len());
                        tab.cursor = cursor_bytes;
                        let before = tab.content.get(..cursor_bytes).unwrap_or("");
                        let (prefix, dot_target) = crate::ui::editor::completion_target(before);
                        if !prefix.is_empty() || dot_target.is_some() {
                            let aliases = pgnative_schema::completion::extract_aliases_with_model(
                                &tab.content,
                                cursor_bytes,
                                schema,
                            );
                            let completions = crate::ui::editor::completions_for(
                                &engine,
                                &prefix,
                                &aliases,
                                dot_target.as_deref(),
                            );
                            if !completions.is_empty() {
                                let mut picked: Option<(usize, String)> = None;
                                egui::Frame::new()
                                    .fill(self.theme.elevated)
                                    .stroke(egui::Stroke::new(1.0, self.theme.accent))
                                    .corner_radius(egui::CornerRadius::same(
                                        self.theme.radius_md,
                                    ))
                                    .inner_margin(egui::Margin::symmetric(8, 6))
                                    .show(ui, |ui| {
                                        for (idx, item) in completions.iter().take(8).enumerate() {
                                            let kind_color = match item.kind {
                                                pgnative_schema::completion::CompletionKind::Column => {
                                                    self.theme.accent
                                                }
                                                pgnative_schema::completion::CompletionKind::Table => {
                                                    self.theme.success
                                                }
                                                pgnative_schema::completion::CompletionKind::Schema => {
                                                    self.theme.warn
                                                }
                                                pgnative_schema::completion::CompletionKind::Function => {
                                                    self.theme.text_secondary
                                                }
                                            };
                                            ui.horizontal(|ui| {
                                                if ui
                                                    .selectable_label(
                                                        false,
                                                        egui::RichText::new(&item.label).strong(),
                                                    )
                                                    .clicked()
                                                {
                                                    picked =
                                                        Some((idx, item.insert_text.clone()));
                                                }
                                                ui.label(
                                                    egui::RichText::new(format!(
                                                        "{:?}",
                                                        item.kind
                                                    ))
                                                    .small()
                                                    .color(kind_color),
                                                );
                                            });
                                        }
                                        if completions.len() > 8 {
                                            ui.label(
                                                egui::RichText::new(format!(
                                                    "+{} more…",
                                                    completions.len() - 8
                                                ))
                                                .small()
                                                .color(self.theme.text_faint),
                                            );
                                        }
                                    });
                                // Splice insert_text over the typed prefix.
                                if let Some((_, insert)) = picked {
                                    let replace_start = cursor_bytes.saturating_sub(prefix.len());
                                    if tab.content.get(replace_start..cursor_bytes).is_some() {
                                        tab.content
                                            .replace_range(replace_start..cursor_bytes, &insert);
                                        tab.cursor = replace_start + insert.len();
                                    }
                                }
                            }
                        }
                    }
                    ui.horizontal(|ui| {
                        if ui
                            .add(self.theme.primary_button("Run (Ctrl+Enter)"))
                            .clicked()
                        {
                            if let Some(conn_id) = self.active_connection {
                                self.controller.send_command(AppCommand::Execute {
                                    tab: tab.id.clone(),
                                    sql: tab.content.clone(),
                                    connection: conn_id,
                                });
                            }
                        }
                        if ui.button("Cancel (Esc)").clicked() {
                            // Cancel last query if any
                            if let Some(qid) = self.active_query {
                                self.controller
                                    .send_command(AppCommand::Cancel { query_id: qid });
                            }
                        }
                    });
                }
            }

            ui.separator();

            // Virtualized results — only visible + overscan rows (§18).
            // Size the snapshot window from the available height so the
            // scrolled-to rows are actually resident; the grid reports back
            // the visible range for the next frame (see show_results).
            let visible = (ui.available_height() / self.viewport.row_height)
                .ceil()
                .max(1.0) as usize;
            self.viewport.len = visible + 2 * self.viewport.overscan;
            let store_guard = self.store.read();
            // Columns + rows under one read lock so header and body agree.
            let columns = store_guard.columns();
            let snap = self.viewport.snapshot(&store_guard);
            drop(store_guard);
            // Show via ui/results helper (ScrollArea::show_rows internally)
            crate::ui::results::show_results(ui, &mut self.viewport, &snap, &columns, &self.theme);
            self.theme.band().show(ui, |ui| {
                ui.label(
                    egui::RichText::new(format!("{} rows · {:?}", snap.total, snap.state))
                        .small()
                        .color(self.theme.text_secondary),
                );
            });
            // Minimal CSV export (§28): buffered rows → temp-dir file via runtime.
            // JSON/SQL formats stay unwired until demanded (no fake buttons).
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Export:").weak().small());
                let export_target = self.active_query.or(self.last_completed_query);
                if ui
                    .small_button("CSV")
                    .on_hover_text("Save buffered rows as CSV")
                    .clicked()
                {
                    if let Some(qid) = export_target {
                        self.export_status = None;
                        self.controller.send_command(AppCommand::Export {
                            query_id: qid,
                            format: ExportFormat::Csv,
                        });
                    }
                }
                if let Some(status) = &self.export_status {
                    ui.label(egui::RichText::new(status).weak().small());
                }
            });
        });

        // Connections panel at bottom (collapsible)
        egui::Panel::bottom("connections").show(ui, |ui| {
            ui.collapsing("Connections", |ui| {
                crate::ui::connections::show_connections(ui, &mut self.connection_form);
                if let Some(err) = &self.connect_error {
                    ui.colored_label(self.theme.danger, err);
                }
                ui.horizontal(|ui| {
                    if ui.add(self.theme.primary_button("Connect")).clicked() {
                        let display_name = if self.connection_form.name.trim().is_empty() {
                            format!(
                                "{} / {}",
                                self.connection_form.host, self.connection_form.dbname
                            )
                        } else {
                            self.connection_form.name.clone()
                        };
                        match build_connection_from_form(&self.connection_form) {
                            Ok((cfg, password)) => {
                                self.connect_error = None;
                                self.active_connection_name = Some(display_name);
                                persist_connection(&cfg, password.as_ref());
                                self.controller.send_command(AppCommand::ConnectDirect {
                                    config: cfg,
                                    password,
                                });
                            }
                            Err(e) => {
                                self.connect_error = Some(e);
                            }
                        }
                    }
                    if let Some(id) = self.active_connection {
                        if ui.button("Disconnect").clicked() {
                            self.controller.send_command(AppCommand::Disconnect { id });
                        }
                    }
                });
            });
        });

        // Disconnect decision dialog (§22): an open txn blocks teardown until
        // the user commits, rolls back, or keeps the connection open.
        if let Some(id) = self.pending_disconnect {
            egui::Window::new("Transaction active")
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .collapsible(false)
                .resizable(false)
                .show(&ctx, |ui| {
                    ui.label(
                        "This connection has an open transaction.\n\
                         Disconnecting must commit it, roll it back, or wait.",
                    );
                    ui.horizontal(|ui| {
                        if ui
                            .add(self.theme.primary_button("Commit + disconnect"))
                            .clicked()
                        {
                            self.controller
                                .send_command(AppCommand::ResolveTransaction { id, commit: true });
                        }
                        if ui
                            .add(self.theme.danger_button("Rollback + disconnect"))
                            .clicked()
                        {
                            self.controller
                                .send_command(AppCommand::ResolveTransaction { id, commit: false });
                        }
                        if ui.button("Keep open").clicked() {
                            // UI-local: the session is untouched, just dismiss.
                            self.pending_disconnect = None;
                        }
                    });
                });
        }

        // Repaint when streaming results
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}

impl Drop for PgnativeApp {
    fn drop(&mut self) {
        if let Some(h) = self.runtime_handle.take() {
            h.abort();
        }
    }
}

/// Launch the native eframe window.
///
/// Uses `directories` for storage path, Tokio runtime re-used from `eframe`
/// winit loop where available. Returns `eframe::Result` for caller (e.g. `main.rs`).
pub fn run_native() -> eframe::Result<()> {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_title("pgNative"),
        ..Default::default()
    };
    eframe::run_native(
        "pgNative",
        opts,
        Box::new(|cc| Ok(Box::new(PgnativeApp::new(cc)))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tx_decision_required() {
        let mut s = AppState::new();
        let id = ConnectionId(Uuid::new_v4());
        assert!(!s.disconnect_requires_decision(id));
        s.set_tx(id, TxState::InFailedTransaction);
        assert!(s.disconnect_requires_decision(id));
    }

    #[test]
    fn build_connection_prefers_url() {
        let form = crate::ui::connections::ConnectionForm {
            url: "postgres://bob:pw@db.example:5433/mydb?sslmode=require".into(),
            password: String::new(),
            ..Default::default()
        };
        let (cfg, pw) = build_connection_from_form(&form).unwrap();
        assert_eq!(cfg.host, "db.example");
        assert_eq!(cfg.port, 5433);
        assert_eq!(cfg.dbname, "mydb");
        assert_eq!(cfg.username, "bob");
        assert!(pw.is_some());
    }

    #[test]
    fn build_connection_manual_fields() {
        let form = crate::ui::connections::ConnectionForm {
            url: String::new(),
            password: "s3cret".into(),
            host: "127.0.0.1".into(),
            port: 5432,
            dbname: "app".into(),
            username: "ann".into(),
            ssl_mode: "disable".into(),
            name: String::new(),
        };
        let (cfg, pw) = build_connection_from_form(&form).unwrap();
        assert_eq!(cfg.host, "127.0.0.1");
        assert_eq!(cfg.dbname, "app");
        assert_eq!(cfg.ssl_mode, pgnative_db::connection::SslMode::Disable);
        assert!(pw.is_some());
    }

    #[test]
    fn build_connection_rejects_empty() {
        let form = crate::ui::connections::ConnectionForm::default();
        assert!(build_connection_from_form(&form).is_err());
    }

    #[test]
    fn command_roundtrip() {
        let c = AppController::new();
        c.send_command(AppCommand::HistorySearch {
            query: "users".into(),
        });
        let cmd = c.cmd_rx.try_recv().unwrap();
        assert!(matches!(cmd, AppCommand::HistorySearch { .. }));
    }

    #[test]
    fn migrations_idempotent() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap(); // second run must be no-op
        let v: i32 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, APP_DB_VERSION);
    }

    #[test]
    fn app_db_path_non_empty() {
        let p = app_db_path();
        assert!(!p.as_os_str().is_empty());
    }

    #[test]
    fn open_app_db_creates_file() {
        let path = std::env::temp_dir().join(format!("pgnative-test-{}.db", Uuid::new_v4()));
        let conn = open_app_db(&path).unwrap();
        let v: i32 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, APP_DB_VERSION);
        assert!(path.exists());
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
    }
}
