//! The [`SchemaModel`] container and its lookup indexes.

use std::collections::HashMap;

use crate::relation::{Function, Relation};
use crate::schema::Schema;
use crate::types::{ColumnId, FunctionId, Oid, RelationId, RelationKind, SchemaId, TypeId};

/// The canonical in-memory schema model for a single database connection.
///
/// Populated once by introspection (see `pgnative-db-introspection`) and held
/// behind an [`arc_swap`](https://docs.rs/arc-swap)-style slot so readers never
/// block. See `ADR-0007`.
#[derive(Debug, Default, Clone)]
pub struct SchemaModel {
    schemas: Vec<Schema>,
    relations: Vec<Relation>,
    functions: Vec<Function>,
    /// Raw type names keyed by type id. Types are few and rarely inspected in
    /// detail, so a flat map is sufficient.
    types: HashMap<TypeId, String>,

    // --- indexes -----------------------------------------------------------
    schema_by_name: HashMap<String, SchemaId>,
    relation_by_oid: HashMap<Oid, RelationId>,
    /// Relations grouped by schema, in schema + name order.
    relations_by_schema: HashMap<SchemaId, Vec<RelationId>>,
    /// Function ids grouped by schema.
    functions_by_schema: HashMap<SchemaId, Vec<FunctionId>>,
    /// Relation ids sorted by (schema, name) for deterministic iteration.
    sorted_relations: Vec<RelationId>,
    /// First column of each relation, to support editing joins cheaply.
    column_owner: HashMap<ColumnId, RelationId>,
}

impl SchemaModel {
    /// Build an empty model.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// All schemas, in no particular order.
    #[must_use]
    pub fn schemas(&self) -> &[Schema] {
        &self.schemas
    }

    /// All relations (tables and views).
    #[must_use]
    pub fn relations(&self) -> &[Relation] {
        &self.relations
    }

    /// All functions / procedures.
    #[must_use]
    pub fn functions(&self) -> &[Function] {
        &self.functions
    }

    /// Look up a schema by name.
    #[must_use]
    pub fn schema_named(&self, name: &str) -> Option<&Schema> {
        self.schema_by_name
            .get(name)
            .map(|&id| &self.schemas[id.0 as usize])
    }

    /// Look up a relation by its local id.
    #[must_use]
    pub fn relation(&self, id: RelationId) -> Option<&Relation> {
        self.relations.get(id.0 as usize)
    }

    /// Look up a relation by server OID.
    #[must_use]
    pub fn relation_by_oid(&self, oid: Oid) -> Option<&Relation> {
        self.relation_by_oid
            .get(&oid)
            .and_then(|&id| self.relation(id))
    }

    /// Look up a function by its local id.
    #[must_use]
    pub fn function(&self, id: FunctionId) -> Option<&Function> {
        self.functions.get(id.0 as usize)
    }

    /// Relations belonging to a schema, ordered by (schema, name).
    #[must_use]
    pub fn relations_in(&self, schema: SchemaId) -> &[RelationId] {
        self.relations_by_schema
            .get(&schema)
            .map_or(&[], Vec::as_slice)
    }

    /// Functions belonging to a schema.
    #[must_use]
    pub fn functions_in(&self, schema: SchemaId) -> &[FunctionId] {
        self.functions_by_schema
            .get(&schema)
            .map_or(&[], Vec::as_slice)
    }

    /// The relation that owns a column.
    #[must_use]
    pub fn owner_of(&self, column: ColumnId) -> Option<RelationId> {
        self.column_owner.get(&column).copied()
    }

    /// The type name for a type id.
    #[must_use]
    pub fn type_name(&self, ty: TypeId) -> Option<&str> {
        self.types.get(&ty).map(String::as_str)
    }

    /// All relations of a given kind, ordered by (schema, name).
    #[must_use]
    pub fn relations_of_kind(&self, kind: RelationKind) -> Vec<&Relation> {
        self.sorted_relations
            .iter()
            .filter_map(|&id| self.relation(id))
            .filter(|r| r.kind == kind)
            .collect()
    }
}
