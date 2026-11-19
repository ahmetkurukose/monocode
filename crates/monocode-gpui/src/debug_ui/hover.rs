//! Hover while a program is paused. The provider wraps whichever hover
//! provider is already installed on a file (code insight's): when the name
//! under the mouse evaluates in the selected frame of the paused session,
//! its value is shown first and the wrapped provider's text below it;
//! otherwise the wrapped provider answers alone.
use super::{breakpoints, sessions, values};
use crate::code_editor::{self, Hover, HoverProvider, Position};
use gpui::{App, Task};
use monocode_core::code::dap::{EvaluateContext, Session};
use std::{ops::Range, rc::Rc, sync::Arc};

/// A value longer than this is cut in the popover; the variables column has it whole.
const VALUE_LENGTH: usize = 2000;

pub(super) fn init(cx: &mut App) {
    code_editor::register(cx, |doc, providers, _| {
        if breakpoints::serves(doc) {
            providers.hover = Some(Rc::new(DebugHover {
                inner: providers.hover.take(),
            }));
        }
    });
}

pub(super) struct DebugHover {
    pub(super) inner: Option<Rc<dyn HoverProvider>>,
}

/// What to evaluate for a hover at `at`: only in the file of the selected
/// frame of a paused session, where the frame's names mean something.
fn target(at: &Position, cx: &App) -> Option<(Arc<Session>, i64, Range<usize>)> {
    let model = sessions::current(cx)?;
    let model = model.read(cx);
    let (path, _) = model.selected_location()?;
    if path != at.doc.path {
        return None;
    }
    let range = values::expression_at(&at.text, at.offset)?;
    Some((model.selected(), model.selected_frame_id()?, range))
}

impl HoverProvider for DebugHover {
    fn hover(&self, at: Position, cx: &mut App) -> Task<Option<Hover>> {
        let found = target(&at, cx);
        let text = at.text.clone();
        let wrapped = self.inner.as_ref().map(|inner| inner.hover(at, cx));
        let Some((session, frame, range)) = found else {
            return wrapped.unwrap_or_else(|| Task::ready(None));
        };
        let expression = text[range.clone()].to_owned();
        let asked = expression.clone();
        let value = cx.background_executor().spawn(async move {
            session
                .evaluate(&asked, Some(frame), EvaluateContext::Hover)
                .ok()
        });
        cx.spawn(async move |_| {
            let value = value.await;
            let below = match wrapped {
                Some(wrapped) => wrapped.await,
                None => None,
            };
            let Some(value) = value else {
                return below;
            };
            let mut shown: String = value.result.chars().take(VALUE_LENGTH).collect();
            if shown.len() < value.result.len() {
                shown.push('…');
            }
            let mut markdown = format!("```python\n{expression} = {shown}\n```");
            if let Some(type_name) = value.type_name.filter(|name| !name.is_empty()) {
                markdown.push_str(&format!("\n`{type_name}`, while paused"));
            }
            if let Some(below) = below.filter(|below| !below.markdown.trim().is_empty()) {
                markdown.push_str("\n\n---\n\n");
                markdown.push_str(&below.markdown);
            }
            Some(Hover {
                range: Some(range),
                markdown,
            })
        })
    }
}
