//! SQL for the console editor: a small PostgreSQL lexer for colours, and a
//! catalog-aware completer (schemas, tables, aliases, columns, keywords,
//! functions). Pure functions over text and a [`Catalog`] snapshot.

use crate::editor_analysis::{Analysis, Completion, Token};
use monocode_core::db::{CatalogEntry, TableKind};
use std::collections::BTreeMap;

const KEYWORD: &str = "var(--syntax-keyword)";
const STRING: &str = "var(--syntax-string)";
const NUMBER: &str = "var(--syntax-number)";
const COMMENT: &str = "var(--syntax-comment)";
const CALLABLE: &str = "var(--syntax-callable)";
const PROPERTY: &str = "var(--syntax-property)";
const TYPE: &str = "var(--syntax-type)";

const KEYWORDS: &[&str] = &[
    "select",
    "from",
    "where",
    "and",
    "or",
    "not",
    "in",
    "is",
    "null",
    "like",
    "ilike",
    "between",
    "join",
    "inner",
    "left",
    "right",
    "full",
    "outer",
    "cross",
    "natural",
    "on",
    "using",
    "group",
    "by",
    "order",
    "having",
    "limit",
    "offset",
    "fetch",
    "union",
    "all",
    "intersect",
    "except",
    "distinct",
    "as",
    "case",
    "when",
    "then",
    "else",
    "end",
    "insert",
    "into",
    "values",
    "update",
    "set",
    "delete",
    "returning",
    "create",
    "alter",
    "drop",
    "table",
    "view",
    "index",
    "schema",
    "database",
    "sequence",
    "trigger",
    "function",
    "procedure",
    "materialized",
    "temporary",
    "temp",
    "unlogged",
    "if",
    "exists",
    "cascade",
    "restrict",
    "primary",
    "key",
    "foreign",
    "references",
    "unique",
    "check",
    "default",
    "constraint",
    "add",
    "column",
    "rename",
    "to",
    "with",
    "recursive",
    "over",
    "partition",
    "window",
    "asc",
    "desc",
    "nulls",
    "first",
    "last",
    "true",
    "false",
    "begin",
    "commit",
    "rollback",
    "savepoint",
    "transaction",
    "explain",
    "analyze",
    "verbose",
    "vacuum",
    "truncate",
    "grant",
    "revoke",
    "copy",
    "lateral",
    "only",
    "for",
    "share",
    "nowait",
    "skip",
    "locked",
    "conflict",
    "do",
    "nothing",
    "any",
    "some",
    "array",
    "interval",
    "cast",
    "collate",
    "escape",
    "similar",
    "isnull",
    "notnull",
    "show",
    "reset",
    "comment",
    "tablesample",
];

const FUNCTIONS: &[&str] = &[
    "count",
    "sum",
    "avg",
    "min",
    "max",
    "coalesce",
    "nullif",
    "greatest",
    "least",
    "now",
    "current_date",
    "current_timestamp",
    "date_trunc",
    "date_part",
    "extract",
    "age",
    "to_char",
    "to_date",
    "to_timestamp",
    "lower",
    "upper",
    "length",
    "substring",
    "substr",
    "trim",
    "ltrim",
    "rtrim",
    "replace",
    "split_part",
    "concat",
    "concat_ws",
    "left",
    "right",
    "position",
    "regexp_replace",
    "regexp_match",
    "regexp_matches",
    "string_agg",
    "array_agg",
    "json_agg",
    "jsonb_agg",
    "json_build_object",
    "jsonb_build_object",
    "jsonb_each",
    "jsonb_each_text",
    "jsonb_array_elements",
    "jsonb_array_elements_text",
    "jsonb_object_keys",
    "jsonb_set",
    "jsonb_pretty",
    "to_jsonb",
    "row_number",
    "rank",
    "dense_rank",
    "lag",
    "lead",
    "first_value",
    "last_value",
    "ntile",
    "generate_series",
    "unnest",
    "round",
    "ceil",
    "floor",
    "abs",
    "random",
    "md5",
    "gen_random_uuid",
    "pg_sleep",
    "pg_size_pretty",
    "pg_total_relation_size",
    "pg_relation_size",
    "pg_database_size",
    "pg_cancel_backend",
    "pg_terminate_backend",
    "current_setting",
    "set_config",
    "version",
    "current_user",
    "current_database",
    "current_schema",
    "exists",
    "cast",
    "bool_and",
    "bool_or",
];

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// Colours for `text`.
pub(super) fn analyse(text: &str) -> Analysis {
    let bytes = text.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    let push = |tokens: &mut Vec<Token>, from: usize, to: usize, kind: &'static str| {
        tokens.push(Token {
            from,
            to,
            kind: kind.into(),
        });
    };
    let mut after_cast = false;
    while i < bytes.len() {
        let rest = &text[i..];
        let c = rest.chars().next().unwrap_or(' ');
        if rest.starts_with("--") {
            let end = rest.find('\n').map_or(text.len(), |n| i + n);
            push(&mut tokens, i, end, COMMENT);
            i = end;
        } else if rest.starts_with("/*") {
            let mut depth = 0;
            let mut j = i;
            while j < bytes.len() {
                if text[j..].starts_with("/*") {
                    depth += 1;
                    j += 2;
                } else if text[j..].starts_with("*/") {
                    depth -= 1;
                    j += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    j += text[j..].chars().next().map_or(1, char::len_utf8);
                }
            }
            push(&mut tokens, i, j.min(text.len()), COMMENT);
            i = j.min(text.len());
        } else if c == '\'' || c == '"' {
            let escapes = c == '\'' && i > 0 && matches!(bytes[i - 1], b'e' | b'E');
            let mut j = i + 1;
            while j < bytes.len() {
                if escapes && bytes[j] == b'\\' {
                    j += 2;
                    continue;
                }
                if bytes[j] == c as u8 {
                    if bytes.get(j + 1) == Some(&(c as u8)) {
                        j += 2;
                        continue;
                    }
                    j += 1;
                    break;
                }
                j += 1;
            }
            let end = j.min(text.len());
            push(
                &mut tokens,
                i,
                end,
                if c == '\'' { STRING } else { PROPERTY },
            );
            i = end;
        } else if c == '$' && dollar_tag_end(text, i).is_some() {
            let open = dollar_tag_end(text, i).unwrap_or(i + 1);
            let tag = &text[i..open];
            let end = text[open..]
                .find(tag)
                .map_or(text.len(), |n| open + n + tag.len());
            push(&mut tokens, i, end, STRING);
            i = end;
        } else if c.is_ascii_digit() {
            let mut j = i;
            while j < bytes.len()
                && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'.' || bytes[j] == b'_')
            {
                j += 1;
            }
            push(&mut tokens, i, j, NUMBER);
            i = j;
            after_cast = false;
        } else if is_word(c) && c != '$' {
            let mut j = i;
            while j < text.len() {
                let ch = text[j..].chars().next().unwrap_or(' ');
                if !is_word(ch) {
                    break;
                }
                j += ch.len_utf8();
            }
            let word = &text[i..j];
            let lower = word.to_lowercase();
            let call = text[j..].trim_start_matches([' ', '\t']).starts_with('(');
            let kind = if after_cast {
                Some(TYPE)
            } else if KEYWORDS.contains(&lower.as_str())
                && !(call && FUNCTIONS.contains(&lower.as_str()))
            {
                Some(KEYWORD)
            } else if call {
                Some(CALLABLE)
            } else {
                None
            };
            if let Some(kind) = kind {
                push(&mut tokens, i, j, kind);
            }
            after_cast = false;
            i = j;
        } else {
            after_cast = text[i..].starts_with("::");
            i += if after_cast { 2 } else { c.len_utf8() };
        }
    }
    Analysis {
        tokens,
        ..Default::default()
    }
}

/// End of a `$tag$` opener at `at`, if there is one.
fn dollar_tag_end(text: &str, at: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    if at > 0 && (bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'_') {
        return None;
    }
    let mut j = at + 1;
    while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
        if j == at + 1 && bytes[j].is_ascii_digit() {
            return None;
        }
        j += 1;
    }
    (bytes.get(j) == Some(&b'$')).then_some(j + 1)
}

/// A relation's kind and its (column, type) pairs.
type Relation = (TableKind, Vec<(String, String)>);

/// The catalog a console completes from.
#[derive(Default, Clone)]
pub(crate) struct Catalog {
    /// schema → table → relation.
    pub schemas: BTreeMap<String, BTreeMap<String, Relation>>,
}

impl Catalog {
    pub(crate) fn from_entries(entries: Vec<CatalogEntry>) -> Self {
        let mut catalog = Self::default();
        for e in entries {
            catalog
                .schemas
                .entry(e.schema)
                .or_default()
                .entry(e.table)
                .or_insert((e.kind, Vec::new()))
                .1
                .push((e.column, e.type_name));
        }
        catalog
    }

    fn tables_named<'a>(
        &'a self,
        schema: Option<&str>,
        table: &str,
    ) -> Vec<(&'a str, &'a Vec<(String, String)>)> {
        self.schemas
            .iter()
            .filter(|(s, _)| schema.is_none_or(|want| want.eq_ignore_ascii_case(s)))
            .filter_map(|(s, tables)| {
                tables
                    .iter()
                    .find(|(t, _)| t.eq_ignore_ascii_case(table))
                    .map(|(_, (_, columns))| (s.as_str(), columns))
            })
            .collect()
    }
}

fn unquote(word: &str) -> String {
    word.trim_matches('"').to_owned()
}

/// `schema.table alias` pairs introduced by FROM / JOIN / UPDATE / INTO in `statement`.
fn references(statement: &str) -> Vec<(Option<String>, String, Option<String>)> {
    let words: Vec<&str> = statement
        .split(|c: char| c.is_whitespace() || matches!(c, ',' | '(' | ')' | ';'))
        .filter(|w| !w.is_empty())
        .collect();
    let mut found = Vec::new();
    for (i, word) in words.iter().enumerate() {
        if !matches!(
            word.to_lowercase().as_str(),
            "from" | "join" | "update" | "into"
        ) {
            continue;
        }
        let Some(target) = words.get(i + 1) else {
            continue;
        };
        if KEYWORDS.contains(&target.to_lowercase().as_str()) {
            continue;
        }
        let (schema, table) = match target.split_once('.') {
            Some((s, t)) => (Some(unquote(s)), unquote(t)),
            None => (None, unquote(target)),
        };
        let mut alias = words.get(i + 2).copied();
        if alias.is_some_and(|a| a.eq_ignore_ascii_case("as")) {
            alias = words.get(i + 3).copied();
        }
        let alias = alias
            .filter(|a| !KEYWORDS.contains(&a.to_lowercase().as_str()) && a.chars().all(is_word))
            .map(unquote);
        found.push((schema, table, alias));
    }
    found
}

fn item(
    label: &str,
    detail: &str,
    kind: &str,
    from: usize,
    to: usize,
    insert: String,
) -> Completion {
    Completion {
        label: label.to_owned(),
        detail: detail.to_owned(),
        kind: kind.to_owned(),
        from,
        to,
        cursor: insert.len(),
        insert,
    }
}

fn quote_if_needed(name: &str) -> String {
    let plain = name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && !KEYWORDS.contains(&name);
    if plain {
        name.to_owned()
    } else {
        format!("\"{}\"", name.replace('"', "\"\""))
    }
}

/// Completions at byte offset `at` of `text`.
pub(crate) fn complete(
    catalog: &Catalog,
    text: &str,
    at: usize,
    explicit: bool,
) -> Vec<Completion> {
    let at = at.min(text.len());
    let before = &text[..at];
    let start = before
        .char_indices()
        .rev()
        .take_while(|(_, c)| is_word(*c))
        .last()
        .map_or(at, |(i, _)| i);
    let prefix = &text[start..at];
    let lower = prefix.to_lowercase();
    // The statement being edited: from the previous `;` on.
    let statement_start = before.rfind(';').map_or(0, |n| n + 1);
    let statement_end = text[at..].find(';').map_or(text.len(), |n| at + n);
    let statement = &text[statement_start..statement_end];
    let matches = |name: &str| lower.is_empty() || name.to_lowercase().starts_with(&lower);

    let mut out: Vec<Completion> = Vec::new();
    // `qualifier.` → members.
    if text[..start].ends_with('.') {
        let head = &text[..start - 1];
        let qualifier_start = head
            .char_indices()
            .rev()
            .take_while(|(_, c)| is_word(*c) || *c == '"')
            .last()
            .map_or(head.len(), |(i, _)| i);
        let qualifier = unquote(&head[qualifier_start..]);
        if let Some(tables) = catalog
            .schemas
            .iter()
            .find(|(s, _)| s.eq_ignore_ascii_case(&qualifier))
            .map(|(_, t)| t)
        {
            for (name, (kind, _)) in tables.iter().filter(|(n, _)| matches(n)) {
                let detail = if *kind == TableKind::Table {
                    "table"
                } else {
                    "view"
                };
                out.push(item(
                    name,
                    detail,
                    "table",
                    start,
                    at,
                    quote_if_needed(name),
                ));
            }
        } else {
            let mut columns: Vec<(&String, &String)> = Vec::new();
            let refs = references(statement);
            let target = refs
                .iter()
                .find(|(_, table, alias)| {
                    alias
                        .as_deref()
                        .is_some_and(|a| a.eq_ignore_ascii_case(&qualifier))
                        || (alias.is_none() && table.eq_ignore_ascii_case(&qualifier))
                })
                .map(|(s, t, _)| (s.clone(), t.clone()))
                .unwrap_or((None, qualifier.clone()));
            for (_, cols) in catalog.tables_named(target.0.as_deref(), &target.1) {
                columns.extend(cols.iter().map(|(n, t)| (n, t)));
            }
            for (name, type_name) in columns.into_iter().filter(|(n, _)| matches(n)) {
                out.push(item(
                    name,
                    type_name,
                    "column",
                    start,
                    at,
                    quote_if_needed(name),
                ));
            }
        }
        out.dedup_by(|a, b| a.label == b.label);
        out.truncate(80);
        return out;
    }
    if prefix.is_empty() && !explicit {
        return out;
    }
    let previous = before[..start]
        .split_whitespace()
        .next_back()
        .map(str::to_lowercase)
        .unwrap_or_default();
    let wants_relation = matches!(
        previous.as_str(),
        "from" | "join" | "into" | "update" | "table" | "truncate" | "analyze"
    );
    if wants_relation || !prefix.is_empty() || explicit {
        for (schema, tables) in &catalog.schemas {
            if matches(schema) {
                out.push(item(
                    schema,
                    "schema",
                    "schema",
                    start,
                    at,
                    format!("{}.", quote_if_needed(schema)),
                ));
            }
            for (name, (kind, _)) in tables.iter().filter(|(n, _)| matches(n)) {
                let detail = format!(
                    "{schema} · {}",
                    if *kind == TableKind::Table {
                        "table"
                    } else {
                        "view"
                    }
                );
                let insert = if schema == "public" {
                    quote_if_needed(name)
                } else {
                    format!("{}.{}", quote_if_needed(schema), quote_if_needed(name))
                };
                out.push(item(name, &detail, "table", start, at, insert));
            }
        }
    }
    if !wants_relation {
        for (schema, table, alias) in references(statement) {
            let _ = alias;
            for (_, cols) in catalog.tables_named(schema.as_deref(), &table) {
                for (name, type_name) in cols.iter().filter(|(n, _)| matches(n)) {
                    out.push(item(
                        name,
                        type_name,
                        "column",
                        start,
                        at,
                        quote_if_needed(name),
                    ));
                }
            }
        }
        for word in KEYWORDS.iter().filter(|w| matches(w)) {
            out.push(item(word, "", "keyword", start, at, word.to_uppercase()));
        }
        for word in FUNCTIONS.iter().filter(|w| matches(w)) {
            out.push(item(
                word,
                "function",
                "function",
                start,
                at,
                format!("{word}()"),
            ));
            if let Some(last) = out.last_mut() {
                last.cursor = last.insert.len() - 1;
            }
        }
    }
    // Columns and relations first, then the rest; exact-case prefix before others.
    let rank = |c: &Completion| match c.kind.as_str() {
        "column" => 0,
        "table" | "schema" => 1,
        "function" => 2,
        _ => 3,
    };
    out.sort_by_key(|c| {
        (
            if wants_relation && c.kind == "keyword" {
                9
            } else {
                rank(c)
            },
            c.label.len(),
        )
    });
    let mut seen = std::collections::HashSet::new();
    out.retain(|c| seen.insert((c.kind.clone(), c.label.clone(), c.insert.clone())));
    out.truncate(80);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> Catalog {
        let entry = |schema: &str, table: &str, column: &str, ty: &str| CatalogEntry {
            schema: schema.into(),
            table: table.into(),
            kind: TableKind::Table,
            column: column.into(),
            type_name: ty.into(),
        };
        Catalog::from_entries(vec![
            entry("public", "orders", "id", "integer"),
            entry("public", "orders", "customer_id", "integer"),
            entry("public", "orders", "Total Amount", "numeric"),
            entry("public", "customers", "id", "integer"),
            entry("public", "customers", "name", "text"),
            entry("audit", "events", "payload", "jsonb"),
        ])
    }

    fn labels(items: &[Completion]) -> Vec<&str> {
        items.iter().map(|c| c.label.as_str()).collect()
    }

    fn at_end(sql: &str) -> Vec<Completion> {
        complete(&catalog(), sql, sql.len(), false)
    }

    #[test]
    fn colours_keywords_strings_numbers_comments_calls_and_casts() {
        let text =
            "select count(*), 'a;b', 42 from t -- done\nwhere x::int > 1 and \"Col\" = $$s$$";
        let tokens = analyse(text).tokens;
        let kind = |word: &str| {
            let from = text.find(word).unwrap();
            tokens
                .iter()
                .find(|t| t.from == from)
                .map(|t| t.kind.to_string())
                .unwrap_or_default()
        };
        assert_eq!(kind("select"), KEYWORD);
        assert_eq!(kind("count"), CALLABLE);
        assert_eq!(kind("'a;b'"), STRING);
        assert_eq!(kind("42"), NUMBER);
        assert_eq!(kind("-- done"), COMMENT);
        assert_eq!(kind("int"), TYPE);
        assert_eq!(kind("\"Col\""), PROPERTY);
        assert_eq!(kind("$$s$$"), STRING);
        assert_eq!(kind("x::"), "", "plain identifiers stay uncoloured");
        for t in &tokens {
            assert!(t.from < t.to && t.to <= text.len());
        }
    }

    #[test]
    fn unterminated_strings_and_comments_do_not_run_past_the_text() {
        for text in ["select 'open", "/* open", "select $$open", "select \"open"] {
            for t in analyse(text).tokens {
                assert!(t.to <= text.len(), "{text}");
            }
        }
    }

    #[test]
    fn relations_complete_after_from_and_join() {
        let items = at_end("select * from cu");
        assert_eq!(labels(&items), ["customers"]);
        let items = at_end("select * from ");
        assert!(
            items.is_empty(),
            "nothing is offered for an empty prefix unless asked"
        );
        let items = complete(&catalog(), "select * from ", 14, true);
        assert!(labels(&items).contains(&"orders") && labels(&items).contains(&"audit"));
    }

    #[test]
    fn schema_dot_lists_that_schemas_tables() {
        let items = at_end("select * from audit.");
        assert_eq!(labels(&items), ["events"]);
        let items = at_end("select * from audit.ev");
        assert_eq!(items[0].insert, "events");
        assert_eq!(items[0].from, "select * from audit.".len());
    }

    #[test]
    fn alias_dot_lists_the_columns_of_its_table() {
        let sql = "select o.cu from orders o join customers c on c.id = o.customer_id";
        let items = complete(&catalog(), sql, "select o.cu".len(), false);
        assert_eq!(labels(&items), ["customer_id"]);
        let sql = "select c. from orders o join customers c on true";
        let items = complete(&catalog(), sql, "select c.".len(), false);
        assert_eq!(labels(&items), ["id", "name"]);
        assert_eq!(items[1].detail, "text");
    }

    #[test]
    fn columns_of_referenced_tables_and_quoting() {
        let sql = "select tot from orders";
        let items = complete(&catalog(), sql, "select tot".len(), false);
        assert_eq!(items[0].label, "Total Amount");
        assert_eq!(items[0].insert, "\"Total Amount\"");
    }

    #[test]
    fn keywords_and_functions_complete_with_the_cursor_inside_the_parentheses() {
        let items = at_end("sel");
        assert!(items.iter().any(|c| c.insert == "SELECT"));
        let items = at_end("select coun");
        let count = items.iter().find(|c| c.label == "count").unwrap();
        assert_eq!(count.insert, "count()");
        assert_eq!(count.cursor, "count(".len());
    }

    #[test]
    fn only_the_statement_at_the_cursor_supplies_columns() {
        let sql = "select 1 from customers; select na from orders";
        let items = complete(&catalog(), sql, sql.len() - " from orders".len(), false);
        assert!(
            !labels(&items).contains(&"name"),
            "customers belongs to the other statement"
        );
    }
}
