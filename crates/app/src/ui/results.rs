//! Virtualized results grid — only visible+overscan widgets (§18).
//! Wiring: `BoundedStore (Arc<RwLock>)` → `ViewportState` → `egui::ScrollArea::show_rows`.
//! No per-frame full-materialization, no egui widget per row for 500k rows.

use super::theme::Theme;
use egui::{Align, Layout};
use pgnative_results::value::CellValue;
use pgnative_results::viewport::{ViewportSnapshot, ViewportState};
use std::collections::HashMap;

/// Render the virtualized grid for the current viewport snapshot.
/// Caller owns `ViewportState` and updates `offset` from scroll delta;
/// `snapshot` is obtained via `viewport.snapshot(&store.read())` under a
/// short read lock (no lock held during render).
pub fn show_results(
    ui: &mut egui::Ui,
    viewport: &mut ViewportState,
    snapshot: &ViewportSnapshot,
    columns: &[String],
    theme: &Theme,
) {
    if snapshot.rows.is_empty() && snapshot.total == 0 {
        ui.label(
            egui::RichText::new("No rows — run a query with Ctrl+Enter")
                .weak()
                .small(),
        );
        return;
    }
    // Header band
    let overscan = viewport.overscan;
    let mut first_visible: Option<usize> = None;
    egui::ScrollArea::horizontal().show(ui, |ui| {
        theme.band().show(ui, |ui| {
            ui.horizontal(|ui| {
                for name in columns {
                    ui.label(
                        egui::RichText::new(name)
                            .small()
                            .strong()
                            .color(theme.text_secondary),
                    );
                    ui.separator();
                }
            });
        });
        ui.separator();

        // Virtualized vertical scroll — only visible rows are materialized.
        let total_rows = snapshot.total;
        let row_height = viewport.row_height;
        egui::ScrollArea::vertical()
            .auto_shrink([false; 2])
            .show_rows(ui, row_height, total_rows, |ui, range| {
                first_visible = Some(range.start);
                // Clamp range to snapshot window (store may have evicted oldest rows)
                // Map global range to snapshot offset
                let start = range.start;
                let end = range.end.min(total_rows);
                // Update viewport offset for next snapshot fetch (eframe integration):
                // record the visible start; after `show_rows` the caller backs
                // off by `overscan` so the next snapshot covers visible rows
                // plus margin above. One frame lag during scroll, exact at rest.
                for idx in start..end {
                    // Determine if idx is within snapshot window
                    let snap_idx = idx.checked_sub(snapshot.offset);
                    let row = snap_idx.and_then(|i| snapshot.rows.get(i));
                    // Even-row striping for scanability (transparent on odd rows)
                    let stripe = if idx % 2 == 0 {
                        theme.panel
                    } else {
                        egui::Color32::TRANSPARENT
                    };
                    egui::Frame::new()
                        .fill(stripe)
                        .inner_margin(egui::Margin::symmetric(4, 1))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(format!("{}", idx + 1))
                                        .small()
                                        .color(theme.text_faint),
                                );
                                ui.separator();
                                if let Some(row) = row {
                                    for cell in &row.cells {
                                        let text = format_cell(cell);
                                        // Right-align numerics, left-align text
                                        if cell.is_textual() {
                                            ui.with_layout(
                                                Layout::left_to_right(Align::Center),
                                                |ui| {
                                                    ui.label(text);
                                                },
                                            );
                                        } else {
                                            ui.with_layout(
                                                Layout::right_to_left(Align::Center),
                                                |ui| {
                                                    ui.label(text);
                                                },
                                            );
                                        }
                                        ui.separator();
                                    }
                                } else {
                                    // Row evicted or not yet streamed — placeholder
                                    ui.label("…");
                                }
                            });
                        });
                }
            });
    });
    // Report the visible window back: the caller sizes `len` from the panel
    // height before snapshotting, and the next frame's snapshot starts at the
    // scrolled-to row (one frame lag during active scroll, exact at rest).
    if let Some(start) = first_visible {
        viewport.offset = start.saturating_sub(overscan);
    }

    // Footer: truncation affordance
    status_footer(ui, snapshot);
}

/// Truncation/status footer shared by the read-only and editable grids.
fn status_footer(ui: &mut egui::Ui, snapshot: &ViewportSnapshot) {
    if snapshot.total > 0 {
        let status = match snapshot.state {
            pgnative_results::store::StoreState::Streaming => {
                format!("Streaming… {} rows", snapshot.total)
            }
            pgnative_results::store::StoreState::Complete { total } => {
                if total as usize > snapshot.total {
                    format!(
                        "Showing {} of {} rows (truncated — export to see all)",
                        snapshot.total, total
                    )
                } else {
                    format!("{} rows", snapshot.total)
                }
            }
            pgnative_results::store::StoreState::Error => "Error".into(),
            pgnative_results::store::StoreState::Cancelled { received } => {
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

/// A committed row edit awaiting diff-preview confirmation (§20).
///
/// `edits` holds `(store column index, new text)` for every pending cell in
/// the row. The caller builds `original` display strings from the snapshot
/// row and runs `diff_columns` + `update_sql_optimistic`.
pub struct EditRequest {
    /// Stable global row index (`Row.index`) the edits belong to.
    pub row_global: u64,
    /// Position of the row inside `EditableGrid::snapshot.rows`.
    pub snap_pos: usize,
    /// Pending cell values as `(store column index, new text)`.
    pub edits: Vec<(usize, String)>,
}

/// Editable variant of [`show_results`] for the table-browser path only (§17).
///
/// Arbitrary query results must keep using read-only `show_results` — row
/// identity there is unsafe to guess. Editing is further gated by the caller:
/// pass `editable[col] == true` only for non-PK `Stored` columns of a
/// PK/unique-not-null relation, and `tx_idle == false` while an explicit
/// transaction is active (edits render read-only with a banner).
///
/// Returns the first row whose Save button was clicked this frame, if any.
///
/// Inputs for [`show_editable_results`] bundled so the grid call stays lean.
pub struct EditableGrid<'a> {
    /// Viewport window the grid renders.
    pub snapshot: &'a ViewportSnapshot,
    /// Store column names aligned with the snapshot rows.
    pub columns: &'a [String],
    /// Per store column: exactly the non-PK `Stored` columns of the browsed
    /// relation; everything else renders read-only.
    pub editable: &'a [bool],
    /// False while an explicit transaction is active (read-only + banner).
    pub tx_idle: bool,
    /// In-progress cell drafts owned by the caller.
    pub pending: &'a mut HashMap<(u64, usize), String>,
}

/// Editable variant of [`show_results`] for the table-browser path only (§17).
///
/// Returns the first row whose Save button was clicked this frame, if any.
pub fn show_editable_results(
    ui: &mut egui::Ui,
    viewport: &mut ViewportState,
    grid: EditableGrid<'_>,
    theme: &Theme,
) -> Option<EditRequest> {
    let EditableGrid {
        snapshot,
        columns,
        editable,
        tx_idle,
        pending,
    } = grid;
    if snapshot.rows.is_empty() && snapshot.total == 0 {
        ui.label(
            egui::RichText::new("No rows — browse a table from the Explorer")
                .weak()
                .small(),
        );
        return None;
    }
    if !tx_idle {
        ui.label(
            egui::RichText::new("Editing disabled — explicit transaction active")
                .weak()
                .small(),
        );
    }
    let mut save_request: Option<EditRequest> = None;
    let mut revert_row: Option<u64> = None;
    // Header band (same as read-only grid)
    let overscan = viewport.overscan;
    let mut first_visible: Option<usize> = None;
    egui::ScrollArea::horizontal().show(ui, |ui| {
        theme.band().show(ui, |ui| {
            ui.horizontal(|ui| {
                for name in columns {
                    ui.label(
                        egui::RichText::new(name)
                            .small()
                            .strong()
                            .color(theme.text_secondary),
                    );
                    ui.separator();
                }
            });
        });
        ui.separator();

        let total_rows = snapshot.total;
        let row_height = viewport.row_height.max(22.0);
        egui::ScrollArea::vertical()
            .auto_shrink([false; 2])
            .show_rows(ui, row_height, total_rows, |ui, range| {
                first_visible = Some(range.start);
                let start = range.start;
                let end = range.end.min(total_rows);
                for idx in start..end {
                    let snap_idx = idx.checked_sub(snapshot.offset);
                    let row = snap_idx.and_then(|i| snapshot.rows.get(i));
                    let stripe = if idx % 2 == 0 {
                        theme.panel
                    } else {
                        egui::Color32::TRANSPARENT
                    };
                    egui::Frame::new()
                        .fill(stripe)
                        .inner_margin(egui::Margin::symmetric(4, 1))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(format!("{}", idx + 1))
                                        .small()
                                        .color(theme.text_faint),
                                );
                                ui.separator();
                                let Some(row) = row else {
                                    ui.label("…");
                                    return;
                                };
                                let global = row.index.unwrap_or(idx as u64);
                                let snap_pos = snap_idx.unwrap_or(0);
                                for (c, cell) in row.cells.iter().enumerate() {
                                    // v1: NULL cells stay read-only — there is
                                    // no typed-NULL param path yet, and the
                                    // optimistic `=` guard can never match
                                    // NULL (would false-conflict).
                                    let can_edit = tx_idle
                                        && !cell.is_null()
                                        && editable.get(c).copied().unwrap_or(false);
                                    if can_edit {
                                        let key = (global, c);
                                        let mut buf = pending.get(&key).cloned().unwrap_or_else(
                                            || match cell {
                                                CellValue::Null => String::new(),
                                                v => v.to_display_string(),
                                            },
                                        );
                                        let resp = ui.add(
                                            egui::TextEdit::singleline(&mut buf)
                                                .id(egui::Id::new(("edit-cell", global, c)))
                                                .desired_width(120.0)
                                                .frame(egui::Frame::NONE),
                                        );
                                        if resp.changed() {
                                            pending.insert(key, buf);
                                        }
                                    } else if cell.is_textual() {
                                        ui.with_layout(
                                            Layout::left_to_right(Align::Center),
                                            |ui| {
                                                ui.label(format_cell(cell));
                                            },
                                        );
                                    } else {
                                        ui.with_layout(
                                            Layout::right_to_left(Align::Center),
                                            |ui| {
                                                ui.label(format_cell(cell));
                                            },
                                        );
                                    }
                                    ui.separator();
                                }
                                // Per-row Save/Revert when pending edits exist
                                let row_pending: Vec<(usize, String)> = pending
                                    .iter()
                                    .filter(|((g, _), _)| *g == global)
                                    .map(|((_, c), v)| (*c, v.clone()))
                                    .collect();
                                if !row_pending.is_empty() {
                                    if ui.small_button("Save").clicked() {
                                        save_request = Some(EditRequest {
                                            row_global: global,
                                            snap_pos,
                                            edits: row_pending,
                                        });
                                    }
                                    if ui.small_button("Revert").clicked() {
                                        revert_row = Some(global);
                                    }
                                }
                            });
                        });
                }
            });
    });
    if let Some(start) = first_visible {
        viewport.offset = start.saturating_sub(overscan);
    }
    if let Some(global) = revert_row {
        pending.retain(|(g, _), _| *g != global);
    }

    // Footer: truncation affordance (same as read-only grid)
    status_footer(ui, snapshot);
    save_request
}
