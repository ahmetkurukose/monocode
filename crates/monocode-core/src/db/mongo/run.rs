//! Running a [`Plan`] with the driver: each shell method becomes the server
//! command mongosh would send, and the reply becomes a grid result.

use super::script::{READ_COMMANDS, WRITE_METHODS};
use super::shell::{to_bson, Plan};
use super::{value, MongoConnection};
use crate::db::conn::{DbError, QueryResult};
use mongodb::bson::{doc, oid::ObjectId, Bson, Document};

/// A tag for one statement's server operations, so it can be killed.
pub(super) fn tag() -> String {
    format!("monocode-{}", uuid::Uuid::new_v4())
}

fn bad(plan: &Plan, what: impl std::fmt::Display) -> DbError {
    DbError::plain(format!("{}(): {what}", plan.method))
}

fn as_i64(value: &Bson) -> Option<i64> {
    match value {
        Bson::Int32(n) => Some(i64::from(*n)),
        Bson::Int64(n) => Some(*n),
        Bson::Double(n) if n.is_finite() => Some(*n as i64),
        _ => None,
    }
}

fn as_u64(value: Option<&Bson>) -> u64 {
    value.and_then(as_i64).map_or(0, |n| n.max(0) as u64)
}

/// A modifier's first argument as BSON.
fn modifier(plan: &Plan, name: &str) -> Result<Option<Bson>, DbError> {
    plan.modifier(name)
        .and_then(|args| args.first())
        .map(to_bson)
        .transpose()
}

fn modifier_i64(plan: &Plan, name: &str) -> Result<Option<i64>, DbError> {
    modifier(plan, name)?
        .map(|v| as_i64(&v).ok_or_else(|| bad(plan, format!(".{name}() takes a number"))))
        .transpose()
}

/// Argument `i` when it is a document; `None` when missing or null.
fn optional_doc(plan: &Plan, i: usize) -> Result<Option<Document>, DbError> {
    match plan.arg(i)? {
        None | Some(Bson::Null) => Ok(None),
        Some(Bson::Document(d)) => Ok(Some(d)),
        Some(other) => Err(bad(
            plan,
            format!(
                "argument {} must be a document, not {}",
                i + 1,
                value::type_name(&other)
            ),
        )),
    }
}

/// Whether running the plan changes anything on the server.
pub(super) fn writes(plan: &Plan) -> bool {
    if plan.has("explain") {
        return false;
    }
    let method = plan.method.as_str();
    if WRITE_METHODS.contains(&method) {
        return true;
    }
    match method {
        "aggregate" => pipeline(plan).is_ok_and(|(stages, _)| pipeline_writes(&stages)),
        "runCommand" | "adminCommand" => match command_arg(plan) {
            Ok(command) => command_writes(&command),
            Err(_) => true,
        },
        _ => false,
    }
}

fn pipeline_writes(stages: &[Document]) -> bool {
    stages
        .last()
        .is_some_and(|s| s.contains_key("$out") || s.contains_key("$merge"))
}

fn command_writes(command: &Document) -> bool {
    let Some(name) = command.keys().next() else {
        return true;
    };
    if !READ_COMMANDS.contains(&name.as_str()) {
        return true;
    }
    match name.as_str() {
        "aggregate" => command
            .get_array("pipeline")
            .map(|p| {
                let stages: Vec<Document> =
                    p.iter().filter_map(|s| s.as_document().cloned()).collect();
                pipeline_writes(&stages)
            })
            .unwrap_or(false),
        // `profile` with a level other than -1 changes the profiler.
        "profile" => command.get("profile").and_then(as_i64) != Some(-1),
        "explain" => false,
        _ => false,
    }
}

/// `runCommand({ … })` or `runCommand("name")`.
fn command_arg(plan: &Plan) -> Result<Document, DbError> {
    match plan.arg(0)? {
        Some(Bson::Document(d)) if !d.is_empty() => Ok(d),
        Some(Bson::String(name)) => Ok(doc! { name: 1 }),
        _ => Err(bad(plan, "takes a command document, such as { ping: 1 }")),
    }
}

/// The stages of an `aggregate`, as an array or as separate arguments, and its options.
fn pipeline(plan: &Plan) -> Result<(Vec<Document>, Document), DbError> {
    let to_stages = |items: Vec<Bson>| -> Result<Vec<Document>, DbError> {
        items
            .into_iter()
            .map(|s| match s {
                Bson::Document(d) => Ok(d),
                other => Err(bad(
                    plan,
                    format!(
                        "a pipeline stage must be a document, not {}",
                        value::type_name(&other)
                    ),
                )),
            })
            .collect()
    };
    match plan.arg(0)? {
        None => Ok((Vec::new(), Document::new())),
        Some(Bson::Array(items)) => Ok((to_stages(items)?, plan.doc_arg(1, "the options")?)),
        Some(Bson::Document(_)) => {
            // mongosh also takes the stages as arguments.
            let items = plan
                .args
                .iter()
                .map(to_bson)
                .collect::<Result<Vec<_>, _>>()?;
            Ok((to_stages(items)?, Document::new()))
        }
        Some(other) => Err(bad(
            plan,
            format!(
                "the pipeline must be an array, not {}",
                value::type_name(&other)
            ),
        )),
    }
}

/// Copies the cursor modifiers (and the options argument) that the server
/// command takes as fields.
fn apply_options(
    plan: &Plan,
    command: &mut Document,
    options: &Document,
    keys: &[&str],
) -> Result<(), DbError> {
    for key in keys {
        if let Some(v) = options.get(*key) {
            command.insert(*key, v.clone());
        }
    }
    for (name, field) in [
        ("sort", "sort"),
        ("projection", "projection"),
        ("project", "projection"),
        ("hint", "hint"),
        ("maxTimeMS", "maxTimeMS"),
        ("collation", "collation"),
        ("comment", "comment"),
        ("min", "min"),
        ("max", "max"),
        ("returnKey", "returnKey"),
        ("showRecordId", "showRecordId"),
        ("noCursorTimeout", "noCursorTimeout"),
        ("allowPartialResults", "allowPartialResults"),
        ("skip", "skip"),
    ] {
        if !keys.contains(&field) || !plan.has(name) {
            continue;
        }
        let value = modifier(plan, name)?.unwrap_or(Bson::Boolean(true));
        command.insert(field, value);
    }
    if keys.contains(&"allowDiskUse") && plan.has("allowDiskUse") {
        let on = modifier(plan, "allowDiskUse")?.unwrap_or(Bson::Boolean(true));
        command.insert("allowDiskUse", on);
    }
    if keys.contains(&"readConcern") {
        if let Some(level) = modifier(plan, "readConcern")? {
            command.insert(
                "readConcern",
                match level {
                    Bson::String(level) => Bson::Document(doc! { "level": level }),
                    other => other,
                },
            );
        }
    }
    Ok(())
}

fn tag_and_timeout(command: &mut Document, tag: &str, timeout_ms: Option<u32>, timed: bool) {
    if !command.contains_key("comment") {
        command.insert("comment", tag);
    }
    if timed && !command.contains_key("maxTimeMS") {
        if let Some(ms) = timeout_ms {
            command.insert("maxTimeMS", i64::from(ms));
        }
    }
}

const FIND_OPTIONS: &[&str] = &[
    "sort",
    "projection",
    "skip",
    "limit",
    "hint",
    "maxTimeMS",
    "collation",
    "comment",
    "batchSize",
    "allowDiskUse",
    "min",
    "max",
    "returnKey",
    "showRecordId",
    "readConcern",
    "noCursorTimeout",
    "allowPartialResults",
    "let",
];

/// The `find` command; `rows` caps what is read (the limit plus one, to
/// tell whether more exist).
fn find_command(
    conn: &MongoConnection,
    plan: &Plan,
    collection: &str,
    tag: &str,
    rows: Option<usize>,
) -> Result<(Document, Option<i64>), DbError> {
    let filter = plan.doc_arg(0, "the filter")?;
    let mut command = doc! { "find": collection, "filter": filter };
    if let Some(projection) = optional_doc(plan, 1)? {
        command.insert("projection", projection);
    }
    let options = optional_doc(plan, 2)?.unwrap_or_default();
    apply_options(plan, &mut command, &options, FIND_OPTIONS)?;
    let user_limit = match modifier_i64(plan, "limit")? {
        Some(n) => Some(n),
        None => command.get("limit").and_then(as_i64),
    }
    .filter(|n| *n != 0)
    .map(i64::abs);
    if let Some(n) = modifier_i64(plan, "batchSize")? {
        command.insert("batchSize", n);
    }
    let limit = match (user_limit, rows) {
        (Some(user), Some(rows)) => Some(user.min(rows as i64)),
        (Some(user), None) => Some(user),
        (None, Some(rows)) => Some(rows as i64),
        (None, None) => None,
    };
    match limit {
        Some(n) => {
            command.insert("limit", n);
            if !command.contains_key("batchSize") {
                command.insert("batchSize", n.min(1000));
            }
        }
        None => {
            command.remove("limit");
        }
    }
    tag_and_timeout(&mut command, tag, conn.timeout_ms, true);
    Ok((command, user_limit))
}

const AGGREGATE_OPTIONS: &[&str] = &[
    "allowDiskUse",
    "maxTimeMS",
    "hint",
    "collation",
    "comment",
    "let",
    "readConcern",
    "bypassDocumentValidation",
];

/// The `aggregate` command. `rows` adds a `$limit` (unless the pipeline writes).
fn aggregate_command(
    conn: &MongoConnection,
    plan: &Plan,
    target: Bson,
    tag: &str,
    rows: Option<usize>,
) -> Result<(Document, bool), DbError> {
    let (mut stages, options) = pipeline(plan)?;
    let writes = pipeline_writes(&stages);
    if let (Some(rows), false) = (rows, writes) {
        stages.push(doc! { "$limit": rows as i64 });
    }
    let batch = modifier_i64(plan, "batchSize")?.or_else(|| {
        options
            .get_document("cursor")
            .ok()
            .and_then(|c| c.get("batchSize").and_then(as_i64))
    });
    let mut command = doc! {
        "aggregate": target,
        "pipeline": stages,
        "cursor": match batch.or(rows.map(|r| (r as i64).min(1000))) {
            Some(n) => doc! { "batchSize": n },
            None => doc! {},
        },
    };
    apply_options(plan, &mut command, &options, AGGREGATE_OPTIONS)?;
    tag_and_timeout(&mut command, tag, conn.timeout_ms, true);
    Ok((command, writes))
}

/// A pipeline counting what a filter matches (`countDocuments`).
fn count_command(
    conn: &MongoConnection,
    collection: &str,
    filter: Document,
    skip: Option<i64>,
    limit: Option<i64>,
    tag: &str,
) -> Document {
    let mut stages = vec![doc! { "$match": filter }];
    if let Some(skip) = skip.filter(|n| *n > 0) {
        stages.push(doc! { "$skip": skip });
    }
    if let Some(limit) = limit.filter(|n| *n != 0) {
        stages.push(doc! { "$limit": limit.abs() });
    }
    stages.push(doc! { "$group": { "_id": 1, "n": { "$sum": 1 } } });
    let mut command = doc! { "aggregate": collection, "pipeline": stages, "cursor": {} };
    tag_and_timeout(&mut command, tag, conn.timeout_ms, true);
    command
}

/// `updateOne`, `updateMany`, `replaceOne`, `update`.
fn update_command(plan: &Plan, collection: &str, tag: &str) -> Result<Document, DbError> {
    let filter = plan.doc_arg(0, "the filter")?;
    let update = match plan.arg(1)? {
        Some(u @ (Bson::Document(_) | Bson::Array(_))) => u,
        _ => return Err(bad(plan, "takes a filter and an update document")),
    };
    let options = plan.doc_arg(2, "the options")?;
    let operators = match &update {
        Bson::Document(d) => d.keys().next().is_some_and(|k| k.starts_with('$')),
        _ => true,
    };
    let multi = match plan.method.as_str() {
        "updateMany" => true,
        "update" => options.get_bool("multi").unwrap_or(false),
        _ => false,
    };
    match plan.method.as_str() {
        "replaceOne" if operators => {
            return Err(bad(
                plan,
                "the replacement must not hold update operators ($set…); use updateOne",
            ));
        }
        "updateOne" | "updateMany" if !operators => {
            return Err(bad(
                plan,
                "the update needs operators such as $set; use replaceOne to replace a document",
            ));
        }
        _ => {}
    }
    let mut statement = doc! { "q": filter, "u": update, "multi": multi };
    for key in ["upsert", "arrayFilters", "hint", "collation"] {
        if let Some(v) = options.get(key) {
            statement.insert(key, v.clone());
        }
    }
    let mut command = doc! { "update": collection, "updates": [statement], "ordered": true };
    if let Some(v) = options.get("let") {
        command.insert("let", v.clone());
    }
    if let Some(v) = options.get("writeConcern") {
        command.insert("writeConcern", v.clone());
    }
    tag_and_timeout(&mut command, tag, None, false);
    Ok(command)
}

/// `deleteOne`, `deleteMany`, `remove`.
fn delete_command(plan: &Plan, collection: &str, tag: &str) -> Result<Document, DbError> {
    let filter = plan.doc_arg(0, "the filter")?;
    let (one, options) = match plan.method.as_str() {
        "deleteOne" => (true, plan.doc_arg(1, "the options")?),
        "remove" => match plan.arg(1)? {
            Some(Bson::Boolean(one)) => (one, Document::new()),
            Some(Bson::Document(o)) => (o.get_bool("justOne").unwrap_or(false), o),
            _ => (false, Document::new()),
        },
        _ => (false, plan.doc_arg(1, "the options")?),
    };
    let mut statement = doc! { "q": filter, "limit": if one { 1 } else { 0 } };
    for key in ["hint", "collation"] {
        if let Some(v) = options.get(key) {
            statement.insert(key, v.clone());
        }
    }
    let mut command = doc! { "delete": collection, "deletes": [statement], "ordered": true };
    if let Some(v) = options.get("writeConcern") {
        command.insert("writeConcern", v.clone());
    }
    tag_and_timeout(&mut command, tag, None, false);
    Ok(command)
}

/// `findOneAndUpdate`, `findOneAndReplace`, `findOneAndDelete`, `findAndModify`.
fn find_and_modify_command(
    conn: &MongoConnection,
    plan: &Plan,
    collection: &str,
    tag: &str,
) -> Result<Document, DbError> {
    if plan.method == "findAndModify" {
        let mut spec = plan.doc_arg(0, "the specification")?;
        let mut command = doc! { "findAndModify": collection };
        if let Some(q) = spec.remove("query") {
            command.insert("query", q);
        }
        command.extend(spec);
        tag_and_timeout(&mut command, tag, conn.timeout_ms, true);
        return Ok(command);
    }
    let filter = plan.doc_arg(0, "the filter")?;
    let mut command = doc! { "findAndModify": collection, "query": filter };
    let options = if plan.method == "findOneAndDelete" {
        command.insert("remove", true);
        plan.doc_arg(1, "the options")?
    } else {
        let change = match plan.arg(1)? {
            Some(u @ (Bson::Document(_) | Bson::Array(_))) => u,
            _ => return Err(bad(plan, "takes a filter and an update")),
        };
        let operators = match &change {
            Bson::Document(d) => d.keys().next().is_some_and(|k| k.starts_with('$')),
            _ => true,
        };
        if plan.method == "findOneAndReplace" && operators {
            return Err(bad(plan, "the replacement must not hold update operators"));
        }
        if plan.method == "findOneAndUpdate" && !operators {
            return Err(bad(plan, "the update needs operators such as $set"));
        }
        command.insert("update", change);
        plan.doc_arg(2, "the options")?
    };
    let after = options.get_str("returnDocument") == Ok("after")
        || options.get_bool("returnNewDocument").unwrap_or(false);
    if !command.contains_key("remove") {
        command.insert("new", after);
    }
    for (from, to) in [
        ("sort", "sort"),
        ("projection", "fields"),
        ("upsert", "upsert"),
        ("arrayFilters", "arrayFilters"),
        ("hint", "hint"),
        ("collation", "collation"),
        ("maxTimeMS", "maxTimeMS"),
        ("let", "let"),
    ] {
        if let Some(v) = options.get(from) {
            command.insert(to, v.clone());
        }
    }
    tag_and_timeout(&mut command, tag, conn.timeout_ms, true);
    Ok(command)
}

/// The inner command of an explain.
pub(super) fn explainable(
    conn: &MongoConnection,
    plan: &Plan,
    tag: &str,
) -> Result<Document, DbError> {
    let not = || {
        DbError::plain(format!(
            "{} cannot be explained; explain takes find, aggregate, count, distinct, update, delete and findAndModify",
            plan.describe()
        ))
    };
    let Some(collection) = plan.collection.as_deref() else {
        return match plan.method.as_str() {
            "aggregate" => Ok(aggregate_command(conn, plan, Bson::Int32(1), tag, None)?.0),
            _ => Err(not()),
        };
    };
    Ok(match plan.method.as_str() {
        "find" | "findOne" => {
            if plan.has("count") || plan.has("size") || plan.has("itcount") {
                let filter = plan.doc_arg(0, "the filter")?;
                count_command(conn, collection, filter, None, None, tag)
            } else {
                let rows = (plan.method == "findOne").then_some(1);
                find_command(conn, plan, collection, tag, rows)?.0
            }
        }
        "aggregate" => aggregate_command(conn, plan, Bson::String(collection.into()), tag, None)?.0,
        "countDocuments" | "count" => {
            let filter = plan.doc_arg(0, "the filter")?;
            count_command(conn, collection, filter, None, None, tag)
        }
        "distinct" => distinct_command(conn, plan, collection, tag)?,
        "updateOne" | "updateMany" | "replaceOne" | "update" => {
            update_command(plan, collection, tag)?
        }
        "deleteOne" | "deleteMany" | "remove" => delete_command(plan, collection, tag)?,
        "findOneAndUpdate" | "findOneAndReplace" | "findOneAndDelete" | "findAndModify" => {
            find_and_modify_command(conn, plan, collection, tag)?
        }
        _ => return Err(not()),
    })
}

fn distinct_command(
    conn: &MongoConnection,
    plan: &Plan,
    collection: &str,
    tag: &str,
) -> Result<Document, DbError> {
    let Some(Bson::String(key)) = plan.arg(0)? else {
        return Err(bad(plan, "takes a field name"));
    };
    let mut command =
        doc! { "distinct": collection, "key": key, "query": plan.doc_arg(1, "the filter")? };
    let options = plan.doc_arg(2, "the options")?;
    for key in ["collation", "maxTimeMS", "hint"] {
        if let Some(v) = options.get(key) {
            command.insert(key, v.clone());
        }
    }
    tag_and_timeout(&mut command, tag, conn.timeout_ms, true);
    Ok(command)
}

/// For export: the cursor command that streams every document the plan
/// shows, ignoring the console's row limit. `None` for plans without a cursor.
pub(super) fn cursor_source(
    conn: &MongoConnection,
    plan: &Plan,
    tag: &str,
) -> Result<Option<(String, Document)>, DbError> {
    if plan.has("explain") || plan.has("count") || plan.has("size") || plan.has("itcount") {
        return Ok(None);
    }
    let command = match (plan.collection.as_deref(), plan.method.as_str()) {
        (Some(c), "find") => find_command(conn, plan, c, tag, None)?.0,
        (Some(c), "aggregate") => {
            aggregate_command(conn, plan, Bson::String(c.into()), tag, None)?.0
        }
        (Some(c), "getIndexes" | "getIndices") => doc! { "listIndexes": c },
        (None, "aggregate") => aggregate_command(conn, plan, Bson::Int32(1), tag, None)?.0,
        (None, "getCollectionInfos") => {
            doc! { "listCollections": 1, "filter": plan.doc_arg(0, "the filter")? }
        }
        _ => return Ok(None),
    };
    Ok(Some((plan.db.clone(), command)))
}

/// Raises the first write error of a write command's reply.
fn check_write(reply: &Document) -> Result<(), DbError> {
    if let Ok(errors) = reply.get_array("writeErrors") {
        if let Some(Bson::Document(first)) = errors.first() {
            let code = first.get("code").and_then(as_i64).unwrap_or(0);
            let name = first.get_str("codeName").unwrap_or("");
            return Err(DbError {
                message: first
                    .get_str("errmsg")
                    .unwrap_or("The write failed")
                    .to_owned(),
                detail: Some(format!(
                    "{} write error{}; the first at document {}",
                    errors.len(),
                    if errors.len() == 1 { "" } else { "s" },
                    first.get("index").and_then(as_i64).unwrap_or(0)
                )),
                code: Some(super::code_label(code as i32, name)),
                ..DbError::default()
            });
        }
    }
    if let Ok(concern) = reply.get_document("writeConcernError") {
        return Err(DbError {
            message: format!(
                "Write concern: {}",
                concern.get_str("errmsg").unwrap_or("not satisfied")
            ),
            ..DbError::default()
        });
    }
    Ok(())
}

/// A result of one document (a command's reply, a write's summary).
fn document(statement: &str, doc: Document, affected: Option<u64>) -> QueryResult {
    let mut result = value::table(statement, std::slice::from_ref(&doc), false);
    result.affected = affected;
    result
}

/// The documents of `reply[key]` as rows.
pub(super) fn array_table(statement: &str, reply: &Document, key: &str) -> QueryResult {
    let items: Vec<Bson> = reply.get_array(key).cloned().unwrap_or_default();
    if items.iter().all(|i| matches!(i, Bson::Document(_))) {
        let docs: Vec<Document> = items
            .into_iter()
            .filter_map(|i| i.as_document().cloned())
            .collect();
        value::table(statement, &docs, false)
    } else {
        value::list(statement, key, &items)
    }
}

/// What a statement evaluated to when it was not a database call.
pub(super) fn value_result(
    statement: &str,
    value: Option<serde_json::Value>,
    printed: Vec<String>,
) -> Result<QueryResult, DbError> {
    if !printed.is_empty() {
        let mut lines: Vec<Bson> = printed.into_iter().map(Bson::String).collect();
        if let Some(v) = &value {
            lines.push(Bson::String(
                value::cell(&to_bson(v)?).unwrap_or_else(|| "null".into()),
            ));
        }
        return Ok(value::list(statement, "output", &lines));
    }
    let Some(value) = value else {
        return Ok(QueryResult {
            statement: statement.to_owned(),
            columns: Vec::new(),
            rows: Vec::new(),
            affected: None,
            has_more: false,
            execution_ms: 0.,
            elapsed_ms: 0.,
        });
    };
    Ok(match to_bson(&value)? {
        Bson::Document(doc) => value::table(statement, &[doc], false),
        Bson::Array(items)
            if !items.is_empty() && items.iter().all(|i| matches!(i, Bson::Document(_))) =>
        {
            let docs: Vec<Document> = items
                .into_iter()
                .filter_map(|i| i.as_document().cloned())
                .collect();
            value::table(statement, &docs, false)
        }
        Bson::Array(items) => value::list(statement, "result", &items),
        other => value::list(statement, "result", &[other]),
    })
}

/// A generated index name: `{ a: 1, b: -1 }` → `a_1_b_-1`.
fn index_name(keys: &Document) -> String {
    keys.iter()
        .map(|(k, v)| format!("{k}_{}", value::cell(v).unwrap_or_default()))
        .collect::<Vec<_>>()
        .join("_")
}

/// Runs `plan` and shows its outcome.
pub(super) async fn run(
    conn: &MongoConnection,
    plan: &Plan,
    statement: &str,
    max_rows: usize,
    tag: &str,
) -> Result<QueryResult, DbError> {
    if let Some(args) = plan.modifier("explain") {
        let verbosity = args
            .first()
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .or_else(|| {
                args.first().and_then(|v| v.as_bool()).map(|all| {
                    if all {
                        "allPlansExecution"
                    } else {
                        "queryPlanner"
                    }
                    .to_owned()
                })
            })
            .unwrap_or_else(|| "queryPlanner".into());
        let inner = explainable(conn, plan, tag)?;
        let reply = conn
            .command(&plan.db, doc! { "explain": inner, "verbosity": verbosity })
            .await?;
        return Ok(document(statement, reply, None));
    }
    match plan.collection.as_deref() {
        Some(collection) => run_collection(conn, plan, collection, statement, max_rows, tag).await,
        None => run_database(conn, plan, statement, max_rows, tag).await,
    }
}

async fn run_collection(
    conn: &MongoConnection,
    plan: &Plan,
    collection: &str,
    statement: &str,
    max_rows: usize,
    tag: &str,
) -> Result<QueryResult, DbError> {
    let db = plan.db.as_str();
    let rows = max_rows.saturating_add(1).min(i64::MAX as usize);
    let count_of = |reply: (Vec<Document>, bool)| reply.0.first().map_or(0, |d| as_u64(d.get("n")));
    match plan.method.as_str() {
        "find" => {
            if plan.has("count") || plan.has("size") || plan.has("itcount") || plan.has("length") {
                let filter = plan.doc_arg(0, "the filter")?;
                let applies = !plan.has("count")
                    || plan
                        .modifier("count")
                        .and_then(|a| a.first())
                        .and_then(|v| v.as_bool())
                        == Some(true);
                let (skip, limit) = if applies {
                    (modifier_i64(plan, "skip")?, modifier_i64(plan, "limit")?)
                } else {
                    (None, None)
                };
                let command = count_command(conn, collection, filter, skip, limit, tag);
                let n = count_of(conn.cursor(db, command, 1).await?);
                return Ok(value::list(statement, "count", &[Bson::Int64(n as i64)]));
            }
            let (command, user_limit) = find_command(conn, plan, collection, tag, Some(rows))?;
            let (mut docs, _) = conn.cursor(db, command, rows).await?;
            let more = docs.len() > max_rows && user_limit.is_none_or(|l| l as usize > max_rows);
            docs.truncate(max_rows);
            Ok(value::table(statement, &docs, more))
        }
        "findOne" => {
            let (mut command, _) = find_command(conn, plan, collection, tag, Some(1))?;
            command.insert("singleBatch", true);
            let (docs, _) = conn.cursor(db, command, 1).await?;
            Ok(value::table(statement, &docs, false))
        }
        "aggregate" => {
            let (command, writes) =
                aggregate_command(conn, plan, Bson::String(collection.into()), tag, Some(rows))?;
            let (mut docs, _) = conn.cursor(db, command, rows).await?;
            if writes {
                return value_result(statement, None, Vec::new());
            }
            let more = docs.len() > max_rows;
            docs.truncate(max_rows);
            Ok(value::table(statement, &docs, more))
        }
        "countDocuments" | "count" => {
            let filter = plan.doc_arg(0, "the filter")?;
            let options = plan.doc_arg(1, "the options")?;
            let skip = options.get("skip").and_then(as_i64);
            let limit = options.get("limit").and_then(as_i64);
            let mut command = count_command(conn, collection, filter, skip, limit, tag);
            for key in ["hint", "collation", "maxTimeMS"] {
                if let Some(v) = options.get(key) {
                    command.insert(key, v.clone());
                }
            }
            let n = count_of(conn.cursor(db, command, 1).await?);
            Ok(value::list(statement, "count", &[Bson::Int64(n as i64)]))
        }
        "estimatedDocumentCount" => {
            let reply = conn.command(db, doc! { "count": collection }).await?;
            Ok(value::list(
                statement,
                "count",
                &[Bson::Int64(as_u64(reply.get("n")) as i64)],
            ))
        }
        "distinct" => {
            let command = distinct_command(conn, plan, collection, tag)?;
            let reply = conn.command(db, command).await?;
            let values = reply.get_array("values").cloned().unwrap_or_default();
            let Some(Bson::String(key)) = plan.arg(0)? else {
                return Err(bad(plan, "takes a field name"));
            };
            Ok(value::list(statement, &key, &values))
        }
        "insertOne" | "insertMany" | "insert" => {
            let (mut docs, ordered) = match (plan.method.as_str(), plan.arg(0)?) {
                ("insertOne", Some(Bson::Document(d))) => (vec![d], true),
                ("insertMany" | "insert", Some(Bson::Array(items))) => {
                    let docs = items
                        .into_iter()
                        .map(|d| match d {
                            Bson::Document(d) => Ok(d),
                            _ => Err(bad(plan, "every document must be an object")),
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    let ordered = plan
                        .doc_arg(1, "the options")?
                        .get_bool("ordered")
                        .unwrap_or(true);
                    (docs, ordered)
                }
                ("insert", Some(Bson::Document(d))) => (vec![d], true),
                _ => {
                    return Err(bad(
                        plan,
                        "takes a document (insertMany: an array of documents)",
                    ))
                }
            };
            if docs.is_empty() {
                return Err(bad(plan, "there is nothing to insert"));
            }
            let mut ids = Vec::with_capacity(docs.len());
            for doc in &mut docs {
                if !doc.contains_key("_id") {
                    // The id goes first, as the server would put it.
                    let mut with_id = doc! { "_id": ObjectId::new() };
                    with_id.extend(std::mem::take(doc));
                    *doc = with_id;
                }
                ids.push(doc.get("_id").cloned().unwrap_or(Bson::Null));
            }
            let mut command = doc! { "insert": collection, "documents": docs, "ordered": ordered };
            tag_and_timeout(&mut command, tag, None, false);
            let reply = conn.command(db, command).await?;
            check_write(&reply)?;
            let n = as_u64(reply.get("n"));
            let summary = if plan.method == "insertOne" {
                doc! { "acknowledged": true, "insertedId": ids.into_iter().next().unwrap_or(Bson::Null) }
            } else {
                doc! { "acknowledged": true, "insertedCount": n as i64, "insertedIds": ids }
            };
            Ok(document(statement, summary, Some(n)))
        }
        "updateOne" | "updateMany" | "replaceOne" | "update" => {
            let command = update_command(plan, collection, tag)?;
            let reply = conn.command(db, command).await?;
            check_write(&reply)?;
            let matched = as_u64(reply.get("n"));
            let modified = as_u64(reply.get("nModified"));
            let upserted = reply.get_array("upserted").cloned().unwrap_or_default();
            let upserted_id = upserted
                .first()
                .and_then(|u| u.as_document()?.get("_id").cloned())
                .unwrap_or(Bson::Null);
            let summary = doc! {
                "acknowledged": true,
                "insertedId": upserted_id,
                "matchedCount": (matched - upserted.len() as u64) as i64,
                "modifiedCount": modified as i64,
                "upsertedCount": upserted.len() as i64,
            };
            Ok(document(
                statement,
                summary,
                Some(modified + upserted.len() as u64),
            ))
        }
        "deleteOne" | "deleteMany" | "remove" => {
            let command = delete_command(plan, collection, tag)?;
            let reply = conn.command(db, command).await?;
            check_write(&reply)?;
            let n = as_u64(reply.get("n"));
            Ok(document(
                statement,
                doc! { "acknowledged": true, "deletedCount": n as i64 },
                Some(n),
            ))
        }
        "findOneAndUpdate" | "findOneAndReplace" | "findOneAndDelete" | "findAndModify" => {
            let command = find_and_modify_command(conn, plan, collection, tag)?;
            let reply = conn.command(db, command).await?;
            let docs: Vec<Document> = reply
                .get_document("value")
                .ok()
                .cloned()
                .into_iter()
                .collect();
            let mut result = value::table(statement, &docs, false);
            result.affected = Some(as_u64(
                reply
                    .get_document("lastErrorObject")
                    .ok()
                    .and_then(|l| l.get("n")),
            ));
            Ok(result)
        }
        "bulkWrite" => bulk_write(conn, plan, collection, statement, tag).await,
        "createIndex" | "ensureIndex" | "createIndexes" => {
            let options = plan.doc_arg(1, "the options")?;
            let keys: Vec<Document> = match (plan.method.as_str(), plan.arg(0)?) {
                ("createIndexes", Some(Bson::Array(items))) => items
                    .into_iter()
                    .map(|k| match k {
                        Bson::Document(d) => Ok(d),
                        _ => Err(bad(plan, "every key specification must be a document")),
                    })
                    .collect::<Result<_, _>>()?,
                (_, Some(Bson::Document(d))) if !d.is_empty() => vec![d],
                _ => return Err(bad(plan, "takes the index keys, such as { email: 1 }")),
            };
            let indexes: Vec<Document> = keys
                .iter()
                .map(|key| {
                    let mut index = doc! {
                        "key": key.clone(),
                        "name": options.get_str("name").map(str::to_owned).unwrap_or_else(|_| index_name(key)),
                    };
                    for (k, v) in &options {
                        if k != "name" {
                            index.insert(k.clone(), v.clone());
                        }
                    }
                    index
                })
                .collect();
            let names: Vec<Bson> = indexes
                .iter()
                .map(|i| Bson::String(i.get_str("name").unwrap_or("").to_owned()))
                .collect();
            let mut command = doc! { "createIndexes": collection, "indexes": indexes };
            tag_and_timeout(&mut command, tag, None, false);
            conn.command(db, command).await?;
            Ok(value::list(statement, "name", &names))
        }
        "dropIndex" | "dropIndexes" => {
            let index = match plan.arg(0)? {
                Some(v @ (Bson::String(_) | Bson::Document(_) | Bson::Array(_))) => v,
                None if plan.method == "dropIndexes" => Bson::String("*".into()),
                _ => return Err(bad(plan, "takes an index name or its keys")),
            };
            let reply = conn
                .command(db, doc! { "dropIndexes": collection, "index": index })
                .await?;
            Ok(document(statement, reply, None))
        }
        "getIndexes" | "getIndices" => {
            let docs = conn.indexes(db, collection).await?;
            Ok(value::table(statement, &docs, false))
        }
        "getIndexKeys" => {
            let keys: Vec<Bson> = conn
                .indexes(db, collection)
                .await?
                .into_iter()
                .filter_map(|i| i.get("key").cloned())
                .collect();
            Ok(value::list(statement, "key", &keys))
        }
        "drop" => {
            let dropped = match conn.command(db, doc! { "drop": collection }).await {
                Ok(_) => true,
                Err(e) if e.code.as_deref().is_some_and(|c| c.starts_with("26 ")) => false,
                Err(e) => return Err(e),
            };
            Ok(value::list(statement, "result", &[Bson::Boolean(dropped)]))
        }
        "renameCollection" => {
            let Some(Bson::String(to)) = plan.arg(0)? else {
                return Err(bad(plan, "takes the new name"));
            };
            let drop_target = matches!(plan.arg(1)?, Some(Bson::Boolean(true)));
            let reply = conn
                .command(
                    "admin",
                    doc! {
                        "renameCollection": format!("{db}.{collection}"),
                        "to": format!("{db}.{to}"),
                        "dropTarget": drop_target,
                    },
                )
                .await?;
            Ok(document(statement, reply, None))
        }
        "stats" | "dataSize" | "storageSize" | "totalIndexSize" | "totalSize" => {
            let scale = match plan.arg(0)? {
                Some(Bson::Document(o)) => o.get("scale").and_then(as_i64),
                Some(v) => as_i64(&v),
                None => None,
            };
            let mut storage = doc! {};
            if let Some(scale) = scale {
                storage.insert("scale", scale);
            }
            let (docs, _) = conn
                .cursor(
                    db,
                    doc! {
                        "aggregate": collection,
                        "pipeline": [{ "$collStats": { "storageStats": storage, "count": {} } }],
                        "cursor": {},
                    },
                    1,
                )
                .await?;
            let stats = docs
                .into_iter()
                .next()
                .and_then(|d| d.get_document("storageStats").ok().cloned())
                .unwrap_or_default();
            let pick = |k: &str| stats.get(k).cloned().unwrap_or(Bson::Int32(0));
            let one = |v: Bson| Ok(value::list(statement, &plan.method, &[v]));
            match plan.method.as_str() {
                "dataSize" => one(pick("size")),
                "storageSize" => one(pick("storageSize")),
                "totalIndexSize" => one(pick("totalIndexSize")),
                "totalSize" => one(Bson::Int64(
                    as_i64(&pick("storageSize")).unwrap_or(0)
                        + as_i64(&pick("totalIndexSize")).unwrap_or(0),
                )),
                _ => {
                    let mut stats = stats;
                    stats.remove("wiredTiger");
                    stats.remove("indexDetails");
                    Ok(document(statement, stats, None))
                }
            }
        }
        "validate" => {
            let full = match plan.arg(0)? {
                Some(Bson::Boolean(full)) => full,
                Some(Bson::Document(o)) => o.get_bool("full").unwrap_or(false),
                _ => false,
            };
            let reply = conn
                .command(db, doc! { "validate": collection, "full": full })
                .await?;
            Ok(document(statement, reply, None))
        }
        "isCapped" => {
            let capped = conn
                .info(db, collection)
                .await?
                .and_then(|i| {
                    i.get_document("options")
                        .ok()
                        .and_then(|o| o.get_bool("capped").ok())
                })
                .unwrap_or(false);
            Ok(value::list(statement, "result", &[Bson::Boolean(capped)]))
        }
        other => Err(DbError::plain(format!(
            "db.collection.{other}() is not available in a console"
        ))),
    }
}

async fn run_database(
    conn: &MongoConnection,
    plan: &Plan,
    statement: &str,
    max_rows: usize,
    tag: &str,
) -> Result<QueryResult, DbError> {
    let db = plan.db.as_str();
    let rows = max_rows.saturating_add(1).min(i64::MAX as usize);
    match plan.method.as_str() {
        "runCommand" | "adminCommand" => {
            let target = if plan.method == "adminCommand" {
                "admin"
            } else {
                db
            };
            let reply = conn.command(target, command_arg(plan)?).await?;
            // A cursor reply reads best as its documents.
            if let Ok(cursor) = reply.get_document("cursor") {
                if let Ok(batch) = cursor.get_array("firstBatch") {
                    let docs: Vec<Document> = batch
                        .iter()
                        .filter_map(|d| d.as_document().cloned())
                        .collect();
                    let more = cursor.get("id").and_then(as_i64).unwrap_or(0) != 0;
                    return Ok(value::table(statement, &docs, more));
                }
            }
            Ok(document(statement, reply, None))
        }
        "getCollectionNames" => {
            let names = conn.collection_names(db).await?;
            let values: Vec<Bson> = names.into_iter().map(Bson::String).collect();
            Ok(value::list(statement, "name", &values))
        }
        "getCollectionInfos" => {
            let (docs, _) = conn
                .cursor(
                    db,
                    doc! { "listCollections": 1, "filter": plan.doc_arg(0, "the filter")? },
                    usize::MAX,
                )
                .await?;
            Ok(value::table(statement, &docs, false))
        }
        "createCollection" => {
            let Some(Bson::String(name)) = plan.arg(0)? else {
                return Err(bad(plan, "takes the collection's name"));
            };
            let mut command = doc! { "create": name };
            command.extend(plan.doc_arg(1, "the options")?);
            Ok(document(statement, conn.command(db, command).await?, None))
        }
        "createView" => {
            let (Some(Bson::String(name)), Some(Bson::String(on))) = (plan.arg(0)?, plan.arg(1)?)
            else {
                return Err(bad(
                    plan,
                    "takes the view's name, its source collection and a pipeline",
                ));
            };
            let pipeline = match plan.arg(2)? {
                Some(Bson::Array(stages)) => stages,
                None => Vec::new(),
                _ => return Err(bad(plan, "the pipeline must be an array")),
            };
            let mut command = doc! { "create": name, "viewOn": on, "pipeline": pipeline };
            command.extend(plan.doc_arg(3, "the options")?);
            Ok(document(statement, conn.command(db, command).await?, None))
        }
        "dropDatabase" => Ok(document(
            statement,
            conn.command(db, doc! { "dropDatabase": 1 }).await?,
            None,
        )),
        "stats" => {
            let mut command = doc! { "dbStats": 1 };
            match plan.arg(0)? {
                Some(Bson::Document(o)) => command.extend(o),
                Some(v) if as_i64(&v).is_some() => {
                    command.insert("scale", v);
                }
                _ => {}
            }
            Ok(document(statement, conn.command(db, command).await?, None))
        }
        "serverStatus" => {
            let mut command = doc! { "serverStatus": 1 };
            command.extend(plan.doc_arg(0, "the options")?);
            Ok(document(
                statement,
                conn.command("admin", command).await?,
                None,
            ))
        }
        "version" => {
            let reply = conn.command("admin", doc! { "buildInfo": 1 }).await?;
            let version = reply.get("version").cloned().unwrap_or(Bson::Null);
            Ok(value::list(statement, "version", &[version]))
        }
        "buildInfo" | "serverBuildInfo" => Ok(document(
            statement,
            conn.command("admin", doc! { "buildInfo": 1 }).await?,
            None,
        )),
        "hostInfo" => Ok(document(
            statement,
            conn.command("admin", doc! { "hostInfo": 1 }).await?,
            None,
        )),
        "isMaster" | "hello" => Ok(document(
            statement,
            conn.command("admin", doc! { "hello": 1 }).await?,
            None,
        )),
        "currentOp" => {
            let (all, filter) = match plan.arg(0)? {
                Some(Bson::Boolean(all)) => (all, Document::new()),
                Some(Bson::Document(f)) => (false, f),
                _ => (false, Document::new()),
            };
            let mut stages =
                vec![doc! { "$currentOp": { "allUsers": true, "idleConnections": all } }];
            if !filter.is_empty() {
                stages.push(doc! { "$match": filter });
            }
            let (docs, more) = conn
                .cursor(
                    "admin",
                    doc! { "aggregate": 1, "pipeline": stages, "cursor": {} },
                    max_rows,
                )
                .await?;
            Ok(value::table(statement, &docs, more))
        }
        "killOp" => {
            let Some(op) = plan.arg(0)? else {
                return Err(bad(plan, "takes an operation id"));
            };
            Ok(document(
                statement,
                conn.command("admin", doc! { "killOp": 1, "op": op })
                    .await?,
                None,
            ))
        }
        "aggregate" => {
            let (command, writes) = aggregate_command(conn, plan, Bson::Int32(1), tag, Some(rows))?;
            let (mut docs, _) = conn.cursor(db, command, rows).await?;
            if writes {
                return value_result(statement, None, Vec::new());
            }
            let more = docs.len() > max_rows;
            docs.truncate(max_rows);
            Ok(value::table(statement, &docs, more))
        }
        "getProfilingStatus" => Ok(document(
            statement,
            conn.command(db, doc! { "profile": -1 }).await?,
            None,
        )),
        "setProfilingLevel" => {
            let level = plan.arg(0)?.and_then(|v| as_i64(&v)).unwrap_or(0);
            let mut command = doc! { "profile": level };
            match plan.arg(1)? {
                Some(Bson::Document(o)) => command.extend(o),
                Some(v) if as_i64(&v).is_some() => {
                    command.insert("slowms", v);
                }
                _ => {}
            }
            Ok(document(statement, conn.command(db, command).await?, None))
        }
        "getUsers" => {
            let mut command = doc! { "usersInfo": 1 };
            command.extend(plan.doc_arg(0, "the options")?);
            Ok(array_table(
                statement,
                &conn.command(db, command).await?,
                "users",
            ))
        }
        "getRoles" => {
            let mut command = doc! { "rolesInfo": 1 };
            command.extend(plan.doc_arg(0, "the options")?);
            Ok(array_table(
                statement,
                &conn.command(db, command).await?,
                "roles",
            ))
        }
        "listCommands" => {
            let reply = conn.command(db, doc! { "listCommands": 1 }).await?;
            let docs: Vec<Document> = reply
                .get_document("commands")
                .map(|c| {
                    c.iter()
                        .map(|(name, spec)| {
                            let mut row = doc! { "name": name };
                            if let Bson::Document(spec) = spec {
                                for key in ["help", "adminOnly", "secondaryOk"] {
                                    if let Some(v) = spec.get(key) {
                                        row.insert(key, v.clone());
                                    }
                                }
                            }
                            row
                        })
                        .collect()
                })
                .unwrap_or_default();
            Ok(value::table(statement, &docs, false))
        }
        other => Err(DbError::plain(format!(
            "db.{other}() is not available in a console"
        ))),
    }
}

/// `bulkWrite([{ insertOne: … }, { updateMany: … }, …], { ordered })`, one
/// operation at a time, stopping at the first that fails.
async fn bulk_write(
    conn: &MongoConnection,
    plan: &Plan,
    collection: &str,
    statement: &str,
    tag: &str,
) -> Result<QueryResult, DbError> {
    let Some(Bson::Array(ops)) = plan.arg(0)? else {
        return Err(bad(plan, "takes an array of operations"));
    };
    let mut totals = [0i64; 5]; // inserted, matched, modified, deleted, upserted
    for (index, op) in ops.into_iter().enumerate() {
        let Bson::Document(op) = op else {
            return Err(bad(plan, format!("operation {index} must be a document")));
        };
        let Some((kind, Bson::Document(spec))) = op.into_iter().next() else {
            return Err(bad(
                plan,
                format!("operation {index} must be {{ insertOne: {{ … }} }} or alike"),
            ));
        };
        let json = |b: Bson| b.into_relaxed_extjson();
        let get = |k: &str| {
            spec.get(k)
                .cloned()
                .unwrap_or(Bson::Document(Document::new()))
        };
        let mut options = Document::new();
        for k in ["upsert", "arrayFilters", "hint", "collation"] {
            if let Some(v) = spec.get(k) {
                options.insert(k, v.clone());
            }
        }
        let (method, args): (&str, Vec<serde_json::Value>) = match kind.as_str() {
            "insertOne" => ("insertOne", vec![json(get("document"))]),
            "updateOne" | "updateMany" => (
                kind.as_str(),
                vec![
                    json(get("filter")),
                    json(get("update")),
                    json(Bson::Document(options)),
                ],
            ),
            "replaceOne" => (
                "replaceOne",
                vec![
                    json(get("filter")),
                    json(get("replacement")),
                    json(Bson::Document(options)),
                ],
            ),
            "deleteOne" | "deleteMany" => (kind.as_str(), vec![json(get("filter"))]),
            other => {
                return Err(bad(
                    plan,
                    format!("operation {index}: {other} is not a bulk operation"),
                ))
            }
        };
        let step = Plan {
            db: plan.db.clone(),
            collection: Some(collection.to_owned()),
            method: method.to_owned(),
            args,
            chain: Vec::new(),
        };
        let result = Box::pin(run_collection(conn, &step, collection, statement, 1, tag))
            .await
            .map_err(|mut e| {
                e.detail = Some(format!(
                    "Operation {index} ({kind}) failed; the ones before it were applied"
                ));
                e
            })?;
        let field = |name: &str| -> i64 {
            result
                .columns
                .iter()
                .position(|c| c.name == name)
                .and_then(|i| result.rows.first()?.get(i)?.as_deref()?.parse().ok())
                .unwrap_or(0)
        };
        match method {
            "insertOne" => totals[0] += 1,
            "deleteOne" | "deleteMany" => totals[3] += field("deletedCount"),
            _ => {
                totals[1] += field("matchedCount");
                totals[2] += field("modifiedCount");
                totals[4] += field("upsertedCount");
            }
        }
    }
    let summary = doc! {
        "acknowledged": true,
        "insertedCount": totals[0],
        "matchedCount": totals[1],
        "modifiedCount": totals[2],
        "deletedCount": totals[3],
        "upsertedCount": totals[4],
    };
    let affected = (totals[0] + totals[2] + totals[3] + totals[4]) as u64;
    Ok(document(statement, summary, Some(affected)))
}
