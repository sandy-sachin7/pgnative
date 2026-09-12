//! History search panel (FTS) — reads storage/history via AppEvent.

/// Render history entries; returns the full text of the clicked entry, if any.
pub fn show_history(ui: &mut egui::Ui, query: &str, results: &[String]) -> Option<String> {
    if results.is_empty() {
        if query.is_empty() {
            ui.label(
                egui::RichText::new("No history yet — run a query")
                    .weak()
                    .small(),
            );
        } else {
            ui.label(egui::RichText::new("No matches").weak().small());
        }
        return None;
    }
    ui.label(
        egui::RichText::new(format!("{} result(s) — click to re-run", results.len()))
            .weak()
            .small(),
    );
    let mut picked: Option<String> = None;
    egui::ScrollArea::vertical().show(ui, |ui| {
        for (idx, entry) in results.iter().enumerate() {
            // Truncate very long entries for display (§19) but keep full text selectable.
            // Truncate at a char boundary to avoid panicking on multibyte text.
            let display = if entry.len() > 300 {
                let mut end = 300;
                while !entry.is_char_boundary(end) && end > 0 {
                    end -= 1;
                }
                format!("{}…", &entry[..end])
            } else {
                entry.clone()
            };
            let resp = ui.selectable_label(false, display);
            if resp.clicked() {
                picked = Some(entry.clone());
            }
            if idx + 1 < results.len() {
                ui.separator();
            }
        }
    });
    picked
}
