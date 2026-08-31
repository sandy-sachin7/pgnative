//! Column metadata.

use serde::{Deserialize, Serialize};

use crate::types::{ColumnId, Nullability, RelationId, TypeId, ValueSource};

/// A single column of a relation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Column {
    /// Local dense id.
    pub id: ColumnId,
    /// The relation this column belongs to.
    pub owner: RelationId,
    /// Column name (unqualified, i.e. `name`, not `schema.table.name`).
    pub name: String,
    /// Position of the column within its relation (1-based, as in PostgreSQL).
    pub position: u16,
    /// The type of the column.
    pub ty: TypeId,
    /// Nullability per declaration.
    pub nullability: Nullability,
    /// Whether the column has a default that is user-meaningful for inserts.
    ///
    /// `bool` is intentionally coarse for Phase 1; callers needing the exact
    /// default expression should join against a richer source.
    pub has_default: bool,
    /// Default expression, when known and safe to surface.
    pub default_expr: Option<String>,
    /// How the value is produced.
    pub value_source: ValueSource,
}
