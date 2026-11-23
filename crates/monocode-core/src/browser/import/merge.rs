//! What importing again keeps (K61): pure functions over what was read, so
//! the cookie write and the password vault apply the same rules.
//!
//! - Cookies: an imported cookie replaces the one with the same name,
//!   domain and path.
//! - Logins: one per (origin, username). A login already there stays unless
//!   the imported one has another password and is newer (its password
//!   changed, or else it was saved, later); a tie keeps what is there.

use super::cookies::ImportedCookie;
use super::logins::{origin_of, ImportedLogin};
use std::collections::HashMap;

/// `existing` with `incoming` laid over it, one cookie per name, domain and
/// path; the order is `existing`'s, new cookies after.
pub fn merge_cookies(
    existing: Vec<ImportedCookie>,
    incoming: impl IntoIterator<Item = ImportedCookie>,
) -> Vec<ImportedCookie> {
    let mut out = existing;
    let mut at: HashMap<(String, String, String), usize> = HashMap::new();
    for (ix, cookie) in out.iter().enumerate() {
        at.insert(owned_key(cookie), ix);
    }
    for cookie in incoming {
        match at.get(&owned_key(&cookie)) {
            Some(&ix) => out[ix] = cookie,
            None => {
                at.insert(owned_key(&cookie), out.len());
                out.push(cookie);
            }
        }
    }
    out
}

fn owned_key(cookie: &ImportedCookie) -> (String, String, String) {
    let (name, domain, path) = cookie.key();
    (
        name.to_owned(),
        domain.to_ascii_lowercase(),
        path.to_owned(),
    )
}

/// The logins after a merge, and what the merge did to them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LoginMerge {
    pub logins: Vec<ImportedLogin>,
    /// Incoming logins that were not there.
    pub added: usize,
    /// Incoming logins that replaced an older password.
    pub updated: usize,
    /// Incoming logins that changed nothing: the same password, or an older
    /// one.
    pub unchanged: usize,
}

/// The key a login is merged by.
pub fn login_key(login: &ImportedLogin) -> (String, String) {
    (
        origin_of(&login.origin).to_ascii_lowercase(),
        login.username.clone(),
    )
}

/// Merges `incoming` into `existing` by the rules above.
pub fn merge_logins(
    existing: Vec<ImportedLogin>,
    incoming: impl IntoIterator<Item = ImportedLogin>,
) -> LoginMerge {
    let mut merge = LoginMerge {
        logins: existing,
        ..Default::default()
    };
    let mut at: HashMap<(String, String), usize> = HashMap::new();
    for (ix, login) in merge.logins.iter().enumerate() {
        at.entry(login_key(login)).or_insert(ix);
    }
    for login in incoming {
        let key = login_key(&login);
        let Some(&ix) = at.get(&key) else {
            at.insert(key, merge.logins.len());
            merge.logins.push(login);
            merge.added += 1;
            continue;
        };
        let kept = &mut merge.logins[ix];
        if kept.password != login.password && login.stamp() > kept.stamp() {
            *kept = login;
            merge.updated += 1;
        } else {
            // The same password, or an older one: what is there stays, and
            // learns how much the login was used.
            if kept.password == login.password {
                kept.last_used_ms = kept.last_used_ms.max(login.last_used_ms);
                kept.times_used = kept.times_used.max(login.times_used);
            }
            merge.unchanged += 1;
        }
    }
    merge
}
