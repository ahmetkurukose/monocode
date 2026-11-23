//! A Chromium profile's saved passwords: `Login Data`, and `Login Data For
//! Account` (the passwords kept in the Google account).

use super::cookies::{columns, open};
use super::crypto::{self, Decrypted, SafeStorageKey};
use super::{chrome_time_to_unix_ms, ImportError};
use std::path::{Path, PathBuf};

/// A saved password, decrypted. Times are Unix milliseconds, zero when the
/// browser did not record one.
#[derive(Clone, PartialEq, Eq)]
pub struct ImportedLogin {
    /// `scheme://host[:port]` of the page the password was saved on.
    pub origin: String,
    /// Chromium's `signon_realm`, as stored (usually the origin plus `/`).
    pub signon_realm: String,
    pub username: String,
    pub password: String,
    pub created_ms: i64,
    pub last_used_ms: i64,
    /// When the password last changed; [`ImportedLogin::stamp`] falls back
    /// to `created_ms` when the browser did not record it.
    pub password_modified_ms: i64,
    pub times_used: i64,
}

impl std::fmt::Debug for ImportedLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the password.
        f.debug_struct("ImportedLogin")
            .field("origin", &self.origin)
            .field("username", &self.username)
            .finish_non_exhaustive()
    }
}

impl ImportedLogin {
    /// How new the password is: when it changed, else when it was saved.
    pub fn stamp(&self) -> i64 {
        self.password_modified_ms.max(self.created_ms)
    }
}

/// What the `Login Data` databases gave.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LoginRead {
    pub logins: Vec<ImportedLogin>,
    /// Origins the user told the browser never to save a password for.
    pub never_save: Vec<String>,
    /// `v20` values.
    pub skipped_app_bound: usize,
    /// Not a web form's password (HTTP authentication, federated sign-in),
    /// or an empty one.
    pub skipped_unsupported: usize,
    /// Values that would not decrypt.
    pub failed: usize,
}

impl LoginRead {
    /// Adds another database's rows; a login both hold is kept once, the
    /// newer one (see [`super::merge_logins`]).
    pub fn absorb(&mut self, other: LoginRead) {
        let merged = super::merge::merge_logins(std::mem::take(&mut self.logins), other.logins);
        self.logins = merged.logins;
        for origin in other.never_save {
            if !self.never_save.contains(&origin) {
                self.never_save.push(origin);
            }
        }
        self.skipped_app_bound += other.skipped_app_bound;
        self.skipped_unsupported += other.skipped_unsupported;
        self.failed += other.failed;
    }
}

/// The profile's password databases that exist.
pub fn login_paths(profile_dir: &Path) -> Vec<PathBuf> {
    ["Login Data", "Login Data For Account"]
        .into_iter()
        .map(|name| profile_dir.join(name))
        .filter(|p| p.is_file())
        .collect()
}

/// `scheme://host[:port]` of `url`, or `url` itself when it does not parse.
pub fn origin_of(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(parsed) if parsed.has_host() => parsed.origin().ascii_serialization(),
        _ => url.trim_end_matches('/').to_owned(),
    }
}

/// Reads the `logins` table of the database at `db` (a copy).
pub fn read_logins(db: &Path, key: &SafeStorageKey) -> Result<LoginRead, ImportError> {
    let conn = open(db)?;
    let unreadable = |e: rusqlite::Error| ImportError::Unreadable(e.to_string());
    let columns = columns(&conn, "logins");
    if columns.is_empty() {
        return Err(ImportError::Unreadable(
            "the database has no logins table".into(),
        ));
    }
    let pick = |c: &str, fallback: &'static str| -> String {
        if columns.iter().any(|known| known == c) {
            c.to_owned()
        } else {
            fallback.to_owned()
        }
    };
    let sql = format!(
        "SELECT origin_url, signon_realm, username_value, password_value, date_created, \
         {}, {}, {}, {}, {} FROM logins",
        pick("date_last_used", "0"),
        pick("date_password_modified", "0"),
        pick("times_used", "0"),
        pick("blacklisted_by_user", "0"),
        pick("scheme", "0"),
    );
    let mut stmt = conn.prepare(&sql).map_err(unreadable)?;
    let rows = stmt
        .query_map([], |row| {
            Ok(Row {
                origin_url: row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                signon_realm: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                username: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                password: row.get::<_, Option<Vec<u8>>>(3)?.unwrap_or_default(),
                created: row.get::<_, Option<i64>>(4)?.unwrap_or_default(),
                last_used: row.get::<_, Option<i64>>(5)?.unwrap_or_default(),
                modified: row.get::<_, Option<i64>>(6)?.unwrap_or_default(),
                times_used: row.get::<_, Option<i64>>(7)?.unwrap_or_default(),
                never_save: row.get::<_, Option<i64>>(8)?.unwrap_or_default() != 0,
                scheme: row.get::<_, Option<i64>>(9)?.unwrap_or_default(),
            })
        })
        .map_err(unreadable)?;
    let mut read = LoginRead::default();
    let mut logins = Vec::new();
    for row in rows {
        let row = row.map_err(unreadable)?;
        let origin = origin_of(if row.origin_url.is_empty() {
            &row.signon_realm
        } else {
            &row.origin_url
        });
        if row.never_save {
            if !origin.is_empty() && !read.never_save.contains(&origin) {
                read.never_save.push(origin);
            }
            continue;
        }
        // Scheme 0 is an HTML form; the others are HTTP authentication.
        if row.scheme != 0 || !origin.starts_with("http") || row.password.is_empty() {
            read.skipped_unsupported += 1;
            continue;
        }
        let password = match crypto::decrypt(&row.password, key) {
            Decrypted::Plain(plain) => match String::from_utf8(plain) {
                Ok(password) => password,
                Err(_) => {
                    read.failed += 1;
                    continue;
                }
            },
            Decrypted::AppBound => {
                read.skipped_app_bound += 1;
                continue;
            }
            Decrypted::Failed => {
                read.failed += 1;
                continue;
            }
        };
        if password.is_empty() {
            read.skipped_unsupported += 1;
            continue;
        }
        logins.push(ImportedLogin {
            origin,
            signon_realm: row.signon_realm,
            username: row.username,
            password,
            created_ms: chrome_time_to_unix_ms(row.created),
            last_used_ms: chrome_time_to_unix_ms(row.last_used),
            password_modified_ms: chrome_time_to_unix_ms(row.modified),
            times_used: row.times_used,
        });
    }
    // One database may hold a login twice (two forms of a site).
    read.logins = super::merge::merge_logins(Vec::new(), logins).logins;
    Ok(read)
}

struct Row {
    origin_url: String,
    signon_realm: String,
    username: String,
    password: Vec<u8>,
    created: i64,
    last_used: i64,
    modified: i64,
    times_used: i64,
    never_save: bool,
    scheme: i64,
}
