//! What the agent tools and the external CDP endpoint need from the app
//! (context.md §6, K21–K42). The native app implements these over its
//! browser pages; the tests over fakes.
//!
//! Main thread only: nothing here is `Send`, and every future runs on the
//! app's foreground executor.

use std::{any::Any, rc::Rc, time::Duration};

use futures_util::future::LocalBoxFuture;

use super::{cdp::CdpClient, log::PageLog};

/// A page's id: the app's pane id, stable for the page's life (context.md
/// §3.1 item 1).
pub type PageId = i64;

/// Whose calls these are: an agent session and its project (K21).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Scope {
    pub session: i64,
    /// The project's folder (the session summary's `cwd`, not a worktree).
    pub project: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PageInfo {
    pub id: PageId,
    pub url: String,
    pub title: String,
    pub loading: bool,
    /// The user can see it now (its tab is the selected one of a shown group).
    pub shown: bool,
    /// An agent opened it (`browser_tab_open`, or `Target.createTarget`).
    pub by_agent: bool,
    /// It browses with a throwaway in-memory profile (K22).
    pub isolated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogKind {
    Alert,
    Confirm,
    Prompt,
    BeforeUnload,
}

/// A JavaScript dialog the page is waiting on.
#[derive(Clone, Debug, PartialEq)]
pub struct DialogInfo {
    pub kind: DialogKind,
    pub message: String,
    /// `prompt`'s default text.
    pub default_text: Option<String>,
}

pub trait Page {
    fn info(&self) -> PageInfo;

    /// The page's CDP multiplexer; `None` while its browser does not exist
    /// yet (a restored tab not shown yet, K20) or is gone.
    fn cdp(&self) -> Option<Rc<CdpClient>>;

    /// The page's console and network log, kept from its first load (K36).
    fn log(&self) -> Option<Rc<PageLog>>;

    /// Makes sure the page's browser exists (a restored tab loads now, in the
    /// background). Resolves once [`Page::cdp`] is `Some`, or with why not.
    fn ensure_loaded(&self) -> LocalBoxFuture<'static, Result<(), String>>;

    /// While the returned lease lives, the page keeps drawing even when the
    /// user does not see it: the app moves its view out of the window instead
    /// of hiding it (K23), so screenshots and layout keep working.
    fn keep_drawn(&self) -> Box<dyn Any>;

    /// The JavaScript dialog the page waits on, if any (K24 `browser_dialog`).
    fn dialog(&self) -> Option<DialogInfo>;

    /// Answers [`Page::dialog`]; false when there is none.
    fn answer_dialog(&self, accept: bool, text: Option<&str>) -> bool;

    /// The user gave the page input in the last 2 seconds (K34).
    fn user_active(&self) -> bool;
}

pub trait Host {
    /// Whether Chromium runs; tools start it on demand through
    /// [`Host::open`] or [`Page::ensure_loaded`].
    fn engine_started(&self) -> bool;

    /// The pages `scope` may use (K21): browser pages of its project, and
    /// those its session opened.
    fn pages(&self, scope: &Scope) -> Vec<Rc<dyn Page>>;

    fn page(&self, scope: &Scope, id: PageId) -> Option<Rc<dyn Page>> {
        self.pages(scope).into_iter().find(|p| p.info().id == id)
    }

    /// Opens `url` as a new tab for `scope`'s session, in the background
    /// (K39), in the project's profile or a throwaway one (K22). Resolves
    /// once the page's CDP client exists.
    fn open(
        &self,
        scope: &Scope,
        url: &str,
        isolated: bool,
    ) -> LocalBoxFuture<'static, Result<Rc<dyn Page>, String>>;

    /// Closes a page of `scope`; false when it has no such page.
    fn close(&self, scope: &Scope, id: PageId) -> bool;

    /// Shows a page of `scope` to the user (selects its tab).
    fn show(&self, scope: &Scope, id: PageId) -> bool;

    /// A timer on the app's executor.
    fn sleep(&self, duration: Duration) -> LocalBoxFuture<'static, ()>;
}
