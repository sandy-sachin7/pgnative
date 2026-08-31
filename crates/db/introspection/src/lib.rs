//! pg_catalog introspection → SchemaModel.
//! Implements AGENTS.md §12 per ADR-0007. Five qualified queries, real Client wiring,
//! plus pure hydrate via Builder.

use std::collections::HashMap;

use pgnative_schema_model::build::Builder;
use pgnative_schema_model::column::Column;
use pgnative_schema_model::relation::{ForeignKey, Function, PrimaryKey, Relation, UniqueKey};
use pgnative_schema_model::schema::Schema;
use pgnative_schema_model::types::{Id, Nullability, Oid, RelationKind, ValueSource};
use pgnative_schema_model::SchemaModel;
use thiserror::Error;

/// Qualified query texts — caller executes via `tokio_postgres::Client`.
pub mod queries {
    pub const SCHEMAS: &str = r#"
SELECT n.oid, n.nspname, obj_description(n.oid,'pg_namespace')
FROM pg_catalog.pg_namespace n
WHERE n.nspname NOT LIKE 'pg_temp%'
ORDER BY n.nspname"#;

    pub const RELATIONS: &str = r#"
SELECT c.oid, c.relnamespace, c.relname, c.relkind,
       obj_description(c.oid,'pg_class')
FROM pg_catalog.pg_class c
JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace
WHERE c.relkind IN ('r','v','m','f','p') AND n.nspname NOT LIKE 'pg_temp%'
ORDER BY n.nspname, c.relname"#;

    pub const COLUMNS: &str = r#"
SELECT a.attrelid, a.attnum, a.attname, a.atttypid, t.typname,
       a.attnotnull, a.atthasdef,
       pg_get_expr(ad.adbin, ad.adrelid),
       a.attgenerated, a.attidentity
FROM pg_catalog.pg_attribute a
JOIN pg_catalog.pg_type t ON t.oid=a.atttypid
LEFT JOIN pg_catalog.pg_attrdef ad ON ad.adrelid=a.attrelid AND ad.adnum=a.attnum
WHERE a.attnum>0 AND NOT a.attisdropped
ORDER BY a.attrelid, a.attnum"#;

    pub const CONSTRAINTS_PK_UNIQUE: &str = r#"
SELECT conrelid, conname, contype, conkey
FROM pg_catalog.pg_constraint
WHERE contype IN ('p','u')"#;

    pub const CONSTRAINTS_FK: &str = r#"
SELECT oid, conname, conrelid, confrelid, conkey, confkey
FROM pg_catalog.pg_constraint WHERE contype='f'"#;

    pub const FUNCTIONS: &str = r#"
SELECT p.oid, p.pronamespace, p.proname,
       pg_get_function_identity_arguments(p.oid), t.typname, p.prokind
FROM pg_catalog.pg_proc p
JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace
JOIN pg_catalog.pg_type t ON t.oid=p.prorettype
WHERE n.nspname NOT LIKE 'pg_temp%'"#;
}

#[derive(Debug, Error)]
pub enum IntrospectionError {
    #[error("query failed: {0}")]
    Query(#[from] tokio_postgres::Error),
    #[error("introspection failed: {0}")]
    Other(String),
}

// ---------------------------------------------------------------------------
// Client-bound fetch helpers
// ---------------------------------------------------------------------------

/// Set introspection session parameters (statement_timeout 5s).
/// Previously set `default_transaction_read_only=on` session-scoped, which
/// leaked to user queries and broke writes (§7). Now only sets timeout;
/// introspection queries are read-only by nature and don't need session
/// persistence. Caller may wrap introspection in `BEGIN READ ONLY` if desired.
pub async fn prepare_session(client: &tokio_postgres::Client) -> Result<(), IntrospectionError> {
    client
        .batch_execute("SET statement_timeout = '5s';")
        .await?;
    Ok(())
}

pub async fn fetch_schemas(
    client: &tokio_postgres::Client,
) -> Result<Vec<(Oid, String, Option<String>)>, IntrospectionError> {
    let rows = client.query(queries::SCHEMAS, &[]).await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let oid: u32 = r.get(0);
        let name: String = r.get(1);
        let comment: Option<String> = r.get(2);
        out.push((Oid(oid), name, comment));
    }
    Ok(out)
}

pub async fn fetch_relations(
    client: &tokio_postgres::Client,
) -> Result<Vec<(Oid, Oid, String, char, Option<String>)>, IntrospectionError> {
    let rows = client.query(queries::RELATIONS, &[]).await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let oid: u32 = r.get(0);
        let ns: u32 = r.get(1);
        let name: String = r.get(2);
        let kind_s: String = r.get(3);
        let kind = kind_s.chars().next().unwrap_or('r');
        let comment: Option<String> = r.get(4);
        out.push((Oid(oid), Oid(ns), name, kind, comment));
    }
    Ok(out)
}

pub async fn fetch_columns(
    client: &tokio_postgres::Client,
) -> Result<
    Vec<(
        Oid,
        i16,
        String,
        Oid,
        String,
        bool,
        Option<String>,
        String,
        String,
    )>,
    IntrospectionError,
> {
    let rows = client.query(queries::COLUMNS, &[]).await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let attrelid: u32 = r.get(0);
        let attnum: i16 = r.get(1);
        let attname: String = r.get(2);
        let atttypid: u32 = r.get(3);
        let typname: String = r.get(4);
        let attnotnull: bool = r.get(5);
        let atthasdef: bool = r.get(6);
        let default_expr: Option<String> = r.get(7);
        let attgenerated: String = r.get(8);
        let attidentity: String = r.get(9);
        // atthasdef is redundant — has_default derived from default_expr is_some.
        let _ = atthasdef;
        out.push((
            Oid(attrelid),
            attnum,
            attname,
            Oid(atttypid),
            typname,
            attnotnull,
            default_expr,
            attgenerated,
            attidentity,
        ));
    }
    Ok(out)
}

pub async fn fetch_constraints_pk_unique(
    client: &tokio_postgres::Client,
) -> Result<Vec<(Oid, String, char, Option<Vec<i16>>)>, IntrospectionError> {
    let rows = client.query(queries::CONSTRAINTS_PK_UNIQUE, &[]).await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let conrelid: u32 = r.get(0);
        let conname: String = r.get(1);
        let contype_s: String = r.get(2);
        let contype = contype_s.chars().next().unwrap_or('p');
        let conkey: Option<Vec<i16>> = r.get(3);
        out.push((Oid(conrelid), conname, contype, conkey));
    }
    Ok(out)
}

pub async fn fetch_constraints_fk(
    client: &tokio_postgres::Client,
) -> Result<Vec<(Oid, String, Oid, Oid, Option<Vec<i16>>, Option<Vec<i16>>)>, IntrospectionError> {
    let rows = client.query(queries::CONSTRAINTS_FK, &[]).await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let oid: u32 = r.get(0);
        let conname: String = r.get(1);
        let conrelid: u32 = r.get(2);
        let confrelid: u32 = r.get(3);
        let conkey: Option<Vec<i16>> = r.get(4);
        let confkey: Option<Vec<i16>> = r.get(5);
        out.push((
            Oid(oid),
            conname,
            Oid(conrelid),
            Oid(confrelid),
            conkey,
            confkey,
        ));
    }
    Ok(out)
}

pub async fn fetch_functions(
    client: &tokio_postgres::Client,
) -> Result<Vec<(Oid, Oid, String, String, String, char)>, IntrospectionError> {
    let rows = client.query(queries::FUNCTIONS, &[]).await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let oid: u32 = r.get(0);
        let ns: u32 = r.get(1);
        let proname: String = r.get(2);
        let args: String = r.get(3);
        let rettype: String = r.get(4);
        let prokind_s: String = r.get(5);
        let prokind = prokind_s.chars().next().unwrap_or('f');
        // Build display signature: name(args)
        let signature = format!("{proname}({args})");
        out.push((Oid(oid), Oid(ns), proname, signature, rettype, prokind));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Top-level introspection — two-phase per ADR-0007
// ---------------------------------------------------------------------------

/// Full introspection via live Client: Phase A + Phase B in one model.
pub async fn introspect(
    client: &tokio_postgres::Client,
) -> Result<SchemaModel, IntrospectionError> {
    // Phase A — schemas, relations, columns, types
    let schemas = fetch_schemas(client).await?;
    let relations = fetch_relations(client).await?;
    let columns = fetch_columns(client).await?;

    // Phase B — constraints + functions (second swap in caller could publish Phase A first)
    let pk_unique = fetch_constraints_pk_unique(client).await?;
    let fks = fetch_constraints_fk(client).await?;
    let functions = fetch_functions(client).await?;

    hydrate::build_full(schemas, relations, columns, pk_unique, fks, functions)
}

/// Phase-A only introspection — caller can publish this model first, then enrich.
pub async fn introspect_phase_a(
    client: &tokio_postgres::Client,
) -> Result<SchemaModel, IntrospectionError> {
    let schemas = fetch_schemas(client).await?;
    let relations = fetch_relations(client).await?;
    let columns = fetch_columns(client).await?;
    hydrate::phase_a(schemas, relations, columns)
}

// ---------------------------------------------------------------------------
// Pure hydrate — testable without PG
// ---------------------------------------------------------------------------

pub mod hydrate {
    use super::*;

    /// Phase A: schemas + relations + columns + types → SchemaModel.
    pub fn phase_a(
        schemas: Vec<(Oid, String, Option<String>)>,
        relations: Vec<(Oid, Oid, String, char, Option<String>)>,
        columns: Vec<(
            Oid,
            i16,
            String,
            Oid,
            String,
            bool,
            Option<String>,
            String,
            String,
        )>,
    ) -> Result<SchemaModel, IntrospectionError> {
        build_full(
            schemas,
            relations,
            columns,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
    }

    /// Full build with constraints and functions.
    #[allow(clippy::too_many_lines)]
    pub fn build_full(
        schemas: Vec<(Oid, String, Option<String>)>,
        relations: Vec<(Oid, Oid, String, char, Option<String>)>,
        columns: Vec<(
            Oid,
            i16,
            String,
            Oid,
            String,
            bool,
            Option<String>,
            String,
            String,
        )>,
        pk_unique: Vec<(Oid, String, char, Option<Vec<i16>>)>,
        fks: Vec<(Oid, String, Oid, Oid, Option<Vec<i16>>, Option<Vec<i16>>)>,
        functions: Vec<(Oid, Oid, String, String, String, char)>,
    ) -> Result<SchemaModel, IntrospectionError> {
        // BUG #7 fix: never synthesize phantom Id(0) when DB has no schemas.
        if schemas.is_empty() {
            return Err(IntrospectionError::Other("no schemas found".into()));
        }
        let mut b = Builder::new();

        // 1) Schemas — dense Ids, oid→SchemaId map.
        let mut schema_by_oid: HashMap<Oid, Id> = HashMap::new();
        for (idx, (oid, name, comment)) in schemas.into_iter().enumerate() {
            let id = Id(u32::try_from(idx).expect("schema idx fits u32"));
            schema_by_oid.insert(oid, id);
            b.add_schema(Schema { id, name, comment });
        }
        // Fallback schema for orphan relations (safe now: schemas non-empty).
        let fallback_schema = schema_by_oid
            .values()
            .next()
            .copied()
            .expect("schemas non-empty checked above");

        // 2) Types — deduplicate type OIDs.
        let mut type_by_oid: HashMap<Oid, Id> = HashMap::new();
        let mut next_type: u32 = 0;
        // Reserve TypeIds for all columns' type OIDs first.
        for (_, _, _, ty_oid, ty_name, _, _, _, _) in &columns {
            if !type_by_oid.contains_key(ty_oid) {
                let tid = Id(next_type);
                next_type += 1;
                type_by_oid.insert(*ty_oid, tid);
                b.add_type(tid, ty_name.clone());
            }
        }

        // 3) Group columns by attrelid.
        let mut cols_by_rel: HashMap<
            Oid,
            Vec<(i16, String, Oid, bool, Option<String>, String, String)>,
        > = HashMap::new();
        for (
            rel_oid,
            attnum,
            name,
            ty_oid,
            _ty_name,
            not_null,
            default_expr,
            generated,
            identity,
        ) in columns
        {
            cols_by_rel.entry(rel_oid).or_default().push((
                attnum,
                name,
                ty_oid,
                not_null,
                default_expr,
                generated,
                identity,
            ));
        }

        // 4) Build Relations temp vector with Columns.
        // Global column id counter.
        let mut next_col: u32 = 0;
        // Per-relation attnum → ColumnId map for constraint resolution.
        let mut attnum_to_col: HashMap<Oid, HashMap<i16, Id>> = HashMap::new();
        // Relation OID → RelationId map.
        let mut rel_by_oid: HashMap<Oid, Id> = HashMap::new();

        // Collect constraint groups for quick lookup during relation construction.
        // PK/UQ grouped by conrelid.
        let mut pk_by_rel: HashMap<Oid, Vec<(String, char, Vec<i16>)>> = HashMap::new();
        for (rel_oid, conname, contype, conkey) in pk_unique {
            if let Some(keys) = conkey {
                pk_by_rel
                    .entry(rel_oid)
                    .or_default()
                    .push((conname, contype, keys));
            }
        }

        // Prepare FK storage: we will resolve after all relations created so we can look up remote attnums.
        // Keep raw fks for second pass.

        let mut relations_tmp: Vec<Relation> = Vec::with_capacity(relations.len());

        for (idx, (rel_oid, ns_oid, rel_name, kind_char, comment)) in
            relations.into_iter().enumerate()
        {
            let schema = schema_by_oid
                .get(&ns_oid)
                .copied()
                .unwrap_or(fallback_schema);
            let kind = match kind_char {
                'v' => RelationKind::View,
                'm' => RelationKind::MaterializedView,
                'f' => RelationKind::ForeignTable,
                'p' => RelationKind::Table, // partitioned table treated as Table
                _ => RelationKind::Table,
            };
            let rel_id = Id(u32::try_from(idx).expect("relation idx fits u32"));
            rel_by_oid.insert(rel_oid, rel_id);

            // Columns for this relation
            let mut col_tuples = cols_by_rel.remove(&rel_oid).unwrap_or_default();
            col_tuples.sort_by_key(|(attnum, _, _, _, _, _, _)| *attnum);

            let mut cols: Vec<Column> = Vec::with_capacity(col_tuples.len());
            let mut attmap: HashMap<i16, Id> = HashMap::new();
            for (attnum, col_name, ty_oid, not_null, default_expr, generated, identity) in
                col_tuples
            {
                let ty = *type_by_oid.get(&ty_oid).unwrap_or(&Id(0));
                let value_source = match (generated.as_str(), identity.as_str()) {
                    ("s", _) => ValueSource::Generated,
                    (_, "a") | (_, "d") => ValueSource::Identity,
                    _ => ValueSource::Stored,
                };
                let col_id = Id(next_col);
                next_col += 1;
                attmap.insert(attnum, col_id);
                cols.push(Column {
                    id: col_id,
                    owner: rel_id,
                    name: col_name,
                    position: attnum as u16,
                    ty,
                    nullability: if not_null {
                        Nullability::NotNull
                    } else {
                        Nullability::Nullable
                    },
                    has_default: default_expr.is_some(),
                    default_expr,
                    value_source,
                });
            }
            attnum_to_col.insert(rel_oid, attmap);

            // Defer PK/UK/FK population to second pass after all attnum maps known,
            // but create empty relation now.
            relations_tmp.push(Relation {
                id: rel_id,
                schema,
                oid: rel_oid,
                name: rel_name,
                kind,
                columns: cols,
                primary_key: None,
                unique_keys: Vec::new(),
                foreign_keys_out: Vec::new(),
                foreign_keys_in: Vec::new(),
                comment,
            });
        }

        // 5) Apply PK / Unique constraints.
        for (rel_oid, entries) in pk_by_rel {
            let Some(&rel_id) = rel_by_oid.get(&rel_oid) else {
                continue;
            };
            let Some(attmap) = attnum_to_col.get(&rel_oid) else {
                continue;
            };
            // Find mutable relation
            let Some(rel) = relations_tmp.iter_mut().find(|r| r.id == rel_id) else {
                continue;
            };
            for (conname, contype, keys) in entries {
                let col_ids: Vec<Id> = keys.iter().filter_map(|k| attmap.get(k).copied()).collect();
                if col_ids.is_empty() || col_ids.len() != keys.len() {
                    continue; // skip incomplete mapping
                }
                match contype {
                    'p' => {
                        rel.primary_key = Some(PrimaryKey {
                            columns: col_ids,
                            name: Some(conname),
                        });
                    }
                    'u' => {
                        rel.unique_keys.push(UniqueKey {
                            columns: col_ids,
                            name: Some(conname),
                        });
                    }
                    _ => {}
                }
            }
        }

        // 6) Apply FK constraints — out + in.
        for (_oid, conname, conrelid, confrelid, conkey, confkey) in fks {
            let (Some(keys), Some(ref_keys)) = (conkey, confkey) else {
                continue;
            };
            let Some(&src_id) = rel_by_oid.get(&conrelid) else {
                continue;
            };
            let Some(&dst_id) = rel_by_oid.get(&confrelid) else {
                continue;
            };
            let Some(src_map) = attnum_to_col.get(&conrelid) else {
                continue;
            };
            let Some(dst_map) = attnum_to_col.get(&confrelid) else {
                continue;
            };
            let referencing: Vec<Id> = keys
                .iter()
                .filter_map(|k| src_map.get(k).copied())
                .collect();
            let referenced: Vec<Id> = ref_keys
                .iter()
                .filter_map(|k| dst_map.get(k).copied())
                .collect();
            if referencing.is_empty()
                || referenced.is_empty()
                || referencing.len() != keys.len()
                || referenced.len() != ref_keys.len()
            {
                continue;
            }
            let fk = ForeignKey {
                name: Some(conname.clone()),
                referencing: referencing.clone(),
                referenced_relation: dst_id,
                referenced: referenced.clone(),
            };
            // out side (child → parent)
            if let Some(rel) = relations_tmp.iter_mut().find(|r| r.id == src_id) {
                rel.foreign_keys_out.push(fk.clone());
            }
            // in side (parent perspective — swap direction so parent sees inbound)
            // ARCH CRITICAL: previously duplicated fk direction; now correctly:
            // referencing = parent cols (original referenced), referenced = child cols,
            // referenced_relation = child (src_id).
            let fk_in = ForeignKey {
                name: Some(conname),
                referencing: referenced,
                referenced_relation: src_id,
                referenced: referencing,
            };
            if let Some(rel) = relations_tmp.iter_mut().find(|r| r.id == dst_id) {
                rel.foreign_keys_in.push(fk_in);
            }
        }

        // 7) Add relations to builder in order (deterministic).
        for rel in relations_tmp {
            b.add_relation(rel);
        }

        // 8) Functions.
        for (idx, (_oid, ns_oid, name, signature, ret_type, prokind)) in
            functions.into_iter().enumerate()
        {
            let schema = schema_by_oid
                .get(&ns_oid)
                .copied()
                .unwrap_or(fallback_schema);
            let fid = Id(u32::try_from(idx).expect("function idx fits u32"));
            b.add_function(Function {
                id: fid,
                schema,
                name,
                signature,
                return_type: ret_type,
                is_procedure: prokind == 'p',
            });
        }

        Ok(b.build())
    }
}

#[cfg(test)]
mod tests {
    use super::hydrate;
    use super::queries;
    use pgnative_schema_model::types::{Oid, RelationKind};

    #[test]
    fn queries_are_qualified() {
        assert!(queries::SCHEMAS.contains("pg_catalog.pg_namespace"));
        assert!(queries::RELATIONS.contains("pg_catalog.pg_class"));
        assert!(queries::COLUMNS.contains("pg_catalog.pg_attribute"));
        assert!(queries::CONSTRAINTS_PK_UNIQUE.contains("pg_catalog.pg_constraint"));
        assert!(queries::CONSTRAINTS_FK.contains("pg_catalog.pg_constraint"));
        assert!(queries::FUNCTIONS.contains("pg_catalog.pg_proc"));
    }

    #[test]
    fn hydrate_phase_a_builds_model() {
        let schemas = vec![(Oid(2200), "public".into(), None)];
        let relations = vec![(Oid(1), Oid(2200), "users".into(), 'r', None)];
        let columns = vec![
            (
                Oid(1),
                1,
                "id".into(),
                Oid(23),
                "int4".into(),
                true,
                None,
                "".into(),
                "".into(),
            ),
            (
                Oid(1),
                2,
                "email".into(),
                Oid(1043),
                "varchar".into(),
                false,
                None,
                "".into(),
                "".into(),
            ),
        ];
        let m = hydrate::phase_a(schemas, relations, columns).unwrap();
        assert_eq!(m.schemas().len(), 1);
        assert_eq!(m.relations().len(), 1);
        let rel = &m.relations()[0];
        assert_eq!(rel.name, "users");
        assert_eq!(rel.kind, RelationKind::Table);
        assert_eq!(rel.columns.len(), 2);
        assert_eq!(rel.columns[0].name, "id");
        assert_eq!(m.type_name(rel.columns[0].ty), Some("int4"));
        assert!(m.schema_named("public").is_some());
        assert!(m.relation_by_oid(Oid(1)).is_some());
    }

    #[test]
    fn hydrate_with_pk_and_fk() {
        let schemas = vec![(Oid(2200), "public".into(), None)];
        let relations = vec![
            (Oid(1), Oid(2200), "users".into(), 'r', None),
            (Oid(2), Oid(2200), "posts".into(), 'r', None),
        ];
        let columns = vec![
            (
                Oid(1),
                1,
                "id".into(),
                Oid(23),
                "int4".into(),
                true,
                None,
                "".into(),
                "".into(),
            ),
            (
                Oid(2),
                1,
                "id".into(),
                Oid(23),
                "int4".into(),
                true,
                None,
                "".into(),
                "".into(),
            ),
            (
                Oid(2),
                2,
                "user_id".into(),
                Oid(23),
                "int4".into(),
                true,
                None,
                "".into(),
                "".into(),
            ),
        ];
        let pk = vec![(Oid(1), "users_pkey".into(), 'p', Some(vec![1]))];
        let fks = vec![(
            Oid(10),
            "posts_user_fk".into(),
            Oid(2),
            Oid(1),
            Some(vec![2]),
            Some(vec![1]),
        )];
        let m = hydrate::build_full(schemas, relations, columns, pk, fks, Vec::new()).unwrap();
        let users = m.relation_by_oid(Oid(1)).unwrap();
        assert!(users.primary_key.is_some());
        let posts = m.relation_by_oid(Oid(2)).unwrap();
        assert_eq!(posts.foreign_keys_out.len(), 1);
        assert_eq!(users.foreign_keys_in.len(), 1);
        // Verify FK direction is correctly swapped for inbound.
        let fk_in = &users.foreign_keys_in[0];
        // inbound referencing should be parent (users.id) and referenced should be child (posts.user_id)
        // and referenced_relation should point to child (posts)
        let posts_id = posts.id;
        assert_eq!(fk_in.referenced_relation, posts_id);
    }

    #[test]
    fn hydrate_generated_identity() {
        let schemas = vec![(Oid(2200), "public".into(), None)];
        let relations = vec![(Oid(1), Oid(2200), "t".into(), 'r', None)];
        let columns = vec![
            (
                Oid(1),
                1,
                "gen".into(),
                Oid(23),
                "int4".into(),
                false,
                None,
                "s".into(),
                "".into(),
            ),
            (
                Oid(1),
                2,
                "ident".into(),
                Oid(23),
                "int4".into(),
                false,
                None,
                "".into(),
                "a".into(),
            ),
        ];
        let m = hydrate::phase_a(schemas, relations, columns).unwrap();
        let rel = &m.relations()[0];
        assert_eq!(
            rel.columns[0].value_source,
            pgnative_schema_model::types::ValueSource::Generated
        );
        assert_eq!(
            rel.columns[1].value_source,
            pgnative_schema_model::types::ValueSource::Identity
        );
    }

    #[test]
    fn hydrate_empty_schemas_errors() {
        let schemas = vec![];
        let relations = vec![(Oid(1), Oid(2200), "users".into(), 'r', None)];
        let columns = vec![];
        let res = hydrate::phase_a(schemas, relations, columns);
        assert!(res.is_err());
        let res2 = hydrate::build_full(vec![], vec![], vec![], vec![], vec![], vec![]);
        assert!(res2.is_err());
    }
}
