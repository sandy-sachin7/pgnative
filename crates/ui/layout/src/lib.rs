//! Layout + splitter + UiState (§54 vs AppState).
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiState {
    pub active_tab: Option<String>,
    pub splitter: f32,
    pub expanded_nodes: Vec<String>,
    pub scroll_y: f32,
    pub search: String,
}
impl Default for UiState {
    fn default() -> Self {
        Self {
            active_tab: None,
            splitter: 0.3,
            expanded_nodes: vec![],
            scroll_y: 0.0,
            search: String::new(),
        }
    }
}
/// Pure render helper — no async, no FS, no SQL (§30).
pub fn show_layout(_ui: &mut egui::Ui, _state: &mut UiState) {}
