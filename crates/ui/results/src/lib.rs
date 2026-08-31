//! Virtualized results grid — only visible+overscan widgets (§18).
//! Wiring: `BoundedStore (Arc<RwLock>)` → `ViewportState` → `egui::ScrollArea::show_rows`.
//! No per-frame full-materialization, no egui widget per row for 500k rows.

use egui::{Align, Layout};
use pgnative_results_value::CellValue;
use pgnative_results_viewport::{ViewportSnapshot, ViewportState};

/// Render the virtualized grid for the current viewport snapshot.
/// Caller owns `ViewportState` and updates `offset` from scroll delta;
/// `snapshot` is obtained via `viewport.snapshot(&store.read())` under a
/// short read lock (no lock held during render).
pub fn show_results(
    ui: &mut egui::Ui,
    viewport: &mut ViewportState,
    snapshot: &ViewportSnapshot,
    columns: &[String],
) {
    if snapshot.rows.is_empty() && snapshot.total == 0 {
        ui.label("No rows");
        return;
    }
    // Header
    egui::ScrollArea::horizontal().show(ui, |ui| {
        ui.horizontal(|ui| {
            for name in columns {
                ui.label(egui::RichText::new(name).strong());
                ui.separator();
            }
        });
        ui.separator();

        // Virtualized vertical scroll — only visible rows are materialized.
        let total_rows = snapshot.total;
        let row_height = viewport.row_height;
        egui::ScrollArea::vertical()
            .auto_shrink([false; 2])
            .show_rows(ui, row_height, total_rows, |ui, range| {
                // Clamp range to snapshot window (store may have evicted oldest rows)
                // Map global range to snapshot offset
                let start = range.start;
                let end = range.end.min(total_rows);
                // Update viewport offset for next snapshot fetch (eframe integration)
                // We don't mutate viewport offset here to avoid feedback loop; caller
                // should sync via `viewport.set_offset_from_scroll(ui.clip_rect()...)`
                // For now, render the slice that overlaps our snapshot.
                for idx in start..end {
                    // Determine if idx is within snapshot window
                    let snap_idx = idx.checked_sub(snapshot.offset);
                    let row = snap_idx.and_then(|i| snapshot.rows.get(i));
                    ui.horizontal(|ui| {
                        ui.label(format!("{}", idx + 1));
                        ui.separator();
                        if let Some(row) = row {
                            for cell in &row.cells {
                                let text = format_cell(cell);
                                // Right-align numerics, left-align text
                                if cell.is_textual() {
                                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                        ui.label(text);
                                    });
                                } else {
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        ui.label(text);
                                    });
                                }
                                ui.separator();
                            }
                        } else {
                            // Row evicted or not yet streamed — placeholder
                            ui.label("…");
                        }
                    });
                }
            });
    });

    // Footer: truncation affordance
    if snapshot.total > 0 {
        let status = match snapshot.state {
            pgnative_results_store::StoreState::Streaming => {
                format!("Streaming… {} rows", snapshot.total)
            }
            pgnative_results_store::StoreState::Complete { total } => {
                if total as usize > snapshot.total {
                    format!(
                        "Showing {} of {} rows (truncated — export to see all)",
                        snapshot.total, total
                    )
                } else {
                    format!("{} rows", snapshot.total)
                }
            }
            pgnative_results_store::StoreState::Error => "Error".into(),
            pgnative_results_store::StoreState::Cancelled { received } => {
                format!("Cancelled — {} rows", received)
            }
        };
        ui.separator();
        ui.label(egui::RichText::new(status).weak().small());
    }
}

/// Format a cell for display with render cap (C8).
/// Large Bytes truncated at 2 KiB with affordance; never duplicates full value.
pub fn format_cell(v: &CellValue) -> String {
    match v {
        CellValue::Text(b)
        | CellValue::Json(b)
        | CellValue::Jsonb(b)
        | CellValue::Array(b)
        | CellValue::Other(b)
            if b.len() > 2048 =>
        {
            // Slice at char boundary (crates/ui/results/src/lib.rs:115) — avoid splitting UTF-8
            let mut end = 2048.min(b.len());
            // Walk back until char boundary: continuation bytes 0b10xxxxxx are not boundaries
            while end > 0 && end < b.len() && (b[end] & 0b1100_0000) == 0b1000_0000 {
                end -= 1;
            }
            let s = String::from_utf8_lossy(&b[..end]);
            format!("{s}… ({} bytes — expand)", b.len())
        }
        CellValue::Bytea(b) if b.len() > 2048 => {
            format!("\\x{}… ({} bytes)", hex_snippet(b, 64), b.len())
        }
        _ => v.to_display_string(),
    }
}

fn hex_snippet(b: &[u8], n: usize) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    for byte in b.iter().take(n) {
        let _ = write!(s, "{byte:02x}");
    }
    s
}

/// Eframe integration: sync `ViewportState.offset` from egui scroll position.
/// Call once per frame before `snapshot()` when using `ScrollArea::show_rows`.
pub fn sync_viewport_from_scroll(viewport: &mut ViewportState, scroll_y: f32) {
    viewport.set_offset_from_scroll(scroll_y);
}
