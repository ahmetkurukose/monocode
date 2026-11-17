//! From the language server core's values to the editor's: lines and byte
//! columns become byte ranges of one text, and diagnostics, completion items
//! and signatures take the shapes the editor's extension points draw.
use crate::code_editor::{self, ExternalDiagnostic, Severity, TextEdit};
use monocode_core::code::lsp::{self, CompletionKind, LineIndex, Span};
use std::{ops::Range, sync::Arc};

/// The bytes of `text` a span covers. `index` must have been built from `text`.
pub(super) fn range(index: &LineIndex, text: &str, span: Span) -> Range<usize> {
    let start = index.offset_of(text, span.start).min(text.len());
    let end = index.offset_of(text, span.end).min(text.len());
    start..end.max(start)
}

pub(super) fn severity(severity: lsp::Severity) -> Severity {
    match severity {
        lsp::Severity::Error => Severity::Error,
        lsp::Severity::Warning => Severity::Warning,
        lsp::Severity::Information => Severity::Info,
        lsp::Severity::Hint => Severity::Hint,
    }
}

/// A diagnostic placed on `text`, named by the server it came from and
/// carrying its rule code, which is what one searches for.
pub(super) fn diagnostic(
    found: &lsp::Diagnostic,
    index: &LineIndex,
    text: &str,
) -> ExternalDiagnostic {
    let message = match found.code.as_deref() {
        Some(code) if !code.is_empty() => format!("{} ({code})", found.message),
        _ => found.message.clone(),
    };
    ExternalDiagnostic::new(
        range(index, text, found.range),
        severity(found.severity),
        message,
        found.server.clone(),
    )
}

/// The editor's name for a completion's kind, which picks its mark.
fn completion_kind(kind: CompletionKind) -> &'static str {
    use CompletionKind::*;
    match kind {
        Method => "method",
        Function | Constructor => "function",
        Class | Interface | Struct | Enum | TypeParameter => "class",
        Variable | Value | Reference => "variable",
        Keyword => "keyword",
        Module | File | Folder => "module",
        Property => "property",
        Field | EnumMember => "field",
        Constant => "constant",
        _ => "",
    }
}

/// Where the identifier being typed at `offset` starts.
fn word_start(text: &str, offset: usize) -> usize {
    text[..offset]
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_alphanumeric() || *c == '_')
        .last()
        .map_or(offset, |(at, _)| at)
}

/// A completion item for the request made at `offset` of `text`. The core's
/// item rides along so its documentation can be asked for later.
pub(super) fn completion(
    item: lsp::CompletionItem,
    index: &LineIndex,
    text: &str,
    offset: usize,
) -> code_editor::CompletionItem {
    let replace = match item.replace {
        Some(span) => range(index, text, span),
        None => word_start(text, offset)..offset,
    };
    let additional_edits = item
        .additional_edits
        .iter()
        .map(|edit| TextEdit {
            range: range(index, text, edit.range),
            text: edit.text.clone(),
        })
        .collect();
    code_editor::CompletionItem {
        label: item.label.clone(),
        kind: completion_kind(item.kind).into(),
        detail: item.detail.clone().unwrap_or_default(),
        documentation: item.documentation.clone(),
        insert: item.insert_text.clone(),
        replace,
        cursor: None,
        filter: Some(item.filter_text.clone()),
        sort: Some(item.sort_text.clone()),
        additional_edits,
        data: Some(Arc::new(item)),
    }
}

/// The signature being typed, with each parameter's place in its label.
pub(super) fn signature(help: lsp::SignatureHelp) -> Option<code_editor::Signature> {
    let at = help
        .active_signature
        .min(help.signatures.len().checked_sub(1)?);
    let found = help.signatures.into_iter().nth(at)?;
    let mut searched = 0;
    let parameters = found
        .parameters
        .iter()
        .map(|parameter| {
            let range = match parameter.label_range {
                Some((start, end)) => start..end,
                // Servers that name the parameter instead of placing it
                // list them in order, so each is looked for after the last.
                None => match found.label[searched..].find(&parameter.label) {
                    Some(at) if !parameter.label.is_empty() => {
                        searched + at..searched + at + parameter.label.len()
                    }
                    _ => 0..0,
                },
            };
            searched = range.end.clamp(searched, found.label.len());
            range
        })
        .collect();
    Some(code_editor::Signature {
        label: found.label,
        parameters,
        active: found.active_parameter,
        documentation: found.documentation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use monocode_core::code::lsp::{ParameterInfo, Point, SignatureInfo};

    #[test]
    fn spans_become_byte_ranges_of_the_text_they_were_reported_for() {
        let text = "x = 1\r\nnaïve = 'é🙂'\nlast";
        let index = LineIndex::new(text);
        let at = text.find("'é").unwrap();
        let line = text.find("naïve").unwrap();
        let span = Span {
            start: Point {
                line: 1,
                column: (at - line) as u32,
            },
            end: Point {
                line: 1,
                column: (at - line + "'é🙂'".len()) as u32,
            },
        };
        assert_eq!(&text[range(&index, text, span)], "'é🙂'");
        // A span past the end of the text stays inside it.
        let past = Span {
            start: Point { line: 9, column: 0 },
            end: Point { line: 9, column: 4 },
        };
        let clamped = range(&index, text, past);
        assert!(clamped.end <= text.len() && clamped.start <= clamped.end);
        assert_eq!(word_start(text, line + "naïve".len()), line);
    }

    #[test]
    fn a_signature_places_parameters_that_come_as_names() {
        let parameter = |label: &str, range| ParameterInfo {
            label: label.into(),
            label_range: range,
            documentation: None,
        };
        let help = lsp::SignatureHelp {
            signatures: vec![SignatureInfo {
                label: "connect(host: str, port: int = 5432) -> None".into(),
                documentation: Some("Opens a connection.".into()),
                parameters: vec![
                    parameter("host: str", None),
                    parameter("port: int = 5432", Some((19, 35))),
                ],
                active_parameter: Some(1),
            }],
            active_signature: 4,
        };
        let shown = signature(help).unwrap();
        assert_eq!(&shown.label[shown.parameters[0].clone()], "host: str");
        assert_eq!(
            &shown.label[shown.parameters[1].clone()],
            "port: int = 5432"
        );
        assert_eq!(shown.active, Some(1));
        assert!(
            signature(lsp::SignatureHelp {
                signatures: Vec::new(),
                active_signature: 0
            })
            .is_none()
        );
    }
}
