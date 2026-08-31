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
use pgnative_db_connection::{ConnectionConfig, ConnectionId, ConnectionState, QueryId, TxState};
use pgnative_schema_model::SchemaModel;
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
        query_id: QueryId,
        written: u64,
    },
    Error {
        op: String,
        message: String,
    },
    DisconnectRequiresDecision {
        id: ConnectionId,
    },
    PreferencesRestored {
        ui_state: pgnative_ui_layout::UiState,
    },
    HistoryResults {
        results: Vec<String>,
    },
}

/// Domain state — AppState (§54), separate from UiState.
///
/// NOTE: `schema` duplicates `pgnative_schema_cache::SchemaCache` state.
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
            pgnative_storage_connections::init(conn).map_err(|e| match e {
                pgnative_storage_connections::StoreError::Rusqlite(inner) => inner,
            })?;
            pgnative_storage_history::init(conn).map_err(|e| match e {
                pgnative_storage_history::HistoryError::Rusqlite(inner) => inner,
            })?;
            pgnative_storage_editor_state::init(conn).map_err(|e| match e {
                pgnative_storage_editor_state::EditorError::Rusqlite(inner) => inner,
            })?;
            pgnative_storage_preferences::init(conn).map_err(|e| match e {
                pgnative_storage_preferences::PrefError::Rusqlite(inner) => inner,
                pgnative_storage_preferences::PrefError::Json(_) => {
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

/// Resolve password for a connection from the OS keychain.
///
/// Returns `None` if absent (caller should prompt), never logs the secret.
/// Wraps `pgnative_storage_keychain::get_password` with sanitized error mapping.
#[must_use]
pub fn resolve_password(id: ConnectionId) -> Option<secrecy::SecretString> {
    pgnative_storage_keychain::get_password(id.0).ok()
}

/// Persist password to OS keychain.
pub fn store_password(
    id: ConnectionId,
    password: secrecy::SecretString,
) -> Result<(), pgnative_storage_keychain::KeychainError> {
    pgnative_storage_keychain::set_password(id.0, password)
}

/// Remove password from keychain on connection deletion.
pub fn delete_password(id: ConnectionId) -> Result<(), pgnative_storage_keychain::KeychainError> {
    pgnative_storage_keychain::delete_password(id.0)
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
    pub ui_state: pgnative_ui_layout::UiState,
    pub viewport: pgnative_results_viewport::ViewportState,
    pub theme: pgnative_ui_theme::Theme,
    pub schema: Option<Arc<SchemaModel>>,
    pub editor_tabs: HashMap<String, pgnative_ui_editor::EditorTab>,
    pub active_tab: Option<String>,
    pub history_query: String,
    pub history_results: Vec<String>,
    pub connection_form: pgnative_ui_connections::ConnectionForm,
    /// Shared result store (populated by async execution layer).
    pub store: Arc<parking_lot::RwLock<pgnative_results_store::ResultStore>>,
    completion_cache: Option<Arc<pgnative_schema_completion::CompletionEngine>>,
    completion_schema_ptr: Option<*const pgnative_schema_model::SchemaModel>,
    runtime_handle: Option<tokio::task::JoinHandle<()>>,
    last_editor_persist: std::time::Instant,
}

impl PgnativeApp {
    #[must_use]
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // Apply theme eagerly
        let theme = pgnative_ui_theme::Theme::dark();
        cc.egui_ctx.set_visuals(theme.visuals());

        // Restore UI state: default immediately; load persisted state off UI thread.
        let ui_state = pgnative_ui_layout::UiState::default();

        let store: Arc<parking_lot::RwLock<pgnative_results_store::ResultStore>> = Arc::new(
            parking_lot::RwLock::new(pgnative_results_store::ResultStore::new(
                pgnative_results_store::StoreConfig::default(),
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
                    if let Ok(Some(v)) = pgnative_storage_preferences::get(&conn, "ui_state") {
                        if let Ok(restored) =
                            serde_json::from_value::<pgnative_ui_layout::UiState>(v)
                        {
                            let _ = ev_tx
                                .try_send(AppEvent::PreferencesRestored { ui_state: restored });
                        }
                    }
                }
            });
        }

        Self {
            controller,
            ui_state,
            viewport: pgnative_results_viewport::ViewportState::default(),
            theme,
            schema: None,
            editor_tabs: HashMap::new(),
            active_tab: None,
            history_query: String::new(),
            history_results: Vec::new(),
            connection_form: pgnative_ui_connections::ConnectionForm::default(),
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
                AppEvent::ConnectionStateChanged { state, .. } => {
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
                        tracing::warn!(op = %op, message = %message, "app error");
                    }
                }
                AppEvent::DisconnectRequiresDecision { id } => {
                    tracing::warn!(%id, "disconnect requires decision — active tx");
                }
                AppEvent::PreferencesRestored { ui_state } => {
                    self.ui_state = ui_state;
                }
                AppEvent::HistoryResults { results } => {
                    self.history_results = results;
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
                    if let Some(conn_id) = self
                        .controller
                        .state
                        .read()
                        .connections
                        .keys()
                        .next()
                        .copied()
                    {
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
            if let Some(qid) = self.controller.state.read().queries.keys().next().copied() {
                self.controller
                    .send_command(AppCommand::Cancel { query_id: qid });
            }
        }
        // F5 → refresh schema
        if ctx.input(|i| i.key_pressed(egui::Key::F5)) {
            if let Some(conn_id) = self
                .controller
                .state
                .read()
                .connections
                .keys()
                .next()
                .copied()
            {
                self.controller.send_command(AppCommand::RefreshSchema {
                    connection: conn_id,
                });
            }
        }
    }

    fn tx_badge_text(&self) -> Option<(String, egui::Color32)> {
        // Derive from canonical ConnectionState::tx() plus AppState::tx fallback.
        let state = self.controller.state.read();
        for cs in state.connections.values() {
            if let Some(tx) = cs.tx_state() {
                match tx {
                    TxState::Idle => {}
                    TxState::InTransaction { .. } => {
                        return Some(("TX".to_string(), egui::Color32::from_rgb(70, 180, 90)));
                    }
                    TxState::InFailedTransaction => {
                        return Some(("TX ERR".to_string(), egui::Color32::from_rgb(220, 60, 60)));
                    }
                }
            }
        }
        // Fallback to explicit tx map (covers optimistic classify_tx before ReadyForQuery)
        for tx in state.tx.values() {
            match tx {
                TxState::InTransaction { .. } => {
                    return Some(("TX".to_string(), egui::Color32::from_rgb(70, 180, 90)));
                }
                TxState::InFailedTransaction => {
                    return Some(("TX ERR".to_string(), egui::Color32::from_rgb(220, 60, 60)));
                }
                TxState::Idle => {}
            }
        }
        None
    }
}

impl eframe::App for PgnativeApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Poll controller events (non-blocking) before render
        self.poll_events();
        let ctx = ui.ctx().clone();
        // Keyboard shortcuts (§32): Ctrl+Enter execute, Esc cancel, F5 refresh
        self.handle_shortcuts(&ctx);

        // Top bar: connection + Tx badge + theme toggle
        egui::Panel::top("top_bar").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("pgNative").strong());
                // Tx badge (§22) — visible when any connection is in transaction
                if let Some((label, color)) = self.tx_badge_text() {
                    let badge = egui::RichText::new(label)
                        .color(egui::Color32::WHITE)
                        .small()
                        .strong();
                    egui::Frame::new()
                        .fill(color)
                        .corner_radius(4)
                        .inner_margin(egui::Margin::symmetric(6, 2))
                        .show(ui, |ui| {
                            ui.label(badge);
                        });
                }
                if ui.button("New Tab").clicked() {
                    let id = format!("tab-{}", self.editor_tabs.len() + 1);
                    self.editor_tabs
                        .insert(id.clone(), pgnative_ui_editor::EditorTab::new(id.clone()));
                    self.active_tab = Some(id);
                    self.controller.send_command(AppCommand::HistorySearch {
                        query: String::new(),
                    });
                }
                if ui.button("Refresh Schema").clicked() {
                    if let Some(id) = self
                        .controller
                        .state
                        .read()
                        .connections
                        .keys()
                        .next()
                        .copied()
                    {
                        self.controller
                            .send_command(AppCommand::RefreshSchema { connection: id });
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let label = if self.theme.is_dark { "Light" } else { "Dark" };
                    if ui.button(label).clicked() {
                        self.theme = if self.theme.is_dark {
                            pgnative_ui_theme::Theme::light()
                        } else {
                            pgnative_ui_theme::Theme::dark()
                        };
                        ctx.set_visuals(self.theme.visuals());
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
                ui.heading("Explorer");
                ui.text_edit_singleline(&mut self.ui_state.search);
                let model_ref = schema_clone.as_deref();
                pgnative_ui_explorer::show_explorer(ui, model_ref, &self.ui_state.search);
            });

        // Right: history panel (FTS) — driven by HistorySearch command
        egui::Panel::right("history")
            .resizable(true)
            .default_size(280.0)
            .show(ui, |ui| {
                ui.heading("History");
                let resp = ui.text_edit_singleline(&mut self.history_query);
                if resp.changed() {
                    self.controller.send_command(AppCommand::HistorySearch {
                        query: self.history_query.clone(),
                    });
                }
                pgnative_ui_history_panel::show_history(
                    ui,
                    &self.history_query,
                    &self.history_results,
                );
            });

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
                    let resp = ui.add(
                        egui::TextEdit::multiline(&mut content)
                            .desired_rows(12)
                            .desired_width(f32::INFINITY)
                            .hint_text("SELECT * FROM ..."),
                    );
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
                                    let _ = pgnative_storage_editor_state::upsert(
                                        &conn,
                                        &pgnative_storage_editor_state::EditorTab {
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
                    // Completion preview — cache engine per schema Arc ptr per §30.
                    if let Some(schema) = &self.schema {
                        let ptr = Arc::as_ptr(schema);
                        let engine: Arc<pgnative_schema_completion::CompletionEngine> =
                            if self.completion_schema_ptr == Some(ptr) {
                                if let Some(cached) = self.completion_cache.as_ref() {
                                    Arc::clone(cached)
                                } else {
                                    let e = Arc::new(
                                        pgnative_schema_completion::CompletionEngine::new(schema),
                                    );
                                    self.completion_cache = Some(Arc::clone(&e));
                                    self.completion_schema_ptr = Some(ptr);
                                    e
                                }
                            } else {
                                let e = Arc::new(
                                    pgnative_schema_completion::CompletionEngine::new(schema),
                                );
                                self.completion_cache = Some(Arc::clone(&e));
                                self.completion_schema_ptr = Some(ptr);
                                e
                            };
                        // Prefix = last word before cursor
                        let prefix = tab
                            .content
                            .split_whitespace()
                            .last()
                            .unwrap_or("")
                            .to_string();
                        if !prefix.is_empty() {
                            let completions = pgnative_ui_editor::completions_for(&engine, &prefix);
                            if !completions.is_empty() {
                                ui.label(format!("completions: {}", completions.join(", ")));
                            }
                        }
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Run (Ctrl+Enter)").clicked() {
                            if let Some(conn_id) = self
                                .controller
                                .state
                                .read()
                                .connections
                                .keys()
                                .next()
                                .copied()
                            {
                                self.controller.send_command(AppCommand::Execute {
                                    tab: tab.id.clone(),
                                    sql: tab.content.clone(),
                                    connection: conn_id,
                                });
                            }
                        }
                        if ui.button("Cancel (Esc)").clicked() {
                            // Cancel last query if any
                            if let Some(qid) =
                                self.controller.state.read().queries.keys().next().copied()
                            {
                                self.controller
                                    .send_command(AppCommand::Cancel { query_id: qid });
                            }
                        }
                    });
                }
            }

            ui.separator();

            // Virtualized results — only visible + overscan rows (§18)
            let store_guard = self.store.read();
            let snap = self.viewport.snapshot(&store_guard);
            drop(store_guard);
            // Show via ui/results helper (ScrollArea::show_rows internally)
            pgnative_ui_results::show_results(ui, &mut self.viewport, &snap, &[]);
            ui.label(format!(
                "rows: {} total (state: {:?})",
                snap.rows.len(),
                snap.state
            ));
            // Export wiring placeholder (§28) — streams via runtime Export command
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Export:").weak().small());
                if ui.small_button("CSV").clicked() {
                    if let Some(qid) = self.controller.state.read().queries.keys().next().copied() {
                        self.controller.send_command(AppCommand::Export {
                            query_id: qid,
                            format: ExportFormat::Csv,
                        });
                    }
                }
                if ui.small_button("JSON").clicked() {
                    if let Some(qid) = self.controller.state.read().queries.keys().next().copied() {
                        self.controller.send_command(AppCommand::Export {
                            query_id: qid,
                            format: ExportFormat::Json,
                        });
                    }
                }
                if ui.small_button("SQL").clicked() {
                    if let Some(qid) = self.controller.state.read().queries.keys().next().copied() {
                        self.controller.send_command(AppCommand::Export {
                            query_id: qid,
                            format: ExportFormat::SqlInsert,
                        });
                    }
                }
            });
        });

        // Connections panel at bottom (collapsible)
        egui::Panel::bottom("connections").show(ui, |ui| {
            ui.collapsing("Connections", |ui| {
                pgnative_ui_connections::show_connections(ui, &mut self.connection_form);
                ui.horizontal(|ui| {
                    if ui.button("Connect").clicked() {
                        let id = ConnectionId(Uuid::new_v4());
                        self.controller.send_command(AppCommand::Connect { id });
                    }
                });
            });
        });

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
