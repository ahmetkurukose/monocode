//! The MongoDB console editor: colours for its JavaScript, completion that
//! knows the shell (`db.`, collection and cursor methods, operators) and the
//! catalog (databases, collections, fields), and the statements the view
//! writes for a collection (open, page, sort).

use super::sql::Catalog;
use crate::editor_analysis::{Analysis, Completion, Token};
use monocode_core::db::TableKind;
use monocode_core::db::mongo::js_string;

const KEYWORD: &str = "var(--syntax-keyword)";
const STRING: &str = "var(--syntax-string)";
const NUMBER: &str = "var(--syntax-number)";
const COMMENT: &str = "var(--syntax-comment)";
const CALLABLE: &str = "var(--syntax-callable)";
const PROPERTY: &str = "var(--syntax-property)";
const TYPE: &str = "var(--syntax-type)";

const KEYWORDS: &[&str] = &[
    "const",
    "let",
    "var",
    "function",
    "return",
    "if",
    "else",
    "for",
    "of",
    "in",
    "new",
    "true",
    "false",
    "null",
    "undefined",
    "typeof",
    "this",
    "while",
    "do",
    "break",
    "continue",
    "throw",
    "try",
    "catch",
    "finally",
    "use",
    "show",
];

/// The shell's BSON helpers and globals.
const HELPERS: &[(&str, &str)] = &[
    ("ObjectId", "ObjectId(\"…\")"),
    ("ISODate", "ISODate(\"…\")"),
    ("NumberLong", "64-bit integer"),
    ("NumberInt", "32-bit integer"),
    ("NumberDecimal", "128-bit decimal"),
    ("Double", "double"),
    ("Timestamp", "Timestamp(t, i)"),
    ("UUID", "UUID(\"…\")"),
    ("BinData", "BinData(subtype, base64)"),
    ("MinKey", "MinKey()"),
    ("MaxKey", "MaxKey()"),
    ("Date", "JavaScript date"),
    ("print", "prints to the result"),
    ("printjson", "prints indented JSON"),
];

const DATABASE_METHODS: &[(&str, &str)] = &[
    ("getCollection", "(name)"),
    ("getSiblingDB", "(name)"),
    ("getCollectionNames", "()"),
    ("getCollectionInfos", "(filter)"),
    ("createCollection", "(name, options)"),
    ("createView", "(name, source, pipeline)"),
    ("runCommand", "(command)"),
    ("adminCommand", "(command)"),
    ("aggregate", "(pipeline)"),
    ("stats", "()"),
    ("serverStatus", "()"),
    ("version", "()"),
    ("hostInfo", "()"),
    ("currentOp", "(filter)"),
    ("killOp", "(opid)"),
    ("getUsers", "()"),
    ("getRoles", "()"),
    ("getProfilingStatus", "()"),
    ("setProfilingLevel", "(level)"),
    ("dropDatabase", "()"),
    ("getName", "()"),
];

const COLLECTION_METHODS: &[(&str, &str)] = &[
    ("find", "(filter, projection)"),
    ("findOne", "(filter, projection)"),
    ("aggregate", "(pipeline)"),
    ("countDocuments", "(filter)"),
    ("estimatedDocumentCount", "()"),
    ("distinct", "(field, filter)"),
    ("insertOne", "(document)"),
    ("insertMany", "(documents)"),
    ("updateOne", "(filter, update, options)"),
    ("updateMany", "(filter, update, options)"),
    ("replaceOne", "(filter, replacement)"),
    ("deleteOne", "(filter)"),
    ("deleteMany", "(filter)"),
    ("findOneAndUpdate", "(filter, update, options)"),
    ("findOneAndReplace", "(filter, replacement, options)"),
    ("findOneAndDelete", "(filter, options)"),
    ("bulkWrite", "(operations)"),
    ("createIndex", "(keys, options)"),
    ("createIndexes", "(keys[], options)"),
    ("getIndexes", "()"),
    ("dropIndex", "(name)"),
    ("dropIndexes", "()"),
    ("stats", "()"),
    ("validate", "(full)"),
    ("renameCollection", "(name)"),
    ("drop", "()"),
    ("explain", "(verbosity)"),
];

const CURSOR_METHODS: &[(&str, &str)] = &[
    ("sort", "({ field: 1 })"),
    ("limit", "(n)"),
    ("skip", "(n)"),
    ("projection", "({ field: 1 })"),
    ("hint", "(index)"),
    ("collation", "(collation)"),
    ("maxTimeMS", "(ms)"),
    ("batchSize", "(n)"),
    ("comment", "(text)"),
    ("allowDiskUse", "()"),
    ("count", "()"),
    ("explain", "(verbosity)"),
    ("toArray", "()"),
    ("pretty", "()"),
];

const OPERATORS: &[(&str, &str)] = &[
    // query
    ("$eq", "query"),
    ("$ne", "query"),
    ("$gt", "query"),
    ("$gte", "query"),
    ("$lt", "query"),
    ("$lte", "query"),
    ("$in", "query"),
    ("$nin", "query"),
    ("$and", "query"),
    ("$or", "query"),
    ("$nor", "query"),
    ("$not", "query"),
    ("$exists", "query"),
    ("$type", "query"),
    ("$regex", "query"),
    ("$options", "query"),
    ("$expr", "query"),
    ("$elemMatch", "query"),
    ("$size", "query"),
    ("$all", "query"),
    ("$text", "query"),
    ("$search", "query"),
    ("$mod", "query"),
    ("$jsonSchema", "query"),
    // update
    ("$set", "update"),
    ("$unset", "update"),
    ("$inc", "update"),
    ("$mul", "update"),
    ("$min", "update"),
    ("$max", "update"),
    ("$rename", "update"),
    ("$push", "update"),
    ("$pull", "update"),
    ("$pullAll", "update"),
    ("$addToSet", "update"),
    ("$pop", "update"),
    ("$each", "update"),
    ("$currentDate", "update"),
    ("$setOnInsert", "update"),
    // stages
    ("$match", "stage"),
    ("$group", "stage"),
    ("$project", "stage"),
    ("$sort", "stage"),
    ("$limit", "stage"),
    ("$skip", "stage"),
    ("$unwind", "stage"),
    ("$lookup", "stage"),
    ("$addFields", "stage"),
    ("$count", "stage"),
    ("$facet", "stage"),
    ("$bucket", "stage"),
    ("$replaceRoot", "stage"),
    ("$sample", "stage"),
    ("$out", "stage"),
    ("$merge", "stage"),
    ("$unionWith", "stage"),
    ("$sortByCount", "stage"),
    ("$graphLookup", "stage"),
    // expressions and accumulators
    ("$sum", "accumulator"),
    ("$avg", "accumulator"),
    ("$first", "accumulator"),
    ("$last", "accumulator"),
    ("$push", "accumulator"),
    ("$cond", "expression"),
    ("$ifNull", "expression"),
    ("$concat", "expression"),
    ("$toString", "expression"),
    ("$dateToString", "expression"),
    ("$arrayElemAt", "expression"),
    ("$filter", "expression"),
    ("$map", "expression"),
];

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// Colours for `text`.
pub(super) fn analyse(text: &str) -> Analysis {
    let bytes = text.as_bytes();
    let mut tokens = Vec::new();
    let mut push = |from: usize, to: usize, kind: &'static str| {
        tokens.push(Token {
            from,
            to,
            kind: kind.into(),
        })
    };
    let mut i = 0;
    let mut previous: Option<char> = None;
    while i < bytes.len() {
        let rest = &text[i..];
        let c = rest.chars().next().unwrap_or(' ');
        if rest.starts_with("//") {
            let end = rest.find('\n').map_or(text.len(), |n| i + n);
            push(i, end, COMMENT);
            i = end;
            continue;
        }
        if let Some(body) = rest.strip_prefix("/*") {
            let end = body.find("*/").map_or(text.len(), |n| i + 2 + n + 2);
            push(i, end, COMMENT);
            i = end;
            continue;
        }
        if matches!(c, '\'' | '"' | '`') {
            let mut j = i + 1;
            while j < bytes.len() {
                if bytes[j] == b'\\' {
                    j += 2;
                    continue;
                }
                if bytes[j] == c as u8 || (bytes[j] == b'\n' && c != '`') {
                    j += 1;
                    break;
                }
                j += 1;
            }
            let end = j.min(text.len());
            // A quoted key (`"name":`) reads as a property.
            let key = text[end..].trim_start_matches([' ', '\t']).starts_with(':');
            push(i, end, if key { PROPERTY } else { STRING });
            previous = Some(c);
            i = end;
            continue;
        }
        if c == '/'
            && previous
                .is_none_or(|p| !(p.is_alphanumeric() || matches!(p, ')' | ']' | '}' | '_' | '$')))
        {
            // A regular expression literal.
            let mut j = i + 1;
            let mut class = false;
            while j < bytes.len() && bytes[j] != b'\n' {
                match bytes[j] {
                    b'\\' => j += 1,
                    b'[' => class = true,
                    b']' => class = false,
                    b'/' if !class => break,
                    _ => {}
                }
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'/' {
                j += 1;
                while j < bytes.len() && bytes[j].is_ascii_alphabetic() {
                    j += 1;
                }
                push(i, j, STRING);
                previous = Some('/');
                i = j;
                continue;
            }
        }
        if c.is_ascii_digit() {
            let mut j = i;
            while j < bytes.len()
                && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'.' || bytes[j] == b'_')
            {
                j += 1;
            }
            push(i, j, NUMBER);
            previous = Some('0');
            i = j;
            continue;
        }
        if is_word(c) {
            let mut j = i;
            while j < text.len() {
                let ch = text[j..].chars().next().unwrap_or(' ');
                if !is_word(ch) {
                    break;
                }
                j += ch.len_utf8();
            }
            let word = &text[i..j];
            let after = text[j..].trim_start_matches([' ', '\t']);
            let member = text[..i].trim_end_matches([' ', '\t']).ends_with('.');
            let kind = if word.starts_with('$') {
                Some(KEYWORD)
            } else if after.starts_with(':') && !after.starts_with("::") {
                Some(PROPERTY)
            } else if after.starts_with('(') {
                if HELPERS.iter().any(|(h, _)| *h == word) {
                    Some(TYPE)
                } else {
                    Some(CALLABLE)
                }
            } else if !member && KEYWORDS.contains(&word) {
                Some(KEYWORD)
            } else if word == "db" && !member {
                Some(TYPE)
            } else {
                None
            };
            if let Some(kind) = kind {
                push(i, j, kind);
            }
            previous = word.chars().last();
            i = j;
            continue;
        }
        if !c.is_whitespace() {
            previous = Some(c);
        }
        i += c.len_utf8();
    }
    Analysis {
        tokens,
        ..Default::default()
    }
}

fn item(
    label: &str,
    detail: &str,
    kind: &str,
    from: usize,
    to: usize,
    insert: String,
    cursor: usize,
) -> Completion {
    Completion {
        label: label.to_owned(),
        detail: detail.to_owned(),
        kind: kind.to_owned(),
        from,
        to,
        cursor,
        insert,
    }
}

fn plain_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

/// How `db.` reaches a collection: `db.orders`, or `db.getCollection("…")`
/// for names that are not identifiers or that a database method shadows.
fn collection_member(name: &str) -> String {
    if plain_identifier(name) && !DATABASE_METHODS.iter().any(|(m, _)| *m == name) {
        name.to_owned()
    } else {
        format!("getCollection({})", js_string(name))
    }
}

/// `db.getSiblingDB("shop").orders` — the collection, whatever `use` did.
pub(super) fn collection_ref(db: &str, collection: &str) -> String {
    format!(
        "db.getSiblingDB({}).{}",
        js_string(db),
        collection_member(collection)
    )
}

fn sort_doc(sort: &Option<(String, bool)>) -> String {
    match sort {
        Some((field, desc)) => format!(
            ".sort({{ {}: {} }})",
            js_key(field),
            if *desc { -1 } else { 1 }
        ),
        None => String::new(),
    }
}

/// A key of an object literal, quoted when it has to be.
fn js_key(name: &str) -> String {
    if plain_identifier(name) {
        name.to_owned()
    } else {
        js_string(name)
    }
}

/// The statement for one page of a collection.
pub(super) fn page_statement(
    db: &str,
    collection: &str,
    sort: &Option<(String, bool)>,
    page: usize,
    limit: usize,
) -> String {
    let skip = page * limit;
    format!(
        "{}.find({{}}){}{}.limit({limit})",
        collection_ref(db, collection),
        sort_doc(sort),
        if skip > 0 {
            format!(".skip({skip})")
        } else {
            String::new()
        }
    )
}

/// The same collection read whole, for exporting every document.
pub(super) fn collection_statement(
    db: &str,
    collection: &str,
    sort: &Option<(String, bool)>,
) -> String {
    format!(
        "{}.find({{}}){}",
        collection_ref(db, collection),
        sort_doc(sort)
    )
}

/// The database `db` means at `at`: the last `use name` before it, else the source's.
fn current_db<'a>(text: &'a str, at: usize, default: &'a str) -> &'a str {
    text[..at]
        .lines()
        .rev()
        .find_map(|line| {
            line.trim()
                .strip_prefix("use ")
                .map(|r| r.trim().trim_end_matches(';'))
        })
        .filter(|name| !name.is_empty() && !name.contains(char::is_whitespace))
        .unwrap_or(default)
}

/// The collection the statement around `at` works on: `db.name.` or
/// `getCollection("name")`, with the database it is in.
fn statement_collection(statement: &str, db: &str) -> Option<(String, String)> {
    let mut database = db.to_owned();
    if let Some(at) = statement.find("getSiblingDB(") {
        let rest = &statement[at + "getSiblingDB(".len()..];
        if let Some(name) = quoted(rest) {
            database = name;
        }
    }
    if let Some(at) = statement.find("getCollection(") {
        return quoted(&statement[at + "getCollection(".len()..]).map(|c| (database, c));
    }
    let at = statement.find("db.")?;
    let mut rest = &statement[at + 3..];
    if rest.starts_with("getSiblingDB(") {
        let close = rest.find(')')?;
        rest = rest[close + 1..].strip_prefix('.')?;
    }
    let name: String = rest.chars().take_while(|c| is_word(*c)).collect();
    let next = rest[name.len()..].trim_start();
    (!name.is_empty() && next.starts_with('.') && !DATABASE_METHODS.iter().any(|(m, _)| *m == name))
        .then_some((database, name))
}

/// The text of a string literal at the start of `text`.
fn quoted(text: &str) -> Option<String> {
    let text = text.trim_start();
    let quote = text.chars().next().filter(|c| matches!(c, '"' | '\''))?;
    let body = &text[1..];
    body.find(quote).map(|end| body[..end].to_owned())
}

/// Completions at byte offset `at` of `text`. `default_db` is the source's database.
pub(super) fn complete(
    catalog: &Catalog,
    default_db: &str,
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
    let matches = |name: &str| lower.is_empty() || name.to_lowercase().starts_with(&lower);
    let db = current_db(text, at, default_db).to_owned();
    let head = before[..start].trim_end_matches([' ', '\t']);
    let mut out: Vec<Completion> = Vec::new();
    let collections = |db: &str| -> Vec<(&String, TableKind)> {
        catalog
            .schemas
            .get(db)
            .map(|tables| {
                tables
                    .iter()
                    .filter(|(name, _)| !name.is_empty())
                    .map(|(name, (kind, _))| (name, *kind))
                    .collect()
            })
            .unwrap_or_default()
    };
    let databases = || catalog.schemas.keys().filter(|d| !d.is_empty());

    // Inside `getSiblingDB("`, `getCollection("`, or after `use `: names.
    let quote_open = before[..start].ends_with(['"', '\'']);
    if quote_open {
        let call = before[..start - 1].trim_end_matches([' ', '\t']);
        if call.ends_with("getSiblingDB(") {
            for name in databases().filter(|n| matches(n)) {
                out.push(item(
                    name,
                    "database",
                    "schema",
                    start,
                    at,
                    name.clone(),
                    name.len(),
                ));
            }
        } else if call.ends_with("getCollection(") {
            let target = statement_collection(call, &db).map_or(db.clone(), |(d, _)| d);
            for (name, kind) in collections(&target).into_iter().filter(|(n, _)| matches(n)) {
                out.push(item(
                    name,
                    kind_label(kind),
                    "table",
                    start,
                    at,
                    name.clone(),
                    name.len(),
                ));
            }
        }
        return out;
    }
    if before[..start].trim_end().ends_with("use")
        && before[..start].trim_start().starts_with("use")
    {
        for name in databases().filter(|n| matches(n)) {
            out.push(item(
                name,
                "database",
                "schema",
                start,
                at,
                name.clone(),
                name.len(),
            ));
        }
        return out;
    }

    if let Some(chain) = head.strip_suffix('.') {
        let chain = chain.trim_end();
        let receiver: String = chain
            .chars()
            .rev()
            .take_while(|c| is_word(*c))
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let before_receiver = chain[..chain.len() - receiver.len()].trim_end();
        // `name()`, the cursor between the parentheses when it takes arguments.
        let method = |name: &str, args: &str, detail: &str| {
            let insert = format!("{name}()");
            let cursor = if args == "()" {
                insert.len()
            } else {
                name.len() + 1
            };
            item(name, detail, "function", start, at, insert, cursor)
        };
        // `db.` / `db.getSiblingDB("x").`: collections and database methods.
        let database = if receiver == "db" && !before_receiver.ends_with('.') {
            Some(db.clone())
        } else if chain.ends_with(')')
            && chain.contains("getSiblingDB(")
            && !chain.contains("getCollection(")
        {
            let at = chain.rfind("getSiblingDB(").unwrap_or(0);
            let after = &chain[at + "getSiblingDB(".len()..];
            let tail = after.find(')').map_or("", |n| &after[n + 1..]);
            tail.trim().is_empty().then(|| quoted(after)).flatten()
        } else {
            None
        };
        if let Some(database) = database {
            for (name, kind) in collections(&database)
                .into_iter()
                .filter(|(n, _)| matches(n))
            {
                let member = collection_member(name);
                out.push(item(
                    name,
                    kind_label(kind),
                    "table",
                    start,
                    at,
                    member.clone(),
                    member.len(),
                ));
            }
            for (name, args) in DATABASE_METHODS.iter().filter(|(n, _)| matches(n)) {
                out.push(method(name, args, args));
            }
            return out;
        }
        // `db.users.` / `getCollection("x").`: collection methods.
        let on_collection = (!receiver.is_empty() && before_receiver.ends_with('.') && {
            let owner = before_receiver.trim_end_matches('.').trim_end();
            owner.ends_with("db") || owner.ends_with(')') && owner.contains("getSiblingDB(")
        }) || (chain.ends_with(')') && {
            let call = chain.rfind("getCollection(");
            call.is_some_and(|c| !chain[c..].contains(")."))
        });
        if on_collection {
            for (name, args) in COLLECTION_METHODS.iter().filter(|(n, _)| matches(n)) {
                out.push(method(name, args, args));
            }
            return out;
        }
        // `….find({ … }).`: cursor modifiers.
        if chain.ends_with(')') {
            for (name, args) in CURSOR_METHODS.iter().filter(|(n, _)| matches(n)) {
                out.push(method(name, args, args));
            }
        }
        return out;
    }

    if prefix.is_empty() && !explicit {
        return out;
    }
    // Inside an object literal: operators and the collection's fields.
    let depth = before
        .chars()
        .filter(|c| matches!(c, '{' | '[' | '('))
        .count() as isize
        - before
            .chars()
            .filter(|c| matches!(c, '}' | ']' | ')'))
            .count() as isize;
    let statement_start = before.rfind(['\n', ';']).map_or(0, |n| n + 1);
    let statement = &text[statement_start.min(start)..];
    if prefix.starts_with('$') || depth > 0 {
        for (name, detail) in OPERATORS.iter().filter(|(n, _)| matches(n)) {
            out.push(item(
                name,
                detail,
                "keyword",
                start,
                at,
                (*name).to_owned(),
                name.len(),
            ));
        }
        // Fields of the collection the statement is on (the whole statement may span lines).
        let whole = &text[text[..start].rfind("db.").unwrap_or(statement_start)..];
        if let Some((database, collection)) =
            statement_collection(whole, &db).or_else(|| statement_collection(statement, &db))
        {
            let fields = catalog
                .schemas
                .get(&database)
                .and_then(|t| t.get(&collection))
                .map(|(_, columns)| columns.clone())
                .unwrap_or_default();
            for (name, type_name) in fields.iter().filter(|(n, _)| !n.is_empty() && matches(n)) {
                let insert = js_key(name);
                out.push(item(
                    name,
                    type_name,
                    "column",
                    start,
                    at,
                    insert.clone(),
                    insert.len(),
                ));
            }
        }
        out.dedup_by(|a, b| a.label == b.label);
        out.truncate(80);
        return out;
    }
    for (name, detail) in [
        ("db", "the current database"),
        ("use", "switch database"),
        ("show", "dbs, collections, users…"),
    ] {
        if matches(name) {
            out.push(item(
                name,
                detail,
                "keyword",
                start,
                at,
                name.to_owned(),
                name.len(),
            ));
        }
    }
    for (name, detail) in HELPERS.iter().filter(|(n, _)| matches(n)) {
        out.push(item(
            name,
            detail,
            "function",
            start,
            at,
            format!("{name}()"),
            name.len() + 1,
        ));
    }
    for word in KEYWORDS
        .iter()
        .filter(|w| matches(w) && !matches!(**w, "use" | "show"))
    {
        out.push(item(
            word,
            "",
            "keyword",
            start,
            at,
            (*word).to_owned(),
            word.len(),
        ));
    }
    out.truncate(80);
    out
}

fn kind_label(kind: TableKind) -> &'static str {
    if kind == TableKind::View {
        "view"
    } else {
        "collection"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use monocode_core::db::CatalogEntry;

    fn catalog() -> Catalog {
        let entry = |schema: &str, table: &str, column: &str| CatalogEntry {
            schema: schema.into(),
            table: table.into(),
            kind: TableKind::Table,
            column: column.into(),
            type_name: "string".into(),
        };
        Catalog::from_entries(vec![
            entry("shop", "orders", "status"),
            entry("shop", "orders", "total"),
            entry("shop", "order items", ""),
            entry("shop", "stats", ""),
            entry("logs", "events", ""),
        ])
    }

    fn labels(text: &str) -> Vec<String> {
        complete(&catalog(), "shop", text, text.len(), true)
            .into_iter()
            .map(|c| c.label)
            .collect()
    }

    #[test]
    fn db_dot_offers_collections_and_database_methods() {
        let found = complete(&catalog(), "shop", "db.or", 5, false);
        let names: Vec<_> = found.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(names, ["order items", "orders"]);
        assert_eq!(found[0].insert, "getCollection(\"order items\")");
        assert_eq!(found[1].insert, "orders");
        // A collection a method name shadows is reached through getCollection.
        let stats = complete(&catalog(), "shop", "db.sta", 6, false);
        assert!(
            stats
                .iter()
                .any(|c| c.label == "stats" && c.insert == "getCollection(\"stats\")")
        );
        assert!(labels_contain(&labels_of("db."), "runCommand"));
        assert!(
            labels_contain(&labels("use logs\ndb."), "events"),
            "use changes db"
        );
        assert!(labels_contain(
            &labels("db.getSiblingDB('logs')."),
            "events"
        ));
    }

    fn labels_of(text: &str) -> Vec<String> {
        labels(text)
    }

    fn labels_contain(labels: &[String], wanted: &str) -> bool {
        labels.iter().any(|l| l == wanted)
    }

    #[test]
    fn collections_cursors_operators_and_fields() {
        assert!(labels_contain(&labels("db.orders."), "aggregate"));
        assert!(labels_contain(
            &labels("db.getCollection('orders')."),
            "insertOne"
        ));
        assert!(labels_contain(&labels("db.orders.find({})."), "sort"));
        assert!(!labels_contain(&labels("db.orders.find({})."), "insertOne"));
        let inside = labels("db.orders.find({ st");
        assert_eq!(inside, ["status"]);
        let ops = labels("db.orders.find({ total: { $g");
        assert_eq!(ops, ["$gt", "$gte", "$group", "$graphLookup"]);
        assert!(labels_contain(&labels("db.getSiblingDB(\""), "logs"));
        assert!(labels_contain(&labels("use "), "logs"));
        assert!(labels_contain(&labels("Obj"), "ObjectId"));
        let method = complete(&catalog(), "shop", "db.orders.fin", 13, false);
        let find = method.iter().find(|c| c.label == "find").unwrap();
        assert_eq!((find.insert.as_str(), find.cursor), ("find()", 5));
    }

    #[test]
    fn colours_cover_the_shell() {
        let text = "// all\ndb.users.find({ age: { $gt: 30 }, name: /^a/i, \"x\": 'y' }).limit(5)";
        let analysis = analyse(text);
        let kinds: Vec<(&str, &str)> = analysis
            .tokens
            .iter()
            .map(|t| (&text[t.from..t.to], t.kind.as_ref()))
            .collect();
        assert!(kinds.contains(&("// all", COMMENT)));
        assert!(kinds.contains(&("db", TYPE)));
        assert!(kinds.contains(&("find", CALLABLE)));
        assert!(kinds.contains(&("age", PROPERTY)));
        assert!(kinds.contains(&("$gt", KEYWORD)));
        assert!(kinds.contains(&("30", NUMBER)));
        assert!(kinds.contains(&("/^a/i", STRING)));
        assert!(kinds.contains(&("\"x\"", PROPERTY)));
        assert!(kinds.contains(&("'y'", STRING)));
    }

    #[test]
    fn statements_for_a_collection() {
        assert_eq!(
            page_statement("shop", "orders", &None, 0, 100),
            "db.getSiblingDB(\"shop\").orders.find({}).limit(100)"
        );
        assert_eq!(
            page_statement(
                "shop",
                "order items",
                &Some(("created at".into(), true)),
                2,
                50
            ),
            "db.getSiblingDB(\"shop\").getCollection(\"order items\").find({}).sort({ \"created at\": -1 }).skip(100).limit(50)"
        );
        assert_eq!(
            collection_statement("shop", "orders", &Some(("_id".into(), false))),
            "db.getSiblingDB(\"shop\").orders.find({}).sort({ _id: 1 })"
        );
    }
}
