//! A PostgreSQL connection that runs statements over the simple query
//! protocol: the server sends every value as text, so no per-type decoding is
//! needed and any column type (json, arrays, enums, ranges…) just works.

use super::runtime::runtime;
use super::source::DataSource;
use futures_util::StreamExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;
use tokio_postgres::error::ErrorPosition;
use tokio_postgres::{Client, Config, NoTls, SimpleQueryMessage};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultColumn {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QueryResult {
    pub statement: String,
    pub columns: Vec<ResultColumn>,
    /// Cell text; `None` is SQL NULL.
    pub rows: Vec<Vec<Option<String>>>,
    /// Rows changed or selected, as the server reports on completion.
    pub affected: Option<u64>,
    /// More rows exist beyond the limit.
    pub has_more: bool,
    /// Until the server's first answer.
    pub execution_ms: f64,
    /// Until the last row.
    pub elapsed_ms: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DbError {
    pub message: String,
    pub detail: Option<String>,
    pub hint: Option<String>,
    /// Byte offset into the statement that failed.
    pub position: Option<usize>,
    /// SQLSTATE.
    pub code: Option<String>,
    pub cancelled: bool,
}

impl std::fmt::Display for DbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl DbError {
    fn plain(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            ..Self::default()
        }
    }

    fn from_pg(error: &tokio_postgres::Error, statement: &str, cancelled: bool) -> Self {
        let Some(db) = error.as_db_error() else {
            // Connection-level failures carry their cause in the source chain.
            let mut message = error.to_string();
            let mut source = std::error::Error::source(error);
            while let Some(cause) = source {
                message = format!("{message}: {cause}");
                source = cause.source();
            }
            return Self::plain(message);
        };
        let position = match db.position() {
            Some(ErrorPosition::Original(chars)) => Some(
                statement
                    .char_indices()
                    .nth((*chars as usize).saturating_sub(1))
                    .map_or(statement.len(), |(byte, _)| byte),
            ),
            _ => None,
        };
        let code = db.code().code().to_owned();
        Self {
            message: db.message().to_owned(),
            detail: db.detail().map(str::to_owned),
            hint: db.hint().map(str::to_owned),
            position,
            cancelled: cancelled && code == "57014",
            code: Some(code),
        }
    }
}

/// `-c` options that make the session what the data source asks for.
fn session_options(source: &DataSource) -> String {
    let mut options = Vec::new();
    if source.read_only {
        options.push("-c default_transaction_read_only=on".to_owned());
    }
    if let Some(ms) = source.statement_timeout_ms {
        options.push(format!("-c statement_timeout={ms}"));
    }
    if let Some(path) = source.search_path.as_deref().filter(|p| !p.is_empty()) {
        options.push(format!("-c search_path={}", path.replace(' ', "\\ ")));
    }
    options.join(" ")
}

pub fn config(source: &DataSource, password: Option<&str>) -> Config {
    let mut config = Config::new();
    config
        .host(&source.host)
        .port(source.port)
        .user(&source.user)
        .connect_timeout(Duration::from_secs(10))
        .application_name(if source.application_name.is_empty() {
            "Monocode"
        } else {
            &source.application_name
        });
    if !source.database.is_empty() {
        config.dbname(&source.database);
    }
    if let Some(password) = password {
        config.password(password);
    }
    let options = session_options(source);
    if !options.is_empty() {
        config.options(&options);
    }
    config
}

pub struct Connection {
    client: Client,
    driver: JoinHandle<()>,
    cancelling: AtomicBool,
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.driver.abort();
    }
}

impl Connection {
    pub async fn connect(source: &DataSource, password: Option<&str>) -> Result<Self, DbError> {
        Self::connect_with(config(source, password)).await
    }

    pub async fn connect_with(config: Config) -> Result<Self, DbError> {
        let (client, connection) = config
            .connect(NoTls)
            .await
            .map_err(|e| DbError::from_pg(&e, "", false))?;
        let driver = runtime().spawn(async move {
            let _ = connection.await;
        });
        Ok(Self {
            client,
            driver,
            cancelling: AtomicBool::new(false),
        })
    }

    pub fn is_closed(&self) -> bool {
        self.client.is_closed()
    }

    /// Asks the server to stop the running statement. The statement's
    /// `execute` then fails with `cancelled` set.
    pub fn cancel(&self) {
        self.cancelling.store(true, Ordering::Release);
        let token = self.client.cancel_token();
        runtime().spawn(async move {
            let _ = token.cancel_query(NoTls).await;
        });
    }

    /// Runs `statement` (one statement; use [`super::split`] for scripts) and
    /// keeps at most `max_rows` rows of the last result set.
    pub async fn execute(&self, statement: &str, max_rows: usize) -> Result<QueryResult, DbError> {
        self.cancelling.store(false, Ordering::Release);
        let started = Instant::now();
        let stream = self
            .client
            .simple_query_raw(statement)
            .await
            .map_err(|e| self.error(&e, statement))?;
        let mut stream = std::pin::pin!(stream);
        let mut first: Option<Duration> = None;
        let mut columns: Vec<ResultColumn> = Vec::new();
        let mut rows: Vec<Vec<Option<String>>> = Vec::new();
        let mut affected = None;
        let mut has_more = false;
        while let Some(item) = stream.next().await {
            first.get_or_insert_with(|| started.elapsed());
            match item.map_err(|e| self.error(&e, statement))? {
                SimpleQueryMessage::RowDescription(described) => {
                    columns = described
                        .iter()
                        .map(|c| ResultColumn {
                            name: c.name().to_owned(),
                        })
                        .collect();
                    rows.clear();
                    has_more = false;
                }
                SimpleQueryMessage::Row(row) => {
                    if rows.len() >= max_rows {
                        has_more = true;
                        break;
                    }
                    rows.push(
                        (0..row.len())
                            .map(|i| row.get(i).map(str::to_owned))
                            .collect(),
                    );
                }
                SimpleQueryMessage::CommandComplete(n) => affected = Some(n),
                _ => {}
            }
        }
        if has_more {
            // The rest is not wanted: stop the server computing it, then
            // read the stream out so the connection is idle again.
            self.cancel();
            while stream.next().await.is_some() {}
            self.cancelling.store(false, Ordering::Release);
        }
        let elapsed = started.elapsed();
        Ok(QueryResult {
            statement: statement.to_owned(),
            columns,
            rows,
            affected,
            has_more,
            execution_ms: first.unwrap_or(elapsed).as_secs_f64() * 1000.0,
            elapsed_ms: elapsed.as_secs_f64() * 1000.0,
        })
    }

    fn error(&self, error: &tokio_postgres::Error, statement: &str) -> DbError {
        DbError::from_pg(error, statement, self.cancelling.load(Ordering::Acquire))
    }

    /// Runs `statement` and streams every row to `path`, without the row
    /// limit and without holding the result in memory.
    pub async fn export(
        &self,
        statement: &str,
        path: &std::path::Path,
        format: super::export::ExportFormat,
        options: super::export::ExportOptions,
    ) -> Result<u64, DbError> {
        use super::export::RowWriter;
        self.cancelling.store(false, Ordering::Release);
        let io_error = |e: std::io::Error| DbError::plain(format!("Could not write the file: {e}"));
        let file = std::fs::File::create(path).map_err(io_error)?;
        let stream = self
            .client
            .simple_query_raw(statement)
            .await
            .map_err(|e| self.error(&e, statement))?;
        let mut stream = std::pin::pin!(stream);
        let mut writer: Option<RowWriter<std::io::BufWriter<std::fs::File>>> = None;
        let mut file = Some(std::io::BufWriter::new(file));
        let mut columns: Vec<String> = Vec::new();
        while let Some(item) = stream.next().await {
            match item.map_err(|e| self.error(&e, statement))? {
                SimpleQueryMessage::RowDescription(described) => {
                    columns = described.iter().map(|c| c.name().to_owned()).collect();
                }
                SimpleQueryMessage::Row(row) => {
                    if writer.is_none() {
                        let out = file.take().expect("opened once");
                        writer = Some(
                            RowWriter::new(out, format, columns.clone(), options.clone())
                                .map_err(io_error)?,
                        );
                    }
                    let cells: Vec<Option<&str>> = (0..row.len()).map(|i| row.get(i)).collect();
                    writer
                        .as_mut()
                        .expect("created above")
                        .row(&cells)
                        .map_err(io_error)?;
                }
                _ => {}
            }
        }
        let writer = match writer {
            Some(writer) => writer,
            None => RowWriter::new(file.take().expect("never opened"), format, columns, options)
                .map_err(io_error)?,
        };
        writer.finish().map_err(io_error)
    }

    /// Catalog queries: every row, no limit.
    pub(super) async fn rows(&self, sql: &str) -> Result<Vec<Vec<Option<String>>>, DbError> {
        let mut out = Vec::new();
        for message in self
            .client
            .simple_query(sql)
            .await
            .map_err(|e| self.error(&e, sql))?
        {
            if let SimpleQueryMessage::Row(row) = message {
                out.push(
                    (0..row.len())
                        .map(|i| row.get(i).map(str::to_owned))
                        .collect(),
                );
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// `MONOCODE_TEST_PG=host:port:user:database`; tests needing a server skip without it.
    pub(crate) fn test_source() -> Option<DataSource> {
        let spec = std::env::var("MONOCODE_TEST_PG").ok()?;
        let mut parts = spec.splitn(4, ':');
        let mut source = DataSource::new("test");
        source.host = parts.next()?.to_owned();
        source.port = parts.next()?.parse().ok()?;
        source.user = parts.next()?.to_owned();
        source.database = parts.next()?.to_owned();
        source.read_only = false;
        Some(source)
    }

    #[test]
    fn the_session_asks_for_read_only_and_the_other_settings() {
        let mut source = DataSource::new("x");
        source.statement_timeout_ms = Some(60_000);
        source.search_path = Some("a, b".into());
        let options = session_options(&source);
        assert_eq!(
            options,
            "-c default_transaction_read_only=on -c statement_timeout=60000 -c search_path=a,\\ b"
        );
        source.read_only = false;
        source.statement_timeout_ms = None;
        source.search_path = None;
        assert_eq!(session_options(&source), "");
    }

    #[test]
    fn debug_output_never_shows_a_password() {
        let source = DataSource::new("x");
        assert!(!format!("{source:?}").to_lowercase().contains("password"));
    }

    #[test]
    fn queries_limits_json_nulls_errors_and_cancel_work_on_a_server() {
        let Some(mut source) = test_source() else {
            return;
        };
        runtime().block_on(async {
            let conn = Connection::connect(&source, None).await.unwrap();
            let r = conn
                .execute(
                    "select 1 as n, null::text as t, '{\"a\": [1, 2]}'::jsonb as j, array[1,2] as a, 'é' as e",
                    10,
                )
                .await
                .unwrap();
            assert_eq!(r.columns.len(), 5);
            assert_eq!(
                r.rows[0],
                [
                    Some("1".into()),
                    None,
                    Some("{\"a\": [1, 2]}".into()),
                    Some("{1,2}".into()),
                    Some("é".into())
                ]
            );
            assert_eq!(r.affected, Some(1));

            let r = conn.execute("select g from generate_series(1, 1000000) g", 5).await.unwrap();
            assert_eq!(r.rows.len(), 5);
            assert!(r.has_more);
            // The connection is usable straight after a truncated read.
            let r = conn.execute("select 42", 5).await.unwrap();
            assert_eq!(r.rows[0][0].as_deref(), Some("42"));

            let e = conn.execute("select 1 from nope", 5).await.unwrap_err();
            assert_eq!(e.code.as_deref(), Some("42P01"));
            assert_eq!(e.position, Some("select 1 from ".len()));

            let dir = std::env::temp_dir().join(format!("monocode-export-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let file = dir.join("out.csv");
            let n = conn
                .export("select g, 'a,b' as t from generate_series(1, 1500) g", &file, crate::db::ExportFormat::Csv, Default::default())
                .await
                .unwrap();
            assert_eq!(n, 1500, "export ignores the row limit");
            let text = std::fs::read_to_string(&file).unwrap();
            assert!(text.starts_with("g,t\n1,\"a,b\"\n"), "{}", &text[..30]);
            assert_eq!(text.lines().count(), 1501);
            let empty = dir.join("empty.json");
            assert_eq!(conn.export("select 1 where false", &empty, crate::db::ExportFormat::Json, Default::default()).await.unwrap(), 0);
            assert_eq!(std::fs::read_to_string(&empty).unwrap().trim(), "[]");
            let _ = std::fs::remove_dir_all(&dir);

            let conn = std::sync::Arc::new(conn);
            let running = {
                let conn = conn.clone();
                runtime().spawn(async move { conn.execute("select pg_sleep(30)", 5).await })
            };
            tokio::time::sleep(Duration::from_millis(300)).await;
            conn.cancel();
            let e = running.await.unwrap().unwrap_err();
            assert!(e.cancelled, "{e:?}");
            assert_eq!(conn.execute("select 1", 5).await.unwrap().rows.len(), 1);
        });
        source.read_only = true;
        runtime().block_on(async {
            let conn = Connection::connect(&source, None).await.unwrap();
            let e = conn
                .execute("create table monocode_ro_probe(a int)", 5)
                .await
                .unwrap_err();
            assert_eq!(e.code.as_deref(), Some("25006"), "{e:?}");
        });
    }
}
