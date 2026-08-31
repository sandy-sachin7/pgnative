//! Schema (namespace) metadata.

use serde::{Deserialize, Serialize};

use crate::types::SchemaId;

/// A single schema (namespace) within the database.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Schema {
    /// Local dense id.
    pub id: SchemaId,
    /// Schema name, e.g. `public`.
    pub name: String,
    /// User-visible comment, if any.
    pub comment: Option<String>,
}
