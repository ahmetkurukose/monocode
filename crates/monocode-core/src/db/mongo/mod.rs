//! MongoDB for the Database view. A console speaks a mongosh-like language
//! (`db.users.find({ … }).sort({ … })`, `show dbs`, `use shop`), evaluated in
//! a sandboxed [`shell`] and run with the official driver; documents come
//! back as grid rows ([`value`]). The object tree reads databases,
//! collections, the fields a sample of documents holds, indexes and the
//! validator.

mod run;
pub mod script;
pub mod shell;
pub mod value;

use super::conn::{DbError, QueryResult, ResultColumn};
use super::export::{ExportFormat, ExportOptions, RowWriter};
use super::introspect::{
    CatalogEntry, ColumnInfo, ConstraintInfo, IndexInfo, TableDetails, TableInfo, TableKind,
};
use super::source::DataSource;
use futures_util::StreamExt;
use mongodb::bson::{doc, Bson, Document};
use mongodb::error::{ErrorKind, WriteFailure};
use mongodb::options::{ClientOptions, ServerAddress, Tls, TlsOptions};
use mongodb::{Client, Database};
use shell::{Directive, Evaluated, Shell};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Databases the server keeps for itself.
const SYSTEM_DATABASES: [&str; 3] = ["admin", "config", "local"];
/// The database a connection starts in when the source names none.
const FALLBACK_DATABASE: &str = "test";
/// Collections whose estimated size is read for the tree.
const COUNTED_COLLECTIONS: usize = 300;
/// Documents sampled to describe a collection's fields.
const FIELD_SAMPLE: i32 = 200;

pub(crate) fn error(e: &mongodb::error::Error) -> DbError {
    match e.kind.as_ref() {
        ErrorKind::Command(c) => DbError {
            message: if c.message.is_empty() {
                c.code_name.clone()
            } else {
                c.message.clone()
            },
            code: Some(code_label(c.code, &c.code_name)),
            ..DbError::default()
        },
        ErrorKind::Write(WriteFailure::WriteError(w)) => DbError {
            message: w.message.clone(),
            code: Some(code_label(w.code, w.code_name.as_deref().unwrap_or(""))),
            ..DbError::default()
        },
        ErrorKind::Write(WriteFailure::WriteConcernError(w)) => DbError {
            message: format!("Write concern: {}", w.message),
            code: Some(code_label(w.code, &w.code_name)),
            ..DbError::default()
        },
        ErrorKind::Authentication { message, .. } => DbError::plain(format!(
            "Authentication failed: {message}. Check the user, the password and the auth database."
        )),
        ErrorKind::ServerSelection { message, .. } => DbError::plain(format!(
            "Could not reach the server: {}",
            message.lines().next().unwrap_or(message)
        )),
        _ => DbError::plain(e.to_string()),
    }
}

fn code_label(code: i32, name: &str) -> String {
    if name.is_empty() {
        code.to_string()
    } else {
        format!("{code} {name}")
    }
}

/// Whether the command failed because the user may not run it.
fn unauthorized(e: &mongodb::error::Error) -> bool {
    matches!(e.kind.as_ref(), ErrorKind::Command(c) if c.code == 13)
}

/// `host:port`, bracketing a bare IPv6 address.
fn address(host: &str, port: u16) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// The driver options for `source`: its connection string, or its host and
/// port; then the user, password, auth database and TLS from its fields.
pub async fn client_options(
    source: &DataSource,
    password: Option<&str>,
) -> Result<ClientOptions, DbError> {
    let uri = source.uri.trim();
    let mut options = if uri.is_empty() {
        if source.host.trim().is_empty() {
            return Err(DbError::plain("Enter a host or a connection string"));
        }
        let mut options = ClientOptions::default();
        options.hosts = vec![
            ServerAddress::parse(address(source.host.trim(), source.port))
                .map_err(|e| error(&e))?,
        ];
        // One host means that server, as mongosh does; a replica set is
        // reached through a connection string.
        options.direct_connection = Some(true);
        options
    } else {
        if !(uri.starts_with("mongodb://") || uri.starts_with("mongodb+srv://")) {
            return Err(DbError::plain(
                "A connection string starts with mongodb:// or mongodb+srv://",
            ));
        }
        ClientOptions::parse(uri).await.map_err(|e| error(&e))?
    };
    if source.tls && options.tls.is_none() {
        options.tls = Some(Tls::Enabled(TlsOptions::default()));
    }
    options.app_name = Some(if source.application_name.is_empty() {
        "Monocode".to_owned()
    } else {
        source.application_name.clone()
    });
    options.connect_timeout = Some(Duration::from_secs(10));
    options.server_selection_timeout = Some(Duration::from_secs(10));
    let user = source.user.trim();
    let has_user = !user.is_empty()
        || options
            .credential
            .as_ref()
            .is_some_and(|c| c.username.is_some());
    if has_user {
        let mut credential = options.credential.take().unwrap_or_default();
        if !user.is_empty() {
            credential.username = Some(user.to_owned());
        }
        if let Some(password) = password {
            credential.password = Some(password.to_owned());
        }
        if !source.auth_source.trim().is_empty() {
            credential.source = Some(source.auth_source.trim().to_owned());
        }
        options.credential = Some(credential);
    }
    Ok(options)
}

/// The database a console's `db` starts as: the source's, else the one the
/// connection string names, else `test`.
pub fn default_database(source: &DataSource) -> String {
    let named = source.database.trim();
    if !named.is_empty() {
        return named.to_owned();
    }
    let uri = source.uri.trim();
    let in_uri = uri
        .split_once("://")
        .and_then(|(_, rest)| rest.split_once('/'))
        .map(|(_, path)| path.split(['?', '#']).next().unwrap_or(""))
        .filter(|db| !db.is_empty());
    in_uri.unwrap_or(FALLBACK_DATABASE).to_owned()
}

pub struct MongoConnection {
    client: Client,
    /// Where the connection starts and the tree's completion looks first.
    default_db: String,
    /// What `db` means in the console; `use other` changes it.
    current_db: Mutex<String>,
    read_only: bool,
    timeout_ms: Option<u32>,
    shell: Mutex<Option<Arc<Shell>>>,
    cancelling: AtomicBool,
    notify: tokio::sync::Notify,
}

impl MongoConnection {
    pub async fn connect(source: &DataSource, password: Option<&str>) -> Result<Self, DbError> {
        let options = client_options(source, password).await?;
        let default_db = Some(source.database.trim().to_owned())
            .filter(|d| !d.is_empty())
            .or_else(|| options.default_database.clone())
            .unwrap_or_else(|| FALLBACK_DATABASE.to_owned());
        let client = Client::with_options(options).map_err(|e| error(&e))?;
        // The driver connects lazily; ask once so a wrong host or password
        // shows now, not at the first statement.
        client
            .database("admin")
            .run_command(doc! { "ping": 1 })
            .await
            .map_err(|e| error(&e))?;
        Ok(Self {
            client,
            current_db: Mutex::new(default_db.clone()),
            default_db,
            read_only: source.read_only,
            timeout_ms: source.statement_timeout_ms,
            shell: Mutex::new(None),
            cancelling: AtomicBool::new(false),
            notify: tokio::sync::Notify::new(),
        })
    }

    pub fn is_closed(&self) -> bool {
        false
    }

    pub fn current_db(&self) -> String {
        self.current_db
            .lock()
            .map(|db| db.clone())
            .unwrap_or_else(|_| self.default_db.clone())
    }

    pub fn default_db(&self) -> &str {
        &self.default_db
    }

    fn db(&self, name: &str) -> Database {
        self.client.database(name)
    }

    fn shell(&self) -> Result<Arc<Shell>, DbError> {
        let mut slot = self
            .shell
            .lock()
            .map_err(|_| DbError::plain("The MongoDB shell is busy"))?;
        if let Some(shell) = slot.as_ref() {
            return Ok(shell.clone());
        }
        let shell = Arc::new(Shell::new()?);
        *slot = Some(shell.clone());
        Ok(shell)
    }

    /// Stops the running statement: the wait ends at once, and the server is
    /// asked to kill the operation.
    pub fn cancel(&self) {
        self.cancelling.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    async fn cancelled(&self) {
        loop {
            let notified = self.notify.notified();
            let mut notified = std::pin::pin!(notified);
            notified.as_mut().enable();
            if self.cancelling.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }

    /// Kills, best effort, the server operations tagged `tag`.
    fn kill(&self, tag: String) {
        let admin = self.db("admin");
        super::runtime::runtime().spawn(async move {
            let work = async {
                let mut cursor = admin
                    .run_cursor_command(doc! {
                        "aggregate": 1,
                        "pipeline": [
                            { "$currentOp": { "allUsers": true, "idleCursors": true } },
                            { "$match": { "$or": [
                                { "command.comment": &tag },
                                { "cursor.originatingCommand.comment": &tag },
                            ] } },
                        ],
                        "cursor": {},
                    })
                    .await?;
                while let Some(op) = cursor.next().await {
                    let op = op?;
                    if let Some(id) = op.get("opid") {
                        let _ = admin
                            .run_command(doc! { "killOp": 1, "op": id.clone() })
                            .await;
                    }
                }
                Ok::<_, mongodb::error::Error>(())
            };
            let _ = tokio::time::timeout(Duration::from_secs(5), work).await;
        });
    }

    /// Runs `work`, unless the statement is cancelled first.
    async fn guarded<T>(
        &self,
        tag: &str,
        work: impl std::future::Future<Output = Result<T, DbError>>,
    ) -> Result<T, DbError> {
        tokio::select! {
            out = work => out,
            _ = self.cancelled() => {
                self.kill(tag.to_owned());
                Err(DbError {
                    message: "The statement was cancelled".into(),
                    cancelled: true,
                    ..DbError::default()
                })
            }
        }
    }

    /// Evaluates and runs one console statement.
    pub async fn execute(&self, statement: &str, max_rows: usize) -> Result<QueryResult, DbError> {
        self.cancelling.store(false, Ordering::Release);
        let started = Instant::now();
        let shell = self.shell()?;
        let evaluated = shell.eval(statement, &self.current_db()).await?;
        let mut result = match evaluated {
            Evaluated::Directive(Directive::Use(name)) => {
                if let Ok(mut db) = self.current_db.lock() {
                    *db = name.clone();
                }
                message(statement, &format!("switched to db {name}"))
            }
            Evaluated::Directive(Directive::Show(what)) => {
                let tag = run::tag();
                self.guarded(&tag, self.show(statement, &what)).await?
            }
            Evaluated::Value(value, printed) => run::value_result(statement, value, printed)?,
            Evaluated::Plan(plan) => {
                if self.read_only && run::writes(&plan) {
                    return Err(read_only_error(&plan));
                }
                let tag = run::tag();
                self.guarded(&tag, run::run(self, &plan, statement, max_rows, &tag))
                    .await?
            }
        };
        let elapsed = started.elapsed().as_secs_f64() * 1000.0;
        result.execution_ms = elapsed;
        result.elapsed_ms = elapsed;
        Ok(result)
    }

    async fn show(&self, statement: &str, what: &str) -> Result<QueryResult, DbError> {
        let current = self.current_db();
        match what {
            "dbs" | "databases" => {
                let reply = self
                    .db("admin")
                    .run_command(doc! { "listDatabases": 1, "authorizedDatabases": true })
                    .await
                    .map_err(|e| error(&e))?;
                let docs: Vec<Document> = reply
                    .get_array("databases")
                    .map(|a| a.iter().filter_map(|d| d.as_document().cloned()).collect())
                    .unwrap_or_default();
                Ok(value::table(statement, &docs, false))
            }
            "collections" | "tables" => {
                let names = self.collection_names(&current).await?;
                let values: Vec<Bson> = names.into_iter().map(Bson::String).collect();
                Ok(value::list(statement, "name", &values))
            }
            "users" => {
                let reply = self.command(&current, doc! { "usersInfo": 1 }).await?;
                Ok(run::array_table(statement, &reply, "users"))
            }
            "roles" => {
                let reply = self.command(&current, doc! { "rolesInfo": 1 }).await?;
                Ok(run::array_table(statement, &reply, "roles"))
            }
            "profile" => {
                let docs = self
                    .cursor(
                        &current,
                        doc! { "find": "system.profile", "sort": { "ts": -1 }, "limit": 5 },
                        5,
                    )
                    .await?
                    .0;
                Ok(value::table(statement, &docs, false))
            }
            "log" | "logs" => {
                let reply = self
                    .command("admin", doc! { "getLog": "global" })
                    .await?;
                Ok(run::array_table(statement, &reply, "log"))
            }
            other => Err(DbError::plain(format!(
                "show {other}: try show dbs, show collections, show users, show roles, show profile or show log"
            ))),
        }
    }

    /// Runs a command on `db` and returns the reply.
    async fn command(&self, db: &str, command: Document) -> Result<Document, DbError> {
        self.db(db)
            .run_command(command)
            .await
            .map_err(|e| error(&e))
    }

    /// Runs a cursor command and reads at most `max` documents; the flag
    /// says more were there.
    async fn cursor(
        &self,
        db: &str,
        command: Document,
        max: usize,
    ) -> Result<(Vec<Document>, bool), DbError> {
        let mut cursor = self
            .db(db)
            .run_cursor_command(command)
            .await
            .map_err(|e| error(&e))?;
        let mut docs = Vec::new();
        while let Some(doc) = cursor.next().await {
            let doc = doc.map_err(|e| error(&e))?;
            if docs.len() >= max {
                return Ok((docs, true));
            }
            docs.push(doc);
        }
        Ok((docs, false))
    }

    /// The server's name, version and topology.
    pub async fn version(&self) -> Result<String, DbError> {
        let info = self.command("admin", doc! { "buildInfo": 1 }).await?;
        let version = info.get_str("version").unwrap_or("?").to_owned();
        let hello = self.command("admin", doc! { "hello": 1 }).await.ok();
        let topology = hello.as_ref().and_then(|h| {
            if let Ok(set) = h.get_str("setName") {
                Some(format!("replica set {set}"))
            } else if h.get_str("msg").is_ok_and(|m| m == "isdbgrid") {
                Some("sharded cluster".to_owned())
            } else {
                None
            }
        });
        Ok(match topology {
            Some(t) => format!("MongoDB {version} · {t}"),
            None => format!("MongoDB {version}"),
        })
    }

    /// The plan the server would use, as indented JSON. `analyze` asks for
    /// execution statistics; MongoDB's explain never applies writes.
    pub async fn explain(&self, statement: &str, analyze: bool) -> Result<String, DbError> {
        self.cancelling.store(false, Ordering::Release);
        let shell = self.shell()?;
        let Evaluated::Plan(plan) = shell.eval(statement, &self.current_db()).await? else {
            return Err(DbError::plain(
                "Explain takes a database call, such as db.users.find({ … })",
            ));
        };
        let tag = run::tag();
        let inner = run::explainable(self, &plan, &tag)?;
        let verbosity = if analyze {
            "executionStats"
        } else {
            "queryPlanner"
        };
        let reply = self
            .guarded(
                &tag,
                self.command(&plan.db, doc! { "explain": inner, "verbosity": verbosity }),
            )
            .await?;
        Ok(value::pretty(&Bson::Document(reply)))
    }

    /// Runs `statement` again without the row limit and streams what it
    /// returns to `path`. Only statements that read may run twice.
    pub async fn export(
        &self,
        statement: &str,
        path: &std::path::Path,
        format: ExportFormat,
        options: ExportOptions,
    ) -> Result<u64, DbError> {
        self.cancelling.store(false, Ordering::Release);
        let shell = self.shell()?;
        let evaluated = shell.eval(statement, &self.current_db()).await?;
        let io_error = |e: std::io::Error| DbError::plain(format!("Could not write the file: {e}"));
        let tag = run::tag();
        let source = match &evaluated {
            Evaluated::Plan(plan) if run::writes(plan) => {
                return Err(DbError::plain(
                    "That statement writes; only statements that read are exported again",
                ));
            }
            Evaluated::Plan(plan) => run::cursor_source(self, plan, &tag)?,
            _ => None,
        };
        let file = std::io::BufWriter::new(std::fs::File::create(path).map_err(io_error)?);
        let Some((db, command)) = source else {
            // Not a cursor: what it shows is all there is.
            let result = self.execute(statement, usize::MAX).await?;
            let columns: Vec<String> = result.columns.iter().map(|c| c.name.clone()).collect();
            let mut writer = RowWriter::new(file, format, columns, options).map_err(io_error)?;
            for row in &result.rows {
                let cells: Vec<Option<&str>> = row.iter().map(|c| c.as_deref()).collect();
                writer.row(&cells).map_err(io_error)?;
            }
            return writer.finish().map_err(io_error);
        };
        let work = async {
            let mut cursor = self
                .db(&db)
                .run_cursor_command(command)
                .await
                .map_err(|e| error(&e))?;
            if format == ExportFormat::Json {
                return stream_json(file, &mut cursor).await;
            }
            // The columns come from the first documents; later fields that
            // none of those had are left out.
            let mut head: Vec<Document> = Vec::new();
            let mut exhausted = true;
            while let Some(doc) = cursor.next().await {
                head.push(doc.map_err(|e| error(&e))?);
                if head.len() >= 1000 {
                    exhausted = false;
                    break;
                }
            }
            let columns = value::columns(&head);
            let mut writer =
                RowWriter::new(file, format, columns.clone(), options).map_err(io_error)?;
            let mut write = |doc: &Document| {
                let cells = value::row(doc, &columns);
                let cells: Vec<Option<&str>> = cells.iter().map(|c| c.as_deref()).collect();
                writer.row(&cells).map_err(io_error)
            };
            for doc in &head {
                write(doc)?;
            }
            if !exhausted {
                while let Some(doc) = cursor.next().await {
                    write(&doc.map_err(|e| error(&e))?)?;
                }
            }
            writer.finish().map_err(io_error)
        };
        self.guarded(&tag, work).await
    }

    // ---- the object tree ----

    /// Database names, the server's own ones only with `system`. A user who
    /// may not list databases still sees the source's database.
    pub async fn databases(&self, system: bool) -> Result<Vec<String>, DbError> {
        let reply = self
            .db("admin")
            .run_command(doc! { "listDatabases": 1, "nameOnly": true, "authorizedDatabases": true })
            .await;
        let mut names: Vec<String> = match reply {
            Ok(reply) => reply
                .get_array("databases")
                .map(|a| {
                    a.iter()
                        .filter_map(|d| d.as_document()?.get_str("name").ok().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default(),
            Err(e) if unauthorized(&e) => Vec::new(),
            Err(e) => return Err(error(&e)),
        };
        if !system {
            names.retain(|n| !SYSTEM_DATABASES.contains(&n.as_str()));
        }
        // An empty database is not listed by the server, but it is where the
        // source points, and where new collections will go.
        if !names.contains(&self.default_db) {
            names.push(self.default_db.clone());
        }
        names.sort();
        Ok(names)
    }

    async fn collection_names(&self, db: &str) -> Result<Vec<String>, DbError> {
        let (docs, _) = self
            .cursor(
                db,
                doc! { "listCollections": 1, "nameOnly": true, "authorizedCollections": true },
                usize::MAX,
            )
            .await?;
        let mut names: Vec<String> = docs
            .iter()
            .filter_map(|d| d.get_str("name").ok().map(str::to_owned))
            .collect();
        names.sort();
        Ok(names)
    }

    /// The collection descriptions of `db` (`listCollections`), falling
    /// back to names only for users who may not read more.
    async fn collection_infos(&self, db: &str) -> Result<Vec<Document>, DbError> {
        let full = self
            .db(db)
            .run_cursor_command(doc! { "listCollections": 1 })
            .await;
        let mut cursor = match full {
            Ok(cursor) => cursor,
            Err(e) if unauthorized(&e) => self
                .db(db)
                .run_cursor_command(
                    doc! { "listCollections": 1, "nameOnly": true, "authorizedCollections": true },
                )
                .await
                .map_err(|e| error(&e))?,
            Err(e) => return Err(error(&e)),
        };
        let mut out = Vec::new();
        while let Some(doc) = cursor.next().await {
            out.push(doc.map_err(|e| error(&e))?);
        }
        Ok(out)
    }

    /// Collections and views of `db`, with estimated document counts.
    /// `system.*` collections are left out.
    pub async fn collections(&self, db: &str) -> Result<Vec<TableInfo>, DbError> {
        let mut infos: Vec<Document> = self
            .collection_infos(db)
            .await?
            .into_iter()
            .filter(|d| d.get_str("name").is_ok_and(|n| !n.starts_with("system.")))
            .collect();
        infos.sort_by(|a, b| {
            a.get_str("name")
                .unwrap_or("")
                .cmp(b.get_str("name").unwrap_or(""))
        });
        let database = self.db(db);
        let counts = futures_util::future::join_all(infos.iter().enumerate().map(|(i, info)| {
            let database = database.clone();
            let name = info.get_str("name").unwrap_or("").to_owned();
            let countable = i < COUNTED_COLLECTIONS
                && matches!(info.get_str("type"), Ok("collection") | Err(_));
            async move {
                if !countable {
                    return None;
                }
                let reply = database.run_command(doc! { "count": &name }).await.ok()?;
                match reply.get("n")? {
                    Bson::Int32(n) => u64::try_from(*n).ok(),
                    Bson::Int64(n) => u64::try_from(*n).ok(),
                    Bson::Double(n) => Some(*n as u64),
                    _ => None,
                }
            }
        }))
        .await;
        Ok(infos
            .iter()
            .zip(counts)
            .map(|(info, estimated_rows)| {
                let options = info.get_document("options").ok();
                let (kind, comment) = match info.get_str("type") {
                    Ok("view") => (
                        TableKind::View,
                        options
                            .and_then(|o| o.get_str("viewOn").ok())
                            .map(|on| format!("view on {on}")),
                    ),
                    Ok("timeseries") => (TableKind::Table, Some("time series".to_owned())),
                    _ => (
                        TableKind::Table,
                        options
                            .filter(|o| o.get_bool("capped").unwrap_or(false))
                            .map(|_| "capped".to_owned()),
                    ),
                };
                TableInfo {
                    name: info.get_str("name").unwrap_or("").to_owned(),
                    kind,
                    estimated_rows,
                    comment,
                }
            })
            .collect())
    }

    /// A sample of the collection's documents (random when the server can).
    async fn sample(
        &self,
        db: &str,
        collection: &str,
        size: i32,
    ) -> Result<Vec<Document>, DbError> {
        let sampled = self
            .cursor(
                db,
                doc! {
                    "aggregate": collection,
                    "pipeline": [{ "$sample": { "size": size } }],
                    "cursor": {},
                    "maxTimeMS": 10_000,
                },
                size as usize,
            )
            .await;
        match sampled {
            Ok((docs, _)) => Ok(docs),
            Err(_) => Ok(self
                .cursor(
                    db,
                    doc! { "find": collection, "limit": size, "maxTimeMS": 10_000 },
                    size as usize,
                )
                .await?
                .0),
        }
    }

    /// The fields a sample of the collection's documents holds.
    pub async fn fields(&self, db: &str, collection: &str) -> Result<Vec<ColumnInfo>, DbError> {
        let docs = self.sample(db, collection, FIELD_SAMPLE).await?;
        Ok(value::infer_fields(&docs))
    }

    async fn info(&self, db: &str, collection: &str) -> Result<Option<Document>, DbError> {
        let (docs, _) = self
            .cursor(
                db,
                doc! { "listCollections": 1, "filter": { "name": collection } },
                1,
            )
            .await?;
        Ok(docs.into_iter().next())
    }

    async fn indexes(&self, db: &str, collection: &str) -> Result<Vec<Document>, DbError> {
        match self
            .cursor(db, doc! { "listIndexes": collection }, usize::MAX)
            .await
        {
            Ok((docs, _)) => Ok(docs),
            // Views have no indexes; a missing collection has none either.
            Err(e) if matches!(e.code.as_deref(), Some(c) if c.starts_with("166 ") || c.starts_with("26 ")) => {
                Ok(Vec::new())
            }
            Err(e) => Err(e),
        }
    }

    /// Indexes and the validator.
    pub async fn collection_details(
        &self,
        db: &str,
        collection: &str,
    ) -> Result<TableDetails, DbError> {
        let indexes = self.indexes(db, collection).await?;
        let info = self.info(db, collection).await.ok().flatten();
        let mut constraints = Vec::new();
        if let Some(options) = info.as_ref().and_then(|i| i.get_document("options").ok()) {
            if let Ok(validator) = options.get_document("validator") {
                let mut rule = doc! { "validator": validator.clone() };
                for key in ["validationLevel", "validationAction"] {
                    if let Some(v) = options.get(key) {
                        rule.insert(key, v.clone());
                    }
                }
                constraints.push(ConstraintInfo {
                    name: "validator".into(),
                    kind: 'c',
                    definition: value::pretty(&Bson::Document(rule)),
                });
            }
        }
        Ok(TableDetails {
            indexes: indexes.iter().map(index_info).collect(),
            constraints,
            referenced_by: Vec::new(),
            triggers: Vec::new(),
        })
    }

    /// The shell commands that recreate the collection (or view) and its indexes.
    pub async fn collection_script(&self, db: &str, collection: &str) -> Result<String, DbError> {
        let Some(info) = self.info(db, collection).await? else {
            return Err(DbError::plain(format!(
                "Collection {db}.{collection} not found"
            )));
        };
        let target = format!("db.getSiblingDB({})", js_string(db));
        let mut options = info.get_document("options").cloned().unwrap_or_default();
        let mut out = String::new();
        if info.get_str("type") == Ok("view") {
            let view_on = options.get_str("viewOn").unwrap_or("").to_owned();
            let pipeline = options
                .get("pipeline")
                .cloned()
                .unwrap_or_else(|| Bson::Array(Vec::new()));
            options.remove("viewOn");
            options.remove("pipeline");
            out.push_str(&format!(
                "{target}.createView({}, {}, {}{});\n",
                js_string(collection),
                js_string(&view_on),
                value::pretty(&pipeline),
                if options.is_empty() {
                    String::new()
                } else {
                    format!(", {}", value::pretty(&Bson::Document(options)))
                }
            ));
            return Ok(out);
        }
        out.push_str(&format!(
            "{target}.createCollection({}{});\n",
            js_string(collection),
            if options.is_empty() {
                String::new()
            } else {
                format!(", {}", value::pretty(&Bson::Document(options)))
            }
        ));
        for index in self.indexes(db, collection).await? {
            if index.get_str("name") == Ok("_id_") {
                continue;
            }
            let key = index
                .get("key")
                .cloned()
                .unwrap_or(Bson::Document(Document::new()));
            let mut rest = index.clone();
            for k in ["v", "key", "ns"] {
                rest.remove(k);
            }
            out.push_str(&format!(
                "\n{target}.getCollection({}).createIndex({}, {});\n",
                js_string(collection),
                value::json(&key),
                value::json(&Bson::Document(rest))
            ));
        }
        Ok(out)
    }

    /// Completion's catalog: every database's collections, and the fields
    /// of the source database's collections.
    pub async fn catalog(&self) -> Result<Vec<CatalogEntry>, DbError> {
        let mut out = Vec::new();
        let databases = self.databases(false).await?;
        for db in databases.iter().take(30) {
            // Names and kinds only: counting every collection is for the tree.
            let infos: Vec<TableInfo> = self
                .collection_infos(db)
                .await
                .unwrap_or_default()
                .iter()
                .filter_map(|info| {
                    let name = info.get_str("name").ok()?;
                    (!name.starts_with("system.")).then(|| TableInfo {
                        name: name.to_owned(),
                        kind: if info.get_str("type") == Ok("view") {
                            TableKind::View
                        } else {
                            TableKind::Table
                        },
                        estimated_rows: None,
                        comment: None,
                    })
                })
                .collect();
            let describe = *db == self.default_db;
            for (i, info) in infos.iter().enumerate() {
                let fields = if describe && i < 50 {
                    self.sample(db, &info.name, 20)
                        .await
                        .map(|docs| value::infer_fields(&docs))
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };
                if fields.is_empty() {
                    out.push(CatalogEntry {
                        schema: db.clone(),
                        table: info.name.clone(),
                        kind: info.kind,
                        column: String::new(),
                        type_name: String::new(),
                    });
                }
                for field in fields {
                    out.push(CatalogEntry {
                        schema: db.clone(),
                        table: info.name.clone(),
                        kind: info.kind,
                        column: field.name,
                        type_name: field.type_name,
                    });
                }
            }
            if infos.is_empty() {
                out.push(CatalogEntry {
                    schema: db.clone(),
                    table: String::new(),
                    kind: TableKind::Table,
                    column: String::new(),
                    type_name: String::new(),
                });
            }
        }
        Ok(out)
    }
}

fn read_only_error(plan: &shell::Plan) -> DbError {
    DbError {
        message: format!(
            "This data source is read-only, and {} would change data. Turn off Read-only in the data source's settings to run it.",
            plan.describe()
        ),
        ..DbError::default()
    }
}

/// A one-cell result carrying a message.
fn message(statement: &str, text: &str) -> QueryResult {
    QueryResult {
        statement: statement.to_owned(),
        columns: vec![ResultColumn {
            name: "result".into(),
        }],
        rows: vec![vec![Some(text.to_owned())]],
        affected: None,
        has_more: false,
        execution_ms: 0.,
        elapsed_ms: 0.,
    }
}

/// A string as a JavaScript literal.
pub fn js_string(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| "\"\"".into())
}

/// How the tree shows an index: its keys, then what is special about it.
fn index_info(index: &Document) -> IndexInfo {
    let name = index.get_str("name").unwrap_or("").to_owned();
    let key = index
        .get("key")
        .map(value::json)
        .unwrap_or_else(|| "{}".into());
    let mut flags = Vec::new();
    if index.get_bool("sparse").unwrap_or(false) {
        flags.push("sparse".to_owned());
    }
    if let Some(ttl) = index.get("expireAfterSeconds") {
        flags.push(format!("ttl {}s", value::cell(ttl).unwrap_or_default()));
    }
    if index.contains_key("partialFilterExpression") {
        flags.push("partial".into());
    }
    if index.get_bool("hidden").unwrap_or(false) {
        flags.push("hidden".into());
    }
    let definition = if flags.is_empty() {
        key
    } else {
        format!("{key} · {}", flags.join(", "))
    };
    IndexInfo {
        primary: name == "_id_",
        unique: index.get_bool("unique").unwrap_or(false) || name == "_id_",
        name,
        definition,
    }
}

/// Streams a cursor's documents to `out` as a JSON array of relaxed
/// Extended JSON documents.
async fn stream_json<W: std::io::Write>(
    mut out: W,
    cursor: &mut mongodb::Cursor<Document>,
) -> Result<u64, DbError> {
    let io_error = |e: std::io::Error| DbError::plain(format!("Could not write the file: {e}"));
    out.write_all(b"[").map_err(io_error)?;
    let mut count = 0u64;
    while let Some(doc) = cursor.next().await {
        let doc = doc.map_err(|e| error(&e))?;
        out.write_all(if count == 0 { b"\n  " } else { b",\n  " })
            .map_err(io_error)?;
        out.write_all(value::json(&Bson::Document(doc)).as_bytes())
            .map_err(io_error)?;
        count += 1;
    }
    out.write_all(if count == 0 { b"]\n" } else { b"\n]\n" })
        .map_err(io_error)?;
    out.flush().map_err(io_error)?;
    Ok(count)
}

#[cfg(test)]
pub(crate) mod tests;
