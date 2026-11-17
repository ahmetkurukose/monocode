//! `MONOCODE_TEST_MONGO=host:port` runs these against a server (a throwaway
//! `mongod` is enough); without it the server tests skip.

use super::*;
use crate::db::conn::Connection;
use crate::db::runtime::runtime;
use crate::db::source::DbKind;

pub(crate) fn test_source(database: &str) -> Option<DataSource> {
    let spec = std::env::var("MONOCODE_TEST_MONGO").ok()?;
    let (host, port) = spec.rsplit_once(':')?;
    let mut source = DataSource::new_of(DbKind::MongoDb, "test");
    source.host = host.to_owned();
    source.port = port.parse().ok()?;
    source.database = database.to_owned();
    source.read_only = false;
    Some(source)
}

fn cells(result: &QueryResult, column: &str) -> Vec<Option<String>> {
    let at = result
        .columns
        .iter()
        .position(|c| c.name == column)
        .unwrap_or_else(|| panic!("no column {column} in {:?}", result.columns));
    result.rows.iter().map(|r| r[at].clone()).collect()
}

fn one(result: &QueryResult, column: &str) -> String {
    cells(result, column)[0].clone().unwrap_or_default()
}

#[test]
fn options_come_from_the_fields_or_the_connection_string() {
    runtime().block_on(async {
        let mut source = DataSource::new_of(DbKind::MongoDb, "x");
        source.host = "db.example.com".into();
        source.user = "app".into();
        source.auth_source = "admin".into();
        source.tls = true;
        let options = client_options(&source, Some("secret")).await.unwrap();
        assert_eq!(options.hosts[0].to_string(), "db.example.com:27017");
        assert_eq!(options.direct_connection, Some(true));
        let credential = options.credential.unwrap();
        assert_eq!(credential.username.as_deref(), Some("app"));
        assert_eq!(credential.password.as_deref(), Some("secret"));
        assert_eq!(credential.source.as_deref(), Some("admin"));
        assert!(matches!(options.tls, Some(Tls::Enabled(_))));

        source.uri = "mongodb://a:1,b:2/shop?replicaSet=rs0".into();
        source.tls = false;
        let options = client_options(&source, None).await.unwrap();
        assert_eq!(options.hosts.len(), 2);
        assert_eq!(options.repl_set_name.as_deref(), Some("rs0"));
        assert_eq!(options.default_database.as_deref(), Some("shop"));
        assert_eq!(options.direct_connection, None);

        source.uri = "postgres://nope".into();
        assert!(client_options(&source, None).await.is_err());
        // IPv6 hosts are bracketed.
        source.uri.clear();
        source.host = "::1".into();
        let options = client_options(&source, None).await.unwrap();
        assert!(
            matches!(&options.hosts[0], ServerAddress::Tcp { host, port: Some(27017) } if host == "::1"),
            "{:?}",
            options.hosts
        );
    });
}

#[test]
fn the_default_database_comes_from_the_field_then_the_connection_string() {
    let mut source = DataSource::new_of(DbKind::MongoDb, "x");
    assert_eq!(default_database(&source), "test");
    source.uri = "mongodb+srv://u@cluster0.example.net/shop?retryWrites=true".into();
    assert_eq!(default_database(&source), "shop");
    source.uri = "mongodb://h:1/?replicaSet=rs".into();
    assert_eq!(default_database(&source), "test");
    source.database = "app".into();
    assert_eq!(default_database(&source), "app");
}

#[test]
fn console_statements_run_end_to_end_on_a_server() {
    let db = format!("mc_probe_{}", std::process::id());
    let Some(source) = test_source(&db) else {
        return;
    };
    runtime().block_on(async {
        let conn = Connection::connect(&source, None).await.unwrap();
        assert_eq!(conn.kind(), DbKind::MongoDb);
        assert!(conn.version().await.unwrap().starts_with("MongoDB "));
        let run = |s: &'static str| conn.execute(s, 100);

        let r = run("db.users.insertMany([{ _id: 1, name: 'ada', age: 36, tags: ['x'] }, { _id: 2, name: 'bob', age: 25 }, { _id: 3, name: 'cy', age: 41, address: { city: 'Rome' } }])").await.unwrap();
        assert_eq!(r.affected, Some(3));
        assert_eq!(one(&r, "insertedCount"), "3");

        let r = run("db.users.find({ age: { $gt: 30 } }).sort({ age: -1 })").await.unwrap();
        assert_eq!(cells(&r, "name"), [Some("cy".into()), Some("ada".into())]);
        assert_eq!(r.columns[0].name, "_id");
        assert!(!r.has_more);

        // The row limit, and whether more exist.
        let r = conn.execute("db.users.find().sort({ _id: 1 })", 2).await.unwrap();
        assert_eq!(r.rows.len(), 2);
        assert!(r.has_more);
        let r = conn.execute("db.users.find().limit(2)", 2).await.unwrap();
        assert!(!r.has_more, "the statement's own limit is not 'more'");

        let r = run("db.users.findOne({ _id: 3 }, { address: 1 })").await.unwrap();
        assert_eq!(one(&r, "address"), r#"{"city":"Rome"}"#);

        let r = run("db.users.countDocuments({ age: { $lt: 40 } })").await.unwrap();
        assert_eq!(one(&r, "count"), "2");
        let r = run("db.users.find({}).count()").await.unwrap();
        assert_eq!(one(&r, "count"), "3");
        let r = run("db.users.distinct('name', { age: { $gte: 30 } })").await.unwrap();
        assert_eq!(cells(&r, "name").len(), 2);

        let r = run("db.users.aggregate([{ $group: { _id: null, total: { $sum: '$age' } } }])").await.unwrap();
        assert_eq!(one(&r, "total"), "102");

        let r = run("db.users.updateOne({ _id: 2 }, { $set: { age: 26 } })").await.unwrap();
        assert_eq!((one(&r, "matchedCount"), one(&r, "modifiedCount")), ("1".into(), "1".into()));
        let r = run("db.users.updateOne({ _id: 9 }, { $set: { name: 'new' } }, { upsert: true })").await.unwrap();
        assert_eq!(one(&r, "insertedId"), "9");
        let e = run("db.users.updateOne({ _id: 2 }, { age: 1 })").await.unwrap_err();
        assert!(e.message.contains("operators"), "{e:?}");
        let r = run("db.users.findOneAndUpdate({ _id: 2 }, { $inc: { age: 1 } }, { returnDocument: 'after' })").await.unwrap();
        assert_eq!(one(&r, "age"), "27");

        let e = run("db.users.insertOne({ _id: 1 })").await.unwrap_err();
        assert!(e.code.as_deref().unwrap().starts_with("11000"), "{e:?}");

        let r = run("db.users.createIndex({ name: 1 }, { unique: true })").await.unwrap();
        assert_eq!(one(&r, "name"), "name_1");
        let r = run("db.users.getIndexes()").await.unwrap();
        assert_eq!(r.rows.len(), 2);

        let r = run("db.users.bulkWrite([{ insertOne: { document: { _id: 20, name: 'z' } } }, { deleteMany: { filter: { _id: { $gte: 9 } } } }])").await.unwrap();
        assert_eq!((one(&r, "insertedCount"), one(&r, "deletedCount")), ("1".into(), "2".into()));

        let r = run("db.users.deleteOne({ _id: 3 })").await.unwrap();
        assert_eq!(one(&r, "deletedCount"), "1");

        // Shell state: variables and `use`.
        run("const wanted = 1").await.unwrap();
        let r = run("db.users.find({ _id: wanted })").await.unwrap();
        assert_eq!(cells(&r, "name"), [Some("ada".into())]);
        let r = run("use other_db").await.unwrap();
        assert_eq!(one(&r, "result"), "switched to db other_db");
        assert_eq!(run("db.getName()").await.unwrap().rows[0][0].as_deref(), Some("other_db"));
        conn.execute(&format!("use {db}"), 1).await.unwrap();

        let r = run("show collections").await.unwrap();
        assert!(cells(&r, "name").contains(&Some("users".into())));
        let r = run("show dbs").await.unwrap();
        assert!(!r.rows.is_empty());
        let r = run("db.runCommand({ ping: 1 })").await.unwrap();
        assert_eq!(one(&r, "ok"), "1");
        let r = run("print('hello', 1 + 1)").await.unwrap();
        assert_eq!(one(&r, "output"), "hello 2");

        // The tree.
        assert!(conn.schemas(false).await.unwrap().contains(&db));
        run("db.createView('adults', 'users', [{ $match: { age: { $gte: 30 } } }])").await.unwrap();
        let tables = conn.tables(&db).await.unwrap();
        let users = tables.iter().find(|t| t.name == "users").unwrap();
        assert_eq!(users.kind, TableKind::Table);
        assert_eq!(users.estimated_rows, Some(2));
        let adults = tables.iter().find(|t| t.name == "adults").unwrap();
        assert_eq!(adults.kind, TableKind::View);
        assert_eq!(adults.comment.as_deref(), Some("view on users"));
        let fields = conn.columns(&db, "users").await.unwrap();
        assert_eq!(fields[0].name, "_id");
        assert!(fields[0].primary_key);
        assert!(fields.iter().any(|f| f.name == "tags" && f.type_name == "array<string>"));
        let details = conn.table_details(&db, "users").await.unwrap();
        assert!(details.indexes.iter().any(|i| i.name == "name_1" && i.unique));
        assert!(details.indexes[0].primary);
        assert!(conn.table_details(&db, "adults").await.unwrap().indexes.is_empty());

        // The script recreates the collection with its validator and indexes.
        run("db.createCollection('checked', { validator: { $jsonSchema: { required: ['a'] } } })").await.unwrap();
        run("db.checked.createIndex({ a: -1, b: 1 }, { name: 'ab', sparse: true })").await.unwrap();
        let validator = conn.table_details(&db, "checked").await.unwrap().constraints;
        assert!(validator[0].definition.contains("$jsonSchema"));
        let script = conn.ddl(&db, "checked", TableKind::Table).await.unwrap();
        assert!(script.contains("createCollection(\"checked\""), "{script}");
        assert!(script.contains("createIndex({\"a\":-1,\"b\":1}"), "{script}");
        run("db.checked.drop()").await.unwrap();
        for statement in super::script::split(&script) {
            conn.execute(&statement.text, 10).await.unwrap();
        }
        let again = conn.table_details(&db, "checked").await.unwrap();
        assert!(again.indexes.iter().any(|i| i.name == "ab"));
        assert_eq!(again.constraints.len(), 1);
        let view = conn.ddl(&db, "adults", TableKind::View).await.unwrap();
        assert!(view.contains("createView(\"adults\", \"users\""), "{view}");

        let catalog = conn.catalog().await.unwrap();
        assert!(catalog.iter().any(|e| e.schema == db && e.table == "users" && e.column == "name"));

        // Explain does not run the write.
        let plan = conn.explain("db.users.find({ name: 'ada' })", false).await.unwrap();
        assert!(plan.contains("queryPlanner"), "{plan}");
        let stats = conn.explain("db.users.deleteMany({})", true).await.unwrap();
        assert!(stats.contains("executionStats"));
        assert_eq!(one(&run("db.users.countDocuments()").await.unwrap(), "count"), "2", "the delete was only explained");
        let r = run("db.users.find().explain()").await.unwrap();
        assert!(r.columns.iter().any(|c| c.name == "queryPlanner"));

        // Export streams past the limit; JSON keeps the documents' types.
        run("db.many.insertMany(Array.from({ length: 1500 }, (_, i) => ({ i, at: new Date(0) })))").await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        let csv = dir.path().join("many.csv");
        let n = conn
            .export("db.many.find({}, { _id: 0 }).sort({ i: 1 })", &csv, crate::db::ExportFormat::Csv, Default::default())
            .await
            .unwrap();
        assert_eq!(n, 1500);
        let text = std::fs::read_to_string(&csv).unwrap();
        assert!(text.starts_with("i,at\n0,1970-01-01T00:00:00Z\n"), "{}", &text[..40]);
        let json = dir.path().join("many.json");
        conn.export("db.many.find().limit(2)", &json, crate::db::ExportFormat::Json, Default::default())
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&json).unwrap()).unwrap();
        assert_eq!(parsed.as_array().unwrap().len(), 2);
        assert!(parsed[0]["_id"]["$oid"].is_string(), "{parsed}");
        let e = conn
            .export("db.many.deleteMany({})", &json, crate::db::ExportFormat::Json, Default::default())
            .await
            .unwrap_err();
        assert!(e.message.contains("writes"));

        // Cancelling a slow statement.
        let conn = std::sync::Arc::new(conn);
        let running = {
            let conn = conn.clone();
            runtime().spawn(async move {
                conn.execute("db.many.find({ $where: 'sleep(100) || true' })", 10).await
            })
        };
        tokio::time::sleep(Duration::from_millis(400)).await;
        conn.cancel();
        let e = running.await.unwrap().unwrap_err();
        assert!(e.cancelled, "{e:?}");
        assert_eq!(one(&conn.execute("db.users.countDocuments()", 1).await.unwrap(), "count"), "2");

        conn.execute("db.dropDatabase()", 1).await.unwrap();
    });
}

#[test]
fn a_read_only_source_refuses_writes_before_the_server_sees_them() {
    let db = format!("mc_probe_ro_{}", std::process::id());
    let Some(mut source) = test_source(&db) else {
        return;
    };
    source.read_only = true;
    runtime().block_on(async {
        let conn = Connection::connect(&source, None).await.unwrap();
        for statement in [
            "db.t.insertOne({ a: 1 })",
            "db.t.deleteMany({})",
            "db.t.aggregate([{ $match: {} }, { $out: 'copy' }])",
            "db.runCommand({ drop: 't' })",
            "db.t.drop()",
        ] {
            let e = conn.execute(statement, 1).await.unwrap_err();
            assert!(e.message.contains("read-only"), "{statement}: {e:?}");
        }
        assert!(conn.execute("db.t.find()", 1).await.is_ok());
        assert!(conn.execute("db.runCommand({ ping: 1 })", 1).await.is_ok());
    });
}

#[test]
fn a_wrong_host_fails_with_a_clear_message() {
    // Waits out the server selection timeout, so only with the server tests.
    if test_source("x").is_none() {
        return;
    }
    let mut source = DataSource::new_of(DbKind::MongoDb, "x");
    source.host = "127.0.0.1".into();
    source.port = 1;
    runtime().block_on(async {
        let started = Instant::now();
        let error = match Connection::connect(&source, None).await {
            Err(e) => e,
            Ok(_) => panic!("connected to port 1"),
        };
        assert!(
            error.message.starts_with("Could not reach the server"),
            "{error:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(20));
    });
}
