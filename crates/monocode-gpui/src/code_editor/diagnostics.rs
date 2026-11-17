//! Diagnostics that come from outside the editor (a type checker, a linter).
//! The editor paints them with the squiggles and scrollbar marks of its own
//! syntax errors and moves them through edits until their source replaces them.
use crate::editor_analysis::Edit;
use gpui::{Hsla, SharedString, rgb};
use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Severity {
    Hint,
    Info,
    Warning,
    Error,
}
impl Severity {
    /// The editor's own diagnostics name their severity in text.
    pub(crate) fn parse(name: &str) -> Self {
        match name {
            "error" => Self::Error,
            "warning" => Self::Warning,
            "info" => Self::Info,
            _ => Self::Hint,
        }
    }
    pub(crate) fn color(self) -> Hsla {
        match self {
            Self::Error => rgb(0xf87171).into(),
            Self::Warning => rgb(0xfbbf24).into(),
            Self::Info => rgb(0x60a5fa).into(),
            Self::Hint => crate::theme::content(0.48),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ExternalDiagnostic {
    /// Bytes of the text the diagnostic was computed for.
    pub range: Range<usize>,
    pub severity: Severity,
    pub message: SharedString,
    /// Who reported it, such as "basedpyright".
    pub source: SharedString,
    /// What the marker hover shows, built once so painting only clones it.
    label: SharedString,
}
impl ExternalDiagnostic {
    pub(crate) fn new(
        range: Range<usize>,
        severity: Severity,
        message: impl Into<SharedString>,
        source: impl Into<SharedString>,
    ) -> Self {
        let (message, source) = (message.into(), source.into());
        let label = if source.is_empty() {
            message.clone()
        } else {
            format!("{source}: {message}").into()
        };
        Self {
            range,
            severity,
            message,
            source,
            label,
        }
    }
    pub(crate) fn label(&self) -> &SharedString {
        &self.label
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DiagnosticCounts {
    pub errors: usize,
    pub warnings: usize,
    pub infos: usize,
    pub hints: usize,
}
impl DiagnosticCounts {
    pub(crate) fn add(&mut self, severity: Severity) {
        match severity {
            Severity::Error => self.errors += 1,
            Severity::Warning => self.warnings += 1,
            Severity::Info => self.infos += 1,
            Severity::Hint => self.hints += 1,
        }
    }
    pub(crate) fn total(&self) -> usize {
        self.errors + self.warnings + self.infos + self.hints
    }
}

/// Moves diagnostics through an edit. One whose whole range was replaced is
/// dropped; the server reports it again if it still holds.
pub(crate) fn remap(items: &mut Vec<ExternalDiagnostic>, edit: Edit) {
    items.retain_mut(|item| {
        // A point diagnostic (end of file, a missing token) has no width to
        // trim, so it follows the text on its left.
        if item.range.is_empty() {
            let at = item.range.start;
            let at = if at <= edit.start {
                at
            } else if at >= edit.old_end {
                at - edit.old_end + edit.new_end
            } else {
                edit.start
            };
            item.range = at..at;
            return true;
        }
        match edit.map(item.range.start, item.range.end) {
            Some((from, to)) => {
                item.range = from..to;
                true
            }
            None => false,
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_follow_edits_and_vanish_with_their_text() {
        let old = "import os\nvalue = undefined_name\n";
        let at = old.find("undefined_name").unwrap();
        let mut items = vec![
            ExternalDiagnostic::new(at..at + 14, Severity::Error, "not defined", "basedpyright"),
            ExternalDiagnostic::new(7..9, Severity::Warning, "unused import", "ruff"),
        ];
        let new = "import os\n\n\nvalue = undefined_name\n";
        remap(&mut items, Edit::between(old, new).unwrap());
        assert_eq!(&new[items[0].range.clone()], "undefined_name");
        assert_eq!(&new[items[1].range.clone()], "os");
        // Text typed over a diagnostic keeps it until its source reports
        // again; removing the whole line removes it.
        let gone = "import os\n\n\n";
        remap(&mut items, Edit::between(new, gone).unwrap());
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label().as_ref(), "ruff: unused import");
    }
}
