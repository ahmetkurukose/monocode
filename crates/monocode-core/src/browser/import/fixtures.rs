//! Synthetic browser profiles for the tests: fake `Local State`, `Cookies`
//! and `Login Data` files encrypted with a known test password. No test
//! touches a real browser's data or the Keychain.

use super::keychain::{KeychainError, SafeStorage};
use super::{unix_ms_to_chrome_time, SafeStorageKey, SourceBrowser};
use rusqlite::{params, Connection};
use std::path::Path;

pub const TEST_PASSWORD: &[u8] = b"test-safe-storage-password";

pub fn key() -> SafeStorageKey {
    SafeStorageKey::derive(TEST_PASSWORD)
}

/// 2026-01-01T00:00:00Z, as Chromium time: "now" in the tests.
pub fn now() -> i64 {
    unix_ms_to_chrome_time(1_767_225_600_000)
}

pub const DAY: i64 = 86_400 * 1_000_000;

/// A Safe Storage that answers with a fixed password, or refuses.
pub struct FakeStorage {
    pub answer: Result<Vec<u8>, KeychainError>,
    pub asked: std::sync::atomic::AtomicUsize,
}

impl FakeStorage {
    pub fn ok() -> Self {
        FakeStorage {
            answer: Ok(TEST_PASSWORD.to_vec()),
            asked: Default::default(),
        }
    }
    pub fn failing(error: KeychainError) -> Self {
        FakeStorage {
            answer: Err(error),
            asked: Default::default(),
        }
    }
    pub fn times_asked(&self) -> usize {
        self.asked.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl SafeStorage for FakeStorage {
    fn password(&self, _: SourceBrowser) -> Result<Vec<u8>, KeychainError> {
        self.asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.answer.clone()
    }
}

pub fn write_local_state(root: &Path, profiles: &[(&str, &str)]) {
    std::fs::create_dir_all(root).unwrap();
    let cache: serde_json::Map<String, serde_json::Value> = profiles
        .iter()
        .map(|(folder, name)| ((*folder).to_owned(), serde_json::json!({ "name": name })))
        .collect();
    let state = serde_json::json!({ "profile": { "info_cache": cache } });
    std::fs::write(root.join("Local State"), state.to_string()).unwrap();
}

/// A row of the fake `cookies` table.
pub struct CookieRow<'a> {
    pub host_key: &'a str,
    pub name: &'a str,
    pub value: &'a str,
    /// The stored value; `None` encrypts `value` as Chromium does for the
    /// database version.
    pub encrypted: Option<Vec<u8>>,
    pub path: &'a str,
    pub expires: i64,
    pub secure: bool,
    pub http_only: bool,
    pub same_site: i64,
    pub partition: &'a str,
}

impl Default for CookieRow<'_> {
    fn default() -> Self {
        CookieRow {
            host_key: ".example.com",
            name: "sid",
            value: "abc",
            encrypted: None,
            path: "/",
            expires: now() + 30 * DAY,
            secure: true,
            http_only: true,
            same_site: 1,
            partition: "",
        }
    }
}

/// Chromium's encrypted value of `value` for `host_key` at `version`.
pub fn encrypt_cookie(value: &str, host_key: &str, version: i64) -> Vec<u8> {
    use sha2::Digest;
    let mut plain = Vec::new();
    if version >= 24 {
        plain.extend(sha2::Sha256::digest(host_key.as_bytes()));
    }
    plain.extend(value.as_bytes());
    key().encrypt_v10(&plain)
}

/// A `Cookies` database at `path` of meta `version`.
pub fn write_cookies(path: &Path, version: i64, rows: &[CookieRow]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE meta(key LONGVARCHAR NOT NULL UNIQUE PRIMARY KEY, value LONGVARCHAR);
         CREATE TABLE cookies(creation_utc INTEGER NOT NULL, host_key TEXT NOT NULL,
           top_frame_site_key TEXT NOT NULL, name TEXT NOT NULL, value TEXT NOT NULL,
           encrypted_value BLOB NOT NULL, path TEXT NOT NULL, expires_utc INTEGER NOT NULL,
           is_secure INTEGER NOT NULL, is_httponly INTEGER NOT NULL,
           last_access_utc INTEGER NOT NULL, has_expires INTEGER NOT NULL,
           is_persistent INTEGER NOT NULL, priority INTEGER NOT NULL,
           samesite INTEGER NOT NULL, source_scheme INTEGER NOT NULL,
           source_port INTEGER NOT NULL, last_update_utc INTEGER NOT NULL);",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO meta(key, value) VALUES ('version', ?1)",
        params![version.to_string()],
    )
    .unwrap();
    for (ix, row) in rows.iter().enumerate() {
        let encrypted = row
            .encrypted
            .clone()
            .unwrap_or_else(|| encrypt_cookie(row.value, row.host_key, version));
        conn.execute(
            "INSERT INTO cookies VALUES (?1, ?2, ?3, ?4, '', ?5, ?6, ?7, ?8, ?9, ?10, ?11, 1, 1,
               ?12, 2, 443, ?10)",
            params![
                now() - DAY + ix as i64,
                row.host_key,
                row.partition,
                row.name,
                encrypted,
                row.path,
                row.expires,
                row.secure as i64,
                row.http_only as i64,
                now() - DAY,
                (row.expires > 0) as i64,
                row.same_site,
            ],
        )
        .unwrap();
    }
}

/// A row of the fake `logins` table.
pub struct LoginRow<'a> {
    pub origin: &'a str,
    pub username: &'a str,
    pub password: &'a str,
    pub encrypted: Option<Vec<u8>>,
    pub created_ms: i64,
    pub modified_ms: i64,
    pub never_save: bool,
    pub scheme: i64,
}

impl Default for LoginRow<'_> {
    fn default() -> Self {
        LoginRow {
            origin: "https://example.com/login",
            username: "me@example.com",
            password: "hunter2",
            encrypted: None,
            created_ms: 1_700_000_000_000,
            modified_ms: 1_700_000_000_000,
            never_save: false,
            scheme: 0,
        }
    }
}

pub fn write_logins(path: &Path, rows: &[LoginRow]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE logins (origin_url VARCHAR NOT NULL, action_url VARCHAR,
           username_element VARCHAR, username_value VARCHAR, password_element VARCHAR,
           password_value BLOB, submit_element VARCHAR, signon_realm VARCHAR NOT NULL,
           date_created INTEGER NOT NULL, blacklisted_by_user INTEGER NOT NULL,
           scheme INTEGER NOT NULL, password_type INTEGER, times_used INTEGER,
           form_data BLOB, display_name VARCHAR, icon_url VARCHAR, federation_url VARCHAR,
           skip_zero_click INTEGER, generation_upload_status INTEGER,
           possible_username_pairs BLOB, id INTEGER PRIMARY KEY AUTOINCREMENT,
           date_last_used INTEGER NOT NULL DEFAULT 0, moving_blocked_for BLOB,
           date_password_modified INTEGER NOT NULL DEFAULT 0);",
    )
    .unwrap();
    for row in rows {
        let encrypted = if row.never_save {
            Vec::new()
        } else {
            row.encrypted
                .clone()
                .unwrap_or_else(|| key().encrypt_v10(row.password.as_bytes()))
        };
        let realm = super::logins::origin_of(row.origin) + "/";
        conn.execute(
            "INSERT INTO logins (origin_url, username_value, password_value, signon_realm,
               date_created, blacklisted_by_user, scheme, times_used, date_last_used,
               date_password_modified)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 3, ?8, ?9)",
            params![
                row.origin,
                if row.never_save { "" } else { row.username },
                encrypted,
                realm,
                unix_ms_to_chrome_time(row.created_ms),
                row.never_save as i64,
                row.scheme,
                unix_ms_to_chrome_time(row.created_ms + 1000),
                if row.modified_ms == 0 {
                    0
                } else {
                    unix_ms_to_chrome_time(row.modified_ms)
                },
            ],
        )
        .unwrap();
    }
}
