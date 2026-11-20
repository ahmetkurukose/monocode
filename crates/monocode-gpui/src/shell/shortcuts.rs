//! Workspace fallback for orphaned GPUI focus and embedded AppKit previews.
//! Normal focused inputs keep the keymap's context ordering. Resolve from the
//! live keymap, including user overrides, rather than maintaining another list
//! of physical shortcuts.
use super::*;
use gpui::{Action, KeyContext, Keystroke, WeakEntity};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::{Duration, Instant},
};

#[cfg(target_os = "macos")]
mod macos;

#[derive(Default)]
struct Forwarding {
    native: Cell<bool>,
    handled: Cell<bool>,
    /// The native view is composing text with an input method.
    composing: Cell<bool>,
    chord_deadline: Cell<Option<Instant>>,
}
pub(super) struct Shortcuts {
    _keys: Subscription,
    #[cfg(target_os = "macos")]
    _native: Option<macos::NativeKeys>,
}

fn workspace_action(action: &dyn Action) -> bool {
    let name = action.name();
    (name.starts_with("workspace::")
        && !matches!(
            name,
            "workspace::FocusNext" | "workspace::FocusPrevious" | "workspace::DismissMenu"
        ))
        || name == "application::Quit"
}

/// A browser page's own shortcuts (⌘L, ⌘R, ⌘F, page zoom), which also reach
/// it while Chromium has the keyboard.
fn browser_action(action: &dyn Action) -> bool {
    action.name().starts_with("browser::")
}

/// Whether a key-down that a native view (a Chromium page, a web preview, a
/// video) has the keyboard for stays with it instead of running an app
/// shortcut (docs/native/browser/context.md K17). The app's ⌘ and ⌃
/// shortcuts win, except the editing and text-navigation keys a text field
/// needs, and anything typed while an input method is composing.
fn native_keeps(stroke: &Keystroke, composing: bool) -> bool {
    if composing {
        return true;
    }
    let m = &stroke.modifiers;
    let key = stroke.key.as_str();
    // ⌘C/V/X/A/Z and ⇧⌘Z.
    if m.platform && !m.control && !m.alt {
        let editing = matches!(key, "c" | "v" | "x" | "a" | "z");
        if editing && (!m.shift || key == "z") {
            return true;
        }
    }
    // Caret moves and deletes, with ⇧ to select: ⌘ for line and document
    // ends, ⌥ for words and paragraphs.
    let caret = matches!(
        key,
        "left" | "right" | "up" | "down" | "home" | "end" | "pageup" | "pagedown"
    ) || matches!(key, "backspace" | "delete");
    if caret && !m.control && !(m.platform && m.alt) {
        return true;
    }
    // Cocoa's Emacs keys in text fields: ⌃A/E line ends, ⌃B/F/N/P moves,
    // ⌃D/H/K deletes, ⌃Y yank, ⌃T transpose, ⌃O open line.
    m.control
        && !m.platform
        && !m.alt
        && matches!(
            key,
            "a" | "e" | "b" | "f" | "n" | "p" | "d" | "h" | "k" | "y" | "t" | "o"
        )
}

/// Keymap owns precedence, disabled bindings, physical-key matching and chords.
/// `browser` adds a browser page's context, whose bindings outrank the
/// workspace's (page zoom over interface zoom).
fn candidates(keys: &[Keystroke], browser: bool, cx: &App) -> Vec<KeyBinding> {
    let mut contexts = vec![KeyContext::parse("Workspace").expect("static workspace key context")];
    if browser {
        contexts.push(KeyContext::parse(BROWSER_CONTEXT).expect("static browser key context"));
    }
    let keymap = cx.key_bindings();
    let keymap = keymap.borrow();
    let (complete, _) = keymap.bindings_for_input(keys, &contexts);
    complete
        .into_iter()
        .chain(keymap.possible_next_bindings_for_input(keys, &contexts))
        .filter(|binding| {
            workspace_action(binding.action()) || (browser && browser_action(binding.action()))
        })
        .collect()
}

const BROWSER_CONTEXT: &str = crate::browser::KEY_CONTEXT;

pub(super) fn install(owner: WeakEntity<Shell>, window: &Window, cx: &mut App) -> Shortcuts {
    let window_id = window.window_handle().window_id();
    let forwarding = Rc::new(Forwarding::default());
    #[cfg(target_os = "macos")]
    let native = macos::NativeKeys::new(window, forwarding.clone());
    let pending = RefCell::new(Vec::<Keystroke>::new());
    let keys = cx.intercept_keystrokes(move |event, window, cx| {
        if window.window_handle().window_id() != window_id {
            return;
        }
        let Some(owner) = owner.upgrade() else {
            return;
        };
        let root = owner.read(cx).focus.clone();
        let native = forwarding.native.get();
        let now = Instant::now();
        if forwarding
            .chord_deadline
            .get()
            .is_some_and(|until| now >= until)
        {
            pending.borrow_mut().clear();
            forwarding.chord_deadline.set(None);
        }
        if !native && root.contains_focused(window, cx) && pending.borrow().is_empty() {
            return;
        }
        // Modifier release observations are not strokes in a shortcut chord.
        if matches!(
            event.keystroke.key.as_str(),
            "control" | "platform" | "alt" | "shift" | "function"
        ) {
            return;
        }
        if native
            && pending.borrow().is_empty()
            && native_keeps(&event.keystroke, forwarding.composing.get())
        {
            // The monitor re-delivers the original event to the native view.
            cx.stop_propagation();
            return;
        }
        // GPUI focus follows a browser page that takes the keyboard, so its
        // context is in the focused stack while Chromium has the keys.
        let browser = window
            .context_stack()
            .iter()
            .any(|context| context.contains(BROWSER_CONTEXT));
        let mut sequence = pending.borrow_mut();
        sequence.push(event.keystroke.clone());
        let mut matches = candidates(&sequence, browser, cx);
        if matches.is_empty() && sequence.len() > 1 {
            sequence.clear();
            sequence.push(event.keystroke.clone());
            matches = candidates(&sequence, browser, cx);
        }
        let action = matches
            .iter()
            .find(|binding| binding.match_keystrokes(sequence.as_slice()) == Some(false))
            .map(|binding| binding.action().boxed_clone());
        let partial = action.is_none() && !matches.is_empty();
        if !partial {
            sequence.clear();
        }
        drop(sequence);
        forwarding
            .chord_deadline
            .set(partial.then(|| now + Duration::from_secs(1)));
        if let Some(action) = action {
            if browser_action(action.as_ref()) {
                // From the focused page, which handles it.
                if let Some(focused) = window.focused(cx) {
                    focused.dispatch_action(action.as_ref(), window, cx);
                }
                forwarding.handled.set(true);
                cx.stop_propagation();
                return;
            }
            if !native && !root.contains_focused(window, cx) {
                // Repair lost GPUI focus without disturbing an embedded native
                // view's selection or its first-responder ownership.
                owner.update(cx, |shell, cx| shell.focus_workspace(window, cx));
            }
            root.dispatch_action(action.as_ref(), window, cx);
            forwarding.handled.set(true);
            cx.stop_propagation();
        } else if partial {
            forwarding.handled.set(true);
            cx.stop_propagation();
        } else if native {
            // The monitor re-delivers the original event to AppKit. Prevent
            // the stale GPUI input from processing Copy, typing, IME, etc.
            cx.stop_propagation();
        }
    });
    Shortcuts {
        _keys: keys,
        #[cfg(target_os = "macos")]
        _native: native,
    }
}

#[cfg(test)]
mod tests {
    use gpui::{Keystroke, TestAppContext};

    #[test]
    fn a_native_view_keeps_editing_and_caret_keys_and_ime_input() {
        let keeps = |stroke: &str| super::native_keeps(&Keystroke::parse(stroke).unwrap(), false);
        for stroke in [
            "cmd-c",
            "cmd-v",
            "cmd-x",
            "cmd-a",
            "cmd-z",
            "cmd-shift-z",
            "cmd-left",
            "cmd-right",
            "cmd-shift-left",
            "cmd-shift-right",
            "cmd-up",
            "cmd-down",
            "cmd-shift-up",
            "cmd-shift-down",
            "cmd-backspace",
            "cmd-delete",
            "alt-left",
            "alt-shift-right",
            "alt-up",
            "alt-backspace",
            "shift-left",
            "home",
            "pagedown",
            "ctrl-a",
            "ctrl-e",
            "ctrl-k",
            "ctrl-shift-e",
        ] {
            assert!(keeps(stroke), "{stroke} should stay with the page");
        }
        for stroke in [
            "cmd-shift-c",
            "cmd-shift-a",
            "cmd-alt-c",
            "cmd-l",
            "cmd-r",
            "cmd-shift-r",
            "cmd-f",
            "cmd-=",
            "cmd--",
            "cmd-0",
            "cmd-w",
            "cmd-t",
            "cmd-1",
            "cmd-[",
            "cmd-alt-left",
            "ctrl-left",
            "ctrl-tab",
            "ctrl-c",
            "cmd-ctrl-a",
        ] {
            assert!(!keeps(stroke), "{stroke} should run the app's shortcut");
        }
        // Nothing is taken while an input method composes.
        assert!(super::native_keeps(
            &Keystroke::parse("cmd-w").unwrap(),
            true
        ));
    }

    #[gpui::test]
    fn a_browser_page_outranks_the_workspace_while_chromium_has_the_keys(cx: &mut TestAppContext) {
        cx.update(|cx| {
            super::super::bind_keys(cx);
            let first = |stroke: &str, browser| {
                let keys = [Keystroke::parse(stroke).unwrap()];
                super::candidates(&keys, browser, cx)
                    .first()
                    .map(|b| b.action().name())
            };
            let m = if cfg!(target_os = "macos") {
                "cmd"
            } else {
                "ctrl"
            };
            assert_eq!(first(&format!("{m}-="), false), Some("workspace::ZoomIn"));
            assert_eq!(first(&format!("{m}-="), true), Some("browser::ZoomIn"));
            assert_eq!(first(&format!("{m}-0"), true), Some("browser::ResetZoom"));
            assert_eq!(
                first(&format!("{m}-shift-r"), true),
                Some("browser::ReloadIgnoringCache")
            );
            assert_eq!(first(&format!("{m}-r"), true), Some("browser::Reload"));
            assert_eq!(
                first(&format!("{m}-l"), true),
                Some("browser::FocusAddress")
            );
            assert_eq!(first(&format!("{m}-f"), true), Some("browser::Find"));
            assert_eq!(first(&format!("{m}-r"), false), None);
            // Workspace shortcuts keep working from a page.
            assert_eq!(first(&format!("{m}-w"), true), Some("workspace::CloseTab"));
            assert_eq!(first(&format!("{m}-["), true), Some("workspace::VisitBack"));
        });
    }

    #[gpui::test]
    fn lost_focus_shortcuts_follow_live_remaps_and_disabled_bindings(cx: &mut TestAppContext) {
        cx.update(|cx| {
            crate::input::bind(cx);
            super::super::bind_keys(cx);
            cx.set_reduce_motion(true);
        });
        let (window, _, _) = super::super::tests::window(cx);
        cx.refresh().unwrap();
        let orphan = window
            .update(cx, |_, window, cx| {
                let orphan = cx.focus_handle();
                window.focus(&orphan, cx);
                orphan
            })
            .unwrap();
        let modifier = if cfg!(target_os = "macos") {
            "cmd"
        } else {
            "ctrl"
        };
        cx.simulate_keystrokes(window.into(), &format!("{modifier}-b"));
        cx.run_until_parked();
        window
            .update(cx, |shell, _, _| assert!(!shell.workspace.sidebar_visible))
            .unwrap();
        for (binding, stroke, expected) in [
            (None, format!("{modifier}-b"), false),
            (
                Some(format!("{modifier}-alt-b")),
                format!("{modifier}-b"),
                false,
            ),
            (
                Some(format!("{modifier}-alt-b")),
                format!("{modifier}-alt-b"),
                true,
            ),
            (
                Some(format!("{modifier}-alt-y {modifier}-alt-b")),
                format!("{modifier}-alt-y {modifier}-alt-b"),
                false,
            ),
        ] {
            window
                .update(cx, |shell, window, cx| {
                    shell
                        .workspace
                        .preferences
                        .keybindings
                        .insert("App: Toggle Sidebar".into(), binding);
                    crate::settings::apply_keybindings(&shell.workspace.preferences, cx);
                    window.focus(&orphan, cx);
                })
                .unwrap();
            cx.simulate_keystrokes(window.into(), &stroke);
            cx.run_until_parked();
            window
                .update(cx, |shell, _, _| {
                    assert_eq!(shell.workspace.sidebar_visible, expected)
                })
                .unwrap();
        }
    }
}
