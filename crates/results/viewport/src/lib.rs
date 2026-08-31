//! Viewport snapshot for virtualized rendering — egui-independent.
//! Implements AGENTS.md §18 per plan C7.

use std::ops::Range;
use std::sync::Arc;

use pgnative_results_store::{ResultStore, StoreState};
use pgnative_results_value::Row;

/// UI-owned viewport state (§18) — cheap, no locks.
#[derive(Debug, Clone)]
pub struct ViewportState {
    pub offset: usize,
    pub len: usize,
    pub overscan: usize,
    pub row_height: f32,
}

impl Default for ViewportState {
    fn default() -> Self {
        Self {
            offset: 0,
            len: 25,
            overscan: 10,
            row_height: 22.0,
        }
    }
}

impl ViewportState {
    /// Compute visible row range from scroll.
    #[must_use]
    pub fn visible_range(&self, scroll_y: f32, viewport_h: f32) -> Range<usize> {
        let first = (scroll_y / self.row_height).floor() as usize;
        let visible = (viewport_h / self.row_height).ceil() as usize;
        let start = first.saturating_sub(self.overscan);
        let end = first + visible + self.overscan;
        start..end
    }

    /// Snapshot rows for rendering — copies only `Arc` + index math, no per-cell alloc.
    #[must_use]
    pub fn snapshot(&self, store: &ResultStore) -> ViewportSnapshot {
        let total = store.len();
        let rows = store.snapshot_range(self.offset, self.len);
        ViewportSnapshot {
            rows,
            total,
            state: store.state(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ViewportSnapshot {
    pub rows: Arc<[Row]>,
    pub total: usize,
    pub state: StoreState,
}

#[cfg(test)]
mod tests {
    use super::*;
    use pgnative_results_store::{ResultStore, StoreConfig};
    use pgnative_results_value::{CellValue, Row};

    #[test]
    fn visible_range_clamped() {
        let vp = ViewportState {
            offset: 0,
            len: 10,
            overscan: 2,
            row_height: 20.0,
        };
        let r = vp.visible_range(40.0, 100.0);
        // first=2, visible=5, start=0, end=9
        assert_eq!(r, 0..9);
    }

    #[test]
    fn snapshot_copies_arc() {
        let mut store = ResultStore::new(StoreConfig::default());
        store.push_batch(vec![Row::new(vec![CellValue::Int(1)]); 5]);
        let vp = ViewportState {
            offset: 1,
            len: 2,
            ..Default::default()
        };
        let snap = vp.snapshot(&store);
        assert_eq!(snap.rows.len(), 2);
        assert_eq!(snap.total, 5);
    }
}
