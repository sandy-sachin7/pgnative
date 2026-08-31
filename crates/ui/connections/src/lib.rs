//! Connection form UI (§26).
pub struct ConnectionForm {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub dbname: String,
    pub username: String,
    pub ssl_mode: String,
}
impl Default for ConnectionForm {
    fn default() -> Self {
        Self {
            name: String::new(),
            host: "localhost".into(),
            port: 5432,
            dbname: String::new(),
            username: String::new(),
            ssl_mode: "prefer".into(),
        }
    }
}
pub fn show_connections(ui: &mut egui::Ui, form: &mut ConnectionForm) {
    egui::Grid::new("connection_form_grid")
        .num_columns(2)
        .spacing([8.0, 6.0])
        .show(ui, |ui| {
            ui.label("Name");
            ui.text_edit_singleline(&mut form.name);
            ui.end_row();
            ui.label("Host");
            ui.text_edit_singleline(&mut form.host);
            ui.end_row();
            ui.label("Port");
            let mut port_str = form.port.to_string();
            if ui.text_edit_singleline(&mut port_str).changed() {
                if let Ok(p) = port_str.parse::<u16>() {
                    form.port = p;
                }
            }
            ui.end_row();
            ui.label("Database");
            ui.text_edit_singleline(&mut form.dbname);
            ui.end_row();
            ui.label("Username");
            ui.text_edit_singleline(&mut form.username);
            ui.end_row();
            ui.label("SSL mode");
            egui::ComboBox::from_id_salt("ssl_mode_combo")
                .selected_text(&form.ssl_mode)
                .show_ui(ui, |ui| {
                    for m in ["disable", "prefer", "require", "verify-ca", "verify-full"] {
                        ui.selectable_value(&mut form.ssl_mode, m.to_string(), m);
                    }
                });
            ui.end_row();
        });
}
