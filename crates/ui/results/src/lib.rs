//! Virtualized results grid — only visible+overscan widgets (§18).
use pgnative_results_value::CellValue;
use pgnative_results_viewport::{ViewportSnapshot, ViewportState};
pub fn show_results(_ui: &mut egui::Ui, _viewport: &ViewportState, _snapshot: &ViewportSnapshot) {
    // Real impl uses `egui::ScrollArea::vertical().show_rows` with `snapshot.rows` slice.
}
pub fn format_cell(v: &CellValue) -> String {
    // Truncate large Bytes at render (§19).
    match v {
        CellValue::Text(b) if b.len() > 2048 => format!(
            "{}… ({} bytes)",
            String::from_utf8_lossy(&b[..2048]),
            b.len()
        ),
        _ => v.to_display_string(),
    }
}
