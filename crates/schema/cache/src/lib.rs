//! Schema cache — Arc<SchemaModel> behind ArcSwap, two-phase + 24h TTL.
//! Implements ADR-0007 per plan B5-B7. Readers are lock-free via ArcSwap.
//! Persisted cache: 24h TTL, stale-while-refresh, explicit refresh bypasses TTL.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use arc_swap::ArcSwap;
use pgnative_schema_model::SchemaModel;

/// 24h TTL for persisted schema cache per product decision.
pub const SCHEMA_TTL: Duration = Duration::from_secs(24 * 3600);

#[derive(Debug, Clone)]
pub enum CacheState {
    Empty,
    Loading {
        since: Instant,
    },
    Ready {
        model: Arc<SchemaModel>,
        epoch: u64,
        /// Wall-clock time when this snapshot was cached (for TTL).
        cached_at: SystemTime,
        /// Explicit `stale` flag (e.g. DDL observed) — orthogonal to TTL expiry.
        stale: bool,
    },
    Error {
        msg: String,
    },
}

#[derive(Debug)]
pub struct SchemaCache {
    state: ArcSwap<CacheState>,
    epoch: AtomicU64,
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
            state: ArcSwap::from_pointee(CacheState::Empty),
            epoch: AtomicU64::new(0),
        }
    }

    /// Lock-free for readers: clones Arc without blocking.
    #[must_use]
    pub fn get(&self) -> Option<Arc<SchemaModel>> {
        match &**self.state.load() {
            CacheState::Ready { model, .. } => Some(Arc::clone(model)),
            _ => None,
        }
    }

    /// Current CacheState snapshot (Arc cloned).
    #[must_use]
    pub fn state_snapshot(&self) -> Arc<CacheState> {
        Arc::clone(&self.state.load())
    }

    #[must_use]
    pub fn is_loading(&self) -> bool {
        matches!(&**self.state.load(), CacheState::Loading { .. })
    }

    pub fn set_loading(&self) {
        self.state.store(Arc::new(CacheState::Loading {
            since: Instant::now(),
        }));
    }

    pub fn set_ready(&self, model: SchemaModel) {
        let epoch = self.epoch.fetch_add(1, Ordering::AcqRel) + 1;
        self.state.store(Arc::new(CacheState::Ready {
            model: Arc::new(model),
            epoch,
            cached_at: SystemTime::now(),
            stale: false,
        }));
    }

    /// Set ready from an already-Arc model (avoids extra clone).
    pub fn set_ready_arc(&self, model: Arc<SchemaModel>) {
        let epoch = self.epoch.fetch_add(1, Ordering::AcqRel) + 1;
        self.state.store(Arc::new(CacheState::Ready {
            model,
            epoch,
            cached_at: SystemTime::now(),
            stale: false,
        }));
    }

    /// Set ready with explicit `cached_at` (for loading persisted cache).
    pub fn set_ready_at(&self, model: Arc<SchemaModel>, cached_at: SystemTime) {
        let epoch = self.epoch.fetch_add(1, Ordering::AcqRel) + 1;
        self.state.store(Arc::new(CacheState::Ready {
            model,
            epoch,
            cached_at,
            stale: false,
        }));
    }

    pub fn set_error(&self, msg: String) {
        self.state.store(Arc::new(CacheState::Error { msg }));
    }

    pub fn mark_stale(&self) {
        let current = self.state.load();
        if let CacheState::Ready {
            model,
            epoch,
            cached_at,
            stale: false,
        } = &**current
        {
            self.state.store(Arc::new(CacheState::Ready {
                model: Arc::clone(model),
                epoch: *epoch,
                cached_at: *cached_at,
                stale: true,
            }));
        }
    }

    pub fn clear_stale(&self) {
        let current = self.state.load();
        if let CacheState::Ready {
            model,
            epoch,
            cached_at,
            stale: true,
        } = &**current
        {
            self.state.store(Arc::new(CacheState::Ready {
                model: Arc::clone(model),
                epoch: *epoch,
                cached_at: *cached_at,
                stale: false,
            }));
        }
    }

    #[must_use]
    pub fn is_stale(&self) -> bool {
        matches!(&**self.state.load(), CacheState::Ready { stale: true, .. })
    }

    /// True if the cached snapshot is still within 24h TTL.
    #[must_use]
    pub fn is_fresh(&self) -> bool {
        match &**self.state.load() {
            CacheState::Ready { cached_at, .. } => SystemTime::now()
                .duration_since(*cached_at)
                .map(|d| d < SCHEMA_TTL)
                .unwrap_or(false),
            _ => false,
        }
    }

    /// True if TTL has expired — caller should use stale-while-refresh
    /// (serve current model immediately, trigger background refresh).
    #[must_use]
    pub fn is_expired(&self) -> bool {
        match &**self.state.load() {
            CacheState::Ready { cached_at, .. } => SystemTime::now()
                .duration_since(*cached_at)
                .map(|d| d >= SCHEMA_TTL)
                .unwrap_or(true),
            _ => true,
        }
    }

    /// Explicit refresh bypasses TTL — always fetches fresh.
    #[must_use]
    pub fn should_refresh(&self, explicit: bool) -> bool {
        if explicit {
            return true;
        }
        self.is_expired() || self.is_stale()
    }

    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }

    /// Persist current model to `path` (bincode/json). Caller decides format;
    /// this helper writes `cached_at` + model epoch for TTL checks on load.
    pub fn persisted_epoch(&self) -> Option<(u64, SystemTime)> {
        match &**self.state.load() {
            CacheState::Ready {
                epoch, cached_at, ..
            } => Some((*epoch, *cached_at)),
            _ => None,
        }
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
        assert!(c.is_loading());
        c.set_ready(SchemaModel::empty());
        assert!(c.get().is_some());
        assert_eq!(c.epoch(), 1);
        c.mark_stale();
        assert!(c.is_stale());
        c.clear_stale();
        assert!(!c.is_stale());
    }

    #[test]
    fn cache_error_state() {
        let c = SchemaCache::new();
        c.set_error("boom".into());
        assert!(c.get().is_none());
        assert!(matches!(&*c.state_snapshot(), CacheState::Error { .. }));
    }

    #[test]
    fn epoch_increments() {
        let c = SchemaCache::new();
        c.set_ready(SchemaModel::empty());
        c.set_ready(SchemaModel::empty());
        assert_eq!(c.epoch(), 2);
    }

    #[test]
    fn set_ready_arc_preserves_arc() {
        let c = SchemaCache::new();
        let m = Arc::new(SchemaModel::empty());
        c.set_ready_arc(Arc::clone(&m));
        assert!(Arc::ptr_eq(&c.get().unwrap(), &m));
    }
}
