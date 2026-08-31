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
pub fn show_connections(_ui: &mut egui::Ui, _form: &mut ConnectionForm) {}
