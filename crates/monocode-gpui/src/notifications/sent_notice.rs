//! A quiet confirmation that text reached a data source's console. It clears
//! itself: sending never moves the user off the file, so this is the answer.
use crate::theme::*;
use gpui::{Context, FontWeight, Render, Role, Task, Window, div, prelude::*, px, rgb};
use std::time::Duration;

#[derive(Default)]
pub(crate) struct SentNotice {
    message: Option<String>,
    clear: Option<Task<()>>,
}

impl SentNotice {
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
        super::surface::toast_frame()
            .id("database-sent")
            .debug_selector(|| "database-sent".into())
            .role(Role::Status)
            .child(
                div()
                    .px(px(14.))
                    .py_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(icon("database", 16.).text_color(rgb(0x34d399)))
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
                                    .child("Sent to Database"),
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
                        icon_button("database-sent-close", "Dismiss", "close").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.message = None;
                                this.clear = None;
                                cx.notify();
                            }),
                        ),
                    ),
            )
            .into_any_element()
    }
}
