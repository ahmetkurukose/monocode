//! A read-only dock tab for a run that has no terminal: what an agent
//! started through its tools. It shows the registry's output tail and
//! follows it while the run goes on.
use super::{chip, start};
use crate::{
    code_shell::dock::{self, DockTab},
    theme::*,
};
use gpui::{
    App, Context, Entity, FocusHandle, Focusable, ScrollStrategy, SharedString, Task,
    UniformListScrollHandle, Window, div, prelude::*, px, uniform_list,
};
use monocode_core::code::{
    run::{RunEvent, RunId, RunInfo, RunState},
    services::services,
};

/// The most lines kept on screen; the registry's tail is about this much.
const MAX_LINES: usize = 20_000;
/// The most of a line without its end that is held back before it is listed.
const MAX_PARTIAL: usize = 64 * 1024;

pub(crate) struct OutputView {
    id: RunId,
    lines: Vec<SharedString>,
    /// The line being written, until its end arrives.
    partial: String,
    /// Where the registry's tail was read up to.
    next: u64,
    /// Output older than the tail was never seen here.
    truncated: bool,
    state: Option<RunState>,
    follow: bool,
    scroll: UniformListScrollHandle,
    focus: FocusHandle,
    _events: Task<()>,
}

impl Focusable for OutputView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl OutputView {
    fn new(id: RunId, cx: &mut Context<Self>) -> Self {
        let events = services().runs().subscribe();
        let task = cx.spawn(async move |this, cx| {
            while let Ok(event) = events.recv().await {
                let ours = match event {
                    RunEvent::Output(run) | RunEvent::Changed(run) | RunEvent::Removed(run) => {
                        run == id
                    }
                    _ => false,
                };
                if ours && this.update(cx, |this, cx| this.read(cx)).is_err() {
                    break;
                }
            }
        });
        let mut this = Self {
            id,
            lines: Vec::new(),
            partial: String::new(),
            next: 0,
            truncated: false,
            state: None,
            follow: true,
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            _events: task,
        };
        this.read(cx);
        this
    }

    /// Takes what the run wrote since the last read.
    fn read(&mut self, cx: &mut Context<Self>) {
        let registry = services().runs();
        self.state = registry.get(self.id).map(|info| info.state);
        if let Some(output) = registry.output(self.id, self.next) {
            self.truncated |= output.truncated;
            self.next = output.next;
            self.partial.push_str(&output.text);
            while let Some(end) = self.partial.find('\n') {
                let line: String = self.partial.drain(..=end).collect();
                self.lines
                    .push(line.trim_end_matches('\n').to_owned().into());
            }
            // A program that never ends its line would grow this without end.
            if self.partial.len() > MAX_PARTIAL {
                let cut = self.partial.floor_char_boundary(MAX_PARTIAL);
                let line: String = self.partial.drain(..cut).collect();
                self.lines.push(line.into());
            }
            if self.lines.len() > MAX_LINES {
                let extra = self.lines.len() - MAX_LINES;
                self.lines.drain(..extra);
                self.truncated = true;
            }
        }
        if self.follow {
            self.scroll
                .scroll_to_item(self.count().saturating_sub(1), ScrollStrategy::Bottom);
        }
        cx.notify();
    }

    fn count(&self) -> usize {
        self.lines.len() + usize::from(!self.partial.is_empty())
    }

    /// Everything shown, for a test.
    #[cfg(test)]
    pub(crate) fn text(&self) -> String {
        let mut text = self.lines.join("\n");
        if !self.lines.is_empty() {
            text.push('\n');
        }
        text + &self.partial
    }

    fn status(&self) -> String {
        match self.state {
            Some(RunState::Running) => "Running".into(),
            Some(RunState::Exited(Some(code))) => format!("Ended with exit code {code}"),
            Some(RunState::Exited(None)) => "Ended".into(),
            Some(RunState::Stopped) => "Stopped".into(),
            None => "No longer listed".into(),
        }
    }
}

impl Render for OutputView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let running = self.state == Some(RunState::Running);
        let id = self.id;
        let note = match self.truncated {
            true => format!("{} \u{00b7} earlier output is not kept", self.status()),
            false => self.status(),
        };
        div()
            .id("run-output")
            .debug_selector(|| "run-output".into())
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_shrink_0()
                    .h(px(26.))
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(px(11.))
                    .text_color(content(0.55))
                    .child(div().flex_1().min_w_0().truncate().child(note))
                    .child(
                        div()
                            .id("run-output-follow")
                            .role(gpui::Role::Button)
                            .aria_label("Follow output")
                            .cursor_pointer()
                            .px_1p5()
                            .rounded_md()
                            .when(self.follow, |d| d.bg(content(0.1)).text_color(content(0.9)))
                            .hover(|s| s.bg(content(0.1)))
                            .child("Follow")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.follow = !this.follow;
                                this.read(cx);
                            })),
                    )
                    .when(running, |d| {
                        d.child(
                            icon_button("run-output-stop", "Stop", "square")
                                .on_click(move |_, _, _| start::stop(id)),
                        )
                    }),
            )
            .child(
                uniform_list("run-output-lines", self.count(), move |range, _, cx| {
                    let this = entity.read(cx);
                    range
                        .map(|ix| {
                            let line = match this.lines.get(ix) {
                                Some(line) => line.clone(),
                                None => SharedString::from(this.partial.clone()),
                            };
                            div().px_3().h(px(17.)).whitespace_nowrap().child(line)
                        })
                        .collect::<Vec<_>>()
                })
                .track_scroll(&self.scroll)
                .flex_1()
                .min_h_0()
                .font_family(crate::fonts::mono())
                .text_size(px(12.))
                .line_height(px(17.))
                .text_color(content(0.85)),
            )
    }
}

fn tab_id(id: RunId) -> String {
    format!("run-output:{}", id.0)
}

/// Opens the run's output as a tab of its project's dock, or shows the one
/// that is already there.
pub(super) fn show(info: &RunInfo, window: &mut Window, cx: &mut App) {
    let checkout = chip::checkout_of(info, cx);
    let tab = tab_id(info.id);
    if let Some(open) = dock::find_tab(&checkout.project, &tab, cx) {
        open.reveal(window, cx);
        return;
    }
    let view: Entity<OutputView> = cx.new(|cx| OutputView::new(info.id, cx));
    let added = dock::add_tab(
        DockTab {
            id: tab.into(),
            title: format!("{} (output)", info.label).into(),
            icon: "terminal",
            focus: Some(view.focus_handle(cx)),
            view: view.into(),
            controls: None,
            checkout,
            on_close: None,
            on_placement: None,
            reveal: true,
        },
        window,
        cx,
    );
    if let Err(sentence) = added {
        start::notice(sentence, window, cx);
    }
}

/// A view of a run's output on its own, for a test to read.
#[cfg(test)]
pub(crate) fn view(id: RunId, cx: &mut App) -> Entity<OutputView> {
    cx.new(|cx| OutputView::new(id, cx))
}

/// Whether a run's output tab is open. For tests.
#[cfg(test)]
pub(crate) fn open_view(info: &RunInfo, cx: &App) -> bool {
    let checkout = chip::checkout_of(info, cx);
    dock::find_tab(&checkout.project, &tab_id(info.id), cx).is_some()
}
