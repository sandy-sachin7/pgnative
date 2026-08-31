//! Local completion engine — no tokio, no DB per keystroke.
//! Implements AGENTS.md §14.

use std::collections::HashMap;

use pgnative_schema_model::types::{ColumnId, RelationId};
use pgnative_schema_model::SchemaModel;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompletionKind {
    Schema,
    Table,
    Column,
    Function,
}

#[derive(Debug, Clone)]
pub struct CompletionItem {
    pub label: String,
    pub detail: Option<String>,
    pub kind: CompletionKind,
    pub insert_text: String,
}

#[derive(Debug, Default)]
pub struct CompletionEngine {
    by_name: HashMap<String, Vec<RelationId>>,
    columns_by_relation: HashMap<RelationId, Vec<(String, ColumnId)>>,
    functions: Vec<String>,
}

impl CompletionEngine {
    #[must_use]
    pub fn new(model: &SchemaModel) -> Self {
        let mut by_name = HashMap::new();
        let mut columns_by_relation: HashMap<RelationId, Vec<(String, ColumnId)>> = HashMap::new();
        for rel in model.relations() {
            let key = rel.name.to_ascii_lowercase();
            by_name.entry(key).or_insert_with(Vec::new).push(rel.id);
            let cols = rel.columns.iter().map(|c| (c.name.clone(), c.id)).collect();
            columns_by_relation.insert(rel.id, cols);
        }
        let functions = model.functions().iter().map(|f| f.name.clone()).collect();
        Self {
            by_name,
            columns_by_relation,
            functions,
        }
    }

    /// Complete given `prefix` (case-insensitive). If `dot_target` is `Some(alias_or_table)`,
    /// return columns for that relation via alias map.
    #[must_use]
    pub fn complete(
        &self,
        prefix: &str,
        alias_map: &HashMap<String, RelationId>,
        dot_target: Option<&str>,
    ) -> Vec<CompletionItem> {
        let lower = prefix.to_ascii_lowercase();
        let mut out = Vec::new();

        // Dot completion: `u.` → columns of `u`
        if let Some(target) = dot_target {
            if let Some(rel_id) = alias_map.get(&target.to_ascii_lowercase()).or_else(|| {
                self.by_name
                    .get(&target.to_ascii_lowercase())
                    .and_then(|v| v.first())
            }) {
                if let Some(cols) = self.columns_by_relation.get(rel_id) {
                    for (name, _) in cols {
                        if name.to_ascii_lowercase().starts_with(&lower) {
                            out.push(CompletionItem {
                                label: name.clone(),
                                detail: None,
                                kind: CompletionKind::Column,
                                insert_text: name.clone(),
                            });
                        }
                    }
                }
                out.truncate(50);
                return out;
            }
        }

        // Schema/table prefix
        for (name, rel_ids) in &self.by_name {
            if name.starts_with(&lower) {
                for rel_id in rel_ids {
                    out.push(CompletionItem {
                        label: name.clone(),
                        detail: Some(format!("{rel_id:?}")),
                        kind: CompletionKind::Table,
                        insert_text: name.clone(),
                    });
                    if out.len() >= 50 {
                        return out;
                    }
                }
            }
        }
        // Functions
        for f in &self.functions {
            if f.to_ascii_lowercase().starts_with(&lower) {
                out.push(CompletionItem {
                    label: f.clone(),
                    detail: None,
                    kind: CompletionKind::Function,
                    insert_text: f.clone(),
                });
                if out.len() >= 50 {
                    break;
                }
            }
        }
        // Common PG functions fallback
        for f in COMMON_PG_FUNCTIONS {
            if f.starts_with(&lower) && !out.iter().any(|i| i.label == *f) {
                out.push(CompletionItem {
                    label: (*f).into(),
                    detail: None,
                    kind: CompletionKind::Function,
                    insert_text: (*f).into(),
                });
                if out.len() >= 50 {
                    break;
                }
            }
        }
        out
    }
}

/// Extract `FROM/JOIN ... [AS] alias` map from SQL up to `cursor` offset.
#[must_use]
pub fn extract_aliases(sql: &str, cursor: usize) -> HashMap<String, RelationId> {
    // Placeholder: real impl parses last FROM/JOIN before cursor.
    // WU6 keeps hand-rolled FROM|JOIN regex; alias resolution is best-effort.
    let _ = (sql, cursor);
    HashMap::new()
}

const COMMON_PG_FUNCTIONS: &[&str] = &[
    "now",
    "coalesce",
    "nullif",
    "current_timestamp",
    "current_date",
    "count",
    "sum",
    "avg",
    "min",
    "max",
    "array_agg",
    "json_agg",
    "to_json",
    "row_to_json",
];

#[cfg(test)]
mod tests {
    use super::*;
    use pgnative_schema_model::build::Builder;
    use pgnative_schema_model::relation::Relation;
    use pgnative_schema_model::schema::Schema;
    use pgnative_schema_model::types::{Id, Oid, RelationKind};

    fn sample_model() -> SchemaModel {
        let mut b = Builder::new();
        let sid = b.add_schema(Schema {
            id: Id(0),
            name: "public".into(),
            comment: None,
        });
        let rid = b.add_relation(Relation {
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
        let _ = rid;
        // Add a column manually via relation columns? Simplified.
        b.build()
    }

    #[test]
    fn complete_tables() {
        let m = sample_model();
        let engine = CompletionEngine::new(&m);
        let items = engine.complete("us", &HashMap::new(), None);
        assert!(items.iter().any(|i| i.label == "users"));
    }
}
