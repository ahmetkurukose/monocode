//! Right panel tabs a feature adds beside Explorer and History: the file
//! outline, or find-usages results that appear with their results and close.
//! A tab is there while it is registered; the panel remembers by its id where
//! it was dragged to, as it does for its own tabs.
use gpui::{AnyView, App, Context, FocusHandle, Global, Subscription, Window};
use std::rc::Rc;

/// Told when a tab is removed.
pub(crate) type TabRemoved = Rc<dyn Fn(&mut App)>;

#[derive(Clone)]
pub(crate) struct PanelTab {
    /// Names the tab in the saved strip order and in the element tree; fixed
    /// and unique, such as `"code-outline"`.
    pub id: &'static str,
    pub title: &'static str,
    pub icon: &'static str,
    pub view: AnyView,
    /// What takes the keyboard when the tab is selected.
    pub focus: Option<FocusHandle>,
    /// A middle click on the tab removes it. The view shows its own close
    /// button, which calls `remove`.
    pub closable: bool,
    /// Told when the tab is removed, however that happened.
    pub on_close: Option<TabRemoved>,
}

#[derive(Default)]
struct PanelTabs(Vec<PanelTab>);
impl Global for PanelTabs {}

/// Adds the tab, or replaces the one with the same id. It joins the strip
/// without being selected; `reveal` selects it.
pub(crate) fn show(tab: PanelTab, cx: &mut App) {
    let tabs = &mut cx.default_global::<PanelTabs>().0;
    match tabs.iter_mut().find(|known| known.id == tab.id) {
        Some(known) => *known = tab,
        None => tabs.push(tab),
    }
    super::refresh(cx);
}

/// Takes the tab out of the strip; Explorer takes over if it was selected.
pub(crate) fn remove(id: &'static str, cx: &mut App) {
    let tabs = &mut cx.default_global::<PanelTabs>().0;
    let Some(ix) = tabs.iter().position(|tab| tab.id == id) else {
        return;
    };
    let tab = tabs.remove(ix);
    if let Some(closed) = tab.on_close {
        closed(cx);
    }
    super::refresh(cx);
}

pub(crate) fn is_shown(id: &str, cx: &App) -> bool {
    cx.try_global::<PanelTabs>()
        .is_some_and(|tabs| tabs.0.iter().any(|tab| tab.id == id))
}

/// Opens the right panel on the tab. False when it is not registered or the
/// panel cannot open here (no window yet, or the project has no panel).
pub(crate) fn reveal(id: &'static str, window: &mut Window, cx: &mut App) -> bool {
    is_shown(id, cx)
        && super::with_shell(window, cx, |shell, window, cx| {
            shell.reveal_panel_tab(id, window, cx)
        })
        .unwrap_or(false)
}

/// The registered tabs, in the order they were added.
pub(crate) fn tabs(cx: &App) -> Vec<PanelTab> {
    cx.try_global::<PanelTabs>()
        .map(|tabs| tabs.0.clone())
        .unwrap_or_default()
}

/// Calls `changed` whenever a tab is added, replaced or removed.
pub(crate) fn observe<T: 'static>(
    cx: &mut Context<T>,
    mut changed: impl FnMut(&mut T, &mut Context<T>) + 'static,
) -> Subscription {
    cx.default_global::<PanelTabs>();
    cx.observe_global::<PanelTabs>(move |this, cx| changed(this, cx))
}
