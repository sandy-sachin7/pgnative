//! History search panel (FTS) — reads storage/history via AppEvent.
pub fn show_history(ui: &mut egui::Ui, query: &str, results: &[String]) {
    if results.is_empty() {
        if query.is_empty() {
            ui.label(egui::RichText::new("Type to search history").weak().small());
        } else {
            ui.label(egui::RichText::new("No matches").weak().small());
        }
        return;
    }
    ui.label(
        egui::RichText::new(format!("{} result(s)", results.len()))
            .weak()
            .small(),
    );
    egui::ScrollArea::vertical().show(ui, |ui| {
        for (idx, entry) in results.iter().enumerate() {
            // Truncate very long entries for display (§19) but keep full text selectable
            let display = if entry.len() > 300 {
                format!("{}…", &entry[..300])
            } else {
                entry.clone()
            };
            let label = egui::Label::new(display).selectable(true).wrap();
            ui.add(label);
            if idx + 1 < results.len() {
                ui.separator();
            }
        }
    });
}
