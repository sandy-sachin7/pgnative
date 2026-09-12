//! egui view layer: pure rendering over application state.
//!
//! These modules never run SQL or block on I/O; they emit commands and read
//! snapshots owned by the application layer.

/// Connection form and saved-connection picker.
pub mod connections;
/// SQL editor tabs and completion popup.
pub mod editor;
/// Schema tree explorer.
pub mod explorer;
/// Query history panel.
pub mod history_panel;
/// Panel layout and shared [`UiState`](layout::UiState).
pub mod layout;
/// Virtualized results grid.
pub mod results;
/// Visual theme (light/dark).
pub mod theme;
