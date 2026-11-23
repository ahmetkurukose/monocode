//! A Chromium profile's `Cookies` database.

use super::crypto::{self, Decrypted, SafeStorageKey};
use super::ImportError;
use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};

/// Domains whose cookies are never imported. Google ties its sessions to
/// the browser they were made in: a copied session can sign the user out
/// everywhere or flag the account, so the user signs in to Google in
/// Monocode directly. A trailing `.*` matches any top-level domain.
pub const NON_TRANSPLANTABLE_COOKIE_DOMAINS: &[&str] =
    &["google.com", "youtube.com", "accounts.google.*"];

/// Whether a cookie for `host_key` is one of
/// [`NON_TRANSPLANTABLE_COOKIE_DOMAINS`].
pub fn is_non_transplantable(host_key: &str) -> bool {
    let host = host_key.trim_start_matches('.').to_ascii_lowercase();
    NON_TRANSPLANTABLE_COOKIE_DOMAINS.iter().any(|entry| {
        if let Some(prefix) = entry.strip_suffix('*') {
            host.starts_with(prefix) || host.contains(&format!(".{prefix}"))
        } else {
            host == *entry || host.ends_with(&format!(".{entry}"))
        }
    })
}

/// Chromium's SameSite column: -1 unspecified, 0 none, 1 lax, 2 strict.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SameSite {
    #[default]
    Unspecified,
    None,
    Lax,
    Strict,
}

impl SameSite {
    fn from_column(value: i64) -> Self {
        match value {
            0 => SameSite::None,
            1 => SameSite::Lax,
            2 => SameSite::Strict,
            _ => SameSite::Unspecified,
        }
    }
}

/// A cookie as the source browser stored it, decrypted.
#[derive(Clone, PartialEq, Eq)]
pub struct ImportedCookie {
    pub name: String,
    pub value: String,
    /// Chromium's `host_key`: `.example.com` for a domain cookie,
    /// `example.com` for a host-only one.
    pub host_key: String,
    pub path: String,
    pub secure: bool,
    pub http_only: bool,
    pub same_site: SameSite,
    /// Chromium time (microseconds since 1601); `None` for a session cookie.
    pub expires: Option<i64>,
    pub creation: i64,
    pub last_access: i64,
}

impl std::fmt::Debug for ImportedCookie {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The value is a credential: never in logs.
        f.debug_struct("ImportedCookie")
            .field("name", &self.name)
            .field("host_key", &self.host_key)
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl ImportedCookie {
    /// Host-only cookies go to their host alone, not its subdomains.
    pub fn host_only(&self) -> bool {
        !self.host_key.starts_with('.')
    }

    /// The host without the leading dot of a domain cookie.
    pub fn host(&self) -> &str {
        self.host_key.trim_start_matches('.')
    }

    /// A URL the cookie can be set for: Chromium checks it against the
    /// domain, and a secure cookie needs https.
    pub fn url(&self) -> String {
        let scheme = if self.secure { "https" } else { "http" };
        let path = if self.path.starts_with('/') {
            self.path.as_str()
        } else {
            "/"
        };
        format!("{scheme}://{}{path}", self.host())
    }

    /// What makes it the same cookie as another: Chromium keeps one per
    /// name, domain and path.
    pub fn key(&self) -> (&str, &str, &str) {
        (&self.name, &self.host_key, &self.path)
    }
}

/// What the `Cookies` database gave.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CookieRead {
    pub cookies: Vec<ImportedCookie>,
    /// `v20` values.
    pub skipped_app_bound: usize,
    pub skipped_expired: usize,
    /// [`NON_TRANSPLANTABLE_COOKIE_DOMAINS`].
    pub skipped_google: usize,
    /// Partitioned (CHIPS) cookies, which Monocode cannot set yet, and
    /// rows without a usable domain or name.
    pub skipped_unsupported: usize,
    /// Values that would not decrypt.
    pub failed: usize,
}

/// The profile's cookie database: `Network/Cookies` since Chromium 96, the
/// profile folder before.
pub fn cookies_path(profile_dir: &Path) -> Option<PathBuf> {
    [
        profile_dir.join("Network").join("Cookies"),
        profile_dir.join("Cookies"),
    ]
    .into_iter()
    .find(|p| p.is_file())
}

/// Opens a copied database; read-write, so SQLite can replay a copied WAL.
pub(super) fn open(db: &Path) -> Result<Connection, ImportError> {
    Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| ImportError::Unreadable(e.to_string()))
}

pub(super) fn columns(conn: &Connection, table: &str) -> Vec<String> {
    let Ok(mut stmt) = conn.prepare(&format!("PRAGMA table_info({table})")) else {
        return Vec::new();
    };
    stmt.query_map([], |row| row.get::<_, String>(1))
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
}

/// Reads every cookie of the database at `db` (a copy), decrypting with
/// `key`; cookies expired at `now` (Chromium time) are left out.
pub fn read_cookies(db: &Path, key: &SafeStorageKey, now: i64) -> Result<CookieRead, ImportError> {
    let conn = open(db)?;
    let unreadable = |e: rusqlite::Error| ImportError::Unreadable(e.to_string());
    let version: i64 = conn
        .query_row("SELECT value FROM meta WHERE key = 'version'", [], |row| {
            row.get::<_, String>(0)
        })
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let columns = columns(&conn, "cookies");
    if columns.is_empty() {
        return Err(ImportError::Unreadable(
            "the database has no cookies table".into(),
        ));
    }
    let has = |c: &str| columns.iter().any(|known| known == c);
    let pick = |c: &str, fallback: &'static str| -> String {
        if has(c) {
            c.to_owned()
        } else {
            fallback.to_owned()
        }
    };
    let sql = format!(
        "SELECT host_key, name, value, encrypted_value, path, expires_utc, {}, {}, {}, \
         creation_utc, {}, {} FROM cookies",
        pick("is_secure", "0"),
        pick("is_httponly", "0"),
        pick("samesite", "-1"),
        pick("last_access_utc", "0"),
        pick("top_frame_site_key", "''"),
    );
    let mut stmt = conn.prepare(&sql).map_err(unreadable)?;
    let rows = stmt
        .query_map([], |row| {
            Ok(Row {
                host_key: row.get(0)?,
                name: row.get(1)?,
                value: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                encrypted: row.get::<_, Option<Vec<u8>>>(3)?.unwrap_or_default(),
                path: row.get(4)?,
                expires: row.get(5)?,
                secure: row.get::<_, i64>(6)? != 0,
                http_only: row.get::<_, i64>(7)? != 0,
                same_site: row.get(8)?,
                creation: row.get(9)?,
                last_access: row.get(10)?,
                partition: row.get::<_, Option<String>>(11)?.unwrap_or_default(),
            })
        })
        .map_err(unreadable)?;
    let mut read = CookieRead::default();
    for row in rows {
        let row = row.map_err(unreadable)?;
        read.take(row, version, key, now);
    }
    Ok(read)
}

struct Row {
    host_key: String,
    name: String,
    value: String,
    encrypted: Vec<u8>,
    path: String,
    expires: i64,
    secure: bool,
    http_only: bool,
    same_site: i64,
    creation: i64,
    last_access: i64,
    partition: String,
}

impl CookieRead {
    fn take(&mut self, row: Row, version: i64, key: &SafeStorageKey, now: i64) {
        if is_non_transplantable(&row.host_key) {
            self.skipped_google += 1;
            return;
        }
        let host = row.host_key.trim_start_matches('.');
        if host.is_empty() || host.contains(char::is_whitespace) || !row.partition.is_empty() {
            self.skipped_unsupported += 1;
            return;
        }
        if row.expires > 0 && row.expires <= now {
            self.skipped_expired += 1;
            return;
        }
        let value = if row.encrypted.is_empty() {
            row.value
        } else {
            let plain = match crypto::decrypt(&row.encrypted, key) {
                Decrypted::Plain(plain) => plain,
                Decrypted::AppBound => {
                    self.skipped_app_bound += 1;
                    return;
                }
                Decrypted::Failed => {
                    self.failed += 1;
                    return;
                }
            };
            let plain = if version >= 24 {
                match crypto::strip_host_hash(plain, &row.host_key) {
                    Some(plain) => plain,
                    None => {
                        self.failed += 1;
                        return;
                    }
                }
            } else {
                plain
            };
            // Cookie values are bytes; Latin-1 keeps any that are not UTF-8.
            String::from_utf8(plain)
                .unwrap_or_else(|e| e.into_bytes().into_iter().map(char::from).collect())
        };
        self.cookies.push(ImportedCookie {
            name: row.name,
            value,
            host_key: row.host_key,
            path: if row.path.is_empty() {
                "/".into()
            } else {
                row.path
            },
            secure: row.secure,
            http_only: row.http_only,
            same_site: SameSite::from_column(row.same_site),
            expires: (row.expires > 0).then_some(row.expires),
            creation: row.creation,
            last_access: row.last_access,
        });
    }
}
