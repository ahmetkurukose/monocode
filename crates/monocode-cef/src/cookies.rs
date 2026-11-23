//! Writing cookies into a profile (docs/native/browser/context.md K61): the
//! cookies an import brought, through the profile's `CefCookieManager`.
//! A cookie replaces the one with the same name, domain and path.
//!
//! CEF answers each write on its UI thread, the main thread here, so the
//! batch's tally needs no lock.

use std::{cell::RefCell, rc::Rc};

use cef::*;

/// Chromium's SameSite.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SameSite {
    #[default]
    Unspecified,
    NoRestriction,
    Lax,
    Strict,
}

/// A cookie to write.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct NewCookie {
    /// A URL the cookie belongs to (`https://host/path` for a secure one).
    pub url: String,
    pub name: String,
    pub value: String,
    /// `.example.com` for a domain cookie; empty for a host-only one.
    pub domain: String,
    pub path: String,
    pub secure: bool,
    pub http_only: bool,
    pub same_site: SameSite,
    /// Microseconds since 1601 (Chromium's and CEF's base time); `None` for
    /// a session cookie.
    pub expires: Option<i64>,
    pub creation: i64,
    pub last_access: i64,
}

impl std::fmt::Debug for NewCookie {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewCookie")
            .field("url", &self.url)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// How a batch went.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CookieWrite {
    pub written: usize,
    pub failed: usize,
}

struct Batch {
    pending: usize,
    tally: CookieWrite,
    manager: Option<CookieManager>,
    done: Option<Box<dyn FnOnce(CookieWrite)>>,
}

impl Batch {
    fn settle(&mut self, ok: bool) {
        if ok {
            self.tally.written += 1;
        } else {
            self.tally.failed += 1;
        }
        self.release();
    }

    /// One fewer answer to wait for; the last finishes the batch.
    fn release(&mut self) {
        self.pending -= 1;
        if self.pending > 0 {
            return;
        }
        if let Some(manager) = self.manager.take() {
            // Onto the disk now, not at the next flush.
            manager.flush_store(None);
        }
        if let Some(done) = self.done.take() {
            done(self.tally);
        }
    }
}

wrap_set_cookie_callback! {
    struct CookieSet {
        batch: Rc<RefCell<Batch>>,
    }

    impl SetCookieCallback {
        fn on_complete(&self, success: ::std::os::raw::c_int) {
            self.batch.borrow_mut().settle(success != 0);
        }
    }
}

/// Writes `cookies` into `profile` (on the main thread, with CEF running);
/// `done` runs on the main thread once every write was answered.
pub fn set_cookies(
    profile: &crate::Profile,
    cookies: Vec<NewCookie>,
    done: impl FnOnce(CookieWrite) + 'static,
) {
    let total = cookies.len();
    let failed_all = CookieWrite {
        written: 0,
        failed: total,
    };
    if !crate::runtime::is_started() {
        return done(failed_all);
    }
    let context =
        crate::profile::request_context(profile).or_else(cef::request_context_get_global_context);
    let Some(manager) = context.and_then(|c| c.cookie_manager(None)) else {
        return done(failed_all);
    };
    let batch = Rc::new(RefCell::new(Batch {
        // One more than there are cookies, until the loop is over: an
        // answer inside `set_cookie` must not finish the batch early.
        pending: total + 1,
        tally: CookieWrite::default(),
        manager: Some(manager.clone()),
        done: Some(Box::new(done)),
    }));
    for cookie in cookies {
        let url = CefString::from(cookie.url.as_str());
        let raw = Cookie {
            size: std::mem::size_of::<sys::_cef_cookie_t>(),
            name: cookie.name.as_str().into(),
            value: cookie.value.as_str().into(),
            domain: cookie.domain.as_str().into(),
            path: cookie.path.as_str().into(),
            secure: cookie.secure as i32,
            httponly: cookie.http_only as i32,
            creation: Basetime {
                val: cookie.creation,
            },
            last_access: Basetime {
                val: cookie.last_access,
            },
            has_expires: cookie.expires.is_some() as i32,
            expires: Basetime {
                val: cookie.expires.unwrap_or_default(),
            },
            same_site: match cookie.same_site {
                SameSite::Unspecified => CookieSameSite::UNSPECIFIED,
                SameSite::NoRestriction => CookieSameSite::NO_RESTRICTION,
                SameSite::Lax => CookieSameSite::LAX_MODE,
                SameSite::Strict => CookieSameSite::STRICT_MODE,
            },
            priority: CookiePriority::default(),
        };
        let mut callback = CookieSet::new(batch.clone());
        // Zero: the URL or cookie was refused at once, and no answer comes.
        if manager.set_cookie(Some(&url), Some(&raw), Some(&mut callback)) == 0 {
            batch.borrow_mut().settle(false);
        }
    }
    batch.borrow_mut().release();
}
