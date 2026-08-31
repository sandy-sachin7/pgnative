//! Table browser with keyset pagination — separate from arbitrary query (§17).
//! Implements plan C6: keyset `WHERE (pk) > ($X) ORDER BY pk LIMIT $n` when
//! `Editability != Disabled`, never guess identity.

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

/// Build `SELECT ... FROM "schema"."rel" ORDER BY pk LIMIT $1`
/// or keyset `WHERE (pk) > ($X) ORDER BY pk LIMIT $n`.
///
/// Returns `(sql, params)` where params are textual bound values (caller
/// converts to `ToSql` via `postgres-types`). `cursor` must have same
/// length as PK/unique key columns. Uses `ORDER BY` on the identity columns
/// so pagination is stable and efficient (no OFFSET).
pub fn build_sql(
    relation: &Relation,
    limit: usize,
    cursor: Option<&[String]>,
) -> Result<(String, Vec<String>), BrowserError> {
    build_sql_with_schema(relation, None, limit, cursor)
}

/// Variant that qualifies `FROM` with schema name when available.
pub fn build_sql_with_schema(
    relation: &Relation,
    schema_name: Option<&str>,
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
    // SELECT list — quoted column names
    let cols: Vec<_> = relation
        .columns
        .iter()
        .map(|c| format!("\"{}\"", c.name.replace('"', "\"\"")))
        .collect();
    let cols_sql = if cols.is_empty() {
        "*".into()
    } else {
        cols.join(", ")
    };

    // Identity columns (PK preferred, else first unique-not-null)
    let order_cols = relation
        .primary_key
        .as_ref()
        .map(|pk| pk.columns.clone())
        .or_else(|| relation.unique_keys.first().map(|k| k.columns.clone()))
        .unwrap_or_default();

    // Resolve identity column names (quoted)
    let order_names: Vec<String> = order_cols
        .iter()
        .filter_map(|cid| {
            relation
                .column_by_id(*cid)
                .map(|c| format!("\"{}\"", c.name.replace('"', "\"\"")))
        })
        .collect();

    if order_names.is_empty() {
        return Err(BrowserError::NotBrowsable(
            "identity columns not found".into(),
        ));
    }

    let order_sql = format!(" ORDER BY {}", order_names.join(", "));

    // WHERE clause for keyset pagination
    let (where_sql, mut params) = if let Some(cur) = cursor {
        if cur.len() != order_names.len() {
            return Err(BrowserError::NotBrowsable(format!(
                "cursor length {} != key columns {}",
                cur.len(),
                order_names.len()
            )));
        }
        // For composite keys use row comparison: (a,b) > ($1,$2)
        // For single key use: "col" > $1
        let where_clause = if order_names.len() == 1 {
            format!(" WHERE {} > $1", order_names[0])
        } else {
            let placeholders: Vec<_> = (1..=cur.len()).map(|i| format!("${i}")).collect();
            format!(
                " WHERE ({}) > ({})",
                order_names.join(", "),
                placeholders.join(", ")
            )
        };
        (where_clause, cur.to_vec())
    } else {
        (String::new(), vec![])
    };

    let limit_idx = params.len() + 1;
    params.push(limit.to_string());

    // FROM clause — schema-qualified when provided
    let from_sql = if let Some(schema) = schema_name {
        format!(
            "\"{}\".\"{}\"",
            schema.replace('"', "\"\""),
            relation.name.replace('"', "\"\"")
        )
    } else {
        format!("\"{}\"", relation.name.replace('"', "\"\""))
    };

    // ORDER BY must come before LIMIT; WHERE before ORDER BY
    let sql = format!("SELECT {cols_sql} FROM {from_sql}{where_sql}{order_sql} LIMIT ${limit_idx}");
    Ok((sql, params))
}

/// Extract next cursor from a row (identity column values as strings).
/// Used by the UI to fetch the next page.
#[must_use]
pub fn next_cursor(relation: &Relation, row: &pgnative_results_value::Row) -> Option<Vec<String>> {
    let cols = relation
        .primary_key
        .as_ref()
        .map(|pk| &pk.columns)
        .or_else(|| relation.unique_keys.first().map(|k| &k.columns))?;
    let mut cursor = Vec::with_capacity(cols.len());
    for cid in cols {
        let pos = relation.columns.iter().position(|c| &c.id == cid)?;
        let val = row.cells.get(pos)?;
        cursor.push(val.to_display_string());
    }
    Some(cursor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pgnative_schema_model::column::Column;
    use pgnative_schema_model::relation::{PrimaryKey, Relation};
    use pgnative_schema_model::types::{ColumnId, Id, Oid, RelationKind};
    use pgnative_schema_model::types::{Nullability, TypeId, ValueSource};

    fn make_rel_with_pk(name: &str, pk_cols: Vec<(&str, ColumnId)>) -> Relation {
        let columns: Vec<Column> = pk_cols
            .iter()
            .enumerate()
            .map(|(i, (n, id))| Column {
                id: *id,
                owner: Id(0),
                name: (*n).into(),
                position: (i + 1) as u16,
                ty: Id(0),
                nullability: Nullability::NotNull,
                has_default: false,
                default_expr: None,
                value_source: ValueSource::Stored,
            })
            .collect();
        let pk_ids = pk_cols.iter().map(|(_, id)| *id).collect();
        Relation {
            id: Id(0),
            schema: Id(0),
            oid: Oid(1),
            name: name.into(),
            kind: RelationKind::Table,
            columns,
            primary_key: Some(PrimaryKey {
                columns: pk_ids,
                name: Some(format!("{name}_pkey")),
            }),
            unique_keys: vec![],
            foreign_keys_out: vec![],
            foreign_keys_in: vec![],
            comment: None,
        }
    }

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
        let rel = make_rel_with_pk("users", vec![("id", Id(0))]);
        let (sql, _) = build_sql(&rel, 50, None).unwrap();
        assert!(sql.contains("ORDER BY"));
        assert!(sql.contains("LIMIT"));
        assert!(sql.contains("\"users\""));
        assert!(sql.contains("\"id\""));
    }

    #[test]
    fn keyset_single_col() {
        let rel = make_rel_with_pk("users", vec![("id", Id(0))]);
        let cursor = vec!["42".to_string()];
        let (sql, params) = build_sql(&rel, 20, Some(&cursor)).unwrap();
        assert!(sql.contains("WHERE \"id\" > $1"));
        assert!(sql.contains("ORDER BY \"id\""));
        assert!(sql.contains("LIMIT $2"));
        assert_eq!(params, vec!["42", "20"]);
    }

    #[test]
    fn keyset_composite() {
        let rel = make_rel_with_pk("orders", vec![("order_id", Id(0)), ("item_id", Id(1))]);
        let cursor = vec!["10".to_string(), "5".to_string()];
        let (sql, params) = build_sql(&rel, 20, Some(&cursor)).unwrap();
        assert!(sql.contains("WHERE (\"order_id\", \"item_id\") > ($1, $2)"));
        assert!(sql.contains("ORDER BY \"order_id\", \"item_id\""));
        assert!(sql.contains("LIMIT $3"));
        assert_eq!(params, vec!["10", "5", "20"]);
    }

    #[test]
    fn schema_qualified() {
        let rel = make_rel_with_pk("users", vec![("id", Id(0))]);
        let (sql, _) = build_sql_with_schema(&rel, Some("public"), 10, None).unwrap();
        assert!(sql.contains("\"public\".\"users\""));
    }

    #[test]
    fn cursor_length_mismatch_errors() {
        let rel = make_rel_with_pk("users", vec![("id", Id(0))]);
        let cursor = vec!["1".to_string(), "2".to_string()];
        assert!(build_sql(&rel, 10, Some(&cursor)).is_err());
    }

    #[test]
    fn sql_is_parameterized_no_concat() {
        let rel = make_rel_with_pk("users", vec![("id", Id(0))]);
        // Injection attempt in cursor value must be parameterized, not concatenated
        let cursor = vec!["1; DROP TABLE users; --".to_string()];
        let (sql, params) = build_sql(&rel, 10, Some(&cursor)).unwrap();
        assert!(!sql.contains("DROP"));
        assert_eq!(params[0], "1; DROP TABLE users; --");
        assert!(sql.contains("$1"));
    }
}
