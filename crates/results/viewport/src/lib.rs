//! Viewport snapshot for virtualized rendering — egui-independent.
//! Implements AGENTS.md §18 per plan C7.
//! Eframe integration: UI owns ViewportState, store is SharedStore mutated
//! only by Tokio, rendering pulls `snapshot_range` (no per-frame alloc).

use std::ops::Range;
use std::sync::Arc;

use pgnative_results_store::{ResultStore, StoreState};
use pgnative_results_value::Row;

/// UI-owned viewport state (§18) — cheap, no locks.
/// Designed for `eframe::egui::ScrollArea::show_rows` virtualization.
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
    /// Compute visible row range from scroll (crates/results/viewport/src/lib.rs:39).
    /// `scroll_y` = current scroll offset, `viewport_h` = visible height.
    /// Includes `overscan` rows above *and* below visible window for smooth
    /// scrolling (total extra = 2×overscan). Clamped by caller via
    /// `fetch_range` to `total_rows`. Uses f64 to avoid f32 precision loss
    /// beyond 2^24 (~16M) for 500k+ rows.
    #[must_use]
    pub fn visible_range(&self, scroll_y: f32, viewport_h: f32) -> Range<usize> {
        let h = self.row_height as f64;
        if h <= 0.0 {
            return 0..self.overscan;
        }
        let first = ((scroll_y as f64) / h).floor().max(0.0) as usize;
        let visible = ((viewport_h as f64) / h).ceil().max(0.0) as usize;
        // start includes overscan above, end includes visible + overscan below
        // (crates/results/viewport/src/lib.rs:39 overscan double intentional, documented)
        let start = first.saturating_sub(self.overscan);
        let end = first.saturating_add(visible).saturating_add(self.overscan);
        start..end
    }

    /// Total virtual height for `ScrollArea` (used by eframe).
    #[must_use]
    pub fn total_height(&self, total_rows: usize) -> f32 {
        (total_rows as f64 * self.row_height as f64) as f32
    }

    /// Clamp offset to valid range given current store length.
    #[must_use]
    pub fn clamped_offset(&self, total_rows: usize) -> usize {
        if total_rows == 0 || self.len >= total_rows {
            0
        } else {
            self.offset.min(total_rows - self.len)
        }
    }

    /// Update offset from scroll position (eframe scroll → offset).
    pub fn set_offset_from_scroll(&mut self, scroll_y: f32) {
        let h = self.row_height as f64;
        self.offset = if h <= 0.0 {
            0
        } else {
            ((scroll_y as f64) / h).floor().max(0.0) as usize
        };
    }

    /// Snapshot rows for rendering — copies only `Arc` + index math, no per-cell alloc.
    /// Backpressure: store is `Arc<RwLock<ResultStore>>` in app; this takes `&ResultStore`
    /// under read lock for one call, then releases (no lock held during render).
    #[must_use]
    pub fn snapshot(&self, store: &ResultStore) -> ViewportSnapshot {
        let total = store.len();
        let clamped = self.clamped_offset(total);
        let rows = store.snapshot_range(clamped, self.len);
        ViewportSnapshot {
            rows,
            total,
            state: store.state(),
            offset: clamped,
        }
    }

    /// Eframe helper: given scroll_y/viewport_h, produce the range that should be
    /// fetched from store for rendering (visible + overscan, clamped to store len).
    #[must_use]
    pub fn fetch_range(&self, scroll_y: f32, viewport_h: f32, total_rows: usize) -> Range<usize> {
        let r = self.visible_range(scroll_y, viewport_h);
        let end = r.end.min(total_rows);
        let start = r.start.min(end);
        start..end
    }
}

#[derive(Debug, Clone)]
pub struct ViewportSnapshot {
    pub rows: Arc<[Row]>,
    pub total: usize,
    pub state: StoreState,
    /// Clamped offset used for this snapshot.
    pub offset: usize,
}

impl ViewportSnapshot {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Whether the store has been truncated (rows evicted due to budget).
    #[must_use]
    pub fn is_truncated(&self, total_pushed: u64) -> bool {
        total_pushed as usize > self.total
    }
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

    #[test]
    fn clamped_offset() {
        let vp = ViewportState {
            offset: 100,
            len: 25,
            ..Default::default()
        };
        assert_eq!(vp.clamped_offset(30), 5);
        assert_eq!(vp.clamped_offset(0), 0);
    }

    #[test]
    fn total_height() {
        let vp = ViewportState {
            row_height: 22.0,
            ..Default::default()
        };
        assert_eq!(vp.total_height(10), 220.0);
    }

    #[test]
    fn fetch_range_clamped_to_total() {
        let vp = ViewportState {
            row_height: 20.0,
            overscan: 2,
            ..Default::default()
        };
        let r = vp.fetch_range(0.0, 100.0, 3);
        assert!(r.end <= 3);
    }
}
