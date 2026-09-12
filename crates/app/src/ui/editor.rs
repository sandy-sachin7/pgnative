//! SQL editor tabs — schema-aware completion (§14).
use std::collections::HashMap;

use pgnative_schema::completion::{CompletionEngine, CompletionItem};
use pgnative_schema::model::types::RelationId;
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

/// Split text-before-cursor into `(prefix, dot_target)`.
///
/// Scans back over identifier chars (`alphanumeric + _ $ "`) and dots:
/// - `us` → `("us", None)`
/// - `u.` → `("", Some("u"))` (list all columns)
/// - `u.em` → `("em", Some("u"))`
/// - `a.b.c` → `("c", Some("b"))` (engine resolves single-level targets)
/// - trailing whitespace / empty → `("", None)`
/// Surrounding double quotes are stripped (quoted identifiers).
#[must_use]
pub fn completion_target(before: &str) -> (String, Option<String>) {
    let mut start = before.len();
    for (idx, ch) in before.char_indices().rev() {
        if ch.is_alphanumeric() || ch == '_' || ch == '$' || ch == '.' || ch == '"' {
            start = idx;
        } else {
            break;
        }
    }
    let run = &before[start..];
    if run.is_empty() {
        return (String::new(), None);
    }
    if let Some(dot) = run.rfind('.') {
        let target = run[..dot]
            .rsplit('.')
            .next()
            .unwrap_or("")
            .trim_matches('"');
        let prefix = run[dot + 1..].trim_matches('"');
        if target.is_empty() {
            return (prefix.to_string(), None);
        }
        return (prefix.to_string(), Some(target.to_string()));
    }
    (run.trim_matches('"').to_string(), None)
}

pub fn completions_for(
    engine: &CompletionEngine,
    prefix: &str,
    aliases: &HashMap<String, RelationId>,
    dot_target: Option<&str>,
) -> Vec<CompletionItem> {
    engine.complete(prefix, aliases, dot_target)
}

/// Convert an egui char-based cursor index to a byte offset into `text`.
#[must_use]
pub fn char_to_byte(text: &str, char_idx: usize) -> usize {
    text.char_indices()
        .nth(char_idx)
        .map_or(text.len(), |(b, _)| b)
}

#[cfg(test)]
mod tests {
    use super::completion_target;

    #[test]
    fn target_plain_prefix() {
        assert_eq!(completion_target("SELECT us"), ("us".into(), None));
    }

    #[test]
    fn target_dot_lists_all_columns() {
        assert_eq!(
            completion_target("SELECT u."),
            (String::new(), Some("u".into()))
        );
    }

    #[test]
    fn target_dot_with_prefix() {
        assert_eq!(completion_target("SELECT u.em WHERE 1"), ("1".into(), None));
        assert_eq!(
            completion_target("SELECT u.em"),
            ("em".into(), Some("u".into()))
        );
    }

    #[test]
    fn target_trailing_space_is_empty() {
        assert_eq!(
            completion_target("SELECT * FROM users u WHERE "),
            (String::new(), None)
        );
    }

    #[test]
    fn target_chained_dots_take_innermost() {
        assert_eq!(
            completion_target("SELECT a.b.c"),
            ("c".into(), Some("b".into()))
        );
    }

    #[test]
    fn target_strips_quotes() {
        assert_eq!(completion_target("SELECT \"my"), ("my".into(), None));
    }

    #[test]
    fn target_empty_is_empty() {
        assert_eq!(completion_target(""), (String::new(), None));
    }
}
