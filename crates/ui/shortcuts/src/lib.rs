//! Keyboard shortcuts per AGENTS §32.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Shortcut {
    NewTab,
    Execute,
    ExecuteSelection,
    Cancel,
    FocusSearch,
    FocusEditor,
    HistorySearch,
    RefreshSchema,
    CloseTab,
    NextTab,
    PrevTab,
}
impl Shortcut {
    #[must_use]
    pub fn key(&self) -> &'static str {
        match self {
            Self::NewTab => "Ctrl+T",
            Self::Execute => "Ctrl+Enter",
            Self::ExecuteSelection => "Ctrl+Shift+Enter",
            Self::Cancel => "Esc",
            Self::FocusSearch => "Ctrl+K",
            Self::FocusEditor => "Ctrl+E",
            Self::HistorySearch => "Ctrl+H",
            Self::RefreshSchema => "F5",
            Self::CloseTab => "Ctrl+W",
            Self::NextTab => "Ctrl+Tab",
            Self::PrevTab => "Ctrl+Shift+Tab",
        }
    }
}
