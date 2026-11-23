//! Whether a sign-in worked (context.md K56, after Orca and Chromium): the
//! watcher reports a form sent with a password; the sign-in counts as
//! successful when
//!
//! - in the same document, the password field goes away (a single-page app
//!   moved on; the watcher looks for [`SAME_DOCUMENT_MS`]);
//! - after a navigation, the next document shows no password field for
//!   [`NEXT_DOCUMENT_MS`] (one that shows one again is the form back with
//!   an error).
//!
//! A user name sent on its own (step one of a two-step sign-in) is kept for
//! [`USERNAME_STEP_MS`] and joins the password sent next on the same site.
//!
//! Pure: the page feeds it signals and the time, and asks [`Tracker::tick`].

use super::vault::origin_of;

pub const SAME_DOCUMENT_MS: u64 = 6_000;
pub const NEXT_DOCUMENT_MS: u64 = 2_500;
/// A next document that never says anything counts after this long.
pub const SILENT_DOCUMENT_MS: u64 = 10_000;
pub const USERNAME_STEP_MS: u64 = 5 * 60 * 1000;

/// A form sent with a password.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Submission {
    /// The document's address when it was sent.
    pub url: String,
    pub origin: String,
    pub username: String,
    pub password: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The sign-in worked: offer to save it.
    Success(Submission),
    /// It did not, or nothing could tell: forget it.
    Failed,
}

#[derive(Debug)]
struct Pending {
    submission: Submission,
    /// The document (execution context) that sent it.
    context: i64,
    /// The top document then (the sending one may be an iframe's).
    top: Option<i64>,
    at: u64,
    /// When another document first reported, and whether it showed a
    /// password field then.
    next_document: Option<u64>,
}

#[derive(Debug, Default)]
pub struct Tracker {
    pending: Option<Pending>,
    /// (origin, user name, when) of a user-name-only step.
    username_step: Option<(String, String, u64)>,
    /// The top document last heard from (only the top one reports forms).
    top: Option<i64>,
}

impl Tracker {
    /// A form of document `context` at `url` was sent with `password`.
    pub fn submitted(
        &mut self,
        url: &str,
        username: &str,
        password: &str,
        context: i64,
        now: u64,
    ) {
        let Some(origin) = origin_of(url) else {
            return;
        };
        if password.is_empty() {
            return;
        }
        let mut username = username.trim().to_owned();
        if username.is_empty() {
            if let Some((step_origin, step_user, at)) = &self.username_step {
                if *step_origin == origin && now.saturating_sub(*at) <= USERNAME_STEP_MS {
                    username = step_user.clone();
                }
            }
        }
        self.pending = Some(Pending {
            submission: Submission {
                url: url.to_owned(),
                origin,
                username,
                password: password.to_owned(),
            },
            context,
            top: self.top,
            at: now,
            next_document: None,
        });
    }

    /// A user name was sent on its own at `url`.
    pub fn username_step(&mut self, url: &str, username: &str, now: u64) {
        let username = username.trim();
        if let (Some(origin), false) = (origin_of(url), username.is_empty()) {
            self.username_step = Some((origin, username.to_owned(), now));
        }
    }

    /// The sent form's password field went away in its document.
    pub fn gone(&mut self, context: i64) -> Option<Verdict> {
        let pending = self.pending.as_ref()?;
        if pending.context != context || pending.next_document.is_some() {
            return None;
        }
        self.finish(true)
    }

    /// The top document `context` reports whether it shows a password
    /// field.
    pub fn forms(&mut self, context: i64, password: bool, now: u64) -> Option<Verdict> {
        self.top = Some(context);
        let pending = self.pending.as_mut()?;
        if pending.context == context || pending.top == Some(context) {
            return None;
        }
        if password {
            // The form came back: the sign-in failed.
            return self.finish(false);
        }
        pending.next_document.get_or_insert(now);
        None
    }

    /// What the time alone decides.
    pub fn tick(&mut self, now: u64) -> Option<Verdict> {
        let pending = self.pending.as_ref()?;
        match pending.next_document {
            Some(since) if now.saturating_sub(since) >= NEXT_DOCUMENT_MS => self.finish(true),
            Some(_) => None,
            None if now.saturating_sub(pending.at) >= SILENT_DOCUMENT_MS => self.finish(false),
            None => None,
        }
    }

    /// Whether a sign-in waits for its verdict.
    pub fn waiting(&self) -> bool {
        self.pending.is_some()
    }

    /// Forgets what waits (the page closed).
    pub fn clear(&mut self) {
        self.pending = None;
    }

    fn finish(&mut self, success: bool) -> Option<Verdict> {
        let pending = self.pending.take()?;
        if success {
            self.username_step = None;
            Some(Verdict::Success(pending.submission))
        } else {
            Some(Verdict::Failed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn success(verdict: Option<Verdict>) -> Submission {
        match verdict {
            Some(Verdict::Success(s)) => s,
            other => panic!("not a success: {other:?}"),
        }
    }

    #[test]
    fn a_single_page_sign_in_works_when_the_form_goes_away() {
        let mut t = Tracker::default();
        t.submitted("https://a.test/login", " alice ", "pw", 1, 0);
        assert!(t.waiting());
        // Another document's field going away says nothing.
        assert_eq!(t.gone(2), None);
        let s = success(t.gone(1));
        assert_eq!(
            (s.origin.as_str(), s.username.as_str(), s.password.as_str()),
            ("https://a.test", "alice", "pw")
        );
        assert!(!t.waiting());
    }

    #[test]
    fn a_form_still_there_is_a_failed_sign_in() {
        let mut t = Tracker::default();
        t.submitted("https://a.test/login", "alice", "wrong", 1, 0);
        assert_eq!(t.tick(SAME_DOCUMENT_MS), None);
        // The next document that never reports: the time decides.
        assert_eq!(t.tick(SILENT_DOCUMENT_MS), Some(Verdict::Failed));
        assert!(!t.waiting());
    }

    #[test]
    fn after_a_navigation_the_next_document_decides() {
        // No password field there for a while: success.
        let mut t = Tracker::default();
        t.submitted("https://a.test/login", "alice", "pw", 1, 0);
        assert_eq!(t.forms(1, true, 10), None, "the sending document itself");
        assert_eq!(t.forms(2, false, 300), None);
        assert_eq!(t.tick(300 + NEXT_DOCUMENT_MS - 1), None);
        assert_eq!(success(t.tick(300 + NEXT_DOCUMENT_MS)).username, "alice");

        // The form shows again (with an error): failure.
        let mut t = Tracker::default();
        t.submitted("https://a.test/login", "alice", "wrong", 1, 0);
        assert_eq!(t.forms(2, false, 300), None);
        assert_eq!(t.forms(2, true, 1200), Some(Verdict::Failed));
        assert_eq!(t.tick(10_000), None);
    }

    #[test]
    fn a_form_in_an_iframe_waits_for_the_next_top_document() {
        let mut t = Tracker::default();
        assert_eq!(t.forms(1, false, 0), None);
        t.submitted("https://a.test/", "alice", "pw", 7, 10);
        // The same top document says again it shows no password field.
        assert_eq!(t.forms(1, false, 20), None);
        assert_eq!(t.tick(20 + NEXT_DOCUMENT_MS), None);
        assert_eq!(t.forms(2, false, 100), None);
        assert!(matches!(t.tick(100 + NEXT_DOCUMENT_MS), Some(Verdict::Success(_))));
    }

    #[test]
    fn a_user_name_step_joins_the_password_step() {
        let mut t = Tracker::default();
        t.username_step("https://a.test/step1", "alice@a.test", 0);
        t.submitted("https://a.test/step2", "", "pw", 3, 1000);
        assert_eq!(success(t.gone(3)).username, "alice@a.test");
        // Another site's, or one too old, does not.
        t.username_step("https://a.test/step1", "alice", 0);
        t.submitted("https://b.test/", "", "pw", 4, 10);
        assert_eq!(success(t.gone(4)).username, "");
        t.username_step("https://a.test/step1", "alice", 0);
        t.submitted("https://a.test/", "", "pw", 5, USERNAME_STEP_MS + 1);
        assert_eq!(success(t.gone(5)).username, "");
    }

    #[test]
    fn nothing_waits_for_forms_without_a_password_or_off_the_web() {
        let mut t = Tracker::default();
        t.submitted("https://a.test/", "alice", "", 1, 0);
        t.submitted("about:blank", "alice", "pw", 1, 0);
        assert!(!t.waiting());
    }
}
