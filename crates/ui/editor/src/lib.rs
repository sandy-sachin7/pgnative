//! SQL editor tabs — schema-aware completion (§14).
use pgnative_schema_completion::CompletionEngine;
pub struct EditorTab {
    pub id: String,
    pub content: String,
    pub cursor: usize,
}
impl EditorTab {
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            content: String::new(),
            cursor: 0,
        }
    }
}
pub fn completions_for(engine: &CompletionEngine, prefix: &str) -> Vec<String> {
    engine
        .complete(prefix, &Default::default(), None)
        .into_iter()
        .map(|c| c.label)
        .collect()
}
