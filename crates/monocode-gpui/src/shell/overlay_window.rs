//! Browser mode's menus and Command Bar in a window of their own
//! (docs/native/modes.md §Overlays). Chromium's pages are native views GPUI
//! cannot draw over, so in the main window a menu over a page made the page
//! give way to its last picture (context.md K16). In Browser mode they are
//! drawn instead in a transparent, borderless child window the size of the
//! main one, above it: the live page stays under them, as SOCode does.
//!
//! The shell decides at each frame whether one is wanted (a menu, the bar, or
//! a page's popover open in Browser mode) and opens or closes it; the child
//! window draws what the shell gives it ([`Shell::browse_overlay_content`]).
//! A page's popovers (the Site Control Center, the site's details, the
//! downloads, the saved passwords) and Capture are the page's own: the shell
//! draws them in the child window ([`BrowserPage::page_popovers`]) and the
//! page leaves them out (`popovers_elsewhere`). A press on the child window's
//! empty room, or Esc, closes what is open. Actions picked in it run in the
//! main window ([`Shell::in_main_window`]).
use super::*;
use crate::browser::BrowserPage;
use gpui::{
    AnyWindowHandle, Bounds, EntityId, WeakEntity, WindowBackgroundAppearance, WindowBounds,
    WindowHandle, WindowKind, WindowOptions,
};

/// The child window's view: the shell's overlay content.
pub(crate) struct OverlayRoot {
    shell: WeakEntity<Shell>,
    _observe: Subscription,
    /// The pages whose popovers it draws: it draws again when they change.
    pages: HashMap<EntityId, Subscription>,
}

impl Render for OverlayRoot {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let shell = self.shell.upgrade();
        let pages = (shell.as_ref())
            .map(|shell| shell.read(cx).browse_popover_pages(cx))
            .unwrap_or_default();
        self.pages
            .retain(|id, _| pages.iter().any(|page| page.entity_id() == *id));
        for page in &pages {
            if !self.pages.contains_key(&page.entity_id()) {
                let observe = cx.observe(page, |_, _, cx| cx.notify());
                self.pages.insert(page.entity_id(), observe);
            }
        }
        let drawn = shell.and_then(|shell| {
            shell.update(cx, |shell, cx| shell.browse_overlay_content(window, cx))
        });
        let weak = self.shell.clone();
        div()
            .id("browse-overlay-root")
            .size_full()
            .relative()
            // As the main window's root: its ink and font.
            .text_color(content(1.))
            .font_family(crate::fonts::UI)
            // Esc closes a page's popover; the menu and the bar hear it first.
            .on_key_down(move |event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    let _ = weak.update(cx, |shell, cx| shell.close_browse_page_popovers(cx));
                }
            })
            .children(drawn)
            // No page sweeps this window: what its popovers covered goes.
            .child(
                gpui::deferred(canvas(
                    |_, _, _| {},
                    |_, _, window, _| crate::browser::forget_covers(window),
                ))
                .with_priority(usize::MAX),
            )
    }
}

/// The open child window, and the main window's bounds when it opened.
pub(super) struct BrowseOverlay {
    handle: WindowHandle<OverlayRoot>,
    bounds: Bounds<Pixels>,
}

impl Shell {
    /// Browser mode's overlays draw in the child window (not in tests).
    fn browse_overlay_available(&self) -> bool {
        !cfg!(test) && cfg!(target_os = "macos") && self.browser_mode()
    }

    /// A menu, the Command Bar or a page's popover is open in Browser mode:
    /// it is drawn in the child window, not in the main one.
    pub(super) fn browse_overlay_wanted(&self) -> bool {
        self.browse_overlay_available()
            && (self.menu.is_some() || self.command_bar.is_some() || self.browse_page_popover)
    }

    /// The pages whose popovers the child window draws.
    pub(super) fn browse_popover_pages(&self, cx: &App) -> Vec<Entity<BrowserPage>> {
        if !self.browse_overlay_available() {
            return Vec::new();
        }
        let mut pages: Vec<(i64, Entity<BrowserPage>)> = (self.browser_pages.iter())
            .filter(|(_, (page, _))| page.read(cx).page_popover_open())
            .map(|(pane, (page, _))| (*pane, page.clone()))
            .collect();
        pages.sort_by_key(|(pane, _)| *pane);
        pages.into_iter().map(|(_, page)| page).collect()
    }

    /// Tells the pages whether the child window draws their popovers, and
    /// notes whether one is open.
    fn sync_page_popovers(&mut self, cx: &mut Context<Self>) {
        let elsewhere = self.browse_overlay_available();
        let pages: Vec<Entity<BrowserPage>> = (self.browser_pages.values())
            .map(|(page, _)| page.clone())
            .collect();
        let mut open = false;
        for page in pages {
            open |= page.update(cx, |page, cx| {
                if page.popovers_elsewhere != elsewhere {
                    page.popovers_elsewhere = elsewhere;
                    cx.notify();
                }
                page.page_popover_open()
            });
        }
        self.browse_page_popover = elsewhere && open;
    }

    /// Closes every page's popovers and Capture.
    pub(super) fn close_browse_page_popovers(&mut self, cx: &mut Context<Self>) {
        let pages: Vec<Entity<BrowserPage>> = (self.browser_pages.values())
            .map(|(page, _)| page.clone())
            .collect();
        for page in pages {
            page.update(cx, |page, cx| page.close_page_popovers(cx));
        }
        if std::mem::take(&mut self.browse_page_popover) {
            cx.notify();
        }
    }

    /// At each frame of the main window: the child window opens when wanted,
    /// closes when not, and a moved or resized main window closes what is
    /// open (the child keeps the old size).
    pub(super) fn sync_browse_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.main_window = Some(window.window_handle());
        self.sync_page_popovers(cx);
        let wanted = self.browse_overlay_wanted();
        if let Some(open) = &self.browse_overlay
            && wanted
            && open.bounds.size != window.bounds().size
        {
            self.close_menu(cx);
            self.command_bar = None;
            self.close_browse_page_popovers(cx);
        }
        let wanted = self.browse_overlay_wanted();
        match (
            self.browse_overlay.is_some() || self.browse_overlay_opening,
            wanted,
        ) {
            (false, true) => self.open_browse_overlay(window, cx),
            (true, false) => self.close_browse_overlay(window, cx),
            _ => {}
        }
    }

    fn open_browse_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(parent) = native_window(window) else {
            return;
        };
        self.browse_overlay_opening = true;
        let bounds = window.bounds();
        let shell = cx.entity();
        let weak = shell.downgrade();
        cx.defer(move |cx| {
            // What wants the keyboard: the menu's rows or the bar's field.
            let focus = shell.read_with(cx, |shell, cx| {
                (shell
                    .command_bar
                    .as_ref()
                    .map(|bar| bar.input.read(cx).focus_handle(cx)))
                .or_else(|| {
                    shell
                        .menu
                        .as_ref()
                        .map(|open| open.menu.read(cx).content_focus())
                })
            });
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: None,
                focus: true,
                show: true,
                kind: WindowKind::Normal,
                is_movable: false,
                is_resizable: false,
                is_minimizable: false,
                window_background: WindowBackgroundAppearance::Transparent,
                ..Default::default()
            };
            let opened = cx.open_window(options, |window, cx| {
                attach_child(parent, window);
                // Nothing is drawn under its popovers: they paint opaque.
                crate::popover::set_opaque_glass(window.window_handle().window_id(), true);
                if let Some(focus) = &focus {
                    window.focus(focus, cx);
                }
                // It draws again whenever the shell does.
                cx.new(|cx| OverlayRoot {
                    shell: weak.clone(),
                    _observe: cx.observe(&shell, |_, _, cx| cx.notify()),
                    pages: HashMap::new(),
                })
            });
            shell.update(cx, |this, cx| {
                this.browse_overlay_opening = false;
                if let Ok(handle) = opened {
                    this.browse_overlay = Some(BrowseOverlay { handle, bounds });
                }
                cx.notify();
            });
        });
    }

    fn close_browse_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(open) = self.browse_overlay.take() else {
            return;
        };
        let main = window.window_handle();
        let handle: AnyWindowHandle = open.handle.into();
        cx.defer(move |cx| {
            let _ = handle.update(cx, |_, window, _| {
                crate::popover::set_opaque_glass(window.window_handle().window_id(), false);
                window.remove_window()
            });
            // The main window takes the keyboard back, for the page.
            let _ = main.update(cx, |_, window, _| window.activate_window());
        });
        if self.browser_mode() {
            self.focus_browse_page(window, cx);
        }
    }

    /// Runs `f` with the main window, as an action picked in the child
    /// window must: focus and pages live there.
    pub(super) fn in_main_window(
        &self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let Some(main) = self.main_window else {
            return;
        };
        let this = cx.entity();
        cx.defer(move |cx| {
            let _ = main.update(cx, |_, window, cx| {
                this.update(cx, |shell, cx| f(shell, window, cx));
            });
        });
    }

    /// What the child window draws: a veil that closes what is open on a
    /// press, the pages' popovers, the Command Bar, and the open menu.
    pub(super) fn browse_overlay_content(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.browse_overlay_wanted() {
            return None;
        }
        let veil = div()
            .id("browse-overlay-veil")
            .absolute()
            .inset_0()
            // Fully clear windows let presses through to the window below.
            .bg(gpui::rgba(0x00000003))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.close_browse_overlay_content(cx)),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _, _, cx| this.close_browse_overlay_content(cx)),
            );
        let popovers: Vec<AnyElement> = (self.browse_popover_pages(cx).into_iter())
            .flat_map(|page| page.update(cx, |page, cx| page.page_popovers(cx)))
            .collect();
        Some(
            div()
                .size_full()
                .relative()
                .child(veil)
                .children(popovers)
                .children(self.browse_command_bar_layer(cx))
                .children(self.menu_layer(cx))
                .into_any_element(),
        )
    }

    /// A press beside the menu, bar or popover: it closes.
    fn close_browse_overlay_content(&mut self, cx: &mut Context<Self>) {
        self.close_browse_page_popovers(cx);
        self.close_menu(cx);
        if self.command_bar.take().is_some() {
            self.invalidate_chrome(cx);
            cx.notify();
        }
    }
}

/// The NSWindow of `window`, as an address.
#[cfg(target_os = "macos")]
pub(super) fn native_window(window: &Window) -> Option<usize> {
    use objc2::{msg_send, runtime::AnyObject};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return None;
    };
    let view = handle.ns_view.as_ptr().cast::<AnyObject>();
    // SAFETY: GPUI keeps its content view alive with the window.
    let ns_window: *mut AnyObject = unsafe { msg_send![view, window] };
    (!ns_window.is_null()).then_some(ns_window as usize)
}

#[cfg(not(target_os = "macos"))]
pub(super) fn native_window(_: &Window) -> Option<usize> {
    None
}

/// Makes `child` a child window of the NSWindow `parent`, above it: it moves
/// with it and never casts a shadow or detaches.
#[cfg(target_os = "macos")]
pub(super) fn attach_child(parent: usize, child: &Window) {
    use objc2::{
        msg_send,
        runtime::{AnyObject, Bool},
    };
    let Some(child) = native_window(child) else {
        return;
    };
    let (parent, child) = (parent as *mut AnyObject, child as *mut AnyObject);
    // SAFETY: both windows are alive: GPUI just made the child, and the
    // parent is the window whose frame asked for it.
    unsafe {
        let _: () = msg_send![child, setHasShadow: Bool::NO];
        let _: () = msg_send![child, setMovable: Bool::NO];
        let _: () = msg_send![child, setMovableByWindowBackground: Bool::NO];
        // NSWindowAbove
        let _: () = msg_send![parent, addChildWindow: child, ordered: 1isize];
    }
}

#[cfg(not(target_os = "macos"))]
pub(super) fn attach_child(_: usize, _: &Window) {}
