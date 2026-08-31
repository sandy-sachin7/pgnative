//! Builder used by introspection to construct a [`SchemaModel`].

use crate::index::SchemaModel;
use crate::relation::{Function, PrimaryKey, Relation};
use crate::schema::Schema;
use crate::types::{FunctionId, Oid, RelationId, SchemaId, TypeId};

/// Result of building a model, allowing incremental population.
#[derive(Debug, Default)]
pub struct Builder {
    model: SchemaModel,
}

impl Builder {
    /// Create an empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a schema.
    pub fn add_schema(&mut self, schema: Schema) -> SchemaId {
        let id = schema.id;
        self.model.schemas.push(schema);
        self.model
            .schema_by_name
            .insert(self.model.schemas.last().unwrap().name.clone(), id);
        id
    }

    /// Add a relation (table or view) and index it.
    pub fn add_relation(&mut self, relation: Relation) -> RelationId {
        let id = relation.id;
        self.model.relations.push(relation);
        let rt = self.model.relations.last().unwrap();
        self.model.sorted_relations.push(id);
        self.model.relation_by_oid.insert(rt.oid, id);
        self.model
            .relations_by_schema
            .entry(rt.schema)
            .or_default()
            .push(id);
        for col in &rt.columns {
            self.model.column_owner.insert(col.id, id);
        }
        id
    }

    /// Add a function / procedure.
    pub fn add_function(&mut self, function: Function) -> FunctionId {
        let id = function.id;
        self.model.functions.push(function);
        let f = self.model.functions.last().unwrap();
        self.model
            .functions_by_schema
            .entry(f.schema)
            .or_default()
            .push(id);
        id
    }

    /// Register a raw type name for a type id.
    pub fn add_type(&mut self, id: TypeId, name: String) {
        self.model.types.insert(id, name);
    }

    /// Set the primary key of a relation (replacing any existing one).
    ///
    /// # Panics
    ///
    /// Panics if `relation` is not a known relation id in this model.
    pub fn set_primary_key(&mut self, relation: RelationId, pk: PrimaryKey) {
        let idx = usize::try_from(relation.0).expect("RelationId fits usize");
        let rel = self
            .model
            .relations
            .get_mut(idx)
            .expect("relation must exist before setting its primary key");
        rel.primary_key = Some(pk);
    }

    /// Finish building.
    #[must_use]
    pub fn build(mut self) -> SchemaModel {
        // Ensure deterministic ordering of relations by schema then name.
        self.model.sorted_relations.sort_by(|a, b| {
            let ai = usize::try_from(a.0).expect("RelationId fits usize");
            let bi = usize::try_from(b.0).expect("RelationId fits usize");
            self.model.relations[ai]
                .name
                .cmp(&self.model.relations[bi].name)
        });
        let mut by_schema: Vec<(SchemaId, Vec<RelationId>)> =
            self.model.relations_by_schema.drain().collect();
        for (_schema, ids) in &mut by_schema {
            ids.sort_by(|a, b| {
                let ai = usize::try_from(a.0).expect("RelationId fits usize");
                let bi = usize::try_from(b.0).expect("RelationId fits usize");
                self.model.relations[ai]
                    .name
                    .cmp(&self.model.relations[bi].name)
            });
        }
        self.model.relations_by_schema = by_schema.into_iter().collect();
        self.model
    }
}
