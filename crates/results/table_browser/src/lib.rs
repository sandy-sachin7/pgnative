//! Table browser with keyset pagination — separate from arbitrary query (§17).
use pgnative_schema_model::relation::Relation;
use pgnative_schema_model::types::RelationId;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum BrowserError {
    #[error("not browsable: {0}")]
    NotBrowsable(String),
}

pub struct BrowserQuery {
    pub relation: RelationId,
    pub limit: usize,
    pub cursor: Option<Vec<String>>,
}

/// Build `SELECT ... FROM schema.rel ORDER BY pk LIMIT $1` or keyset `WHERE (pk) > ($X)`.
///
pub fn build_sql(
    relation: &Relation,
    limit: usize,
    cursor: Option<&[String]>,
) -> Result<(String, Vec<String>), BrowserError> {
    let editability = relation.editability();
    if editability == pgnative_schema_model::types::Editability::Disabled {
        return Err(BrowserError::NotBrowsable(format!(
            "{} has no PK/unique-not-null",
            relation.name
        )));
    }
    let cols: Vec<_> = relation
        .columns
        .iter()
        .map(|c| format!("\"{}\"", c.name))
        .collect();
    let cols_sql = if cols.is_empty() {
        "*".into()
    } else {
        cols.join(", ")
    };
    let order_cols = relation
        .primary_key
        .as_ref()
        .map(|pk| pk.columns.clone())
        .or_else(|| relation.unique_keys.first().map(|k| k.columns.clone()))
        .unwrap_or_default();
    let order_sql = if order_cols.is_empty() {
        String::new()
    } else {
        let names: Vec<_> = order_cols
            .iter()
            .filter_map(|cid| {
                relation
                    .column_by_id(*cid)
                    .map(|c| format!("\"{}\"", c.name))
            })
            .collect();
        if names.is_empty() {
            String::new()
        } else {
            format!(" ORDER BY {}", names.join(", "))
        }
    };
    let (where_sql, mut params) = if let Some(cur) = cursor {
        // Simplified single-col keyset; composite uses row comparison (col1, col2) > ($1,$2).
        if cur.len() != order_cols.len() {
            return Err(BrowserError::NotBrowsable("cursor length mismatch".into()));
        }
        let placeholders: Vec<_> = (1..=cur.len()).map(|i| format!("${i}")).collect();
        let cols: Vec<_> = order_cols
            .iter()
            .filter_map(|cid| {
                relation
                    .column_by_id(*cid)
                    .map(|c| format!("\"{}\"", c.name))
            })
            .collect();
        let where_clause = if cols.len() == 1 {
            format!(" WHERE {} > ${}", cols[0], 1)
        } else {
            format!(
                " WHERE ({}) > ({})",
                cols.join(", "),
                placeholders.join(", ")
            )
        };
        (where_clause, cur.to_vec())
    } else {
        (String::new(), vec![])
    };
    let limit_idx = params.len() + 1;
    params.push(limit.to_string());
    let sql = format!(
        "SELECT {cols_sql} FROM \"{}\"{} LIMIT ${limit_idx}",
        relation.name,
        format!("{where_sql}{order_sql}")
    );
    Ok((sql, params))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pgnative_schema_model::column::Column;
    use pgnative_schema_model::relation::{PrimaryKey, Relation};
    use pgnative_schema_model::types::{ColumnId, Id, Oid, RelationKind};
    use pgnative_schema_model::types::{Nullability, TypeId, ValueSource};
    #[test]
    fn not_browsable_without_pk() {
        let rel = Relation {
            id: Id(0),
            schema: Id(0),
            oid: Oid(1),
            name: "logs".into(),
            kind: RelationKind::Table,
            columns: vec![],
            primary_key: None,
            unique_keys: vec![],
            foreign_keys_out: vec![],
            foreign_keys_in: vec![],
            comment: None,
        };
        assert!(build_sql(&rel, 50, None).is_err());
    }
    #[test]
    fn browsable_with_pk() {
        let col = Column {
            id: ColumnId(Id(0)),
            owner: Id(0),
            name: "id".into(),
            position: 1,
            ty: TypeId(Id(0)),
            nullability: Nullability::NotNull,
            has_default: false,
            default_expr: None,
            value_source: ValueSource::Stored,
        };
        let rel = Relation {
            id: Id(0),
            schema: Id(0),
            oid: Oid(1),
            name: "users".into(),
            kind: RelationKind::Table,
            columns: vec![col],
            primary_key: Some(PrimaryKey {
                columns: vec![Id(0)],
                name: Some("users_pkey".into()),
            }),
            unique_keys: vec![],
            foreign_keys_out: vec![],
            foreign_keys_in: vec![],
            comment: None,
        };
        let (sql, _) = build_sql(&rel, 50, None).unwrap();
        assert!(sql.contains("ORDER BY"));
        assert!(sql.contains("LIMIT"));
    }
}
