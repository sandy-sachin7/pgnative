//! Application orchestration — Command/Event, state machines, tx badge.
//! Implements AGENTS.md §8, §29, §54 per plan D1/D2.

use std::collections::HashMap;
use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender};
use parking_lot::RwLock;
use pgnative_db_connection::{ConnectionId, ConnectionState, QueryId, TxState};
use pgnative_schema_model::SchemaModel;
use uuid::Uuid;

/// UI → App commands (§29).
#[derive(Debug, Clone)]
pub enum AppCommand {
    Connect {
        id: ConnectionId,
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
}

/// Domain state — AppState (§54), separate from UiState.
#[derive(Debug, Default)]
pub struct AppState {
    pub connections: HashMap<ConnectionId, ConnectionState>,
    pub queries: HashMap<QueryId, String>,
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

/// Controller — owns channels + JoinSet (simplified for WU7).
pub struct AppController {
    pub cmd_tx: Sender<AppCommand>,
    pub cmd_rx: Receiver<AppCommand>,
    pub event_tx: Sender<AppEvent>,
    pub event_rx: Receiver<AppEvent>,
    pub state: AppState,
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
            state: AppState::new(),
        }
    }

    pub fn send_command(&self, cmd: AppCommand) {
        let _ = self.cmd_tx.send(cmd);
    }

    pub fn drain_events(&self) -> Vec<AppEvent> {
        let mut out = vec![];
        while let Ok(ev) = self.event_rx.try_recv() {
            out.push(ev);
        }
        out
    }
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
}
