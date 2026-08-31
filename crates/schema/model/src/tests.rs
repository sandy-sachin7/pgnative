//! Unit tests for the schema model.

use crate::build::Builder;
use crate::column::Column;
use crate::relation::{Function, PrimaryKey, Relation};
use crate::schema::Schema;
use crate::types::{
    Editability, Id, Nullability, Oid, RelationKind, SchemaId, TypeId, ValueSource,
};

fn sample_model() -> crate::index::SchemaModel {
    let mut b = Builder::new();
    let s = SchemaId(Id(0));
    b.add_schema(Schema {
        id: s,
        name: "public".into(),
        comment: None,
    });

    let rel = RelationId(Id(0));
    b.add_relation(Relation {
        id: rel,
        schema: s,
        oid: Oid(1),
        name: "users".into(),
        kind: RelationKind::Table,
        columns: vec![
            Column {
                id: crate::types::ColumnId(Id(0)),
                owner: rel,
                name: "id".into(),
                position: 1,
                ty: TypeId(Id(0)),
                nullability: Nullability::NotNull,
                has_default: false,
                default_expr: None,
                value_source: ValueSource::Stored,
            },
            Column {
                id: crate::types::ColumnId(Id(1)),
                owner: rel,
                name: "email".into(),
                position: 2,
                ty: TypeId(Id(1)),
                nullability: Nullability::Nullable,
                has_default: false,
                default_expr: None,
                value_source: ValueSource::Stored,
            },
        ],
        primary_key: None,
        unique_keys: Vec::new(),
        foreign_keys_out: Vec::new(),
        foreign_keys_in: Vec::new(),
        comment: None,
    });
    b.set_primary_key(
        rel,
        PrimaryKey {
            columns: vec![crate::types::ColumnId(Id(0))],
            name: Some("users_pkey".into()),
        },
    );
    b.add_type(TypeId(Id(0)), "int4".into());
    b.add_type(TypeId(Id(1)), "varchar".into());
    b.add_function(Function {
        id: crate::types::FunctionId(Id(0)),
        schema: s,
        name: "now".into(),
        signature: "now()".into(),
        return_type: "timestamptz".into(),
        is_procedure: false,
    });
    b.build()
}

#[test]
fn schema_by_name_round_trips() {
    let m = sample_model();
    assert_eq!(m.schema_named("public").map(|s| &*s.name), Some("public"));
    assert!(m.schema_named("missing").is_none());
}

#[test]
fn relation_editability_uses_primary_key() {
    let m = sample_model();
    let rel = m.relations()[0];
    assert_eq!(rel.editability(), Editability::EditableWithPrimaryKey);
}

#[test]
fn relation_lookup_by_oid_and_id() {
    let m = sample_model();
    assert_eq!(m.relation_by_oid(Oid(1)).map(|r| &*r.name), Some("users"));
    assert_eq!(
        m.relation(RelationId(Id(0))).map(|r| &*r.name),
        Some("users")
    );
}

#[test]
fn column_owner_resolves() {
    let m = sample_model();
    assert_eq!(
        m.owner_of(crate::types::ColumnId(Id(0))),
        Some(RelationId(Id(0)))
    );
}

#[test]
fn relations_of_kind_filters_views() {
    let m = sample_model();
    assert_eq!(m.relations_of_kind(RelationKind::Table).len(), 1);
    assert!(m.relations_of_kind(RelationKind::View).is_empty());
}
