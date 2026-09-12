//! pgNative schema model: the canonical local picture of the database.
//!
//! Built once per connection from introspection, then used locally by the
//! explorer, completion, and editing safety — never re-queried per keystroke.

/// Local schema cache over introspected metadata.
pub mod cache;
/// Schema-aware SQL completion (tables, columns, aliases).
pub mod completion;
/// Canonical schema types: relations, columns, keys, constraints.
pub mod model;
