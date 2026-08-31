//! Schema tree explorer — filterable, reads Arc<SchemaModel> (§12).
use pgnative_schema_model::SchemaModel;
pub fn show_explorer(_ui: &mut egui::Ui, _model: Option<&SchemaModel>, _filter: &str) {}
pub fn filter_relations<'a>(model: &'a SchemaModel, filter: &str) -> Vec<&'a str> {
    let lower = filter.to_ascii_lowercase();
    model
        .relations()
        .iter()
        .filter(|r| filter.is_empty() || r.name.to_ascii_lowercase().contains(&lower))
        .map(|r| r.name.as_str())
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    use pgnative_schema_model::{
        build::Builder,
        relation::Relation,
        schema::Schema,
        types::{Id, Oid, RelationKind},
    };
    #[test]
    fn filter() {
        let mut b = Builder::new();
        let sid = b.add_schema(Schema {
            id: Id(0),
            name: "public".into(),
            comment: None,
        });
        b.add_relation(Relation {
            id: Id(0),
            schema: sid,
            oid: Oid(1),
            name: "users".into(),
            kind: RelationKind::Table,
            columns: vec![],
            primary_key: None,
            unique_keys: vec![],
            foreign_keys_out: vec![],
            foreign_keys_in: vec![],
            comment: None,
        });
        let m = b.build();
        assert_eq!(filter_relations(&m, "us").len(), 1);
    }
}
