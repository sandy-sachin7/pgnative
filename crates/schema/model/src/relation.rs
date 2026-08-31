//! Table, view, key, and constraint metadata.

use serde::{Deserialize, Serialize};

use crate::column::Column;
use crate::types::{Editability, Oid, RelationId, RelationKind, SchemaId};

/// A relation: a table or a view together with its columns and identity info.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Relation {
    /// Local dense id.
    pub id: RelationId,
    /// Schema (namespace) this relation lives in.
    pub schema: SchemaId,
    /// Server OID of the relation.
    pub oid: Oid,
    /// Relation name (unqualified).
    pub name: String,
    /// Table, view, materialized view, or foreign table.
    pub kind: RelationKind,
    /// Columns in positional order.
    pub columns: Vec<Column>,
    /// Primary key, when one exists.
    pub primary_key: Option<PrimaryKey>,
    /// Unique key candidates usable for row identity.
    pub unique_keys: Vec<UniqueKey>,
    /// Foreign keys where this relation is the referencing (child) side.
    pub foreign_keys_out: Vec<ForeignKey>,
    /// Foreign keys where this relation is the referenced (parent) side.
    pub foreign_keys_in: Vec<ForeignKey>,
    /// User-visible comment, if any.
    pub comment: Option<String>,
}

impl Relation {
    /// Whether rows of this relation can be safely identified for inline editing.
    pub fn editability(&self) -> Editability {
        if self.primary_key.is_some() {
            Editability::EditableWithPrimaryKey
        } else if self.unique_keys.iter().any(|k| k.is_not_nullable(self)) {
            Editability::EditableWithUniqueKey
        } else {
            Editability::Disabled
        }
    }

    /// Look up a column by name within this relation.
    #[must_use]
    pub fn column_named(&self, name: &str) -> Option<&Column> {
        self.columns.iter().find(|c| c.name == name)
    }

    /// Look up a column by its local dense id within this relation.
    #[must_use]
    pub fn column_by_id(&self, id: crate::types::ColumnId) -> Option<&Column> {
        self.columns.iter().find(|c| c.id == id)
    }
}

/// A primary key defined on a relation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrimaryKey {
    /// Column ids that make up the key, in key order.
    pub columns: Vec<crate::types::ColumnId>,
    /// Constraint name, when known.
    pub name: Option<String>,
}

/// A unique key candidate (a `UNIQUE` constraint or unique index).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UniqueKey {
    /// Column ids that make up the key, in order.
    pub columns: Vec<crate::types::ColumnId>,
    /// Constraint / index name, when known.
    pub name: Option<String>,
}

impl UniqueKey {
    /// True when every column in this key is `NOT NULL`, making it a usable
    /// row identity candidate.
    #[must_use]
    pub fn is_not_nullable(&self, relation: &Relation) -> bool {
        self.columns
            .iter()
            .all(|cid| relation.column_by_id(*cid).is_some_and(|c| c.nullability == crate::types::Nullability::NotNull))
    }
}

/// A foreign key constraint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ForeignKey {
    /// Constraint name, when known.
    pub name: Option<String>,
    /// Column ids on the referencing (child) side.
    pub referencing: Vec<crate::types::ColumnId>,
    /// The referenced relation.
    pub referenced_relation: RelationId,
    /// Column ids on the referenced (parent) side.
    pub referenced: Vec<crate::types::ColumnId>,
}

/// A callable (function or procedure).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Function {
    /// Local dense id.
    pub id: crate::types::FunctionId,
    /// Schema this function lives in.
    pub schema: SchemaId,
    /// Function name (unqualified).
    pub name: String,
    /// Rendered argument signature used for the tree display, e.g.
    /// `add(x integer, y integer)`.
    pub signature: String,
    /// Return type name rendered for display.
    pub return_type: String,
    /// `true` when this is a procedure, `false` when it is a function.
    pub is_procedure: bool,
}
