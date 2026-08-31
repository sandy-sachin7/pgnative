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
///
/// Minimal hand-rolled scan for `(?i)\bFROM\s+(\w+)(?:\s+(?:AS\s+)?(\w+))?`
/// and `JOIN` similarly — no full SQL parser, best-effort per §14.
/// `cursor` is a byte offset clamped to `sql.len()`. Table and alias keys
/// are lower-cased. When a model is available, prefer
/// `extract_aliases_with_model` to resolve table names to `RelationId`.
#[must_use]
pub fn extract_aliases(sql: &str, cursor: usize) -> HashMap<String, RelationId> {
    let raw = extract_alias_map(sql, cursor);
    // Without a model we cannot resolve real RelationIds; fabricate stable
    // ids by hashing the table name so `alias → id` is at least deterministic
    // and distinct per table (caller with a model should use `_with_model`).
    let mut out = HashMap::new();
    for (alias, table) in raw {
        // simple fnv-1a hash to u32
        let mut hash: u32 = 2166136261;
        for b in table.as_bytes() {
            hash ^= u32::from(*b);
            hash = hash.wrapping_mul(16777619);
        }
        let id = pgnative_schema_model::types::Id(hash);
        out.insert(alias, id);
        // also insert table itself → id so `users.` works without alias
        out.entry(table).or_insert(id);
    }
    out
}

/// Best-effort alias → table name map (both lower-cased) from `sql[..cursor]`.
#[must_use]
pub fn extract_alias_map(sql: &str, cursor: usize) -> HashMap<String, String> {
    let end = cursor.min(sql.len());
    let slice = &sql[..end];
    let mut out = HashMap::new();
    // Scan for FROM / JOIN keywords case-insensitively.
    let bytes = slice.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        // Skip non-alpha
        if !bytes[i].is_ascii_alphabetic() {
            i += 1;
            continue;
        }
        // Check for FROM (4 chars) or JOIN (4 chars)
        let is_from = i + 4 <= len
            && slice[i..i + 4].eq_ignore_ascii_case("from")
            && is_word_boundary(slice, i, 4);
        let is_join = i + 4 <= len
            && slice[i..i + 4].eq_ignore_ascii_case("join")
            && is_word_boundary(slice, i, 4);
        if !is_from && !is_join {
            i += 1;
            continue;
        }
        i += 4;
        // Skip whitespace
        while i < len && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= len {
            break;
        }
        // Capture table name (allow schema-qualified a.b and quoted "a")
        let table = capture_ident(slice, &mut i);
        if table.is_empty() {
            continue;
        }
        // Table key is the unqualified lowercased name and also fully-qualified lowercased.
        // Extract short name after last dot.
        let short_table = table
            .rsplit('.')
            .next()
            .unwrap_or(&table)
            .trim_matches('"')
            .to_ascii_lowercase();
        let full_table = table.trim_matches('"').to_ascii_lowercase();

        // Skip whitespace
        let mut j = i;
        while j < len && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if j >= len {
            // No alias — map table → itself
            out.entry(short_table.clone())
                .or_insert(short_table.clone());
            if full_table != short_table {
                out.entry(full_table.clone()).or_insert(short_table.clone());
            }
            i = j;
            continue;
        }
        // Peek next word
        let alias_candidate = peek_word(slice, j);
        if alias_candidate.is_empty() {
            out.entry(short_table.clone())
                .or_insert(short_table.clone());
            i = j;
            continue;
        }
        // If next word is AS, skip it
        if alias_candidate.eq_ignore_ascii_case("as") {
            j += alias_candidate.len();
            while j < len && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            let second = peek_word(slice, j);
            if !second.is_empty() && !is_reserved(second.as_str()) {
                let alias = second.to_ascii_lowercase();
                out.insert(alias, short_table.clone());
                out.entry(short_table.clone())
                    .or_insert(short_table.clone());
                if full_table != short_table {
                    out.entry(full_table).or_insert(short_table.clone());
                }
                j += second.len();
                i = j;
                continue;
            }
            // AS without alias
            out.entry(short_table.clone()).or_insert(short_table);
            i = j;
            continue;
        }
        if is_reserved(alias_candidate.as_str()) {
            // No alias, next keyword (WHERE, ON, etc.)
            out.entry(short_table.clone())
                .or_insert(short_table.clone());
            if full_table != short_table {
                out.entry(full_table).or_insert(short_table);
            }
            i = j;
            continue;
        }
        // Treat as alias
        let alias = alias_candidate.to_ascii_lowercase();
        out.insert(alias, short_table.clone());
        out.entry(short_table.clone())
            .or_insert(short_table.clone());
        if full_table != short_table {
            out.entry(full_table).or_insert(short_table.clone());
        }
        i = j + alias_candidate.len();
    }
    out
}

/// Resolve alias/table names to `RelationId` via `model`.
#[must_use]
pub fn extract_aliases_with_model(
    sql: &str,
    cursor: usize,
    model: &SchemaModel,
) -> HashMap<String, RelationId> {
    let map = extract_alias_map(sql, cursor);
    // Build lookup table name → RelationId (lowercase)
    let mut table_to_id: HashMap<String, RelationId> = HashMap::new();
    for rel in model.relations() {
        table_to_id
            .entry(rel.name.to_ascii_lowercase())
            .or_insert(rel.id);
    }
    let mut out = HashMap::new();
    for (alias, table) in map {
        if let Some(id) = table_to_id.get(&table) {
            out.insert(alias.clone(), *id);
            // ensure table itself maps
            out.entry(table.clone()).or_insert(*id);
        } else {
            // fallback: hash
            let mut hash: u32 = 2166136261;
            for b in table.as_bytes() {
                hash ^= u32::from(*b);
                hash = hash.wrapping_mul(16777619);
            }
            let id = pgnative_schema_model::types::Id(hash);
            out.insert(alias, id);
        }
    }
    out
}

fn is_word_boundary(s: &str, start: usize, kw_len: usize) -> bool {
    let bytes = s.as_bytes();
    let before_ok = start == 0 || !is_ident_char(bytes[start - 1] as char);
    let after_ok = start + kw_len >= bytes.len() || !is_ident_char(bytes[start + kw_len] as char);
    before_ok && after_ok
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '"'
}

fn capture_ident(s: &str, pos: &mut usize) -> String {
    let bytes = s.as_bytes();
    let len = bytes.len();
    let start = *pos;
    // Handle quoted identifier
    if *pos < len && bytes[*pos] == b'"' {
        *pos += 1;
        while *pos < len && bytes[*pos] != b'"' {
            // handle escaped double quote
            if bytes[*pos] == b'"' && *pos + 1 < len && bytes[*pos + 1] == b'"' {
                *pos += 2;
            } else {
                *pos += 1;
            }
        }
        if *pos < len && bytes[*pos] == b'"' {
            *pos += 1;
        }
        // May have schema-qualified dot after quoted?
        // Check for .<ident>
        if *pos < len && bytes[*pos] == b'.' {
            // consume dot and next ident
            *pos += 1;
            let second = capture_ident(s, pos);
            // combine
            let first = &s[start..*pos - second.len() - 1];
            return format!("{first}.{second}");
        }
        return s[start..*pos].to_string();
    }
    // Unquoted: read [A-Za-z0-9_]+ possibly with dots for schema.table
    let mut end = start;
    while end < len
        && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_' || bytes[end] == b'.')
    {
        // dot handling: ensure dot not trailing
        end += 1;
    }
    // Trim trailing dot if any
    while end > start && bytes[end - 1] == b'.' {
        end -= 1;
    }
    *pos = end;
    s[start..end].to_string()
}

fn peek_word(s: &str, pos: usize) -> String {
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut end = pos;
    while end < len
        && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_' || bytes[end] == b'"')
    {
        if bytes[end] == b'"' {
            // quoted word
            end += 1;
            while end < len && bytes[end] != b'"' {
                end += 1;
            }
            if end < len {
                end += 1;
            }
            break;
        }
        end += 1;
    }
    s[pos..end].to_string()
}

fn is_reserved(w: &str) -> bool {
    matches!(
        w.to_ascii_lowercase().as_str(),
        "where"
            | "join"
            | "left"
            | "right"
            | "inner"
            | "outer"
            | "full"
            | "cross"
            | "on"
            | "group"
            | "having"
            | "order"
            | "limit"
            | "offset"
            | "union"
            | "select"
            | "from"
            | "fetch"
            | "window"
            | "values"
            | "set"
            | "insert"
            | "update"
            | "delete"
            | "returning"
            | "with"
            | "as"
            | "and"
            | "or"
            | "not"
            | "in"
            | "is"
            | "null"
            | "like"
            | "ilike"
            | "between"
            | "exists"
            | "case"
            | "when"
            | "then"
            | "else"
            | "end"
            | "by"
            | "asc"
            | "desc"
    )
}

/// Hand-curated ~200 common PostgreSQL functions — Product/SQL UX owned, engineering implements.
/// Reviewed against daily SQL: aggregates, string, math, date/time, JSON, arrays, window, system.
const COMMON_PG_FUNCTIONS: &[&str] = &[
    // Aggregates & window
    "count",
    "sum",
    "avg",
    "min",
    "max",
    "array_agg",
    "array_agg_distinct",
    "string_agg",
    "json_agg",
    "jsonb_agg",
    "json_object_agg",
    "bool_and",
    "bool_or",
    "every",
    "stddev",
    "variance",
    "corr",
    "covar_pop",
    "regr_avgx",
    "regr_avgy",
    "percentile_cont",
    "percentile_disc",
    "mode",
    "rank",
    "dense_rank",
    "row_number",
    "lag",
    "lead",
    "first_value",
    "last_value",
    "nth_value",
    "ntile",
    // String
    "lower",
    "upper",
    "initcap",
    "length",
    "char_length",
    "octet_length",
    "trim",
    "ltrim",
    "rtrim",
    "btrim",
    "substr",
    "substring",
    "left",
    "right",
    "replace",
    "translate",
    "split_part",
    "strpos",
    "position",
    "overlay",
    "repeat",
    "reverse",
    "ascii",
    "chr",
    "concat",
    "concat_ws",
    "format",
    "quote_ident",
    "quote_literal",
    "regexp_match",
    "regexp_matches",
    "regexp_replace",
    "regexp_split_to_array",
    "regexp_split_to_table",
    "starts_with",
    "ends_with",
    "contains",
    // Math & numeric
    "abs",
    "ceil",
    "ceiling",
    "floor",
    "round",
    "trunc",
    "power",
    "sqrt",
    "cbrt",
    "exp",
    "ln",
    "log",
    "mod",
    "div",
    "greatest",
    "least",
    "random",
    "setseed",
    "width_bucket",
    "sign",
    "pi",
    "degrees",
    "radians",
    // Date/time
    "now",
    "current_timestamp",
    "current_date",
    "current_time",
    "localtimestamp",
    "localtime",
    "clock_timestamp",
    "statement_timestamp",
    "transaction_timestamp",
    "timeofday",
    "age",
    "date_part",
    "date_trunc",
    "extract",
    "isodate",
    "justify_days",
    "justify_hours",
    "justify_interval",
    "make_date",
    "make_time",
    "make_timestamp",
    "make_timestamptz",
    "make_interval",
    "timezone",
    "to_date",
    "to_timestamp",
    "to_char",
    "to_number",
    "interval",
    // JSON / JSONB
    "to_json",
    "to_jsonb",
    "row_to_json",
    "array_to_json",
    "json_build_object",
    "jsonb_build_object",
    "json_build_array",
    "jsonb_build_array",
    "json_object",
    "jsonb_object",
    "json_extract_path",
    "jsonb_extract_path",
    "json_extract_path_text",
    "jsonb_extract_path_text",
    "jsonb_pretty",
    "jsonb_set",
    "jsonb_insert",
    "jsonb_strip_nulls",
    "jsonb_typeof",
    "jsonb_array_length",
    "jsonb_object_keys",
    // Arrays & ranges
    "unnest",
    "array_append",
    "array_prepend",
    "array_cat",
    "array_length",
    "array_dims",
    "array_ndims",
    "array_fill",
    "array_position",
    "array_positions",
    "array_remove",
    "array_replace",
    "array_to_string",
    "string_to_array",
    "range_merge",
    "multirange",
    // UUID & system
    "gen_random_uuid",
    "uuid_generate_v4",
    "pg_typeof",
    "pg_column_size",
    "pg_table_size",
    "pg_total_relation_size",
    "current_database",
    "current_schema",
    "current_user",
    "session_user",
    "user",
    "version",
    "pg_backend_pid",
    "coalesce",
    "nullif",
    "greatest",
    "least",
    "nulls_first",
    "nulls_last",
    // Conditional & misc
    "case",
    "coalesce",
    "nullif",
    "greatest",
    "least",
    "decode",
    "encode",
    "pg_sleep",
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
