//! What the browser's extension layer (`browser/extensions`, K69) needs of
//! the windows: their browser pages as tabs, and opening, showing,
//! navigating, muting and closing a tab for an extension's `chrome.tabs`.
//! A tab id is its page's (`BrowserPage::extension_tab_id`), not its pane.
use super::*;
use crate::browser::extensions::{ProfileKey, profile_key};
use gpui::WindowHandle;

fn shells(cx: &App) -> Vec<WindowHandle<Shell>> {
    (cx.windows().into_iter())
        .filter_map(|window| window.downcast::<Shell>())
        .collect()
}

/// Every window's browser pages, as the extension layer's tabs, and the
/// windows. Pages of throwaway profiles are no extension's.
#[cfg(target_os = "macos")]
pub(crate) fn extension_tabs(
    cx: &mut App,
) -> (
    Vec<crate::browser::extensions::PageTab>,
    Vec<crate::browser::extensions::WindowTab>,
) {
    use crate::browser::extensions::{PageTab, WindowTab};
    let active = cx.active_window().map(|w| w.window_id());
    let mut pages = Vec::new();
    let mut windows = Vec::new();
    for handle in shells(cx) {
        let id = handle.window_id();
        let _ = handle.update(cx, |shell, window, cx| {
            let b = window.bounds();
            windows.push(WindowTab {
                window: id,
                focused: active == Some(id),
                bounds: (
                    f32::from(b.origin.x) as i32,
                    f32::from(b.origin.y) as i32,
                    f32::from(b.size.width) as i32,
                    f32::from(b.size.height) as i32,
                ),
            });
            for (pane, (page, _)) in &shell.browser_pages {
                let shown = shell.agent_page_shown(*pane);
                let page = page.read(cx);
                let Some(profile) = profile_key(&page.profile()) else {
                    continue;
                };
                pages.push(PageTab {
                    tab: page.extension_tab_id(),
                    window: id,
                    profile,
                    url: page.url.clone(),
                    title: page.title.clone(),
                    loading: page.loading(),
                    audible: page.audible(),
                    muted: page.muted(),
                    discarded: page.discarded(),
                    shown,
                    last_input: page.last_input(),
                    browser_id: page.browser_id(),
                });
            }
        });
    }
    // Tabs in a steady order (their index): the order pages were made.
    pages.sort_by_key(|p| p.tab);
    (pages, windows)
}

/// The window and pane of the tab `tab`.
fn find_tab(tab: i64, cx: &App) -> Option<(WindowHandle<Shell>, i64)> {
    for handle in shells(cx) {
        let Ok(shell) = handle.read(cx) else { continue };
        for (pane, (page, _)) in &shell.browser_pages {
            if page.read(cx).extension_tab_id() == tab {
                return Some((handle, *pane));
            }
        }
    }
    None
}

/// `chrome.tabs.create`: a new tab of `url` in the window that browses
/// with `profile` (the focused one first). False when there is none.
pub(crate) fn open_extension_tab(
    url: String,
    profile: ProfileKey,
    active: bool,
    cx: &mut App,
) -> bool {
    let focused = cx.active_window().map(|w| w.window_id());
    let mut windows = shells(cx);
    windows.sort_by_key(|w| Some(w.window_id()) != focused);
    // A window whose pages use the profile; else the first.
    let target = (windows.iter())
        .find(|w| {
            w.read(cx).is_ok_and(|shell| {
                shell.browser_pages.values().any(|(page, _)| {
                    profile_key(&page.read(cx).profile()).as_ref() == Some(&profile)
                })
            })
        })
        .or(windows.first())
        .copied();
    let Some(window) = target else { return false };
    window
        .update(cx, |shell, window, cx| {
            if active {
                shell.new_browser_pane(Some(url), window, cx);
            } else {
                shell.new_browser_pane_in_background(url, cx);
            }
        })
        .is_ok()
}

pub(crate) fn show_extension_tab(tab: i64, cx: &mut App) {
    if let Some((window, pane)) = find_tab(tab, cx) {
        let _ = window.update(cx, |shell, window, cx| {
            shell.show_agent_browser_page(pane, window, cx);
            window.activate_window();
        });
    }
}

pub(crate) fn navigate_extension_tab(tab: i64, url: String, cx: &mut App) {
    if let Some((window, pane)) = find_tab(tab, cx) {
        let _ = window.update(cx, |shell, _, cx| {
            if let Some((page, _)) = shell.browser_pages.get(&pane) {
                page.update(cx, |page, cx| page.navigate_for_extension(url, cx));
            }
        });
    }
}

pub(crate) fn mute_extension_tab(tab: i64, muted: bool, cx: &mut App) {
    if let Some((window, pane)) = find_tab(tab, cx) {
        let _ = window.update(cx, |shell, _, cx| {
            if let Some((page, _)) = shell.browser_pages.get(&pane) {
                page.update(cx, |page, cx| page.set_muted(muted, cx));
            }
        });
    }
}

pub(crate) fn close_extension_tab(tab: i64, cx: &mut App) {
    if let Some((window, pane)) = find_tab(tab, cx) {
        let _ = window.update(cx, |shell, window, cx| shell.close_pane(pane, window, cx));
    }
}

/// `windows.update({focused: true})`: the window extensions call `id`.
pub(crate) fn focus_extension_window(id: i64, cx: &mut App) {
    let _ = id;
    if let Some(window) = cx
        .active_window()
        .or_else(|| shells(cx).first().map(|w| (*w).into()))
    {
        let _ = window.update(cx, |_, window, _| window.activate_window());
    }
}

impl Shell {
    /// A tab for an extension that does not take the user's focus.
    pub(super) fn new_browser_pane_in_background(&mut self, url: String, cx: &mut Context<Self>) {
        let Some(owner) = self
            .terminal_owner()
            .or_else(|| self.project_session_for_terminal())
        else {
            return;
        };
        let pane = self.create_browser_pane(owner, url);
        self.place_or_open(pane, OpenIn::default(), cx);
        self.persist(cx);
        cx.notify();
    }
}
