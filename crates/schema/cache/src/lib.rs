//! Schema cache — Arc<SchemaModel> behind RwLock, two-phase.
//! Implements ADR-0007 per plan B5-B7.

use std::sync::Arc;
use std::time::Instant;

use parking_lot::RwLock;
use pgnative_schema_model::SchemaModel;

#[derive(Debug, Clone)]
pub enum CacheState {
    Empty,
    Loading {
        since: Instant,
    },
    Ready {
        model: Arc<SchemaModel>,
        epoch: u64,
        stale: bool,
    },
    Error {
        msg: String,
    },
}

#[derive(Debug)]
pub struct SchemaCache {
    state: RwLock<CacheState>,
    epoch: RwLock<u64>,
}

impl Default for SchemaCache {
    fn default() -> Self {
        Self::new()
    }
}

impl SchemaCache {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: RwLock::new(CacheState::Empty),
            epoch: RwLock::new(0),
        }
    }

    /// Lock-free for readers: clone Arc under read lock (~ns).
    #[must_use]
    pub fn get(&self) -> Option<Arc<SchemaModel>> {
        match &*self.state.read() {
            CacheState::Ready { model, .. } => Some(Arc::clone(model)),
            _ => None,
        }
    }

    pub fn set_loading(&self) {
        *self.state.write() = CacheState::Loading {
            since: Instant::now(),
        };
    }

    pub fn set_ready(&self, model: SchemaModel) {
        let mut epoch = self.epoch.write();
        *epoch += 1;
        *self.state.write() = CacheState::Ready {
            model: Arc::new(model),
            epoch: *epoch,
            stale: false,
        };
    }

    pub fn set_error(&self, msg: String) {
        *self.state.write() = CacheState::Error { msg };
    }

    pub fn mark_stale(&self) {
        let mut state = self.state.write();
        if let CacheState::Ready { stale, .. } = &mut *state {
            *stale = true;
        }
    }

    #[must_use]
    pub fn is_stale(&self) -> bool {
        matches!(&*self.state.read(), CacheState::Ready { stale: true, .. })
    }

    #[must_use]
    pub fn epoch(&self) -> u64 {
        *self.epoch.read()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pgnative_schema_model::SchemaModel;

    #[test]
    fn cache_roundtrip() {
        let c = SchemaCache::new();
        assert!(c.get().is_none());
        c.set_loading();
        c.set_ready(SchemaModel::empty());
        assert!(c.get().is_some());
        assert_eq!(c.epoch(), 1);
        c.mark_stale();
        assert!(c.is_stale());
    }
}
