//! The Little window (docs/native/modes.md §Little window), SOCode's quick
//! lookup window: a link another app hands to Monocode, or one a routing
//! rule sends there, opens in a small window of its own without the
//! sidebar. Its compact header shows the site with Copy URL, the page's
//! history buttons and **Open in Browser** (⌘O), which moves the page,
//! with its history, into Browser mode as a Today tab. Closing it
//! archives nothing.
//!
//! The page is a browser pane of the shell's (owner 0, as Browser mode's
//! own tabs), listed in `BrowseState::little` so it is not pruned: the
//! browser's passwords, permissions and downloads work as for any tab, and
//! agents' browser tools do not see it (user decision).
use super::*;
use crate::browser::BrowserPage;
use gpui::{Bounds, Task, WindowBounds, WindowHandle, WindowOptions, point, size};

/// The header's height, as SOCode's Little window toolbar.
const HEADER_HEIGHT: f32 = 46.;
/// The window's size when it opens, as SOCode's.
const WINDOW_SIZE: (f32, f32) = (1000., 760.);
const KEY_CONTEXT: &str = "LittleWindow";

actions!(
    little_window,
    [
        /// ⌘O: the page moves into Browser mode as a Today tab.
        LittleOpenInBrowser,
        /// ⌘W: the window closes.
        LittleClose,
        /// ⇧⌘C: the page's address is copied.
        LittleCopyUrl,
        LittleReload,
        LittleBack,
        LittleForward,
    ]
);

pub(super) fn bind(cx: &mut App) {
    let m = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };
    let context = Some(KEY_CONTEXT);
    cx.bind_keys([
        KeyBinding::new(&format!("{m}-o"), LittleOpenInBrowser, context),
        KeyBinding::new(&format!("{m}-w"), LittleClose, context),
        KeyBinding::new(&format!("{m}-shift-c"), LittleCopyUrl, context),
        KeyBinding::new(&format!("{m}-r"), LittleReload, context),
        KeyBinding::new(&format!("{m}-["), LittleBack, context),
        KeyBinding::new(&format!("{m}-]"), LittleForward, context),
    ]);
}

/// The Little window's view: its header and the page.
pub(crate) struct LittleWindow {
    shell: WindowHandle<Shell>,
    pub(super) pane: i64,
    page: Entity<BrowserPage>,
    /// The page went into Browser mode: closing keeps its pane.
    moved: bool,
    _page_changed: Subscription,
    #[cfg(target_os = "macos")]
    _keys: Option<keys::Monitor>,
    _commands: Option<Task<()>>,
}

impl LittleWindow {
    fn new(
        shell: WindowHandle<Shell>,
        pane: i64,
        page: Entity<BrowserPage>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let page_changed = cx.observe(&page, |_, _, cx| cx.notify());
        // The page closes with the window unless it moved into Browser mode.
        cx.on_release(|this: &mut Self, cx| {
            if this.moved {
                return;
            }
            let (shell, pane) = (this.shell, this.pane);
            cx.defer(move |cx| {
                let _ = shell.update(cx, |shell, _, cx| shell.little_closed(pane, cx));
            });
        })
        .detach();
        // While Chromium has the keyboard, GPUI never sees the window's
        // keys: a monitor hands them over (the main window's forwarding
        // belongs to its shell).
        let (sender, receiver) = async_channel::unbounded::<Key>();
        #[cfg(target_os = "macos")]
        let keys = keys::Monitor::new(window, sender);
        #[cfg(not(target_os = "macos"))]
        drop(sender);
        let commands = cx.spawn_in(window, async move |this, cx| {
            while let Ok(key) = receiver.recv().await {
                let done = this.update_in(cx, |this, window, cx| this.run(key, window, cx));
                if done.is_err() {
                    break;
                }
            }
        });
        Self {
            shell,
            pane,
            page,
            moved: false,
            _page_changed: page_changed,
            #[cfg(target_os = "macos")]
            _keys: keys,
            _commands: Some(commands),
        }
    }

    fn url(&self, cx: &App) -> String {
        self.page.read(cx).url.clone()
    }

    fn run(&mut self, key: Key, window: &mut Window, cx: &mut Context<Self>) {
        match key {
            Key::Open => self.open_in_browser(window, cx),
            Key::Close => window.remove_window(),
            Key::CopyUrl => cx.write_to_clipboard(ClipboardItem::new_string(self.url(cx))),
            Key::Reload => self.page.update(cx, |page, _| page.reload_or_stop()),
            Key::Back => self.page.update(cx, |page, _| page.go_back()),
            Key::Forward => self.page.update(cx, |page, _| page.go_forward()),
        }
    }

    /// ⌘O: the page becomes a Today tab of the shell's Browser mode, which
    /// comes forward; this window closes.
    fn open_in_browser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.moved {
            return;
        }
        self.moved = true;
        let (shell, pane) = (self.shell, self.pane);
        window.remove_window();
        cx.defer(move |cx| {
            let _ = shell.update(cx, |shell, window, cx| {
                window.activate_window();
                shell.little_to_browser(pane, window, cx);
            });
        });
    }

    fn header(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let (url, back, forward, loading) = {
            let page = self.page.read(cx);
            (
                page.url.clone(),
                page.can_go_back(),
                page.can_go_forward(),
                page.loading(),
            )
        };
        let host = super::modes_ui::host_of(&url);
        let nav = |id: &'static str, label: &'static str, path: &'static str, on: bool| {
            icon_button_enabled(id, label, path, on)
                .size(px(28.))
                .rounded(px(7.))
                .debug_selector(move || id.into())
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        };
        // The traffic lights sit in the header's first 75 points.
        let lights = if cfg!(target_os = "macos") && !window.is_fullscreen() {
            75.
        } else {
            8.
        };
        div()
            .h(px(HEADER_HEIGHT))
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(4.))
            .pr(px(8.))
            .child(titlebar_drag_area().w(px(lights)).h_full().flex_shrink_0())
            .child(
                nav("little-back", "Back", "arrow-left-2", back)
                    .on_click(cx.listener(|this, _, w, cx| this.run(Key::Back, w, cx))),
            )
            .child(
                nav("little-forward", "Forward", "arrow-right-2", forward)
                    .on_click(cx.listener(|this, _, w, cx| this.run(Key::Forward, w, cx))),
            )
            .child(
                nav(
                    "little-reload",
                    if loading { "Stop" } else { "Reload" },
                    if loading { "x" } else { "refresh-cw" },
                    true,
                )
                .on_click(cx.listener(|this, _, w, cx| this.run(Key::Reload, w, cx))),
            )
            .child(
                // The site, centred, in a pill that drags the window; Copy
                // URL at its end keeps the whole address.
                div()
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .h(px(32.))
                    .mx(px(4.))
                    .rounded(px(7.))
                    .bg(content(0.07))
                    .flex()
                    .items_center()
                    .child(
                        titlebar_drag_area()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                div()
                                    .debug_selector(|| "little-address".into())
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(13.))
                                    .text_color(content(0.8))
                                    .child(host),
                            ),
                    )
                    .child(
                        icon_button("little-copy-url", "Copy URL", "copy")
                            .size(px(28.))
                            .mr(px(2.))
                            .rounded(px(6.))
                            .debug_selector(|| "little-copy-url".into())
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(|this, _, w, cx| this.run(Key::CopyUrl, w, cx))),
                    ),
            )
            .child(
                div()
                    .id("little-open-in-browser")
                    .debug_selector(|| "little-open-in-browser".into())
                    .role(Role::Button)
                    .aria_label("Open in Browser (⌘O)")
                    .h(px(32.))
                    .px(px(10.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .gap(px(7.))
                    .rounded(px(7.))
                    .bg(content(0.1))
                    .hover(|s| s.bg(content(0.16)))
                    .cursor_pointer()
                    .text_size(px(13.))
                    .child("Open in")
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("Browser"))
                    .child(
                        div()
                            .px(px(4.))
                            .rounded(px(3.))
                            .bg(content(0.08))
                            .text_size(px(11.))
                            .text_color(content(0.6))
                            .child(if cfg!(target_os = "macos") {
                                "⌘O"
                            } else {
                                "Ctrl+O"
                            }),
                    )
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(|this, _, w, cx| this.run(Key::Open, w, cx))),
            )
            .into_any_element()
    }
}

impl Render for LittleWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The header has the history buttons: the page shows no toolbar.
        self.page.update(cx, |page, cx| {
            if !page.nav_in_sidebar {
                page.nav_in_sidebar = true;
                cx.notify();
            }
        });
        let header = self.header(window, cx);
        div()
            .id("little-window")
            .debug_selector(|| "little-window".into())
            .key_context(KEY_CONTEXT)
            .on_action(
                cx.listener(|this, _: &LittleOpenInBrowser, w, cx| this.run(Key::Open, w, cx)),
            )
            .on_action(cx.listener(|this, _: &LittleClose, w, cx| this.run(Key::Close, w, cx)))
            .on_action(cx.listener(|this, _: &LittleCopyUrl, w, cx| this.run(Key::CopyUrl, w, cx)))
            .on_action(cx.listener(|this, _: &LittleReload, w, cx| this.run(Key::Reload, w, cx)))
            .on_action(cx.listener(|this, _: &LittleBack, w, cx| this.run(Key::Back, w, cx)))
            .on_action(cx.listener(|this, _: &LittleForward, w, cx| this.run(Key::Forward, w, cx)))
            .size_full()
            .flex()
            .flex_col()
            .bg(crate::theme::background_base())
            .text_color(content(1.))
            .font_family(crate::fonts::UI)
            .child(header)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .border_t_1()
                    .border_color(content(0.08))
                    .child(self.page.clone()),
            )
            // Hides the page when it is not painted, as in every window
            // that hosts pages.
            .child(crate::browser::sweep())
    }
}

/// What the window's keys do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Key {
    Open,
    Close,
    CopyUrl,
    Reload,
    Back,
    Forward,
}

impl Key {
    /// The key a ⌘-stroke (`shift` with ⇧) names, by the character it types
    /// without modifiers.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    fn of(character: &str, shift: bool) -> Option<Self> {
        Some(match (character.to_ascii_lowercase().as_str(), shift) {
            ("o", false) => Key::Open,
            ("w", false) => Key::Close,
            ("c", true) => Key::CopyUrl,
            ("r", false) => Key::Reload,
            ("[", false) => Key::Back,
            ("]", false) => Key::Forward,
            _ => return None,
        })
    }
}

impl Shell {
    /// `url` opens in a new Little window.
    pub(super) fn open_little(&mut self, url: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(shell) = window.window_handle().downcast::<Shell>() else {
            return;
        };
        let pane = self.create_browser_pane(super::modes::NO_OWNER, url);
        self.workspace.browse.little.push(pane);
        let Some(page) = self.browser_page(pane, cx) else {
            self.little_closed(pane, cx);
            return;
        };
        cx.defer(move |cx| {
            let (width, height) = WINDOW_SIZE;
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(width), px(height)),
                    cx,
                ))),
                window_min_size: Some(size(px(420.), px(300.))),
                app_owns_titlebar_drag: cfg!(target_os = "macos"),
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some("Little Window".into()),
                    appears_transparent: cfg!(any(target_os = "macos", target_os = "windows")),
                    traffic_light_position: cfg!(target_os = "macos")
                        .then(|| point(px(12.), px((HEADER_HEIGHT - 14.) / 2.))),
                }),
                ..Default::default()
            };
            let opened = cx.open_window(options, |window, cx| {
                #[cfg(target_os = "macos")]
                crate::traffic_lights::pin(window, HEADER_HEIGHT);
                page.update(cx, |page, cx| page.focus(window, cx));
                cx.new(|cx| LittleWindow::new(shell, pane, page.clone(), window, cx))
            });
            match opened {
                Ok(little) => {
                    let _ = little.update(cx, |_, window, _| window.activate_window());
                    cx.activate(true);
                }
                Err(error) => {
                    log::warn!("Cannot open the Little window: {error}");
                    let _ = shell.update(cx, |shell, _, cx| shell.little_closed(pane, cx));
                }
            }
        });
    }

    /// A Little window closed: its page goes, and nothing is archived.
    pub(super) fn little_closed(&mut self, pane: i64, cx: &mut Context<Self>) {
        if !self.workspace.browse.little.contains(&pane) {
            return;
        }
        self.workspace.browse.little.retain(|p| *p != pane);
        self.workspace.browser_panes.remove(&pane);
        self.browser_pages.remove(&pane);
        self.persist(cx);
        cx.notify();
    }

    /// The Little window's page becomes a tab at the top of Today, keeping
    /// its page and history, and Browser mode shows it (as Peek's ⌘O).
    pub(super) fn little_to_browser(
        &mut self,
        pane: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.workspace.browse.little.contains(&pane)
            || !self.workspace.browser_panes.contains_key(&pane)
        {
            return;
        }
        self.workspace.browse.little.retain(|p| *p != pane);
        let url = (self.browser_pages.get(&pane))
            .map(|(page, _)| page.read(cx).url.clone())
            .or_else(|| (self.workspace.browser_panes.get(&pane)).map(|state| state.url.clone()))
            .unwrap_or_default();
        // The model's new tab takes the Little window's pane: its page lives on.
        let tab = (self.workspace.browse.model).new_tab(url, crate::modes::now());
        self.workspace.browse.panes.insert(tab.get(), pane);
        self.browse_reconcile(cx);
        self.browse_select(pane, window, cx);
    }

    /// A Little window's page asked for something: closing closes its
    /// window; the rest (a link opening a tab) goes to Browser mode.
    pub(super) fn little_page_request(
        &mut self,
        request: crate::browser::PageRequest,
        pane: i64,
        cx: &mut Context<Self>,
    ) -> Option<crate::browser::PageRequest> {
        match request {
            crate::browser::PageRequest::Close => {
                close_little_window(pane, cx);
                None
            }
            request => Some(request),
        }
    }
}

/// The Little window of `pane`, if one is open.
fn little_window(pane: i64, cx: &App) -> Option<WindowHandle<LittleWindow>> {
    (cx.windows().into_iter())
        .filter_map(|window| window.downcast::<LittleWindow>())
        .find(|window| window.read(cx).is_ok_and(|little| little.pane == pane))
}

fn close_little_window(pane: i64, cx: &mut App) {
    if let Some(window) = little_window(pane, cx) {
        cx.defer(move |cx| {
            let _ = window.update(cx, |_, window, _| window.remove_window());
        });
    }
}

#[cfg(target_os = "macos")]
mod keys {
    //! The Little window's ⌘ keys while Chromium has the keyboard: a local
    //! monitor sees each key-down before AppKit hands it to the page, as the
    //! main window's forwarding does (shell/shortcuts/macos.rs).
    use super::Key;
    use gpui::Window;
    use objc2::{MainThreadMarker, rc::Retained, runtime::AnyObject};
    use objc2_app_kit::{NSEvent, NSEventMask, NSEventModifierFlags, NSView};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use std::ptr::{self, NonNull};

    pub(super) struct Monitor {
        monitor: Retained<AnyObject>,
    }

    impl Monitor {
        pub(super) fn new(window: &Window, keys: async_channel::Sender<Key>) -> Option<Self> {
            let main = MainThreadMarker::new()?;
            let handle = HasWindowHandle::window_handle(window).ok()?;
            let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
                return None;
            };
            let view = unsafe { Retained::retain(handle.ns_view.as_ptr().cast::<NSView>()) }?;
            let native_window = view.window()?;
            let block = block2::RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
                let original = event.as_ptr();
                let event = unsafe { event.as_ref() };
                if !event
                    .window(main)
                    .is_some_and(|w| ptr::eq(&*w, &*native_window))
                {
                    return original;
                }
                let flags = event.modifierFlags();
                if !flags.contains(NSEventModifierFlags::Command)
                    || flags
                        .intersects(NSEventModifierFlags::Control | NSEventModifierFlags::Option)
                {
                    return original;
                }
                let Some(character) = event.charactersIgnoringModifiers() else {
                    return original;
                };
                let shift = flags.contains(NSEventModifierFlags::Shift);
                match Key::of(&character.to_string(), shift) {
                    Some(key) if keys.try_send(key).is_ok() => ptr::null_mut(),
                    _ => original,
                }
            });
            let monitor = unsafe {
                NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &block)
            }?;
            Some(Self { monitor })
        }
    }

    impl Drop for Monitor {
        fn drop(&mut self) {
            unsafe { NSEvent::removeMonitor(&self.monitor) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modes::AppMode;
    use crate::shell::browser_links::{LinkTarget, add_route, link_match};
    use gpui::TestAppContext;

    fn little_windows(cx: &mut TestAppContext) -> Vec<WindowHandle<LittleWindow>> {
        cx.update(|cx| {
            (cx.windows().into_iter())
                .filter_map(|window| window.downcast::<LittleWindow>())
                .collect()
        })
    }

    #[test]
    fn command_keys_name_the_windows_actions() {
        assert_eq!(Key::of("o", false), Some(Key::Open));
        assert_eq!(Key::of("O", false), Some(Key::Open));
        assert_eq!(Key::of("w", false), Some(Key::Close));
        assert_eq!(Key::of("C", true), Some(Key::CopyUrl));
        assert_eq!(Key::of("c", false), None, "⌘C stays the page's copy");
        assert_eq!(Key::of("[", false), Some(Key::Back));
        assert_eq!(Key::of("t", false), None);
    }

    #[gpui::test]
    fn a_little_window_opens_its_page_and_moves_it_into_browser_mode(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, _, _) = super::super::tests::window(cx);
        window
            .update(cx, |shell, window, cx| {
                shell.open_little("https://docs.test/a".into(), window, cx)
            })
            .unwrap();
        cx.run_until_parked();
        let little = little_windows(cx);
        assert_eq!(little.len(), 1);
        let pane = window
            .update(cx, |shell, _, cx| {
                let pane = shell.workspace.browse.little[0];
                assert!(shell.workspace.browser_panes.contains_key(&pane));
                // No tab of Browser mode, and kept through a save.
                assert!(shell.workspace.browse.tab_of(pane).is_none());
                shell.persist(cx);
                assert!(shell.workspace.browser_panes.contains_key(&pane));
                assert_eq!(shell.workspace.browser_panes[&pane].owner, 0);
                pane
            })
            .unwrap();
        // The window shows the page's site and closes with ⌘W.
        little[0]
            .update(cx, |little, window, cx| {
                assert_eq!(little.pane, pane);
                assert_eq!(little.url(cx), "https://docs.test/a");
                little.run(Key::CopyUrl, window, cx);
            })
            .unwrap();
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("https://docs.test/a".to_owned())
        );
        // ⌘O: the same page becomes Today's first tab, on screen.
        little[0]
            .update(cx, |little, window, cx| little.run(Key::Open, window, cx))
            .unwrap();
        cx.run_until_parked();
        assert!(little_windows(cx).is_empty());
        let today = window
            .update(cx, |shell, _, _| {
                assert!(shell.workspace.browse.little.is_empty());
                assert_eq!(shell.workspace.browse.today()[0], pane);
                assert_eq!(shell.workspace.browse.active, Some(pane));
                assert_eq!(shell.workspace.mode, AppMode::Browser);
                assert!(shell.workspace.browser_panes.contains_key(&pane));
                shell.workspace.browse.today().len()
            })
            .unwrap();
        // Another, closed: its page goes, and nothing reaches Today or the
        // archive.
        window
            .update(cx, |shell, window, cx| {
                shell.open_little("https://other.test/".into(), window, cx)
            })
            .unwrap();
        cx.run_until_parked();
        let (gone, archived) = window
            .update(cx, |shell, _, _| {
                let archived = shell.workspace.browse.model.archive.len();
                (shell.workspace.browse.little[0], archived)
            })
            .unwrap();
        let little = little_windows(cx);
        little[0]
            .update(cx, |little, window, cx| little.run(Key::Close, window, cx))
            .unwrap();
        cx.run_until_parked();
        assert!(little_windows(cx).is_empty());
        window
            .update(cx, |shell, _, _| {
                assert!(shell.workspace.browse.little.is_empty());
                assert!(!shell.workspace.browser_panes.contains_key(&gone));
                assert_eq!(shell.workspace.browse.today().len(), today);
                assert_eq!(shell.workspace.browse.model.archive.len(), archived);
            })
            .unwrap();
    }

    #[gpui::test]
    fn routed_links_open_in_the_little_window_or_browser_mode(cx: &mut TestAppContext) {
        if !cfg!(target_os = "macos") {
            return;
        }
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, _, _) = super::super::tests::window(cx);
        cx.update(|cx| {
            let mut preferences = crate::settings::Preferences::default();
            let routes = &mut preferences.browser_link_routes;
            add_route(
                routes,
                link_match("domain", "docs.test").unwrap(),
                LinkTarget::Little,
            );
            add_route(
                routes,
                link_match("domain", "example.test").unwrap(),
                LinkTarget::Browser,
            );
            cx.set_global(preferences);
        });
        window
            .update(cx, |_, window, cx| {
                crate::shell::browser_links::open_link("https://docs.test/x", false, window, cx);
                crate::shell::browser_links::open_link("https://example.test/", false, window, cx);
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(little_windows(cx).len(), 1);
        window
            .update(cx, |shell, _, _| {
                let little = shell.workspace.browse.little[0];
                assert_eq!(
                    shell.workspace.browser_panes[&little].url,
                    "https://docs.test/x"
                );
                let first = shell.workspace.browse.today()[0];
                assert_eq!(
                    shell.workspace.browser_panes[&first].url,
                    "https://example.test/"
                );
                assert_eq!(shell.workspace.mode, AppMode::Browser);
            })
            .unwrap();
        // Another app's link with no rule: the Little window too.
        cx.update(|cx| {
            crate::shell::browser_links::open_external(vec!["https://rust.test/".into()], cx)
        });
        cx.run_until_parked();
        assert_eq!(little_windows(cx).len(), 2);
    }
}
