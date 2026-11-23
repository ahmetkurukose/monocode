//! A profile's password vault (context.md K55, K56): one SQLite file per
//! browser profile, `<dir>/<profile>.db`, beside Chromium's own folder.
//! Passwords are sealed one by one with the app's vault key
//! ([`super::crypto`]); sites and user names are not, as in Chromium's own
//! `Login Data`, so lists and lookups need no unsealing. A vault whose check
//! value does not open with the key is refused, never rewritten.
//!
//! Throwaway profiles (K22) have no vault.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use rusqlite::{Connection, OptionalExtension, params};

use super::crypto::VaultKey;
use crate::browser::import::{ImportedLogin, merge_logins};

/// The default profile's vault.
pub const DEFAULT_PROFILE: &str = "default";

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta(
  name TEXT PRIMARY KEY,
  value BLOB NOT NULL
);
CREATE TABLE IF NOT EXISTS logins(
  id INTEGER PRIMARY KEY,
  origin TEXT NOT NULL,
  signon_realm TEXT NOT NULL,
  username TEXT NOT NULL,
  password BLOB NOT NULL,
  created_at INTEGER NOT NULL,
  last_used_at INTEGER NOT NULL DEFAULT 0,
  changed_at INTEGER NOT NULL,
  times_used INTEGER NOT NULL DEFAULT 0,
  UNIQUE(origin, username)
);
CREATE INDEX IF NOT EXISTS logins_origin ON logins(origin);
CREATE TABLE IF NOT EXISTS never_save(
  origin TEXT PRIMARY KEY,
  created_at INTEGER NOT NULL
);
";

/// What the check value seals.
const CHECK_CONTEXT: &str = "monocode-browser-passwords-v1";
const CHECK_TEXT: &str = "ok";

fn sql<T>(result: rusqlite::Result<T>) -> Result<T, String> {
    result.map_err(|e| e.to_string())
}

/// A saved login, without its password.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Login {
    pub id: i64,
    /// `scheme://host[:port]`.
    pub origin: String,
    /// Chromium's signon realm: the origin and a slash.
    pub signon_realm: String,
    pub username: String,
    /// Milliseconds since the Unix epoch.
    pub created_at: i64,
    /// 0: never used.
    pub last_used_at: i64,
    pub changed_at: i64,
    pub times_used: i64,
}

/// What [`Vault::import`] did with another browser's logins (K61).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImportReport {
    /// New site and user name: added.
    pub added: usize,
    /// Saved already with an older password: the imported one replaced it.
    pub updated: usize,
    /// Saved already with the same password, or a newer one: kept.
    pub unchanged: usize,
    /// No web origin (an app's, `android://…`) or no password: left out.
    pub skipped: usize,
    /// "Never save" sites put on the vault's list.
    pub never_save: usize,
}

/// What the browser offers after a successful sign-in (Chromium's
/// save-or-update table).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Offer {
    /// A login the vault does not have.
    Save,
    /// The saved login `id` has another password now.
    Update { id: i64, username: String },
    /// Saved as it is, or the site is on the "never" list.
    Nothing,
}

/// The origin (`scheme://host[:port]`) of an http(s) URL; `None` for
/// anything else.
pub fn origin_of(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url.trim()).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    let origin = parsed.origin();
    origin.is_tuple().then(|| origin.ascii_serialization())
}

/// One profile's vault.
pub struct Vault {
    conn: Connection,
    key: VaultKey,
}

impl Vault {
    pub fn open(path: &Path, key: VaultKey) -> Result<Vault, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let conn = sql(Connection::open(path))?;
        sql(conn.busy_timeout(std::time::Duration::from_secs(2)))?;
        sql(conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        Vault::with(conn, key)
    }

    pub fn in_memory(key: VaultKey) -> Result<Vault, String> {
        Vault::with(sql(Connection::open_in_memory())?, key)
    }

    fn with(conn: Connection, key: VaultKey) -> Result<Vault, String> {
        sql(conn.execute_batch(SCHEMA))?;
        let check: Option<Vec<u8>> = sql(conn
            .query_row("SELECT value FROM meta WHERE name = 'check'", [], |r| {
                r.get(0)
            })
            .optional())?;
        match check {
            Some(sealed) => {
                if key.open_text(CHECK_CONTEXT, &sealed).as_deref() != Some(CHECK_TEXT) {
                    return Err("the vault does not open with this key".into());
                }
            }
            None => {
                let sealed = key.seal_text(CHECK_CONTEXT, CHECK_TEXT)?;
                sql(conn.execute(
                    "INSERT INTO meta(name, value) VALUES('check', ?1)",
                    params![sealed],
                ))?;
            }
        }
        Ok(Vault { conn, key })
    }

    fn login(r: &rusqlite::Row) -> rusqlite::Result<Login> {
        Ok(Login {
            id: r.get(0)?,
            origin: r.get(1)?,
            signon_realm: r.get(2)?,
            username: r.get(3)?,
            created_at: r.get(4)?,
            last_used_at: r.get(5)?,
            changed_at: r.get(6)?,
            times_used: r.get(7)?,
        })
    }

    const COLUMNS: &'static str =
        "id, origin, signon_realm, username, created_at, last_used_at, changed_at, times_used";

    /// Every login, by site and user name; `origin` limits them to one site.
    pub fn logins(&self, origin: Option<&str>) -> Result<Vec<Login>, String> {
        let mut statement = sql(self.conn.prepare(&format!(
            "SELECT {} FROM logins WHERE ?1 IS NULL OR origin = ?1",
            Self::COLUMNS
        )))?;
        let rows = sql(statement.query_map([origin], Self::login))?;
        let mut out: Vec<Login> = sql(rows.collect())?;
        out.sort_by(|a, b| {
            (a.origin.as_str(), a.username.to_lowercase())
                .cmp(&(b.origin.as_str(), b.username.to_lowercase()))
        });
        Ok(out)
    }

    pub fn get(&self, id: i64) -> Result<Option<Login>, String> {
        sql(self
            .conn
            .query_row(
                &format!("SELECT {} FROM logins WHERE id = ?1", Self::COLUMNS),
                [id],
                Self::login,
            )
            .optional())
    }

    /// The password of login `id`.
    pub fn password(&self, id: i64) -> Result<String, String> {
        let row: Option<(String, Vec<u8>)> = sql(self
            .conn
            .query_row(
                "SELECT origin, password FROM logins WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional())?;
        let (origin, sealed) = row.ok_or("there is no such login")?;
        self.key
            .open_text(&origin, &sealed)
            .ok_or_else(|| "the password does not open with the vault key".to_owned())
    }

    /// The logins of `origin` with their passwords (to fill, or to mask).
    pub fn credentials(&self, origin: &str) -> Result<Vec<(Login, String)>, String> {
        let mut out = Vec::new();
        for login in self.logins(Some(origin))? {
            let password = self.password(login.id)?;
            out.push((login, password));
        }
        Ok(out)
    }

    /// Saves `username`'s `password` for the site of `url`: a new login, or
    /// a new password for the saved one. Its id.
    pub fn save(&self, url: &str, username: &str, password: &str, now: i64) -> Result<i64, String> {
        let origin = origin_of(url).ok_or("not a web address")?;
        if password.is_empty() {
            return Err("there is no password".into());
        }
        let sealed = self.key.seal_text(&origin, password)?;
        sql(self.conn.execute(
            "INSERT INTO logins(origin, signon_realm, username, password, created_at, changed_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?5)
             ON CONFLICT(origin, username) DO UPDATE SET
               password = excluded.password, changed_at = excluded.changed_at",
            params![origin, format!("{origin}/"), username, sealed, now],
        ))?;
        sql(self.conn.query_row(
            "SELECT id FROM logins WHERE origin = ?1 AND username = ?2",
            params![origin, username],
            |r| r.get(0),
        ))
    }

    /// Changes login `id`'s user name and password (Settings' edit).
    pub fn edit(&self, id: i64, username: &str, password: &str, now: i64) -> Result<(), String> {
        let login = self.get(id)?.ok_or("there is no such login")?;
        if password.is_empty() {
            return Err("there is no password".into());
        }
        if login.username != username
            && self
                .logins(Some(&login.origin))?
                .iter()
                .any(|l| l.username == username)
        {
            return Err(format!(
                "a login for {username:?} is saved for this site already"
            ));
        }
        let sealed = self.key.seal_text(&login.origin, password)?;
        sql(self.conn.execute(
            "UPDATE logins SET username = ?2, password = ?3, changed_at = ?4 WHERE id = ?1",
            params![id, username, sealed, now],
        ))?;
        Ok(())
    }

    pub fn delete(&self, id: i64) -> Result<(), String> {
        sql(self.conn.execute("DELETE FROM logins WHERE id = ?1", [id]))?;
        Ok(())
    }

    /// Login `id` was filled.
    pub fn note_used(&self, id: i64, now: i64) -> Result<(), String> {
        sql(self.conn.execute(
            "UPDATE logins SET times_used = times_used + 1, last_used_at = ?2 WHERE id = ?1",
            params![id, now],
        ))?;
        Ok(())
    }

    /// "Never for this site".
    pub fn never_save(&self, url: &str, now: i64) -> Result<(), String> {
        let origin = origin_of(url).ok_or("not a web address")?;
        sql(self.conn.execute(
            "INSERT OR IGNORE INTO never_save(origin, created_at) VALUES(?1, ?2)",
            params![origin, now],
        ))?;
        Ok(())
    }

    /// Takes a site off the "never" list.
    pub fn allow_saving(&self, origin: &str) -> Result<(), String> {
        sql(self
            .conn
            .execute("DELETE FROM never_save WHERE origin = ?1", [origin]))?;
        Ok(())
    }

    pub fn is_never(&self, origin: &str) -> Result<bool, String> {
        sql(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM never_save WHERE origin = ?1)",
            [origin],
            |r| r.get(0),
        ))
    }

    /// The sites never saved for, by name.
    pub fn never_list(&self) -> Result<Vec<String>, String> {
        let mut statement = sql(self
            .conn
            .prepare("SELECT origin FROM never_save ORDER BY origin"))?;
        let rows = sql(statement.query_map([], |r| r.get(0)))?;
        let origins: Vec<String> = sql(rows.collect())?;
        Ok(origins)
    }

    /// What to offer after `username` signed in to the site of `url` with
    /// `password` (Chromium's save-or-update table). A sign-in without a
    /// user name (a password-only step) updates the site's only login.
    pub fn offer(&self, url: &str, username: &str, password: &str) -> Result<Offer, String> {
        let Some(origin) = origin_of(url) else {
            return Ok(Offer::Nothing);
        };
        if password.is_empty() || self.is_never(&origin)? {
            return Ok(Offer::Nothing);
        }
        let saved = self.credentials(&origin)?;
        if let Some((login, old)) = saved.iter().find(|(l, _)| l.username == username) {
            return Ok(if old == password {
                Offer::Nothing
            } else {
                Offer::Update {
                    id: login.id,
                    username: login.username.clone(),
                }
            });
        }
        if username.is_empty() {
            if saved.iter().any(|(_, old)| old == password) {
                return Ok(Offer::Nothing);
            }
            if let [(login, _)] = saved.as_slice() {
                return Ok(Offer::Update {
                    id: login.id,
                    username: login.username.clone(),
                });
            }
        }
        Ok(Offer::Save)
    }

    /// The saved login of `origin` and `username` as an import reads one
    /// (its password open), and its id.
    fn as_imported(
        &self,
        origin: &str,
        username: &str,
    ) -> Result<Option<(i64, ImportedLogin)>, String> {
        let row = sql(self
            .conn
            .query_row(
                &format!(
                    "SELECT {} FROM logins WHERE origin = ?1 AND username = ?2",
                    Self::COLUMNS
                ),
                params![origin, username],
                Self::login,
            )
            .optional())?;
        let Some(login) = row else {
            return Ok(None);
        };
        let password = self.password(login.id)?;
        Ok(Some((
            login.id,
            ImportedLogin {
                origin: login.origin,
                signon_realm: login.signon_realm,
                username: login.username,
                password,
                created_ms: login.created_at,
                last_used_ms: login.last_used_at,
                password_modified_ms: login.changed_at,
                times_used: login.times_used,
            },
        )))
    }

    /// Adds another browser's `logins` and "never save" sites (K61) by the
    /// import's own rule ([`merge_logins`]): one login per site and user
    /// name; a saved one gives way only to another password that is newer
    /// (changed, or else saved, later), and a tie keeps what is saved.
    /// Importing again merges again. `now` stands in for missing times.
    pub fn import(
        &self,
        logins: &[ImportedLogin],
        never_save: &[String],
        now: i64,
    ) -> Result<ImportReport, String> {
        let mut report = ImportReport::default();
        let tx = sql(self.conn.unchecked_transaction())?;
        let or_now = |ms: i64| if ms > 0 { ms } else { now };
        for login in logins {
            let Some(origin) = origin_of(&login.origin) else {
                report.skipped += 1;
                continue;
            };
            if login.password.is_empty() {
                report.skipped += 1;
                continue;
            }
            let incoming = ImportedLogin {
                origin: origin.clone(),
                signon_realm: format!("{origin}/"),
                ..login.clone()
            };
            let saved = self.as_imported(&origin, &login.username)?;
            let id = saved.as_ref().map(|(id, _)| *id);
            let merge = merge_logins(saved.map(|(_, l)| l).into_iter().collect(), [incoming]);
            let updated = merge.updated > 0;
            let Some(merged) = merge.logins.into_iter().next() else {
                continue;
            };
            match id {
                None => {
                    let sealed = self.key.seal_text(&origin, &merged.password)?;
                    sql(self.conn.execute(
                        "INSERT INTO logins(origin, signon_realm, username, password, created_at,
                                            last_used_at, changed_at, times_used)
                         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                        params![
                            origin,
                            merged.signon_realm,
                            merged.username,
                            sealed,
                            or_now(merged.created_ms),
                            merged.last_used_ms.max(0),
                            or_now(merged.stamp()),
                            merged.times_used.max(0)
                        ],
                    ))?;
                    report.added += 1;
                }
                Some(id) if updated => {
                    let sealed = self.key.seal_text(&origin, &merged.password)?;
                    sql(self.conn.execute(
                        "UPDATE logins SET password = ?2, changed_at = ?3,
                           last_used_at = MAX(last_used_at, ?4), times_used = MAX(times_used, ?5)
                         WHERE id = ?1",
                        params![
                            id,
                            sealed,
                            or_now(merged.stamp()),
                            merged.last_used_ms,
                            merged.times_used
                        ],
                    ))?;
                    report.updated += 1;
                }
                Some(id) => {
                    // Kept; with the same password it learns how much the
                    // login was used.
                    sql(self.conn.execute(
                        "UPDATE logins SET last_used_at = ?2, times_used = ?3 WHERE id = ?1",
                        params![id, merged.last_used_ms, merged.times_used],
                    ))?;
                    report.unchanged += 1;
                }
            }
        }
        for site in never_save {
            if origin_of(site).is_some() {
                self.never_save(site, now)?;
                report.never_save += 1;
            }
        }
        sql(tx.commit())?;
        Ok(report)
    }
}

/// Every profile's vault of the app, opened as needed.
pub struct Vaults {
    /// `None`: in memory (a data mode without a folder).
    dir: Option<PathBuf>,
    key: VaultKey,
    open: HashMap<String, Vault>,
}

impl Vaults {
    pub fn new(dir: Option<PathBuf>, key: VaultKey) -> Vaults {
        Vaults {
            dir,
            key,
            open: HashMap::new(),
        }
    }

    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    /// Whether any vault file exists in `dir` (then a missing key is lost,
    /// not new).
    pub fn exist_in(dir: &Path) -> bool {
        std::fs::read_dir(dir).is_ok_and(|entries| {
            entries
                .filter_map(Result::ok)
                .any(|e| e.path().extension().is_some_and(|x| x == "db"))
        })
    }

    /// The vault of `profile` (its store name: `default` or a profile id).
    pub fn vault(&mut self, profile: &str) -> Result<&Vault, String> {
        let name = file_name(profile);
        if !self.open.contains_key(&name) {
            let vault = match &self.dir {
                Some(dir) => Vault::open(&dir.join(format!("{name}.db")), self.key.clone())?,
                None => Vault::in_memory(self.key.clone())?,
            };
            self.open.insert(name.clone(), vault);
        }
        Ok(&self.open[&name])
    }

    /// The profiles that have a vault, the default one first.
    pub fn profiles(&self) -> Vec<String> {
        let mut names: Vec<String> = match &self.dir {
            Some(dir) => std::fs::read_dir(dir)
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
                .filter_map(|e| {
                    let path = e.path();
                    (path.extension()? == "db")
                        .then(|| path.file_stem()?.to_str().map(str::to_owned))?
                })
                .collect(),
            None => self.open.keys().cloned().collect(),
        };
        names.sort_by_key(|n| (n != DEFAULT_PROFILE, n.clone()));
        names.dedup();
        names
    }
}

/// A profile name as a file name.
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
#[path = "vault_tests.rs"]
mod tests;
