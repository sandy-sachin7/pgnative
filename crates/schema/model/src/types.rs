//! Low-level identifiers and relational kind enums.

use serde::{Deserialize, Serialize};

/// Stable identifier for a schema, table, view, function, type, or column.
///
/// Opaque and dense within a single [`crate::SchemaModel`]; the integers carry
/// no external meaning and are only meaningful relative to the model they belong
/// to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Id(pub u32);

/// Identifier for a single schema (namespace) within the database.
pub type SchemaId = Id;

/// Identifier for a relation (table or view).
pub type RelationId = Id;

/// Identifier for a single column.
pub type ColumnId = Id;

/// Identifier for a user-defined type.
pub type TypeId = Id;

/// Identifier for a function / procedure.
pub type FunctionId = Id;

/// Raw PostgreSQL object identifier (OID) as reported by `pg_catalog`.
///
/// Kept distinct from [`Id`]: OIDs come from the server and may not be stable
/// across restores, while [`Id`] is the local dense index used at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Oid(pub u32);

/// The kinds of relations the model understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RelationKind {
    /// A base table.
    Table,
    /// A (non-materialized) view.
    View,
    /// A materialized view.
    MaterializedView,
    /// A foreign table.
    ForeignTable,
}

/// Whether and how a column derives its value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ValueSource {
    /// Value is user-supplied on insert.
    Stored,
    /// Value is produced by a `GENERATED ALWAYS AS (...) STORED` expression.
    Generated,
    /// Value is produced by an identity sequence.
    Identity,
    /// This is a virtual / computed column that cannot be written directly.
    Virtual,
}

/// Whether a column is nullable per its declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Nullability {
    /// `NOT NULL` was declared.
    NotNull,
    /// Nullable (no `NOT NULL`, or it is unknown).
    Nullable,
}

/// Which kind of update is safe for a relation, used to gate inline editing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Editability {
    /// A primary key is available to identify rows.
    EditableWithPrimaryKey,
    /// A unique, non-null column set exists that can identify rows.
    EditableWithUniqueKey,
    /// No safe identity exists (no-PK / ambiguous / unsupported relation).
    Disabled,
}
