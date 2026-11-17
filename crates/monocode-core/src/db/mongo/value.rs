//! BSON as the grid shows it: documents become rows, top-level fields become
//! columns, and each value becomes text the way the shell would write it.
//! Nested documents and arrays are relaxed Extended JSON, so the cell preview
//! can pretty-print them.

use crate::db::conn::{QueryResult, ResultColumn};
use crate::db::introspect::ColumnInfo;
use mongodb::bson::{spec::BinarySubtype, Bson, Document};

/// A value as cell text; `None` is null (or a missing field).
pub fn cell(value: &Bson) -> Option<String> {
    Some(match value {
        Bson::Null | Bson::Undefined => return None,
        Bson::String(s) => s.clone(),
        Bson::Int32(n) => n.to_string(),
        Bson::Int64(n) => n.to_string(),
        Bson::Double(n) => double(*n),
        Bson::Decimal128(d) => d.to_string(),
        Bson::Boolean(b) => b.to_string(),
        Bson::ObjectId(id) => format!("ObjectId(\"{id}\")"),
        Bson::DateTime(at) => at
            .try_to_rfc3339_string()
            .unwrap_or_else(|_| format!("Date({})", at.timestamp_millis())),
        Bson::Timestamp(ts) => format!("Timestamp({{ t: {}, i: {} }})", ts.time, ts.increment),
        Bson::RegularExpression(re) => format!("/{}/{}", re.pattern, re.options),
        Bson::Binary(bin) if bin.subtype == BinarySubtype::Uuid && bin.bytes.len() == 16 => {
            let h: String = bin.bytes.iter().map(|b| format!("{b:02x}")).collect();
            format!(
                "UUID(\"{}-{}-{}-{}-{}\")",
                &h[..8],
                &h[8..12],
                &h[12..16],
                &h[16..20],
                &h[20..]
            )
        }
        Bson::Binary(bin) => {
            use base64::Engine;
            format!(
                "BinData({}, \"{}\")",
                u8::from(bin.subtype),
                base64::engine::general_purpose::STANDARD.encode(&bin.bytes)
            )
        }
        Bson::JavaScriptCode(code) => code.clone(),
        Bson::JavaScriptCodeWithScope(code) => code.code.clone(),
        Bson::Symbol(s) => s.clone(),
        Bson::MinKey => "MinKey()".into(),
        Bson::MaxKey => "MaxKey()".into(),
        Bson::Document(_) | Bson::Array(_) | Bson::DbPointer(_) => json(value),
    })
}

/// A double as the shell writes it: integral values without a fraction.
fn double(n: f64) -> String {
    if n.is_nan() {
        "NaN".into()
    } else if n.is_infinite() {
        if n > 0. { "Infinity" } else { "-Infinity" }.into()
    } else {
        n.to_string()
    }
}

/// Relaxed Extended JSON, compact.
pub fn json(value: &Bson) -> String {
    value.clone().into_relaxed_extjson().to_string()
}

/// Relaxed Extended JSON, indented.
pub fn pretty(value: &Bson) -> String {
    serde_json::to_string_pretty(&value.clone().into_relaxed_extjson()).unwrap_or_default()
}

/// The columns of `docs`: every top-level field in the order first seen,
/// `_id` first.
pub fn columns(docs: &[Document]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    if docs.iter().any(|d| d.contains_key("_id")) {
        out.push("_id".into());
        seen.insert("_id".to_owned());
    }
    for doc in docs {
        for key in doc.keys() {
            if seen.insert(key.clone()) {
                out.push(key.clone());
            }
        }
    }
    out
}

/// One row: each column's cell, `None` where the document lacks it.
pub fn row(doc: &Document, columns: &[String]) -> Vec<Option<String>> {
    columns.iter().map(|c| doc.get(c).and_then(cell)).collect()
}

/// A result showing `docs`, one row each.
pub fn table(statement: &str, docs: &[Document], has_more: bool) -> QueryResult {
    let mut names = columns(docs);
    if names.is_empty() {
        // An empty result still reads as "0 rows", not as a status.
        names.push("_id".into());
    }
    QueryResult {
        statement: statement.to_owned(),
        columns: names
            .iter()
            .map(|name| ResultColumn { name: name.clone() })
            .collect(),
        rows: docs.iter().map(|d| row(d, &names)).collect(),
        affected: Some(docs.len() as u64),
        has_more,
        execution_ms: 0.,
        elapsed_ms: 0.,
    }
}

/// A result of one column, `name`, a row per value.
pub fn list(statement: &str, name: &str, values: &[Bson]) -> QueryResult {
    QueryResult {
        statement: statement.to_owned(),
        columns: vec![ResultColumn { name: name.into() }],
        rows: values.iter().map(|v| vec![cell(v)]).collect(),
        affected: Some(values.len() as u64),
        has_more: false,
        execution_ms: 0.,
        elapsed_ms: 0.,
    }
}

/// The value's BSON type name, as `$type` names it.
pub fn type_name(value: &Bson) -> &'static str {
    match value {
        Bson::Double(_) => "double",
        Bson::String(_) => "string",
        Bson::Document(_) => "object",
        Bson::Array(_) => "array",
        Bson::Binary(b) if b.subtype == BinarySubtype::Uuid => "uuid",
        Bson::Binary(_) => "binData",
        Bson::Undefined => "undefined",
        Bson::ObjectId(_) => "objectId",
        Bson::Boolean(_) => "bool",
        Bson::DateTime(_) => "date",
        Bson::Null => "null",
        Bson::RegularExpression(_) => "regex",
        Bson::DbPointer(_) => "dbPointer",
        Bson::JavaScriptCode(_) => "javascript",
        Bson::Symbol(_) => "symbol",
        Bson::JavaScriptCodeWithScope(_) => "javascriptWithScope",
        Bson::Int32(_) => "int",
        Bson::Timestamp(_) => "timestamp",
        Bson::Int64(_) => "long",
        Bson::Decimal128(_) => "decimal",
        Bson::MaxKey => "maxKey",
        Bson::MinKey => "minKey",
    }
}

/// How deep nested documents are described (`a.b.c`).
const FIELD_DEPTH: usize = 3;
/// At most this many fields are described.
const FIELD_LIMIT: usize = 500;

#[derive(Default)]
struct Seen {
    order: Vec<String>,
    stats: std::collections::HashMap<String, (Vec<&'static str>, usize, bool)>,
}

impl Seen {
    fn note(&mut self, path: String, value: &Bson) {
        if !self.stats.contains_key(&path) {
            if self.order.len() >= FIELD_LIMIT {
                return;
            }
            self.order.push(path.clone());
        }
        let entry = self.stats.entry(path).or_default();
        let name = match value {
            Bson::Array(items) => {
                let mut inner: Vec<&str> = items.iter().map(type_name).collect();
                inner.sort_unstable();
                inner.dedup();
                match inner.as_slice() {
                    [] => "array",
                    [one] => array_of(one),
                    _ => "array<mixed>",
                }
            }
            other => type_name(other),
        };
        if !entry.0.contains(&name) {
            entry.0.push(name);
        }
        entry.1 += 1;
        entry.2 |= matches!(value, Bson::Null | Bson::Undefined);
    }

    fn walk(&mut self, prefix: &str, doc: &Document, depth: usize) {
        for (key, value) in doc {
            let path = if prefix.is_empty() {
                key.clone()
            } else {
                format!("{prefix}.{key}")
            };
            self.note(path.clone(), value);
            if depth + 1 >= FIELD_DEPTH {
                continue;
            }
            match value {
                Bson::Document(inner) => self.walk(&path, inner, depth + 1),
                Bson::Array(items) => {
                    for item in items.iter().take(20) {
                        if let Bson::Document(inner) = item {
                            self.walk(&path, inner, depth + 1);
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

fn array_of(inner: &str) -> &'static str {
    match inner {
        "string" => "array<string>",
        "int" => "array<int>",
        "long" => "array<long>",
        "double" => "array<double>",
        "decimal" => "array<decimal>",
        "object" => "array<object>",
        "objectId" => "array<objectId>",
        "date" => "array<date>",
        "bool" => "array<bool>",
        "array" => "array<array>",
        _ => "array<mixed>",
    }
}

/// The fields a sample of documents holds, nested ones as dotted paths: each
/// with the types seen. A field missing from some documents, or null in
/// some, is nullable.
pub fn infer_fields(docs: &[Document]) -> Vec<ColumnInfo> {
    let mut seen = Seen::default();
    for doc in docs {
        seen.walk("", doc, 0);
    }
    let total = docs.len();
    let mut out: Vec<ColumnInfo> = seen
        .order
        .iter()
        .map(|path| {
            let (types, count, has_null) = &seen.stats[path];
            // A field nested under an array counts once per element.
            let top = !path.contains('.');
            ColumnInfo {
                name: path.clone(),
                type_name: types.join(" | "),
                nullable: *has_null || (top && *count < total),
                default: None,
                primary_key: path == "_id",
            }
        })
        .collect();
    if let Some(at) = out.iter().position(|c| c.name == "_id") {
        let id = out.remove(at);
        out.insert(0, id);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use mongodb::bson::{doc, oid::ObjectId, DateTime};

    #[test]
    fn values_read_like_the_shell() {
        let id = ObjectId::parse_str("65a1b2c3d4e5f60718293a4b").unwrap();
        assert_eq!(
            cell(&Bson::ObjectId(id)).unwrap(),
            "ObjectId(\"65a1b2c3d4e5f60718293a4b\")"
        );
        assert_eq!(cell(&Bson::Null), None);
        assert_eq!(cell(&Bson::Double(2.0)).unwrap(), "2");
        assert_eq!(cell(&Bson::Double(2.5)).unwrap(), "2.5");
        assert_eq!(cell(&Bson::Int64(7)).unwrap(), "7");
        assert_eq!(
            cell(&Bson::DateTime(DateTime::from_millis(0))).unwrap(),
            "1970-01-01T00:00:00Z"
        );
        let nested = cell(&Bson::Document(doc! { "a": [1, { "b": id }] })).unwrap();
        assert_eq!(
            nested,
            r#"{"a":[1,{"b":{"$oid":"65a1b2c3d4e5f60718293a4b"}}]}"#
        );
    }

    #[test]
    fn documents_become_rows_with_id_first_and_gaps_null() {
        let docs = vec![doc! { "name": "a", "_id": 1 }, doc! { "_id": 2, "age": 3 }];
        let result = table("q", &docs, false);
        let names: Vec<_> = result.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["_id", "name", "age"]);
        assert_eq!(result.rows[0], [Some("1".into()), Some("a".into()), None]);
        assert_eq!(result.rows[1], [Some("2".into()), None, Some("3".into())]);
        assert_eq!(
            table("q", &[], false).columns.len(),
            1,
            "empty still has a column"
        );
    }

    #[test]
    fn fields_are_inferred_with_types_and_nesting() {
        let docs = vec![
            doc! { "_id": 1, "name": "a", "address": { "city": "x" }, "tags": ["t"] },
            doc! { "_id": 2, "name": Bson::Null, "address": { "city": "y", "zip": 1 }, "age": 3.5 },
        ];
        let fields = infer_fields(&docs);
        let find = |n: &str| fields.iter().find(|f| f.name == n).unwrap();
        assert!(find("_id").primary_key && !find("_id").nullable);
        assert_eq!(find("name").type_name, "string | null");
        assert!(find("name").nullable);
        assert_eq!(find("address.city").type_name, "string");
        assert_eq!(find("tags").type_name, "array<string>");
        assert!(find("age").nullable, "missing in one document");
        assert_eq!(fields[0].name, "_id");
    }
}
