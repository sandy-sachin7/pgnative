//! Schema tree explorer — filterable, reads Arc<SchemaModel> (§12).
use pgnative_schema_model::types::RelationKind;
use pgnative_schema_model::SchemaModel;

/// Render the schema tree: schemas → relations → columns, filtered by `filter`.
///
/// Pure render (§30) — no DB/network/FS. Caller supplies `Arc<SchemaModel>` snapshot.
pub fn show_explorer(ui: &mut egui::Ui, model: Option<&SchemaModel>, filter: &str) {
    let Some(model) = model else {
        ui.label(
            egui::RichText::new("Not connected — no schema")
                .weak()
                .italics(),
        );
        return;
    };
    if model.relations().is_empty() && model.schemas().is_empty() {
        ui.label(egui::RichText::new("No relations").weak());
        return;
    }
    let lower = filter.to_ascii_lowercase();
    let filter_active = !filter.is_empty();

    egui::ScrollArea::vertical().show(ui, |ui| {
        // Iterate schemas in stable order; fall back to sorted_relations grouping if empty
        let schemas = model.schemas();
        if schemas.is_empty() {
            // No schema rows — group by relation sort order
            for rel in model
                .relations()
                .iter()
                .filter(|r| !filter_active || r.name.to_ascii_lowercase().contains(&lower))
            {
                relation_row(ui, model, rel);
            }
            return;
        }
        for schema in schemas {
            let rels: Vec<_> = model
                .relations_in(schema.id)
                .iter()
                .filter_map(|id| model.relation(*id))
                .filter(|r| !filter_active || r.name.to_ascii_lowercase().contains(&lower))
                .collect();
            // When filtering and schema has no matches, hide the group
            if rels.is_empty() && filter_active {
                continue;
            }
            let header = format!("{}  ({})", schema.name, rels.len());
            egui::CollapsingHeader::new(header)
                .default_open(!filter_active)
                .show(ui, |ui| {
                    if rels.is_empty() {
                        ui.label(egui::RichText::new("(empty)").weak().small());
                    }
                    for rel in rels {
                        relation_row(ui, model, rel);
                    }
                });
        }
        // Functions (optional, low noise)
        if !filter_active {
            let funcs = model.functions();
            if !funcs.is_empty() {
                egui::CollapsingHeader::new(format!("functions ({})", funcs.len()))
                    .default_open(false)
                    .show(ui, |ui| {
                        for f in funcs {
                            ui.label(format!("{} → {}", f.signature, f.return_type));
                        }
                    });
            }
        }
    });
}

fn relation_row(
    ui: &mut egui::Ui,
    model: &SchemaModel,
    rel: &pgnative_schema_model::relation::Relation,
) {
    let kind_label = match rel.kind {
        RelationKind::Table => "table",
        RelationKind::View => "view",
        RelationKind::MaterializedView => "mview",
        RelationKind::ForeignTable => "foreign",
    };
    let title = format!("{}  [{}]", rel.name, kind_label);
    egui::CollapsingHeader::new(title).show(ui, |ui| {
        if rel.columns.is_empty() {
            ui.label(egui::RichText::new("(no columns)").weak().small());
            return;
        }
        for col in &rel.columns {
            let ty = model.type_name(col.ty).unwrap_or("?");
            let null = if col.nullability == pgnative_schema_model::types::Nullability::NotNull {
                ""
            } else {
                " null"
            };
            ui.label(format!("{}: {}{}", col.name, ty, null));
        }
    });
}
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
