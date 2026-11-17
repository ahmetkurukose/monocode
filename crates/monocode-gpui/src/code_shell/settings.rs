//! Pages a feature adds to a project's settings, beside "Terminal setup…" in
//! the project menu: the Run page with its list of configurations. A page is
//! a view the feature builds for one project; the shell shows it in the
//! project dialog, hands it what was saved and stores what it saves. The
//! saved values travel with the workspace, by project and page id.
use crate::{popover::AnchorSlot, theme::*};
use gpui::{
    AnyView, App, Context, EventEmitter, FocusHandle, Focusable, Global, Role, SharedString,
    Window, div, prelude::*, px,
};
use monocode_core::{code::run::ProjectRuns, files::path_key};
use serde_json::Value;
use std::{collections::HashMap, rc::Rc};

/// Builds a page's form for one project.
pub(crate) type PageBuilder = Rc<dyn Fn(PageContext, &mut Window, &mut App) -> AnyView>;

#[derive(Clone)]
pub(crate) struct SettingsPage {
    /// Fixed and unique; the key the page's value is saved under.
    pub id: &'static str,
    /// The project menu's row, and the dialog's title: "Run configurations".
    pub title: &'static str,
    pub icon: &'static str,
    /// Builds the form for one project. Runs after the shell's update, so it
    /// may call anything in `code_shell`.
    pub build: PageBuilder,
}

/// What a page is built with.
#[derive(Clone)]
pub(crate) struct PageContext {
    /// The project's path, as the shell knows it.
    pub project: String,
    /// What the page last saved for this project.
    pub value: Option<Value>,
    pub host: PageHost,
}

/// The page's way back to the shell.
#[derive(Clone)]
pub(crate) struct PageHost {
    pub(crate) project: String,
    pub(crate) page: &'static str,
}
impl PageHost {
    /// Stores `value` for this project and page (`None` forgets it) and
    /// publishes it for `value`. The dialog stays open.
    pub(crate) fn save(&self, value: Option<Value>, window: &mut Window, cx: &mut App) {
        let (project, page) = (self.project.clone(), self.page);
        super::with_shell(window, cx, |shell, _, cx| {
            shell.save_code_setting(&project, page, value, cx)
        });
    }
    pub(crate) fn close(&self, window: &mut Window, cx: &mut App) {
        super::with_shell(window, cx, |shell, window, cx| {
            shell.close_project_dialog(window, cx)
        });
    }
}

#[derive(Default)]
struct Pages(Vec<SettingsPage>);
impl Global for Pages {}

/// The saved values by project (`path_key`) and page id, published by the
/// shell so features can read them without it.
#[derive(Default)]
pub(crate) struct Saved(pub(crate) HashMap<String, HashMap<String, Value>>);
impl Global for Saved {}

/// The projects' run configurations (`path_key`), published by the shell
/// from `Workspace::project_runs`, which is where they are kept.
#[derive(Default)]
pub(crate) struct SavedRuns(pub(crate) HashMap<String, ProjectRuns>);
impl Global for SavedRuns {}

/// The project's run configurations, last run and test runner.
pub(crate) fn runs(project: &str, cx: &App) -> ProjectRuns {
    cx.try_global::<SavedRuns>()
        .and_then(|saved| saved.0.get(&path_key(project)).cloned())
        .unwrap_or_default()
}

/// Stores the project's run configurations in the workspace and saves it.
pub(crate) fn save_runs(project: &str, runs: ProjectRuns, window: &mut Window, cx: &mut App) {
    super::with_shell(window, cx, |shell, _, cx| {
        shell.save_project_runs(project, runs, cx)
    });
}

/// Adds a page to every project's settings, replacing one with the same id.
pub(crate) fn register(page: SettingsPage, cx: &mut App) {
    let pages = &mut cx.default_global::<Pages>().0;
    match pages.iter_mut().find(|known| known.id == page.id) {
        Some(known) => *known = page,
        None => pages.push(page),
    }
}

pub(crate) fn pages(cx: &App) -> Vec<SettingsPage> {
    cx.try_global::<Pages>()
        .map(|pages| pages.0.clone())
        .unwrap_or_default()
}

/// What `page` last saved for `project`.
pub(crate) fn value(project: &str, page: &str, cx: &App) -> Option<Value> {
    (cx.try_global::<Saved>()?
        .0
        .get(&path_key(project))?
        .get(page))
    .cloned()
}

/// Opens `page` for `project`, as its row in the project menu does. False
/// when no such page is registered.
pub(crate) fn open(project: &str, page: &'static str, window: &mut Window, cx: &mut App) -> bool {
    let known = pages(cx).iter().any(|known| known.id == page);
    known
        && super::with_shell(window, cx, |shell, window, cx| {
            shell.open_code_settings_page(project.to_owned(), page, window, cx)
        })
        .is_some()
}

pub(crate) enum DialogEvent {
    Close,
}

/// The project dialog's frame around a page: its title, the project it is
/// for, and a close button over the feature's form.
pub(crate) struct PageDialog {
    title: &'static str,
    project: SharedString,
    view: AnyView,
    focus: FocusHandle,
    bounds: AnchorSlot,
}
impl EventEmitter<DialogEvent> for PageDialog {}
impl Focusable for PageDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl PageDialog {
    pub(crate) fn new(
        title: &'static str,
        project: SharedString,
        view: AnyView,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            title,
            project,
            view,
            focus: cx.focus_handle(),
            bounds: AnchorSlot::default(),
        }
    }
    pub(crate) fn measured_height(&self) -> Option<f32> {
        self.bounds.get().map(|bounds| bounds.size.height.as_f32())
    }
}
impl Render for PageDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("project-code-settings-dialog")
            .debug_selector(|| "project-code-settings-dialog".into())
            .role(Role::Dialog)
            .aria_label(self.title)
            .track_focus(&self.focus)
            .relative()
            .min_h_0()
            .max_h(px(window.viewport_size().height.as_f32() - 34.))
            .flex()
            .flex_col()
            .text_color(content(1.))
            .child(self.bounds.layout_probe())
            .on_key_down(cx.listener(|_, event: &gpui::KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    cx.emit(DialogEvent::Close);
                    window.prevent_default();
                    cx.stop_propagation();
                }
            }))
            .child(
                div()
                    .flex_shrink_0()
                    .flex()
                    .items_start()
                    .gap_2()
                    .px_4()
                    .pt_3()
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .pt(px(2.))
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(px(16.))
                                    .line_height(px(20.))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(self.title),
                            )
                            .child(
                                div()
                                    .mt(px(2.))
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(12.))
                                    .line_height(px(16.5))
                                    .text_color(content(0.5))
                                    .child(self.project.clone()),
                            ),
                    )
                    .child(
                        icon_button("close-project-code-settings", "Close", "close")
                            .size(px(28.))
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(DialogEvent::Close))),
                    ),
            )
            .child(
                div()
                    .id("project-code-settings-page")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_4()
                    .child(self.view.clone()),
            )
    }
}
