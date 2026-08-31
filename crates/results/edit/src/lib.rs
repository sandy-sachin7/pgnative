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

/// Generate parameterized `UPDATE "table" SET "col"=$1 WHERE pk=$N RETURNING *`.
/// Product decision: UPDATE only — MERGE is rejected.
pub fn update_sql(
    rel: &Relation,
    diffs: &[ColumnDiff],
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
    let set_clause = diffs
        .iter()
        .enumerate()
        .map(|(i, d)| format!("\"{}\"=${}", d.col, i + 1))
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
            format!("\"{}\"=${}", name, where_start + i)
        })
        .collect::<Vec<_>>()
        .join(" AND ");
    let sql = format!(
        "UPDATE \"{}\" SET {} WHERE {} RETURNING *",
        rel.name, set_clause, where_clause
    );
    let params = diffs.iter().map(|d| d.new.clone()).collect();
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
        let (sql, _) = update_sql(&rel, &diffs).unwrap();
        assert!(sql.contains("UPDATE"));
        assert!(sql.contains("RETURNING *"));
    }
}
