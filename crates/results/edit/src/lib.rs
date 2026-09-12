//! Safe inline editing — PK-gated, parameterized UPDATE-only (§20, §21).
//! Product decision: UPDATE only in v1, No MERGE. Edits disabled inside explicit
//! transactions (Idle vs InTransaction/InFailedTransaction per §22).
use pgnative_schema_model::relation::Relation;
use pgnative_schema_model::types::Editability;
use thiserror::Error;

/// Transaction state as seen by the editor — stringified `TxState` from
/// `pgnative-db-connection` to avoid a DB dep. `Idle` means editable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TxForEdit {
    Idle,
    Active(String),
}

#[derive(Debug, Error)]
pub enum EditError {
    #[error("not editable: {0}")]
    NotEditable(String),
    #[error("no changes")]
    NoChanges,
    /// Edit attempted while explicit transaction is active — must commit/rollback first.
    #[error(
        "edit disabled: explicit transaction {state:?} active — commit or rollback before editing"
    )]
    InTransaction { state: TxForEdit },
    /// MERGE is not supported in v1 — use UPDATE only.
    #[error("MERGE not supported in v1 — use UPDATE only")]
    MergeNotSupported,
}

#[derive(Debug, Clone)]
pub struct ColumnDiff {
    pub col: String,
    pub old: String,
    pub new: String,
}

pub fn is_editable(rel: &Relation) -> bool {
    rel.editability() != Editability::Disabled
}

/// Product decision: edits disabled when explicit transaction is active.
/// Pass `TxForEdit::Idle` for editable, `Active(_)` for blocked.
/// Callers map `pgnative_db_connection::TxState` → `TxForEdit` via `tx.to_string()`.
#[must_use]
pub fn can_edit_in_tx(tx: TxForEdit) -> Result<(), EditError> {
    match tx {
        TxForEdit::Idle => Ok(()),
        s => Err(EditError::InTransaction { state: s }),
    }
}

/// Compute diff skipping GENERATED/IDENTITY/Virtual (§20).
pub fn diff_columns(
    rel: &Relation,
    original: &[(String, String)],
    edited: &[(String, String)],
) -> Result<Vec<ColumnDiff>, EditError> {
    if !is_editable(rel) {
        return Err(EditError::NotEditable(rel.name.clone()));
    }
    let mut out = vec![];
    for ((k1, v1), (k2, v2)) in original.iter().zip(edited.iter()) {
        if k1 != k2 {
            continue;
        }
        if v1 != v2 {
            // Check ValueSource — skip generated/identity (simplified: column lookup)
            if let Some(col) = rel.column_named(k1) {
                if col.value_source != pgnative_schema_model::types::ValueSource::Stored {
                    continue;
                }
            }
            out.push(ColumnDiff {
                col: k1.clone(),
                old: v1.clone(),
                new: v2.clone(),
            });
        }
    }
    if out.is_empty() {
        return Err(EditError::NoChanges);
    }
    Ok(out)
}

fn escape_ident(s: &str) -> String {
    s.replace('"', "\"\"")
}

fn quoted_ident(s: &str) -> String {
    format!("\"{}\"", escape_ident(s))
}

/// Generate parameterized `UPDATE "table" SET "col"=$1 WHERE pk=$N RETURNING *`.
/// `original` must contain the PK column values (full row). Product decision:
/// UPDATE only — MERGE is rejected.
pub fn update_sql(
    rel: &Relation,
    diffs: &[ColumnDiff],
) -> Result<(String, Vec<String>), EditError> {
    update_sql_with_pk(rel, diffs, &[])
}

/// Same as `update_sql` but with explicit PK values sandwich: SET params first,
/// then PK params for WHERE. Callers must supply PK values matching PK column
/// count and order; missing or mismatched PK values are rejected to avoid
/// half-bound SQL with empty defaults (BUG #3).
pub fn update_sql_with_pk(
    rel: &Relation,
    diffs: &[ColumnDiff],
    pk_values: &[(String, String)],
) -> Result<(String, Vec<String>), EditError> {
    if !is_editable(rel) {
        return Err(EditError::NotEditable(rel.name.clone()));
    }
    let pk_cols = rel
        .primary_key
        .as_ref()
        .map(|pk| pk.columns.clone())
        .unwrap_or_default();
    if pk_cols.is_empty() {
        return Err(EditError::NotEditable("no PK".into()));
    }
    if diffs.is_empty() {
        return Err(EditError::NoChanges);
    }
    // BUG #3 fix: require exact PK value count — no empty defaults.
    if pk_cols.len() != pk_values.len() {
        return Err(EditError::NotEditable(format!(
            "missing PK values: expected {} got {}",
            pk_cols.len(),
            pk_values.len()
        )));
    }
    let set_clause = diffs
        .iter()
        .enumerate()
        .map(|(i, d)| format!("{}=${}", quoted_ident(&d.col), i + 1))
        .collect::<Vec<_>>()
        .join(", ");
    let where_start = diffs.len() + 1;
    let where_clause = pk_cols
        .iter()
        .enumerate()
        .map(|(i, cid)| {
            let name = rel
                .column_by_id(*cid)
                .map(|c| c.name.as_str())
                .unwrap_or("id");
            format!("{}=${}", quoted_ident(name), where_start + i)
        })
        .collect::<Vec<_>>()
        .join(" AND ");
    let sql = format!(
        "UPDATE {} SET {} WHERE {} RETURNING *",
        quoted_ident(&rel.name),
        set_clause,
        where_clause
    );
    let mut params: Vec<String> = diffs.iter().map(|d| d.new.clone()).collect();
    // Append PK values in PK column order, looked up from pk_values map.
    // All PK entries must be present — missing key is an error.
    for cid in &pk_cols {
        let pk_name = rel
            .column_by_id(*cid)
            .map(|c| c.name.as_str())
            .unwrap_or("id");
        let Some(val) = pk_values
            .iter()
            .find(|(k, _)| k == pk_name)
            .map(|(_, v)| v.clone())
        else {
            return Err(EditError::NotEditable(format!(
                "missing PK column '{pk_name}' in pk_values"
            )));
        };
        if val.is_empty() {
            return Err(EditError::NotEditable(format!(
                "empty PK value for '{pk_name}'"
            )));
        }
        params.push(val);
    }
    Ok((sql, params))
}

/// Optimistic-concurrency UPDATE (§21): `WHERE pk=$N AND "changed_col"=$M(old)`.
///
/// Appends the *original* values of every changed column to the WHERE clause.
/// Callers execute with the returned params and check the affected-row count:
/// `0` rows ⇒ someone else changed the row first (stale) — surface a conflict
/// instead of silently overwriting. Param order: SET-new…, PK…, WHERE-old….
pub fn update_sql_optimistic(
    rel: &Relation,
    diffs: &[ColumnDiff],
    pk_values: &[(String, String)],
) -> Result<(String, Vec<String>), EditError> {
    let (mut sql, mut params) = update_sql_with_pk(rel, diffs, pk_values)?;
    // `update_sql_with_pk` ends with `RETURNING *` — splice the version check
    // in before it so row identity + freshness gate in one statement.
    let mut where_old = Vec::with_capacity(diffs.len());
    for d in diffs {
        params.push(d.old.clone());
        where_old.push(format!("{}=${}", quoted_ident(&d.col), params.len()));
    }
    let guard = where_old.join(" AND ");
    sql = sql.replacen(" RETURNING *", &format!(" AND {guard} RETURNING *"), 1);
    Ok((sql, params))
}

/// Explicitly rejected: MERGE in v1.
pub fn merge_sql(
    _rel: &Relation,
    _diffs: &[ColumnDiff],
) -> Result<(String, Vec<String>), EditError> {
    Err(EditError::MergeNotSupported)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pgnative_schema_model::column::Column;
    use pgnative_schema_model::relation::{PrimaryKey, Relation};
    use pgnative_schema_model::types::{Id, Nullability, Oid, RelationKind, TypeId, ValueSource};
    fn rel_with_pk() -> Relation {
        let col = Column {
            id: Id(0),
            owner: Id(0),
            name: "id".into(),
            position: 1,
            ty: Id(0),
            nullability: Nullability::NotNull,
            has_default: false,
            default_expr: None,
            value_source: ValueSource::Stored,
        };
        let col2 = Column {
            id: Id(1),
            owner: Id(0),
            name: "email".into(),
            position: 2,
            ty: Id(1),
            nullability: Nullability::Nullable,
            has_default: false,
            default_expr: None,
            value_source: ValueSource::Stored,
        };
        Relation {
            id: Id(0),
            schema: Id(0),
            oid: Oid(1),
            name: "users".into(),
            kind: RelationKind::Table,
            columns: vec![col, col2],
            primary_key: Some(PrimaryKey {
                columns: vec![Id(0)],
                name: None,
            }),
            unique_keys: vec![],
            foreign_keys_out: vec![],
            foreign_keys_in: vec![],
            comment: None,
        }
    }
    #[test]
    fn diff_and_update() {
        let rel = rel_with_pk();
        let orig = vec![("email".into(), "a@b".into())];
        let edited = vec![("email".into(), "c@d".into())];
        let diffs = diff_columns(&rel, &orig, &edited).unwrap();
        assert_eq!(diffs.len(), 1);
        // update_sql without PK must error (BUG #3: no half-bound SQL)
        assert!(update_sql(&rel, &diffs).is_err());
        // correct usage with PK values succeeds
        let pk_values = vec![("id".into(), "1".into())];
        let (sql, params) = update_sql_with_pk(&rel, &diffs, &pk_values).unwrap();
        assert!(sql.contains("UPDATE"));
        assert!(sql.contains("RETURNING *"));
        assert_eq!(params.last().unwrap(), "1");
    }

    #[test]
    fn update_rejects_missing_pk() {
        let rel = rel_with_pk();
        let diffs = vec![ColumnDiff {
            col: "email".into(),
            old: "a".into(),
            new: "b".into(),
        }];
        // empty pk_values → error
        assert!(update_sql_with_pk(&rel, &diffs, &[]).is_err());
        // wrong column name → error
        assert!(update_sql_with_pk(&rel, &diffs, &[("wrong".into(), "1".into())]).is_err());
        // empty value → error
        assert!(update_sql_with_pk(&rel, &diffs, &[("id".into(), "".into())]).is_err());
    }

    fn rel_composite_pk() -> Relation {
        let mut rel = rel_with_pk();
        rel.columns.push(Column {
            id: Id(2),
            owner: Id(0),
            name: "tenant".into(),
            position: 3,
            ty: Id(0),
            nullability: Nullability::NotNull,
            has_default: false,
            default_expr: None,
            value_source: ValueSource::Stored,
        });
        rel.primary_key = Some(PrimaryKey {
            columns: vec![Id(0), Id(2)],
            name: None,
        });
        rel
    }

    #[test]
    fn composite_pk_binds_both_keys_in_order() {
        let rel = rel_composite_pk();
        let diffs = vec![ColumnDiff {
            col: "email".into(),
            old: "a".into(),
            new: "b".into(),
        }];
        // One of two PK values → rejected, never half-bound.
        assert!(update_sql_with_pk(&rel, &diffs, &[("id".into(), "1".into())]).is_err());
        let pk = vec![("id".into(), "1".into()), ("tenant".into(), "t9".into())];
        let (sql, params) = update_sql_with_pk(&rel, &diffs, &pk).unwrap();
        assert!(sql.contains("\"id\"=$2 AND \"tenant\"=$3"), "got: {sql}");
        assert_eq!(
            params,
            vec!["b".to_string(), "1".to_string(), "t9".to_string()]
        );
    }

    #[test]
    fn optimistic_update_guards_on_old_values() {
        let rel = rel_with_pk();
        let diffs = vec![ColumnDiff {
            col: "email".into(),
            old: "a@b".into(),
            new: "c@d".into(),
        }];
        let pk = vec![("id".into(), "1".into())];
        let (sql, params) = update_sql_optimistic(&rel, &diffs, &pk).unwrap();
        // SET-new ($1), PK ($2), then old-value guard ($3).
        assert!(sql.contains("AND \"email\"=$3 RETURNING *"), "got: {sql}");
        assert_eq!(
            params,
            vec!["c@d".to_string(), "1".to_string(), "a@b".to_string()]
        );
    }
}
