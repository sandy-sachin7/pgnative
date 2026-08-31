//! Bounded result store — row+byte budget, ring eviction, stable Row.index.
//! Implements AGENTS.md §15 per plan C3-C4. No tokio, no egui.

use std::collections::VecDeque;
use std::sync::Arc;

use parking_lot::RwLock;
use pgnative_results_value::{CellValue, Row};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreState {
    Streaming,
    Complete { total: u64 },
    Error,
    Cancelled { received: u64 },
}

#[derive(Debug, Clone)]
pub struct StoreConfig {
    pub row_budget: usize,
    pub byte_budget: usize,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            row_budget: 50_000,
            byte_budget: 64 * 1024 * 1024,
        }
    }
}

#[derive(Debug)]
pub struct ResultStore {
    rows: VecDeque<Row>,
    byte_used: usize,
    total_pushed: u64,
    state: StoreState,
    config: StoreConfig,
}

impl ResultStore {
    #[must_use]
    pub fn new(config: StoreConfig) -> Self {
        Self {
            rows: VecDeque::new(),
            byte_used: 0,
            total_pushed: 0,
            state: StoreState::Streaming,
            config,
        }
    }

    /// Push a batch, evicting oldest rows if over budget. `Row.index` is
    /// stable global insertion order, never rewritten on eviction (plan C3).
    pub fn push_batch(&mut self, batch: Vec<Row>) {
        for mut row in batch {
            row.index = Some(self.total_pushed);
            self.total_pushed += 1;
            let bytes = row_byte_len(&row);
            self.byte_used += bytes;
            self.rows.push_back(row);
            // Evict while over budget.
            while self.rows.len() > self.config.row_budget
                || self.byte_used > self.config.byte_budget
            {
                if let Some(front) = self.rows.pop_front() {
                    self.byte_used = self.byte_used.saturating_sub(row_byte_len(&front));
                } else {
                    break;
                }
            }
        }
    }

    pub fn complete(&mut self) {
        self.state = StoreState::Complete {
            total: self.total_pushed,
        };
    }

    pub fn cancel(&mut self) {
        self.state = StoreState::Cancelled {
            received: self.total_pushed,
        };
    }

    #[must_use]
    pub fn snapshot_range(&self, offset: usize, len: usize) -> Arc<[Row]> {
        let end = (offset + len).min(self.rows.len());
        if offset >= end {
            return Arc::from(vec![]);
        }
        self.rows
            .range(offset..end)
            .cloned()
            .collect::<Vec<_>>()
            .into()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    #[must_use]
    pub fn state(&self) -> StoreState {
        self.state
    }

    #[must_use]
    pub fn byte_used(&self) -> usize {
        self.byte_used
    }

    #[must_use]
    pub fn total_pushed(&self) -> u64 {
        self.total_pushed
    }
}

/// Thread-safe handle: `Arc<RwLock<ResultStore>>` — mutated only by Tokio task,
/// read lock-free in render via `snapshot_range`.
pub type SharedStore = Arc<RwLock<ResultStore>>;

#[must_use]
pub fn row_byte_len(row: &Row) -> usize {
    row.cells
        .iter()
        .map(|c| match c {
            CellValue::Null => 0,
            CellValue::Text(b)
            | CellValue::Json(b)
            | CellValue::Jsonb(b)
            | CellValue::Bytea(b)
            | CellValue::Array(b)
            | CellValue::Enum(b)
            | CellValue::Other(b) => b.len(),
            _ => 8,
        })
        .sum::<usize>()
        + 32 // per-row overhead
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    fn row_with_text(n: usize) -> Row {
        Row::new(vec![CellValue::Text(Bytes::from(vec![b'a'; n]))])
    }

    #[test]
    fn evicts_when_over_row_budget() {
        let mut s = ResultStore::new(StoreConfig {
            row_budget: 2,
            byte_budget: 1_000_000,
        });
        s.push_batch(vec![
            row_with_text(10),
            row_with_text(10),
            row_with_text(10),
        ]);
        assert_eq!(s.len(), 2);
        assert_eq!(s.total_pushed(), 3);
        // Oldest evicted, stable index preserved.
        assert_eq!(s.snapshot_range(0, 1)[0].index, Some(1));
    }

    #[test]
    fn snapshot_range_clamped() {
        let mut s = ResultStore::new(StoreConfig::default());
        s.push_batch(vec![row_with_text(5); 5]);
        assert_eq!(s.snapshot_range(10, 5).len(), 0);
        assert_eq!(s.snapshot_range(0, 10).len(), 5);
    }

    #[test]
    fn byte_budget_authoritative() {
        let mut s = ResultStore::new(StoreConfig {
            row_budget: 100_000,
            byte_budget: 100,
        });
        s.push_batch(vec![row_with_text(60), row_with_text(60)]);
        assert_eq!(s.len(), 1); // second evicts first
    }
}
