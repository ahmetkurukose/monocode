//! Markdown for the editor's popovers (hover text, completion and signature
//! documentation), drawn with the application's own markdown rows and the
//! surface the completion list uses.
use crate::{
    markdown::{self, MarkdownRow, Part, Tone},
    markdown_model,
    theme::content,
};
use gpui::{AnyElement, Div, SharedString, div, prelude::*, px};
use std::{rc::Rc, time::Instant};

/// Parsed once when a popover opens; drawing it only walks the rows.
#[derive(Clone)]
pub(crate) struct Document {
    rows: Rc<[MarkdownRow]>,
    /// The source, which tests and accessibility read.
    pub text: SharedString,
}
impl Document {
    pub(crate) fn new(text: &str) -> Self {
        let mut rows: Vec<MarkdownRow> = markdown_model::parse_full(text)
            .blocks
            .iter()
            .flat_map(|block| markdown::rows(&block.block))
            .collect();
        if let Some(first) = rows.first_mut() {
            first.gap = 0.;
        }
        Self {
            rows: rows.into(),
            text: text.to_owned().into(),
        }
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
    pub(crate) fn render(&self, id: &'static str) -> AnyElement {
        div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .text_size(px(12.))
            .children(self.rows.iter().enumerate().map(|(ix, row)| {
                let row_id: SharedString = format!("{id}-{ix}").into();
                let body: AnyElement = match &row.part {
                    Part::Code { fence, lines } => div()
                        .id(row_id)
                        .overflow_x_scroll()
                        .rounded_md()
                        .bg(content(0.05))
                        .py_1()
                        .child(markdown::code_lines(fence, lines.clone(), None, "", false))
                        .into_any_element(),
                    _ => markdown::body(row, row_id, Tone::Normal, None, Instant::now()),
                };
                div()
                    .pt(px(row.gap.min(8.)))
                    .flex()
                    .min_w_0()
                    .gap_2()
                    .pl(px(row.indent as f32 * 12.))
                    .when_some(row.marker.clone(), |d, marker| {
                        d.child(div().flex_shrink_0().text_color(content(0.6)).child(marker))
                    })
                    .child(div().flex_1().min_w_0().child(body))
            }))
            .into_any_element()
    }
}

/// The frame of every editor popover, matching the completion list.
pub(crate) fn surface() -> Div {
    div()
        .rounded(px(6.))
        .border_1()
        .border_color(content(0.12))
        .bg(crate::theme::shell_fill().opacity(1.))
        .shadow_md()
        .text_size(px(12.))
        .text_color(content(1.))
        .font_family(crate::fonts::UI)
}
