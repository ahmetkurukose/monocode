//! Browsing history (docs/native/browser/context.md K55, K65, K66).
//!
//! Each visit is kept with the project of the tab it happened in, so the
//! history is project-scoped by default and still searchable across every
//! project. One SQLite file per browser profile (`<dir>/<profile>.db`); the
//! default profile's is `default.db`, a project's isolated one is named by
//! its profile id. Throwaway profiles (K22) keep nothing.
//!
//! Schema: `urls` holds one row per (project, URL) with its title, favicon,
//! visit count and last visit; `visits` one row per visit with its time and
//! transition. Visits older than [`RETENTION`] go when a store opens.
//!
//! Only `http(s)` pages are kept ([`recordable`]): not `about:blank`, data,
//! blob, DevTools or Chromium's own pages.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use rusqlite::{Connection, OptionalExtension, params};

/// How long a visit is kept.
pub const RETENTION_MS: i64 = 90 * 24 * 60 * 60 * 1000;

/// The store of the default profile.
pub const DEFAULT_PROFILE: &str = "default";

/// How a visit came about.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Transition {
    /// A link, a redirect, or anything the page did itself.
    #[default]
    Link,
    /// Typed in the address bar, or picked from its suggestions or the new
    /// tab page.
    Typed,
    Reload,
    BackForward,
    /// An agent's tool navigated (K65).
    Agent,
}

impl Transition {
    pub fn as_str(self) -> &'static str {
        match self {
            Transition::Link => "link",
            Transition::Typed => "typed",
            Transition::Reload => "reload",
            Transition::BackForward => "back_forward",
            Transition::Agent => "agent",
        }
    }

    pub fn parse(text: &str) -> Transition {
        match text {
            "typed" => Transition::Typed,
            "reload" => Transition::Reload,
            "back_forward" => Transition::BackForward,
            "agent" => Transition::Agent,
            _ => Transition::Link,
        }
    }
}

/// Whether a visit to `url` is kept: web pages only.
pub fn recordable(url: &str) -> bool {
    let Ok(parsed) = url::Url::parse(url.trim()) else {
        return false;
    };
    matches!(parsed.scheme(), "http" | "https") && parsed.host_str().is_some_and(|h| !h.is_empty())
}

/// One visit, as the history view and `browser_history` list them.
#[derive(Clone, Debug, PartialEq)]
pub struct Visit {
    /// The store (profile) it is kept in; with `id`, what deletes it.
    pub store: String,
    pub id: i64,
    pub url: String,
    pub title: String,
    pub favicon: Option<String>,
    /// The project's key (`files::path_key` of its folder).
    pub project: String,
    /// Milliseconds since the Unix epoch.
    pub visited_at: i64,
    pub transition: Transition,
    /// How often the URL was visited in this project.
    pub visit_count: i64,
}

/// A URL of a project, with its visit totals: what suggestions, "recently
/// opened" and "most visited" list.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub url: String,
    pub title: String,
    pub favicon: Option<String>,
    pub project: String,
    pub visit_count: i64,
    pub typed_count: i64,
    pub last_visit: i64,
}

/// What [`Stores::visits`] looks for.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Query {
    /// Words that must all appear in the URL or the title (any case).
    pub text: String,
    /// One project's visits only; `None` is every project.
    pub project: Option<String>,
    /// Visits at or after this time (ms).
    pub since: Option<i64>,
    /// Visits before this time (ms).
    pub until: Option<i64>,
    /// At most this many; 0 is 100.
    pub limit: usize,
}

fn sql<T>(result: rusqlite::Result<T>) -> Result<T, String> {
    result.map_err(|e| e.to_string())
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS urls(
  id INTEGER PRIMARY KEY,
  project TEXT NOT NULL,
  url TEXT NOT NULL,
  title TEXT NOT NULL DEFAULT '',
  favicon TEXT,
  visit_count INTEGER NOT NULL DEFAULT 0,
  typed_count INTEGER NOT NULL DEFAULT 0,
  last_visit INTEGER NOT NULL DEFAULT 0,
  UNIQUE(project, url)
);
CREATE INDEX IF NOT EXISTS urls_last ON urls(project, last_visit);
CREATE TABLE IF NOT EXISTS visits(
  id INTEGER PRIMARY KEY,
  url_id INTEGER NOT NULL REFERENCES urls(id) ON DELETE CASCADE,
  visited_at INTEGER NOT NULL,
  transition TEXT NOT NULL DEFAULT 'link'
);
CREATE INDEX IF NOT EXISTS visits_time ON visits(visited_at);
CREATE INDEX IF NOT EXISTS visits_url ON visits(url_id);
";

/// One profile's history file.
pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path, now: i64) -> Result<Store, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let conn = sql(Connection::open(path))?;
        sql(conn.busy_timeout(std::time::Duration::from_secs(2)))?;
        sql(conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;"))?;
        Store::with(conn, now)
    }

    pub fn in_memory(now: i64) -> Result<Store, String> {
        Store::with(sql(Connection::open_in_memory())?, now)
    }

    fn with(conn: Connection, now: i64) -> Result<Store, String> {
        sql(conn.execute_batch("PRAGMA foreign_keys=ON;"))?;
        sql(conn.execute_batch(SCHEMA))?;
        let store = Store { conn };
        store.prune(now)?;
        Ok(store)
    }

    /// Records a visit of `project` to `url`; returns the visit's id, or
    /// `None` when the URL is not [`recordable`].
    pub fn record(
        &self,
        project: &str,
        url: &str,
        title: &str,
        transition: Transition,
        at: i64,
    ) -> Result<Option<i64>, String> {
        if !recordable(url) {
            return Ok(None);
        }
        let typed = i64::from(transition == Transition::Typed);
        sql(self.conn.execute(
            "INSERT INTO urls(project, url, title, visit_count, typed_count, last_visit)
             VALUES(?1, ?2, ?3, 1, ?4, ?5)
             ON CONFLICT(project, url) DO UPDATE SET
               visit_count = visit_count + 1,
               typed_count = typed_count + excluded.typed_count,
               last_visit = MAX(last_visit, excluded.last_visit),
               title = CASE WHEN excluded.title = '' THEN title ELSE excluded.title END",
            params![project, url, title, typed, at],
        ))?;
        let url_id: i64 = sql(self.conn.query_row(
            "SELECT id FROM urls WHERE project = ?1 AND url = ?2",
            params![project, url],
            |r| r.get(0),
        ))?;
        sql(self.conn.execute(
            "INSERT INTO visits(url_id, visited_at, transition) VALUES(?1, ?2, ?3)",
            params![url_id, at, transition.as_str()],
        ))?;
        Ok(Some(self.conn.last_insert_rowid()))
    }

    /// Changes a visit's transition, e.g. when what caused it is learned
    /// after it was recorded.
    pub fn set_transition(&self, visit: i64, transition: Transition) -> Result<(), String> {
        let old: Option<String> = sql(self
            .conn
            .query_row(
                "SELECT transition FROM visits WHERE id = ?1",
                [visit],
                |r| r.get(0),
            )
            .optional())?;
        let Some(old) = old else { return Ok(()) };
        let old = Transition::parse(&old);
        if old == transition {
            return Ok(());
        }
        sql(self.conn.execute(
            "UPDATE visits SET transition = ?2 WHERE id = ?1",
            params![visit, transition.as_str()],
        ))?;
        let delta =
            i64::from(transition == Transition::Typed) - i64::from(old == Transition::Typed);
        if delta != 0 {
            sql(self.conn.execute(
                "UPDATE urls SET typed_count = MAX(0, typed_count + ?2)
                 WHERE id = (SELECT url_id FROM visits WHERE id = ?1)",
                params![visit, delta],
            ))?;
        }
        Ok(())
    }

    pub fn set_title(&self, project: &str, url: &str, title: &str) -> Result<(), String> {
        if title.trim().is_empty() {
            return Ok(());
        }
        sql(self.conn.execute(
            "UPDATE urls SET title = ?3 WHERE project = ?1 AND url = ?2",
            params![project, url, title],
        ))?;
        Ok(())
    }

    pub fn set_favicon(&self, project: &str, url: &str, favicon: &str) -> Result<(), String> {
        sql(self.conn.execute(
            "UPDATE urls SET favicon = ?3 WHERE project = ?1 AND url = ?2",
            params![project, url, favicon],
        ))?;
        Ok(())
    }

    /// Visits matching `query`, newest first.
    pub fn visits(&self, store: &str, query: &Query) -> Result<Vec<Visit>, String> {
        let mut sql_text = String::from(
            "SELECT v.id, u.url, u.title, u.favicon, u.project, v.visited_at, v.transition,
                    u.visit_count
             FROM visits v JOIN urls u ON u.id = v.url_id WHERE 1",
        );
        let mut args: Vec<rusqlite::types::Value> = Vec::new();
        if let Some(project) = &query.project {
            args.push(project.clone().into());
            sql_text.push_str(&format!(" AND u.project = ?{}", args.len()));
        }
        if let Some(since) = query.since {
            args.push(since.into());
            sql_text.push_str(&format!(" AND v.visited_at >= ?{}", args.len()));
        }
        if let Some(until) = query.until {
            args.push(until.into());
            sql_text.push_str(&format!(" AND v.visited_at < ?{}", args.len()));
        }
        push_words(&query.text, "u", &mut sql_text, &mut args);
        args.push((limit(query.limit) as i64).into());
        sql_text.push_str(&format!(
            " ORDER BY v.visited_at DESC, v.id DESC LIMIT ?{}",
            args.len()
        ));
        let mut statement = sql(self.conn.prepare(&sql_text))?;
        let rows = sql(statement.query_map(rusqlite::params_from_iter(args), |r| {
            Ok(Visit {
                store: store.to_owned(),
                id: r.get(0)?,
                url: r.get(1)?,
                title: r.get(2)?,
                favicon: r.get(3)?,
                project: r.get(4)?,
                visited_at: r.get(5)?,
                transition: Transition::parse(&r.get::<_, String>(6)?),
                visit_count: r.get(7)?,
            })
        }))?;
        sql(rows.collect())
    }

    /// URLs matching `text` (all words, URL or title), most recent first;
    /// `project` limits them to one project.
    pub fn entries(
        &self,
        project: Option<&str>,
        text: &str,
        order: Order,
        limit_to: usize,
    ) -> Result<Vec<Entry>, String> {
        let mut sql_text = String::from(
            "SELECT url, title, favicon, project, visit_count, typed_count, last_visit
             FROM urls u WHERE visit_count > 0",
        );
        let mut args: Vec<rusqlite::types::Value> = Vec::new();
        if let Some(project) = project {
            args.push(project.to_owned().into());
            sql_text.push_str(&format!(" AND u.project = ?{}", args.len()));
        }
        push_words(text, "u", &mut sql_text, &mut args);
        sql_text.push_str(match order {
            Order::Recent => " ORDER BY last_visit DESC",
            Order::MostVisited => " ORDER BY visit_count DESC, last_visit DESC",
        });
        args.push((limit(limit_to) as i64).into());
        sql_text.push_str(&format!(" LIMIT ?{}", args.len()));
        let mut statement = sql(self.conn.prepare(&sql_text))?;
        let rows = sql(statement.query_map(rusqlite::params_from_iter(args), |r| {
            Ok(Entry {
                url: r.get(0)?,
                title: r.get(1)?,
                favicon: r.get(2)?,
                project: r.get(3)?,
                visit_count: r.get(4)?,
                typed_count: r.get(5)?,
                last_visit: r.get(6)?,
            })
        }))?;
        sql(rows.collect())
    }

    /// Deletes one visit; its URL goes with its last visit.
    pub fn delete_visit(&self, id: i64) -> Result<(), String> {
        let url: Option<i64> = sql(self
            .conn
            .query_row("SELECT url_id FROM visits WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional())?;
        sql(self.conn.execute("DELETE FROM visits WHERE id = ?1", [id]))?;
        if let Some(url) = url {
            self.recount(Some(url))?;
        }
        Ok(())
    }

    /// Deletes every visit of `url` (in `project`, or in every project).
    pub fn delete_url(&self, project: Option<&str>, url: &str) -> Result<(), String> {
        match project {
            Some(project) => sql(self.conn.execute(
                "DELETE FROM urls WHERE project = ?1 AND url = ?2",
                params![project, url],
            ))?,
            None => sql(self.conn.execute("DELETE FROM urls WHERE url = ?1", [url]))?,
        };
        Ok(())
    }

    /// Deletes the visits in `[from, to)` (of `project`, or of every one).
    pub fn delete_range(&self, project: Option<&str>, from: i64, to: i64) -> Result<(), String> {
        match project {
            Some(project) => sql(self.conn.execute(
                "DELETE FROM visits WHERE visited_at >= ?2 AND visited_at < ?3
                 AND url_id IN (SELECT id FROM urls WHERE project = ?1)",
                params![project, from, to],
            ))?,
            None => sql(self.conn.execute(
                "DELETE FROM visits WHERE visited_at >= ?1 AND visited_at < ?2",
                params![from, to],
            ))?,
        };
        self.recount(None)
    }

    /// Deletes everything (of `project`, or all of it).
    pub fn clear(&self, project: Option<&str>) -> Result<(), String> {
        match project {
            Some(project) => sql(self
                .conn
                .execute("DELETE FROM urls WHERE project = ?1", [project]))?,
            None => sql(self.conn.execute("DELETE FROM urls", []))?,
        };
        Ok(())
    }

    /// Lets visits older than [`RETENTION_MS`] go.
    pub fn prune(&self, now: i64) -> Result<(), String> {
        let gone = sql(self.conn.execute(
            "DELETE FROM visits WHERE visited_at < ?1",
            [now - RETENTION_MS],
        ))?;
        if gone > 0 {
            self.recount(None)?;
        }
        Ok(())
    }

    /// Brings URL totals in line with their visits (one URL, or all), and
    /// drops URLs left with none.
    fn recount(&self, url: Option<i64>) -> Result<(), String> {
        let filter = if url.is_some() { " WHERE id = ?1" } else { "" };
        let update = format!(
            "UPDATE urls SET
               visit_count = (SELECT COUNT(*) FROM visits WHERE url_id = urls.id),
               typed_count = (SELECT COUNT(*) FROM visits WHERE url_id = urls.id
                              AND transition = 'typed'),
               last_visit = COALESCE((SELECT MAX(visited_at) FROM visits
                                      WHERE url_id = urls.id), 0){filter}"
        );
        match url {
            Some(id) => sql(self.conn.execute(&update, [id]))?,
            None => sql(self.conn.execute(&update, []))?,
        };
        sql(self
            .conn
            .execute("DELETE FROM urls WHERE visit_count = 0", []))?;
        Ok(())
    }
}

/// How [`Store::entries`] orders.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Order {
    Recent,
    MostVisited,
}

fn limit(limit: usize) -> usize {
    if limit == 0 { 100 } else { limit.min(5_000) }
}

/// `AND (url LIKE %w% OR title LIKE %w%)` for each word of `text`.
fn push_words(
    text: &str,
    table: &str,
    sql_text: &mut String,
    args: &mut Vec<rusqlite::types::Value>,
) {
    for word in text.split_whitespace() {
        let escaped = word
            .to_lowercase()
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        args.push(format!("%{escaped}%").into());
        let n = args.len();
        sql_text.push_str(&format!(
            " AND (lower({table}.url) LIKE ?{n} ESCAPE '\\' OR lower({table}.title) LIKE ?{n} ESCAPE '\\')"
        ));
    }
}

/// How likely `entry` is the page wanted: its visits, typed ones counting
/// more, weighed by how recent the last one is (Firefox's frecency, simply).
pub fn frecency(entry: &Entry, now: i64) -> f64 {
    let days = (now - entry.last_visit).max(0) as f64 / 86_400_000.;
    let recency = match days {
        d if d <= 4. => 100.,
        d if d <= 14. => 70.,
        d if d <= 31. => 50.,
        d if d <= 90. => 30.,
        _ => 10.,
    };
    (entry.visit_count as f64 + 2. * entry.typed_count as f64) * recency
}

/// A suggestion's place among others: `typed` matching the start of the
/// URL's host (or the URL) beats a match anywhere.
fn prefix_bonus(entry: &Entry, typed: &str) -> f64 {
    let typed = typed.trim().to_lowercase();
    if typed.is_empty() {
        return 1.;
    }
    let url = entry.url.to_lowercase();
    let bare = url
        .split_once("://")
        .map_or(url.as_str(), |(_, rest)| rest)
        .trim_start_matches("www.");
    if bare.starts_with(&typed) || url.starts_with(&typed) {
        4.
    } else {
        1.
    }
}

/// Every profile's store under one folder, opened as they are needed.
/// Without a folder (tests, a data mode with no disk), stores live in
/// memory.
pub struct Stores {
    dir: Option<PathBuf>,
    open: HashMap<String, Store>,
}

impl Default for Stores {
    fn default() -> Self {
        Stores::new(None)
    }
}

impl Stores {
    pub fn new(dir: Option<PathBuf>) -> Stores {
        Stores {
            dir,
            open: HashMap::new(),
        }
    }

    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    /// The store of the profile `name` (opened, or made, now).
    pub fn store(&mut self, name: &str, now: i64) -> Result<&Store, String> {
        if !self.open.contains_key(name) {
            let store = match &self.dir {
                Some(dir) => Store::open(&dir.join(format!("{}.db", file_name(name))), now)?,
                None => Store::in_memory(now)?,
            };
            self.open.insert(name.to_owned(), store);
        }
        Ok(&self.open[name])
    }

    /// Every store there is: the open ones and those on disk.
    fn all(&mut self, now: i64) -> Vec<(String, &Store)> {
        let read = self
            .dir
            .as_ref()
            .and_then(|dir| std::fs::read_dir(dir).ok());
        if let Some(read) = read {
            let names: Vec<String> = read
                .flatten()
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    name.strip_suffix(".db").map(str::to_owned)
                })
                .collect();
            for name in names {
                if let Err(error) = self.store(&name, now) {
                    eprintln!("browser history: {name}: {error}");
                }
            }
        }
        let mut all: Vec<(String, &Store)> =
            self.open.iter().map(|(n, s)| (n.clone(), s)).collect();
        all.sort_by(|a, b| a.0.cmp(&b.0));
        all
    }

    /// Visits of every store matching `query`, newest first.
    pub fn visits(&mut self, query: &Query, now: i64) -> Vec<Visit> {
        let mut visits: Vec<Visit> = Vec::new();
        for (name, store) in self.all(now) {
            match store.visits(&name, query) {
                Ok(found) => visits.extend(found),
                Err(error) => eprintln!("browser history: {name}: {error}"),
            }
        }
        visits.sort_by(|a, b| (b.visited_at, b.id, &b.store).cmp(&(a.visited_at, a.id, &a.store)));
        visits.truncate(limit(query.limit));
        visits
    }

    /// URLs of `project` (or every project) in `order`, one row per URL
    /// and project.
    pub fn entries(
        &mut self,
        project: Option<&str>,
        text: &str,
        order: Order,
        limit_to: usize,
        now: i64,
    ) -> Vec<Entry> {
        let mut entries: Vec<Entry> = Vec::new();
        for (name, store) in self.all(now) {
            match store.entries(project, text, order, limit_to) {
                Ok(found) => entries.extend(found),
                Err(error) => eprintln!("browser history: {name}: {error}"),
            }
        }
        match order {
            Order::Recent => entries.sort_by_key(|e| std::cmp::Reverse(e.last_visit)),
            Order::MostVisited => {
                entries.sort_by_key(|e| std::cmp::Reverse((e.visit_count, e.last_visit)))
            }
        }
        entries.truncate(limit(limit_to));
        entries
    }

    /// The address bar's history suggestions for `typed` (K55): `project`'s
    /// pages first, then other projects', each by frecency; one per URL.
    pub fn suggest(&mut self, project: &str, typed: &str, limit_to: usize, now: i64) -> Vec<Entry> {
        if typed.trim().is_empty() {
            return Vec::new();
        }
        let candidates = self.entries(None, typed, Order::Recent, 300, now);
        let mut scored: Vec<(bool, f64, Entry)> = candidates
            .into_iter()
            .map(|e| {
                let score = frecency(&e, now) * prefix_bonus(&e, typed);
                (e.project != project, score, e)
            })
            .collect();
        scored.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then(b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal))
        });
        let mut seen = std::collections::HashSet::new();
        scored
            .into_iter()
            .filter(|(_, _, e)| seen.insert(e.url.clone()))
            .map(|(_, _, e)| e)
            .take(limit_to)
            .collect()
    }

    /// What an agent of the project at `folder` may see (K65): that
    /// project's visits only, whatever `query` asks.
    pub fn of_project(&mut self, folder: &str, query: &Query, now: i64) -> Vec<Visit> {
        let mut query = query.clone();
        query.project = Some(crate::files::path_key(folder));
        self.visits(&query, now)
    }

    /// Deletes a visit [`Stores::visits`] listed.
    pub fn delete_visit(&mut self, visit: &Visit, now: i64) -> Result<(), String> {
        self.store(&visit.store, now)?.delete_visit(visit.id)
    }

    /// Deletes every visit of `url` in `project` (or every project), in
    /// every store.
    pub fn delete_url(&mut self, project: Option<&str>, url: &str, now: i64) -> Result<(), String> {
        for (_, store) in self.all(now) {
            store.delete_url(project, url)?;
        }
        Ok(())
    }

    /// Deletes the visits in `[from, to)` of `project` (or all), everywhere.
    pub fn delete_range(
        &mut self,
        project: Option<&str>,
        from: i64,
        to: i64,
        now: i64,
    ) -> Result<(), String> {
        for (_, store) in self.all(now) {
            store.delete_range(project, from, to)?;
        }
        Ok(())
    }

    pub fn clear(&mut self, project: Option<&str>, now: i64) -> Result<(), String> {
        for (_, store) in self.all(now) {
            store.clear(project)?;
        }
        Ok(())
    }
}

/// A profile name as a file name: profile ids already are, but nothing
/// else may climb out of the folder.
fn file_name(name: &str) -> String {
    let clean: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if clean.is_empty() {
        DEFAULT_PROFILE.to_owned()
    } else {
        clean
    }
}

#[cfg(test)]
#[path = "history_tests.rs"]
mod tests;
