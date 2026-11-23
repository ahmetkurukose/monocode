//! The app's side of the browser agent layer (docs/native/browser/context.md
//! §6, K21–K42): the [`Host`] and [`Page`] the `monocode_browser` tools and
//! the external CDP endpoint drive, over the browser pages of every window,
//! and the pump that runs their jobs on the main thread.
//!
//! A page's id is its pane id, never reused during the run. Pages of a
//! scope (K21) are the browser panes whose owner session works in the
//! scope's project, and the ones the scope's session opened.
use super::*;
use crate::workspace::BrowserPane;
use gpui::{AsyncApp, WindowHandle};
use monocode_core::browser::{
    CdpClient, DialogInfo, Host, Page, PageId, PageInfo, Scope, UserControl, log::PageLog,
};
use std::{
    any::Any,
    future::Future,
    path::PathBuf,
    pin::Pin,
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

type LocalFuture<T> = Pin<Box<dyn Future<Output = T>>>;

/// How long an agent waits for a page to come up (K35's command timeout).
const LOAD_PATIENCE: Duration = Duration::from_secs(30);
/// How often a page coming up is looked at.
const LOAD_POLL: Duration = Duration::from_millis(50);

/// Runs the agent layer's jobs (`monocode_core::browser::jobs`) on the main
/// thread from now on, each as soon as it comes, none waiting for another.
/// Once, at app start.
pub(crate) fn start(cx: &mut App) {
    let host: Rc<dyn Host> = Rc::new(AppHost::new(cx.to_async()));
    let jobs = monocode_core::browser::jobs();
    cx.spawn(async move |cx| {
        while let Ok(job) = jobs.recv().await {
            cx.foreground_executor().spawn(job(host.clone())).detach();
        }
    })
    .detach();
}

/// Every window's shell.
fn shells(cx: &App) -> Vec<WindowHandle<Shell>> {
    (cx.windows().into_iter())
        .filter_map(|window| window.downcast::<Shell>())
        .collect()
}

/// Where a new agent tab goes (K39): the group of the session's chat when
/// it is open, else the focused group; `None` when there is neither.
fn agent_tab_anchor(chat: Option<i64>, focused: Option<i64>) -> Option<i64> {
    chat.or(focused)
}

/// Names a throwaway profile, unique for the app's run.
fn ephemeral_profile_name() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!("agent-{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

/// The session id of the dev hook's scope: no real session has it.
const DEV_HOOK_SESSION: i64 = i64::MAX - 1;

impl Shell {
    /// Development hook, not a user feature: with
    /// `MONOCODE_BROWSER_AGENT_DEV=<file>` set, the app issues the browser
    /// bridge's environment for a synthetic session whose project is
    /// `MONOCODE_BROWSER_AGENT_DEV_PROJECT` or the window's project (the
    /// project `MONOCODE_BROWSER_OPEN`'s page belongs to), turns the external
    /// CDP endpoint on, and writes `{bridge_env, cdp_endpoint, devtools_base,
    /// scope}` as JSON to the file. A script then drives `browser-mcp` and the endpoint
    /// as an agent would.
    pub(super) fn start_browser_agent_dev_hook(&mut self, cx: &mut Context<Self>) {
        let Some(file) = std::env::var_os("MONOCODE_BROWSER_AGENT_DEV").filter(|f| !f.is_empty())
        else {
            return;
        };
        let project = std::env::var("MONOCODE_BROWSER_AGENT_DEV_PROJECT")
            .ok()
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| self.workspace.project.clone());
        let written = (|| -> Result<(), String> {
            let executable = std::env::current_exe().map_err(|e| e.to_string())?;
            let bridge_env = monocode_core::browser::bridge::environment(
                DEV_HOOK_SESSION,
                std::path::Path::new(&project),
                &executable,
            )?;
            self.workspace.preferences.browser_external_cdp = true;
            self.apply_preferences(cx);
            let scope = Scope {
                session: DEV_HOOK_SESSION,
                project: project.clone(),
            };
            let cdp_endpoint = monocode_core::browser::proxy::endpoint(&scope)?;
            let devtools_base = monocode_core::browser::proxy::devtools_base(&scope)?;
            let json = serde_json::json!({
                "bridge_env": bridge_env,
                "cdp_endpoint": cdp_endpoint,
                "devtools_base": devtools_base,
                "scope": {"session": DEV_HOOK_SESSION, "project": project},
            });
            std::fs::write(&file, json.to_string()).map_err(|e| e.to_string())
        })();
        if let Err(error) = written {
            eprintln!("MONOCODE_BROWSER_AGENT_DEV: {error}");
        }
    }

    /// Whether `scope` may use the page of `pane` (K21).
    fn agent_scope_has(&self, state: &BrowserPane, scope: &Scope) -> bool {
        if state.opened_by == Some(scope.session) {
            return true;
        }
        let project = monocode_core::files::path_key(&scope.project);
        (self.session_summaries.get(&state.owner))
            .is_some_and(|owner| monocode_core::files::path_key(&owner.cwd) == project)
    }

    /// The browser panes `scope` may use, oldest first.
    fn agent_pages(&self, scope: &Scope) -> Vec<i64> {
        let mut panes: Vec<i64> = (self.workspace.browser_panes.iter())
            .filter(|(pane, state)| {
                self.workspace.tab_of(**pane).is_some() && self.agent_scope_has(state, scope)
            })
            .map(|(pane, _)| *pane)
            .collect();
        panes.sort_by_key(|pane| pane.unsigned_abs());
        panes
    }

    /// Whether the user sees the page of `pane` now: its tab is the shown
    /// one of a group of the title tab in front.
    pub(super) fn agent_page_shown(&self, pane: i64) -> bool {
        self.settings.is_none()
            && (self.workspace.active_tab())
                .is_some_and(|tab| self.workspace.layout(tab).contains(pane))
    }

    /// A page an agent session opens (K39): a tab in the group of the
    /// session's chat if it is open, else in the focused group, in the
    /// background. `isolated` gives it a throwaway profile (K22).
    #[cfg(test)]
    pub(super) fn open_agent_browser_page(
        &mut self,
        session: i64,
        url: String,
        isolated: bool,
        cx: &mut Context<Self>,
    ) -> i64 {
        self.open_agent_browser_page_in(session, None, url, isolated, cx)
    }

    /// [`Self::open_agent_browser_page`] for the scope's `project`, which
    /// its visits go to when the session is not this window's (K55).
    pub(super) fn open_agent_browser_page_in(
        &mut self,
        session: i64,
        project: Option<String>,
        url: String,
        isolated: bool,
        cx: &mut Context<Self>,
    ) -> i64 {
        let pane = self.create_browser_pane(session, url);
        if let Some(state) = self.workspace.browser_panes.get_mut(&pane) {
            state.opened_by = Some(session);
            state.ephemeral = isolated.then(ephemeral_profile_name);
            state.project = project;
        }
        let active = self.workspace.active;
        let chat = self.workspace.tab_of(session).map(|_| session);
        let placed = agent_tab_anchor(chat, active)
            .is_some_and(|anchor| self.workspace.add_to_group(pane, anchor, None, false));
        if !placed {
            // Nothing open: the page starts the scope's title tab.
            self.place_or_open(pane, OpenIn::default(), cx);
            if active.is_some() {
                self.workspace.active = active;
            }
        }
        self.browser_page(pane, cx);
        self.persist(cx);
        cx.notify();
        pane
    }

    /// Shows the page of `pane` (`browser_tab_show`): its tab becomes its
    /// group's shown one. The keyboard stays where it is while what had it
    /// is still on screen.
    pub(super) fn show_agent_browser_page(&mut self, pane: i64, window: &mut Window, cx: &mut Context<Self>) {
        let before = self.workspace.active;
        if self.workspace.tab_of(pane) != self.workspace.active_tab() {
            self.open_browser_pane(pane, window, cx);
            return;
        }
        self.workspace.show(pane);
        let tab = self.workspace.active_tab();
        match before {
            Some(before)
                if before != pane
                    && tab.is_some_and(|tab| self.workspace.layout(tab).contains(before)) =>
            {
                self.workspace.active = Some(before);
            }
            _ => {
                self.focus_browser_pane(window, cx);
            }
        }
        self.persist(cx);
        cx.notify();
    }
}

/// The agent layer's view of the app.
pub(crate) struct AppHost {
    cx: AsyncApp,
}

impl AppHost {
    pub(crate) fn new(cx: AsyncApp) -> Self {
        AppHost { cx }
    }

    /// The window whose shell holds `scope`'s page `id`.
    fn find(&self, scope: &Scope, id: PageId, cx: &App) -> Option<WindowHandle<Shell>> {
        shells(cx).into_iter().find(|window| {
            window
                .read(cx)
                .is_ok_and(|shell| shell.agent_pages(scope).contains(&id))
        })
    }

    /// The window a new page of `scope` opens in: the one showing its
    /// session's chat, else the front one, else one that knows the session.
    fn target(&self, scope: &Scope, cx: &App) -> Option<WindowHandle<Shell>> {
        let windows = shells(cx);
        let holds = |window: &&WindowHandle<Shell>, test: &dyn Fn(&Shell) -> bool| {
            window.read(cx).is_ok_and(test)
        };
        (windows.iter())
            .find(|w| holds(w, &|shell| shell.workspace.tab_of(scope.session).is_some()))
            .copied()
            .or_else(|| cx.active_window().and_then(|w| w.downcast::<Shell>()))
            .or_else(|| {
                (windows.iter())
                    .find(|w| {
                        holds(w, &|shell| {
                            shell.session_summaries.contains_key(&scope.session)
                        })
                    })
                    .copied()
            })
            .or_else(|| windows.first().copied())
    }
}

impl Host for AppHost {
    fn engine_started(&self) -> bool {
        crate::browser::engine_started()
    }

    fn pages(&self, scope: &Scope) -> Vec<Rc<dyn Page>> {
        self.cx.update(|cx| {
            let mut pages: Vec<Rc<dyn Page>> = Vec::new();
            for window in shells(cx) {
                let Ok(shell) = window.read(cx) else { continue };
                for pane in shell.agent_pages(scope) {
                    pages.push(Rc::new(AppPage {
                        cx: self.cx.clone(),
                        window,
                        pane,
                    }));
                }
            }
            pages
        })
    }

    fn open(
        &self,
        scope: &Scope,
        url: &str,
        isolated: bool,
    ) -> LocalFuture<Result<Rc<dyn Page>, String>> {
        let cx = self.cx.clone();
        let target = cx.update(|cx| self.target(scope, cx));
        let (session, url) = (scope.session, url.to_owned());
        let project = scope.project.clone();
        Box::pin(async move {
            let window = target.ok_or("there is no window to open the page in")?;
            let pane = cx
                .update(|cx| {
                    window.update(cx, |shell, _, cx| {
                        shell.open_agent_browser_page_in(session, Some(project), url, isolated, cx)
                    })
                })
                .map_err(|_| "the window closed".to_owned())?;
            let page = AppPage { cx, window, pane };
            page.ensure_loaded().await?;
            Ok(Rc::new(page) as Rc<dyn Page>)
        })
    }

    fn close(&self, scope: &Scope, id: PageId) -> bool {
        self.cx.update(|cx| {
            let Some(window) = self.find(scope, id, cx) else {
                return false;
            };
            window
                .update(cx, |shell, window, cx| shell.close_pane(id, window, cx))
                .is_ok()
        })
    }

    fn show(&self, scope: &Scope, id: PageId) -> bool {
        self.cx.update(|cx| {
            let Some(window) = self.find(scope, id, cx) else {
                return false;
            };
            window
                .update(cx, |shell, window, cx| {
                    shell.show_agent_browser_page(id, window, cx)
                })
                .is_ok()
        })
    }

    fn sleep(&self, duration: Duration) -> LocalFuture<()> {
        let timer = self.cx.background_executor().timer(duration);
        Box::pin(timer)
    }

    /// The scope's project's visits only (K65): the store decides the
    /// project from the scope, whatever the query says.
    fn history(
        &self,
        scope: &Scope,
        query: &monocode_core::browser::history::Query,
    ) -> Result<Vec<monocode_core::browser::history::Visit>, String> {
        self.cx.update(|cx| {
            let stores = crate::browser::history::stores(cx);
            let now = crate::browser::history::now_ms();
            Ok(stores.borrow_mut().of_project(&scope.project, query, now))
        })
    }
}

/// A browser pane of a window's shell, as an agent sees it.
struct AppPage {
    cx: AsyncApp,
    window: WindowHandle<Shell>,
    pane: i64,
}

impl AppPage {
    /// The page's entity, once its pane made it.
    fn entity(&self, cx: &App) -> Option<Entity<crate::browser::BrowserPage>> {
        let shell = self.window.read(cx).ok()?;
        Some(shell.browser_pages.get(&self.pane)?.0.clone())
    }

    fn with_page<R>(&self, f: impl FnOnce(&crate::browser::BrowserPage, &App) -> R) -> Option<R> {
        self.cx.update(|cx| {
            let page = self.entity(cx)?;
            Some(f(page.read(cx), cx))
        })
    }

    /// One look at a page coming up: `Ok(true)` once its CDP client
    /// exists. `first` reloads a page whose renderer went.
    fn bring_up(&self, first: bool, cx: &mut App) -> Result<bool, String> {
        let pane = self.pane;
        self.window
            .update(cx, |shell, window, cx| {
                if !shell.workspace.browser_panes.contains_key(&pane) {
                    return Err("the page is gone".to_owned());
                }
                let page = shell.browser_page(pane, cx).ok_or("the page is gone")?;
                page.update(cx, |page, cx| {
                    if page.cdp().is_some() {
                        return Ok(true);
                    }
                    if !page.start_engine_for_agent(cx)? {
                        return Ok(false);
                    }
                    if !page.has_view() {
                        page.load_in_background(window, cx)?;
                    } else if first {
                        page.reload_for_agent();
                    }
                    Ok(page.cdp().is_some())
                })
            })
            .map_err(|_| "the window closed".to_owned())?
    }
}

impl Page for AppPage {
    fn info(&self) -> PageInfo {
        let pane = self.pane;
        self.cx.update(|cx| {
            let mut info = PageInfo {
                id: pane,
                url: String::new(),
                title: String::new(),
                loading: false,
                shown: false,
                by_agent: false,
                isolated: false,
                discarded: false,
            };
            let Ok(shell) = self.window.read(cx) else {
                return info;
            };
            let Some(state) = shell.workspace.browser_panes.get(&pane) else {
                return info;
            };
            (info.url, info.title) = (state.url.clone(), state.title.clone());
            if let Some((page, _)) = shell.browser_pages.get(&pane) {
                let page = page.read(cx);
                (info.url, info.title) = (page.url.clone(), page.title.clone());
                info.loading = page.loading();
                info.discarded = page.discarded();
            }
            // A new tab page (K66) has no web page: to agents it is blank.
            if info.url.is_empty() {
                info.url = "about:blank".into();
            }
            info.shown = shell.agent_page_shown(pane);
            info.by_agent = state.opened_by.is_some();
            info.isolated = state.ephemeral.is_some();
            info
        })
    }

    fn cdp(&self) -> Option<Rc<CdpClient>> {
        self.with_page(|page, _| page.cdp()).flatten()
    }

    fn log(&self) -> Option<Rc<PageLog>> {
        self.with_page(|page, _| page.log()).flatten()
    }

    fn ensure_loaded(&self) -> LocalFuture<Result<(), String>> {
        let page = AppPage {
            cx: self.cx.clone(),
            window: self.window,
            pane: self.pane,
        };
        Box::pin(async move {
            let mut waited = Duration::ZERO;
            let mut first = true;
            loop {
                if page.cx.update(|cx| page.bring_up(first, cx))? {
                    return Ok(());
                }
                first = false;
                if waited >= LOAD_PATIENCE {
                    return Err("the page did not come up in time".into());
                }
                page.cx.background_executor().timer(LOAD_POLL).await;
                waited += LOAD_POLL;
            }
        })
    }

    fn keep_drawn(&self) -> Box<dyn Any> {
        self.with_page(|page, cx| page.keep_drawn(cx))
            .unwrap_or_else(|| Box::new(()))
    }

    fn dialog(&self) -> Option<DialogInfo> {
        self.with_page(|page, _| page.js_dialog()).flatten()
    }

    fn answer_dialog(&self, accept: bool, text: Option<&str>) -> bool {
        self.cx.update(|cx| {
            let Some(page) = self.entity(cx) else {
                return false;
            };
            page.update(cx, |page, cx| page.answer_js_dialog(accept, text, cx))
        })
    }

    fn user_active(&self) -> bool {
        self.with_page(|page, _| page.user_active())
            .unwrap_or(false)
    }

    fn user_control(&self) -> Option<UserControl> {
        self.with_page(|page, _| page.user_control()).flatten()
    }

    fn agent_used(&self, scope: &Scope) {
        let (pane, session) = (self.pane, scope.session);
        self.cx.update(|cx| {
            let _ = self.window.update(cx, |shell, _, cx| {
                let title = shell.agent_session_title(session, cx);
                if let Some(page) = shell.browser_page(pane, cx) {
                    page.update(cx, |page, cx| page.agent_used(session, title, cx));
                }
            });
        })
    }

    fn permission_notes(&self) -> Vec<monocode_core::browser::sites::PermissionNote> {
        self.with_page(|page, _| page.permission_notes())
            .unwrap_or_default()
    }

    fn waiting_permission(&self) -> Option<monocode_core::browser::sites::PermissionNote> {
        self.with_page(|page, _| page.waiting_permission())
            .flatten()
    }

    fn cert_error(&self) -> Option<monocode_core::browser::sites::CertBlock> {
        self.with_page(|page, _| page.cert_block()).flatten()
    }

    fn saved_logins(&self) -> Result<Vec<monocode_core::browser::passwords::Credential>, String> {
        self.cx.update(|cx| {
            let page = self.entity(cx).ok_or("the page is gone")?;
            page.update(cx, |page, cx| page.agent_logins(cx))
        })
    }

    fn login_used(&self, username: &str) {
        self.cx.update(|cx| {
            if let Some(page) = self.entity(cx) {
                page.update(cx, |page, cx| page.agent_login_used(username, cx));
            }
        })
    }

    fn secrets(&self) -> Vec<String> {
        self.cx.update(|cx| {
            self.entity(cx)
                .map(|page| page.update(cx, |page, cx| page.agent_secrets(cx)))
                .unwrap_or_default()
        })
    }

    fn passkey_notes(&self) -> Vec<monocode_core::browser::passkeys::PasskeyNote> {
        self.with_page(|page, _| page.passkey_notes())
            .unwrap_or_default()
    }

    fn pending_passkey(&self) -> Option<monocode_core::browser::passkeys::PasskeyNote> {
        self.with_page(|page, _| page.pending_passkey()).flatten()
    }

    fn cancel_passkey(&self) -> LocalFuture<bool> {
        match self.with_page(|page, _| page.cancel_passkey()) {
            Some(cancel) => cancel,
            None => Box::pin(async { false }),
        }
    }

    fn expect_download(&self, dir: PathBuf) -> LocalFuture<Result<PathBuf, String>> {
        let page = AppPage {
            cx: self.cx.clone(),
            window: self.window,
            pane: self.pane,
        };
        // Chromium takes the folder from the page's browser. A page that
        // is up takes it now, before the action that starts the download:
        // asked for when first polled, the folder came after a quick one.
        let now = page.cx.update(|cx| {
            let entity = page.entity(cx)?;
            entity.update(cx, |page, _| {
                (page.cdp().is_some()).then(|| page.expect_download(dir.clone()))
            })
        });
        if let Some(waiting) = now {
            return waiting;
        }
        Box::pin(async move {
            page.ensure_loaded().await?;
            let waiting = page.cx.update(|cx| {
                let entity = page.entity(cx).ok_or("the page is gone")?;
                Ok::<_, String>(entity.update(cx, |page, _| page.expect_download(dir)))
            })?;
            waiting.await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(session: i64, project: &str) -> Scope {
        Scope {
            session,
            project: project.into(),
        }
    }

    #[test]
    fn an_agent_tab_joins_its_chats_group_else_the_focused_one() {
        assert_eq!(agent_tab_anchor(Some(3), Some(-7)), Some(3));
        assert_eq!(agent_tab_anchor(None, Some(-7)), Some(-7));
        assert_eq!(agent_tab_anchor(None, None), None);
        assert_ne!(ephemeral_profile_name(), ephemeral_profile_name());
    }

    #[gpui::test]
    fn a_scope_sees_its_projects_pages_and_the_ones_its_session_opened(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| cx.set_reduce_motion(true));
        // `other` works in /work/another-project, `session` in /work/monocode.
        let (window, other, session) = super::super::tests::window(cx);
        let (mine, theirs, opened) = window
            .update(cx, |shell, window, cx| {
                shell.open(session, window, cx);
                shell.new_browser_pane(Some("http://localhost:5173/".into()), window, cx);
                let mine = shell.workspace.active.unwrap();
                shell.open(other, window, cx);
                shell.new_browser_pane(Some("http://localhost:3000/".into()), window, cx);
                let theirs = shell.workspace.active.unwrap();
                // The monocode session opens a page from the other project's
                // window position: still its own.
                let opened = shell.open_agent_browser_page(
                    session,
                    "https://example.com/".into(),
                    false,
                    cx,
                );
                (mine, theirs, opened)
            })
            .unwrap();
        let host = AppHost::new(cx.to_async());
        let ids =
            |scope: &Scope| -> Vec<i64> { host.pages(scope).iter().map(|p| p.info().id).collect() };
        assert_eq!(ids(&scope(session, "/work/monocode")), [mine, opened]);
        // Another session of the same project sees the project's pages,
        // the agent's too: it opened them for its project.
        assert_eq!(ids(&scope(999, "/work/monocode/")), [mine, opened]);
        assert_eq!(ids(&scope(other, "/work/another-project")), [theirs]);
        assert!(ids(&scope(1, "/elsewhere")).is_empty());

        let info = host
            .page(&scope(session, "/work/monocode"), opened)
            .unwrap()
            .info();
        assert_eq!(info.url, "https://example.com/");
        assert!(info.by_agent && !info.isolated);
        let mine = host
            .page(&scope(session, "/work/monocode"), mine)
            .unwrap()
            .info();
        assert!(!mine.by_agent);
        // `other`'s chat is in front, and so is its page's group.
        assert!(!mine.shown);
        assert!(
            host.page(&scope(session, "/work/monocode"), theirs)
                .is_none()
        );
    }

    #[gpui::test]
    fn an_agent_page_opens_behind_its_chat_or_in_the_focused_group(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, other, session) = super::super::tests::window(cx);
        window
            .update(cx, |shell, window, cx| {
                shell.open(session, window, cx);
                let tabs = shell.workspace.tabs.len();
                let page = shell.open_agent_browser_page(
                    session,
                    "http://localhost:5173/".into(),
                    false,
                    cx,
                );
                // In the chat's group, behind it: nothing moved for the user.
                assert_eq!(shell.workspace.group_tabs(session), [session, page]);
                assert_eq!(shell.workspace.active, Some(session));
                assert_eq!(shell.workspace.visible_of(page), Some(session));
                assert_eq!(shell.workspace.tabs.len(), tabs);

                // A session whose chat is not open: the focused group.
                shell.open(other, window, cx);
                let third = shell.open_agent_browser_page(
                    424242,
                    "http://localhost:8080/".into(),
                    true,
                    cx,
                );
                assert!(shell.workspace.group_tabs(other).contains(&third));
                assert_eq!(shell.workspace.active, Some(other));
                assert_eq!(
                    shell.workspace.browser_panes[&third].opened_by,
                    Some(424242)
                );
                assert!(matches!(
                    shell.browser_pane_profile(third),
                    crate::browser::Profile::Ephemeral(_)
                ));
                assert!(!matches!(
                    shell.browser_pane_profile(page),
                    crate::browser::Profile::Ephemeral(_)
                ));
            })
            .unwrap();
    }

    #[gpui::test]
    fn a_throwaway_page_is_never_saved(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, _, session) = super::super::tests::window(cx);
        let (saved, kept, isolated) = window
            .update(cx, |shell, window, cx| {
                shell.open(session, window, cx);
                let kept =
                    shell.open_agent_browser_page(session, "https://a.test/".into(), false, cx);
                let isolated =
                    shell.open_agent_browser_page(session, "https://b.test/".into(), true, cx);
                (shell.workspace.saved_json(), kept, isolated)
            })
            .unwrap();
        assert!(saved.contains("https://a.test/"), "{saved}");
        assert!(!saved.contains("https://b.test/"), "{saved}");
        assert!(!saved.contains("ephemeral") && !saved.contains("opened_by"));
        let sessions = window
            .update(cx, |shell, _, _| {
                (shell.session_summaries.values())
                    .map(|s| (s.id, s.cwd.clone()))
                    .collect::<Vec<_>>()
            })
            .unwrap();
        let restored =
            crate::workspace::Workspace::restore(Some(&saved), &sessions, "/work/monocode");
        assert!(restored.browser_panes.contains_key(&kept));
        assert!(!restored.browser_panes.contains_key(&isolated));
        assert!(!restored.open_sessions().contains(&isolated));
        assert!(restored.group_tabs(session).contains(&kept));
    }

    #[gpui::test]
    fn agents_close_and_show_only_pages_of_their_scope(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, other, session) = super::super::tests::window(cx);
        let page = window
            .update(cx, |shell, window, cx| {
                shell.open(session, window, cx);
                shell.open_agent_browser_page(session, "https://a.test/".into(), false, cx)
            })
            .unwrap();
        let host = AppHost::new(cx.to_async());
        let theirs = scope(other, "/work/another-project");
        let ours = scope(session, "/work/monocode");
        assert!(!host.show(&theirs, page));
        assert!(!host.close(&theirs, page));
        assert!(host.show(&ours, page));
        let shown = window
            .update(cx, |shell, _, _| {
                (
                    shell.workspace.visible_of(page),
                    shell.agent_page_shown(page),
                )
            })
            .unwrap();
        assert_eq!(shown, (Some(page), true));
        assert!(host.page(&ours, page).unwrap().info().shown);
        assert!(host.close(&ours, page));
        cx.run_until_parked();
        assert!(host.pages(&ours).is_empty());
    }

    #[gpui::test]
    fn a_closed_pages_id_never_reaches_another_page(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, _, session) = super::super::tests::window(cx);
        let host = AppHost::new(cx.to_async());
        let ours = scope(session, "/work/monocode");
        let open = |cx: &mut gpui::TestAppContext, url: &str| {
            window
                .update(cx, |shell, window, cx| {
                    shell.open(session, window, cx);
                    shell.open_agent_browser_page(session, url.into(), false, cx)
                })
                .unwrap()
        };
        let first = open(cx, "https://a.test/");
        assert!(host.close(&ours, first));
        cx.run_until_parked();
        // The lowest pane id is free again, but an agent may still hold it.
        let second = open(cx, "https://b.test/");
        assert_ne!(second, first);
        assert!(host.page(&ours, first).is_none());
        // A user's page, and pages a page opens, follow the same rule.
        assert!(host.close(&ours, second));
        cx.run_until_parked();
        let third = window
            .update(cx, |shell, window, cx| {
                shell.new_browser_pane(Some("https://c.test/".into()), window, cx);
                shell.workspace.active.unwrap()
            })
            .unwrap();
        assert!(![first, second].contains(&third));
        assert_eq!(
            host.page(&ours, third).unwrap().info().url,
            "https://c.test/"
        );
    }

    #[gpui::test]
    fn the_external_cdp_setting_turns_the_endpoint_on_and_off(cx: &mut gpui::TestAppContext) {
        use monocode_core::browser::proxy;
        let (window, _, session) = super::super::tests::window(cx);
        window
            .update(cx, |shell, window, cx| {
                shell.open(session, window, cx);
                // On by default (K27).
                assert!(shell.workspace.preferences.browser_external_cdp);
                shell.workspace.preferences.browser_external_cdp = false;
                shell.apply_preferences(cx);
                assert!(!proxy::enabled());
                shell.workspace.preferences.browser_external_cdp = true;
                shell.apply_preferences(cx);
                assert_eq!(proxy::enabled(), cfg!(target_os = "macos"));
                // Saved with the preferences.
                let saved = serde_json::to_string(&shell.workspace.preferences).unwrap();
                let again: crate::settings::Preferences = serde_json::from_str(&saved).unwrap();
                assert!(again.browser_external_cdp);
                // "Copy endpoint" hands out the selected project's scope.
                assert_eq!(
                    shell.browser_endpoint_scope(),
                    Some(scope(session, "/work/monocode"))
                );
                shell.workspace.preferences.browser_external_cdp = false;
                shell.apply_preferences(cx);
                assert!(!proxy::enabled());
            })
            .unwrap();
    }

    #[gpui::test]
    fn the_pump_runs_each_job_with_the_apps_host(cx: &mut gpui::TestAppContext) {
        cx.update(start);
        let (tx, rx) = async_channel::unbounded::<bool>();
        for _ in 0..2 {
            let tx = tx.clone();
            assert!(monocode_core::browser::submit(Box::new(move |host| {
                Box::pin(async move {
                    // The first does not hold up the second.
                    host.sleep(Duration::from_secs(1)).await;
                    let _ = tx.try_send(host.engine_started());
                })
            })));
        }
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        assert_eq!(rx.len(), 2);
    }

    #[gpui::test]
    fn a_page_that_is_up_takes_the_download_folder_before_the_action(
        cx: &mut gpui::TestAppContext,
    ) {
        let (window, _, session) = super::super::tests::window(cx);
        let pane = window
            .update(cx, |shell, window, cx| {
                shell.open(session, window, cx);
                shell.new_browser_pane(Some("https://a.test/".into()), window, cx);
                shell.workspace.active.unwrap()
            })
            .unwrap();
        cx.run_until_parked();
        let page = window
            .update(cx, |shell, _, cx| shell.browser_page(pane, cx).unwrap())
            .unwrap();
        page.read_with(cx, |page, _| {
            page.set_cdp_for_test(monocode_core::browser::CdpClient::new(Box::new(|_| true)))
        });
        let host = AppHost::new(cx.to_async());
        let scope = scope(session, "/work/monocode");
        let dir = tempfile::tempdir().unwrap();
        // Not polled yet: the click that starts a download comes first.
        let waiting = (host.page(&scope, pane).unwrap()).expect_download(dir.path().join("dl"));
        assert!(page.read_with(cx, |page, _| page.waits_for_download()));
        drop(waiting);
    }

    #[gpui::test]
    fn returning_to_a_page_with_a_dialog_focuses_the_dialog(cx: &mut gpui::TestAppContext) {
        use monocode_cef::{DialogKind, JsDialog};
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, _, session) = super::super::tests::window(cx);
        let pane = window
            .update(cx, |shell, window, cx| {
                shell.open(session, window, cx);
                shell.new_browser_pane(Some("https://a.test/".into()), window, cx);
                shell.workspace.active.unwrap()
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |shell, _, cx| {
                let page = shell.browser_page(pane, cx).unwrap();
                page.update(cx, |page, cx| {
                    page.push_dialog_for_test(
                        JsDialog::detached(DialogKind::Confirm, "https://a.test", "Sure?"),
                        cx,
                    )
                });
            })
            .unwrap();
        cx.run_until_parked();
        // Away to the chat, and back.
        window
            .update(cx, |shell, window, cx| shell.open(session, window, cx))
            .unwrap();
        cx.run_until_parked();
        let focused = |cx: &mut gpui::TestAppContext| {
            window
                .update(cx, |shell, window, cx| {
                    let page = shell.browser_page(pane, cx).unwrap();
                    page.read(cx).prompt_focused(window, cx)
                })
                .unwrap()
        };
        assert!(!focused(cx));
        window
            .update(cx, |shell, window, cx| {
                shell.open_browser_pane(pane, window, cx)
            })
            .unwrap();
        cx.run_until_parked();
        assert!(focused(cx));
        // Answered, the page has no dialog to focus any more.
        window
            .update(cx, |shell, _, cx| {
                let page = shell.browser_page(pane, cx).unwrap();
                page.update(cx, |page, cx| page.answer_js_dialog(true, None, cx))
            })
            .unwrap();
        let host = AppHost::new(cx.to_async());
        let ours = scope(session, "/work/monocode");
        assert_eq!(host.page(&ours, pane).unwrap().dialog(), None);
    }
}
