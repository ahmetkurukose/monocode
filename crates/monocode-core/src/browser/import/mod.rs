//! Importing cookies and passwords from Chrome, Arc and Brave on macOS
//! (docs/native/browser/context.md K61, §3.3).
//!
//! Manual and one-shot: the user picks a browser and one of its profiles,
//! and what to bring. Nothing here writes into Monocode: [`read_profile`]
//! returns what it read, and the app writes the cookies into a Chromium
//! profile and hands the logins to the password vault. Importing again
//! merges ([`merge`]).
//!
//! - [`discovery`]: the browsers installed and their profiles (`Local State`).
//! - [`crypto`]: the "Safe Storage" key and Chromium's `v10` values.
//! - [`keychain`]: the browser's Safe Storage password, one Keychain prompt
//!   per browser per import.
//! - [`cookies`]: the `Cookies` database, and the cookies never imported.
//! - [`logins`]: the `Login Data` databases.
//! - [`merge`]: what a second import keeps.
//!
//! The browser may be running and hold its databases: each file is copied,
//! with its `-wal` and `-journal`, into a temporary folder and read there.

pub mod cookies;
pub mod crypto;
pub mod discovery;
pub mod keychain;
pub mod logins;
pub mod merge;

#[cfg(test)]
mod fixtures;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

pub use cookies::{CookieRead, ImportedCookie, SameSite, NON_TRANSPLANTABLE_COOKIE_DOMAINS};
pub use crypto::SafeStorageKey;
pub use discovery::{discover, InstalledBrowser, SourceProfile};
pub use keychain::{Keychain, KeychainError, SafeStorage};
pub use logins::{ImportedLogin, LoginRead};
pub use merge::{merge_cookies, merge_logins, LoginMerge};

/// A browser Monocode imports from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SourceBrowser {
    Chrome,
    Arc,
    Brave,
}

impl SourceBrowser {
    pub const ALL: [SourceBrowser; 3] = [
        SourceBrowser::Chrome,
        SourceBrowser::Arc,
        SourceBrowser::Brave,
    ];

    /// A stable id, for element ids and settings.
    pub fn id(self) -> &'static str {
        match self {
            SourceBrowser::Chrome => "chrome",
            SourceBrowser::Arc => "arc",
            SourceBrowser::Brave => "brave",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SourceBrowser::Chrome => "Google Chrome",
            SourceBrowser::Arc => "Arc",
            SourceBrowser::Brave => "Brave",
        }
    }

    /// The browser's user data folder under `~/Library/Application Support`.
    /// Arc keeps Chromium's layout one level down, in `User Data`.
    pub fn user_data_dir(self, home: &Path) -> PathBuf {
        let support = home.join("Library").join("Application Support");
        match self {
            SourceBrowser::Chrome => support.join("Google").join("Chrome"),
            SourceBrowser::Arc => support.join("Arc").join("User Data"),
            SourceBrowser::Brave => support.join("BraveSoftware").join("Brave-Browser"),
        }
    }

    /// The Keychain item (service, account) holding the browser's Safe
    /// Storage password.
    pub fn keychain_item(self) -> (&'static str, &'static str) {
        match self {
            SourceBrowser::Chrome => ("Chrome Safe Storage", "Chrome"),
            SourceBrowser::Arc => ("Arc Safe Storage", "Arc"),
            SourceBrowser::Brave => ("Brave Safe Storage", "Brave"),
        }
    }
}

/// What to bring from a profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportRequest {
    pub profile: SourceProfile,
    pub cookies: bool,
    pub passwords: bool,
}

/// What a profile gave: `None` for what was not asked for.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ImportData {
    pub cookies: Option<CookieRead>,
    pub logins: Option<LoginRead>,
}

/// Why an import could not run at all. Single values that fail are counted
/// in [`CookieRead`] and [`LoginRead`] instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportError {
    /// The browser's folder is not there.
    NotInstalled(SourceBrowser),
    /// The profile's folder is gone, or holds none of what was asked for.
    ProfileMissing(String),
    /// Keychain access was refused or cancelled.
    KeychainDenied(SourceBrowser),
    /// The browser has no Safe Storage item (it never stored anything
    /// encrypted, or it is not this browser's install).
    KeychainMissing(SourceBrowser),
    /// A database could not be copied or read.
    Unreadable(String),
    /// Not on macOS.
    Unsupported,
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImportError::NotInstalled(b) => write!(f, "{} is not installed.", b.label()),
            ImportError::ProfileMissing(name) => {
                write!(
                    f,
                    "The profile \"{name}\" has no cookies or passwords to import."
                )
            }
            ImportError::KeychainDenied(b) => write!(
                f,
                "Keychain access to \"{}\" was denied. Import again and choose Allow.",
                b.keychain_item().0
            ),
            ImportError::KeychainMissing(b) => write!(
                f,
                "The Keychain has no \"{}\" item, so {}'s data cannot be decrypted.",
                b.keychain_item().0,
                b.label()
            ),
            ImportError::Unreadable(why) => write!(f, "Could not read the browser's data: {why}"),
            ImportError::Unsupported => f.write_str("Importing is only available on macOS."),
        }
    }
}

impl std::error::Error for ImportError {}

/// Reads what `request` asks for from its profile, asking `storage` for the
/// browser's key once. The returned values are plain text: keep them in
/// memory only as long as the write takes.
pub fn read_profile(
    request: &ImportRequest,
    storage: &dyn SafeStorage,
) -> Result<ImportData, ImportError> {
    read_profile_at(request, storage, chrome_now())
}

/// [`read_profile`] at a given time (Chromium microseconds), for the tests.
pub fn read_profile_at(
    request: &ImportRequest,
    storage: &dyn SafeStorage,
    now: i64,
) -> Result<ImportData, ImportError> {
    let profile = &request.profile;
    if !profile.dir.is_dir() {
        return Err(ImportError::ProfileMissing(profile.name.clone()));
    }
    let cookie_db = request
        .cookies
        .then(|| cookies::cookies_path(&profile.dir))
        .flatten();
    let login_dbs: Vec<PathBuf> = if request.passwords {
        logins::login_paths(&profile.dir)
    } else {
        Vec::new()
    };
    if cookie_db.is_none() && login_dbs.is_empty() {
        return Err(ImportError::ProfileMissing(profile.name.clone()));
    }
    let key = storage.password(profile.browser).map_err(|e| match e {
        KeychainError::Denied => ImportError::KeychainDenied(profile.browser),
        KeychainError::NotFound => ImportError::KeychainMissing(profile.browser),
        KeychainError::Unsupported => ImportError::Unsupported,
        KeychainError::Other(why) => ImportError::Unreadable(why),
    })?;
    let key = SafeStorageKey::derive(&key);
    let scratch = tempfile::Builder::new()
        .prefix("monocode-import-")
        .tempdir()
        .map_err(|e| ImportError::Unreadable(e.to_string()))?;
    let mut data = ImportData::default();
    if request.cookies {
        data.cookies = Some(match cookie_db {
            Some(db) => {
                let copy = copy_database(&db, scratch.path(), "Cookies")?;
                cookies::read_cookies(&copy, &key, now)?
            }
            None => CookieRead::default(),
        });
    }
    if request.passwords {
        let mut read = LoginRead::default();
        for (ix, db) in login_dbs.iter().enumerate() {
            let copy = copy_database(db, scratch.path(), &format!("Login Data {ix}"))?;
            read.absorb(logins::read_logins(&copy, &key)?);
        }
        data.logins = Some(read);
    }
    Ok(data)
}

/// Copies a SQLite database and the files beside it that hold changes not
/// yet in it, so SQLite replays them in the copy.
fn copy_database(db: &Path, scratch: &Path, name: &str) -> Result<PathBuf, ImportError> {
    let copy = scratch.join(name);
    std::fs::copy(db, &copy)
        .map_err(|e| ImportError::Unreadable(format!("{}: {e}", db.display())))?;
    for suffix in ["-wal", "-journal"] {
        let side = PathBuf::from(format!("{}{suffix}", db.display()));
        if side.is_file() {
            let _ = std::fs::copy(&side, scratch.join(format!("{name}{suffix}")));
        }
    }
    Ok(copy)
}

/// Seconds between 1601-01-01 (Chromium's epoch) and 1970-01-01.
const CHROME_EPOCH_OFFSET_SECS: i64 = 11_644_473_600;

/// A Chromium time (microseconds since 1601) as Unix milliseconds; zero
/// (never) stays zero.
pub fn chrome_time_to_unix_ms(t: i64) -> i64 {
    if t <= 0 {
        return 0;
    }
    (t / 1000 - CHROME_EPOCH_OFFSET_SECS * 1000).max(0)
}

/// Unix milliseconds as a Chromium time.
pub fn unix_ms_to_chrome_time(ms: i64) -> i64 {
    (ms + CHROME_EPOCH_OFFSET_SECS * 1000) * 1000
}

/// Now, as a Chromium time.
pub fn chrome_now() -> i64 {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default();
    unix_ms_to_chrome_time(ms)
}

/// What an import did, for the result line in Settings.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportSummary {
    /// Cookies written into the profile.
    pub cookies_imported: usize,
    /// Cookies left behind: app-bound (`v20`), expired, Google's, and ones
    /// Monocode cannot set (partitioned, malformed).
    pub cookies_app_bound: usize,
    pub cookies_expired: usize,
    pub cookies_google: usize,
    pub cookies_unsupported: usize,
    /// Cookies that would not decrypt or that Chromium refused.
    pub cookies_failed: usize,
    /// Passwords read, and of them what the vault added or updated.
    pub logins_read: usize,
    pub logins_added: usize,
    pub logins_updated: usize,
    /// "Never save" sites.
    pub never_save: usize,
    pub logins_app_bound: usize,
    pub logins_failed: usize,
    pub cookies_asked: bool,
    pub passwords_asked: bool,
}

impl ImportSummary {
    /// The summary of what was read, before anything is written: cookies
    /// count as failed until the write says otherwise.
    pub fn of_read(data: &ImportData) -> Self {
        let mut s = ImportSummary::default();
        if let Some(c) = &data.cookies {
            s.cookies_asked = true;
            s.cookies_app_bound = c.skipped_app_bound;
            s.cookies_expired = c.skipped_expired;
            s.cookies_google = c.skipped_google;
            s.cookies_unsupported = c.skipped_unsupported;
            s.cookies_failed = c.failed;
        }
        if let Some(l) = &data.logins {
            s.passwords_asked = true;
            s.logins_read = l.logins.len();
            s.never_save = l.never_save.len();
            s.logins_app_bound = l.skipped_app_bound;
            s.logins_failed = l.failed;
        }
        s
    }

    pub fn cookies_skipped(&self) -> usize {
        self.cookies_app_bound
            + self.cookies_expired
            + self.cookies_google
            + self.cookies_unsupported
    }

    /// One line per kind asked for, in plain words.
    pub fn lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if self.cookies_asked {
            let mut line = format!("Cookies: {} imported", self.cookies_imported);
            let skipped = self.cookies_skipped();
            if skipped > 0 {
                let mut why = Vec::new();
                for (n, what) in [
                    (self.cookies_google, "Google"),
                    (self.cookies_expired, "expired"),
                    (self.cookies_app_bound, "app-bound"),
                    (self.cookies_unsupported, "unsupported"),
                ] {
                    if n > 0 {
                        why.push(format!("{n} {what}"));
                    }
                }
                line.push_str(&format!(", {skipped} skipped ({})", why.join(", ")));
            }
            if self.cookies_failed > 0 {
                line.push_str(&format!(", {} failed", self.cookies_failed));
            }
            lines.push(line);
        }
        if self.passwords_asked {
            let mut line = format!("Passwords: {} read", self.logins_read);
            if self.logins_added + self.logins_updated > 0 {
                line.push_str(&format!(
                    " ({} new, {} updated)",
                    self.logins_added, self.logins_updated
                ));
            }
            if self.never_save > 0 {
                line.push_str(&format!(", {} never-save sites", self.never_save));
            }
            if self.logins_app_bound > 0 {
                line.push_str(&format!(", {} app-bound skipped", self.logins_app_bound));
            }
            if self.logins_failed > 0 {
                line.push_str(&format!(", {} failed", self.logins_failed));
            }
            lines.push(line);
        }
        lines
    }
}
