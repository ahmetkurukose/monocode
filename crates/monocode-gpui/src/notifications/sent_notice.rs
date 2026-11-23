//! A quiet confirmation that clears itself: text reached a data source's
//! console, or a project went to sleep. Neither moves the user anywhere, so
//! this is the answer.
use crate::theme::*;
use gpui::{Context, FontWeight, Render, Role, Task, Window, div, prelude::*, px, rgb};
use std::time::Duration;

/// What a notice is about: its selector, icon, tint and heading.
#[derive(Clone, Copy)]
pub(crate) struct NoticeKind {
    pub(crate) id: &'static str,
    pub(crate) close: &'static str,
    pub(crate) icon: &'static str,
    pub(crate) color: u32,
    pub(crate) title: &'static str,
}
impl NoticeKind {
    pub(crate) const DATABASE: Self = Self {
        id: "database-sent",
        close: "database-sent-close",
        icon: "database",
        color: 0x34d399,
        title: "Sent to Database",
    };
    /// A browser grab that reached no composer (context.md K49).
    pub(crate) const BROWSER_GRAB: Self = Self {
        id: "browser-grab",
        close: "browser-grab-close",
        icon: "grab",
        color: 0xfbbf24,
        title: "Element not sent",
    };
    pub(crate) const PROJECT_SLEEP: Self = Self {
        id: "project-slept",
        close: "project-slept-close",
        icon: "moon",
        color: 0xa5b4fc,
        title: "Project asleep",
    };
}

pub(crate) struct SentNotice {
    kind: NoticeKind,
    message: Option<String>,
    clear: Option<Task<()>>,
}
impl Default for SentNotice {
    fn default() -> Self {
        Self::new(NoticeKind::DATABASE)
    }
}

impl SentNotice {
    pub(crate) fn new(kind: NoticeKind) -> Self {
        Self {
            kind,
            message: None,
            clear: None,
        }
    }
    /// Show with a heading of its own ("3 projects asleep").
    pub(crate) fn show_titled(
        &mut self,
        title: &'static str,
        message: String,
        cx: &mut Context<Self>,
    ) {
        self.kind.title = title;
        self.show(message, cx);
    }
    #[cfg(test)]
    pub(crate) fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }
    pub(crate) fn show(&mut self, message: String, cx: &mut Context<Self>) {
        self.message = Some(message);
        self.clear = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(5)).await;
            let _ = this.update(cx, |this, cx| {
                this.message = None;
                cx.notify();
            });
        }));
        cx.notify();
    }
    pub(crate) fn is_visible(&self) -> bool {
        self.message.is_some()
    }
}

impl Render for SentNotice {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(message) = self.message.clone() else {
            return div().into_any_element();
        };
        let kind = self.kind;
        super::surface::toast_frame()
            .id(kind.id)
            .debug_selector(move || kind.id.into())
            .role(Role::Status)
            .child(
                div()
                    .px(px(14.))
                    .py_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(icon(kind.icon, 16.).text_color(rgb(kind.color)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(kind.title),
                            )
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(content(0.7))
                                    .truncate()
                                    .child(message),
                            ),
                    )
                    .child(
                        icon_button(kind.close, "Dismiss", "close").on_click(cx.listener(
                            |this, _, _, cx| {
                                this.message = None;
                                this.clear = None;
                                cx.notify();
                            },
                        )),
                    ),
            )
            .into_any_element()
    }
}
