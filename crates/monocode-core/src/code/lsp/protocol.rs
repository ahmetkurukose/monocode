//! Reading what servers answer. Servers differ in which of the protocol's
//! alternative shapes they use (one location or many, a list or an array, a
//! tree of symbols or a flat run), so everything is read leniently from JSON
//! and turned into the plain values of `types`.
use super::client::uri_to_path;
use super::position::{Encoding, LineIndex, Point, Position, Span};
use super::types::{
    CompletionItem, CompletionKind, CompletionList, Diagnostic, DocumentSymbol, Hover, Location,
    ParameterInfo, ServerId, Severity, SignatureHelp, SignatureInfo, SymbolKind, TextEdit,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

type Indexed = Option<(Arc<str>, LineIndex)>;
/// Finds a file's text: an open document's, else what is on disk.
type Fetch<'a> = Box<dyn FnMut(&Path) -> Option<Arc<str>> + 'a>;

/// Turns protocol positions in any file into points. Counting UTF-16 units
/// needs the line's text, so files are fetched (from open documents, else
/// from disk) once each and only when a position actually needs them.
pub(crate) struct Converter<'a> {
    encoding: Encoding,
    fetch: Fetch<'a>,
    texts: HashMap<PathBuf, Indexed>,
}

impl<'a> Converter<'a> {
    pub(crate) fn new(
        encoding: Encoding,
        fetch: impl FnMut(&Path) -> Option<Arc<str>> + 'a,
    ) -> Self {
        Self {
            encoding,
            fetch: Box::new(fetch),
            texts: HashMap::new(),
        }
    }

    fn indexed(&mut self, path: &Path) -> &Indexed {
        if !self.texts.contains_key(path) {
            let text = (self.fetch)(path).map(|text| {
                let index = LineIndex::new(&text);
                (text, index)
            });
            self.texts.insert(path.to_path_buf(), text);
        }
        &self.texts[path]
    }

    fn point(&mut self, path: &Path, position: &Value) -> Option<Point> {
        let position = Position {
            line: u32::try_from(position.get("line")?.as_u64()?).ok()?,
            character: u32::try_from(position.get("character")?.as_u64()?).unwrap_or(u32::MAX),
        };
        let direct = Point {
            line: position.line,
            column: position.character,
        };
        if self.encoding == Encoding::Utf8 || position.character == 0 {
            return Some(direct);
        }
        let encoding = self.encoding;
        Some(match self.indexed(path) {
            Some((text, index)) => index.point_of(text, position, encoding),
            // An unreadable file: the count is the best there is.
            None => direct,
        })
    }

    pub(crate) fn span(&mut self, path: &Path, range: &Value) -> Option<Span> {
        Some(Span {
            start: self.point(path, range.get("start")?)?,
            end: self.point(path, range.get("end")?)?,
        })
    }

    /// The protocol position of byte `offset` in `path`.
    pub(crate) fn position(&mut self, path: &Path, offset: usize) -> Value {
        let encoding = self.encoding;
        let position = match self.indexed(path) {
            Some((text, index)) => index.position_at(text, offset, encoding),
            None => Position::default(),
        };
        json!({ "line": position.line, "character": position.character })
    }

    fn location(&mut self, uri: &Value, range: &Value) -> Option<Location> {
        let path = uri_to_path(uri.as_str()?)?;
        let range = self.span(&path, range)?;
        Some(Location { path, range })
    }
}

fn text_of(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn list(value: Option<&Value>) -> &[Value] {
    value
        .and_then(Value::as_array)
        .map_or(&[], |items| items.as_slice())
}

/// A string, `MarkupContent` or the older `MarkedString`, as markdown.
fn markup(value: &Value) -> Option<String> {
    let text = match value {
        Value::String(text) => text.clone(),
        Value::Object(fields) => {
            let body = fields.get("value").and_then(Value::as_str)?;
            match fields.get("language").and_then(Value::as_str) {
                // A marked string with a language is a block of code.
                Some(language) if !body.is_empty() => format!("```{language}\n{body}\n```"),
                _ => body.to_string(),
            }
        }
        Value::Array(parts) => parts
            .iter()
            .filter_map(markup)
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => return None,
    };
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

pub(crate) fn diagnostics(
    converter: &mut Converter,
    path: &Path,
    items: &[Value],
    server: &str,
) -> Vec<Diagnostic> {
    let mut found: Vec<Diagnostic> = items
        .iter()
        .filter_map(|item| {
            let tags = list(item.get("tags"));
            let has_tag = |tag: u64| tags.iter().any(|value| value.as_u64() == Some(tag));
            Some(Diagnostic {
                range: converter.span(path, item.get("range")?)?,
                severity: match item.get("severity").and_then(Value::as_u64) {
                    Some(2) => Severity::Warning,
                    Some(3) => Severity::Information,
                    Some(4) => Severity::Hint,
                    _ => Severity::Error,
                },
                message: item
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                code: match item.get("code") {
                    Some(Value::String(code)) => Some(code.clone()),
                    Some(Value::Number(code)) => Some(code.to_string()),
                    _ => None,
                },
                code_href: text_of(
                    item.get("codeDescription")
                        .and_then(|code| code.get("href")),
                ),
                source: text_of(item.get("source")),
                server: server.to_string(),
                unnecessary: has_tag(1),
                deprecated: has_tag(2),
                related: list(item.get("relatedInformation"))
                    .iter()
                    .filter_map(|related| {
                        let location = related.get("location")?;
                        Some((
                            converter.location(location.get("uri")?, location.get("range")?)?,
                            related.get("message")?.as_str()?.to_string(),
                        ))
                    })
                    .collect(),
            })
        })
        .collect();
    found.sort_by_key(|diagnostic| (diagnostic.range, diagnostic.severity));
    found
}

/// `Location`, `Location[]` or `LocationLink[]`, without repeats.
pub(crate) fn locations(converter: &mut Converter, result: &Value) -> Vec<Location> {
    let one;
    let items = match result {
        Value::Array(items) => items.as_slice(),
        Value::Object(_) => {
            one = [result.clone()];
            &one[..]
        }
        _ => &[],
    };
    let mut found: Vec<Location> = Vec::new();
    for item in items {
        let location = match item.get("targetUri") {
            Some(uri) => item
                .get("targetSelectionRange")
                .or_else(|| item.get("targetRange"))
                .and_then(|range| converter.location(uri, range)),
            None => item
                .get("uri")
                .zip(item.get("range"))
                .and_then(|(uri, range)| converter.location(uri, range)),
        };
        if let Some(location) = location {
            if !found.contains(&location) {
                found.push(location);
            }
        }
    }
    found
}

pub(crate) fn hover(converter: &mut Converter, path: &Path, result: &Value) -> Option<Hover> {
    Some(Hover {
        contents: markup(result.get("contents")?)?,
        range: result
            .get("range")
            .and_then(|range| converter.span(path, range)),
    })
}

/// Reduces snippet syntax to the text it would insert untouched: tab stops
/// vanish, placeholders become their default, choices their first option.
pub(crate) fn strip_snippet(snippet: &str) -> String {
    let characters: Vec<char> = snippet.chars().collect();
    let mut out = String::new();
    snippet_text(&characters, &mut out);
    out
}

fn snippet_text(characters: &[char], out: &mut String) {
    let name = |c: char| c.is_ascii_alphanumeric() || c == '_';
    // How many `${1:` placeholders are open. Counted rather than recursed
    // into: the text is a server's, and nesting it deeply must not be able
    // to run this thread out of stack.
    let mut open = 0usize;
    let at = &mut 0usize;
    while let Some(&character) = characters.get(*at) {
        let next = characters.get(*at + 1).copied();
        match character {
            '\\' if matches!(next, Some('$' | '}' | '\\')) => {
                out.extend(next);
                *at += 2;
            }
            '}' if open > 0 => {
                open -= 1;
                *at += 1;
            }
            '$' if next.is_some_and(name) => {
                *at += 1;
                while characters.get(*at).copied().is_some_and(name) {
                    *at += 1;
                }
            }
            '$' if next == Some('{') => {
                let mut end = *at + 2;
                while characters.get(end).copied().is_some_and(name) {
                    end += 1;
                }
                match characters.get(end) {
                    Some('}') => *at = end + 1,
                    Some(':') => {
                        *at = end + 1;
                        open += 1;
                    }
                    Some('|') => {
                        // `${1|one,two|}`: the first choice is what stays.
                        let mut choice = end + 1;
                        let mut first = true;
                        while let Some(&c) = characters.get(choice) {
                            let after = characters.get(choice + 1).copied();
                            if c == '\\' && after.is_some() {
                                if first {
                                    out.extend(after);
                                }
                                choice += 2;
                                continue;
                            }
                            if c == '|' && after == Some('}') {
                                choice += 2;
                                break;
                            }
                            if c == ',' {
                                first = false;
                            } else if first {
                                out.push(c);
                            }
                            choice += 1;
                        }
                        *at = choice;
                    }
                    Some('/') => {
                        // A transform inserts nothing by itself.
                        while characters.get(end).is_some_and(|c| *c != '}') {
                            end += if characters[end] == '\\' { 2 } else { 1 };
                        }
                        *at = end + 1;
                    }
                    _ => {
                        out.push('$');
                        *at += 1;
                    }
                }
            }
            _ => {
                out.push(character);
                *at += 1;
            }
        }
    }
}

pub(crate) fn completion_item(
    converter: &mut Converter,
    path: &Path,
    raw: &Value,
    server: ServerId,
) -> Option<CompletionItem> {
    let label = raw.get("label")?.as_str()?.to_string();
    let edit = raw.get("textEdit").filter(|edit| !edit.is_null());
    let replace = edit.and_then(|edit| {
        // An insert-and-replace edit offers both; inserting is the default.
        let range = edit.get("range").or_else(|| edit.get("insert"))?;
        converter.span(path, range)
    });
    let mut insert_text = edit
        .and_then(|edit| edit.get("newText"))
        .or_else(|| raw.get("insertText"))
        .and_then(Value::as_str)
        .unwrap_or(&label)
        .to_string();
    if raw.get("insertTextFormat").and_then(Value::as_u64) == Some(2) {
        insert_text = strip_snippet(&insert_text);
    }
    let details = raw.get("labelDetails");
    let deprecated = raw.get("deprecated").and_then(Value::as_bool) == Some(true)
        || list(raw.get("tags"))
            .iter()
            .any(|tag| tag.as_u64() == Some(1));
    Some(CompletionItem {
        kind: CompletionKind::from_code(raw.get("kind").and_then(Value::as_u64).unwrap_or(0)),
        detail: text_of(raw.get("detail"))
            .or_else(|| text_of(details.and_then(|details| details.get("description"))))
            .or_else(|| text_of(details.and_then(|details| details.get("detail")))),
        documentation: raw.get("documentation").and_then(markup),
        sort_text: text_of(raw.get("sortText")).unwrap_or_else(|| label.clone()),
        filter_text: text_of(raw.get("filterText")).unwrap_or_else(|| label.clone()),
        insert_text,
        replace,
        additional_edits: list(raw.get("additionalTextEdits"))
            .iter()
            .filter_map(|edit| {
                Some(TextEdit {
                    range: converter.span(path, edit.get("range")?)?,
                    text: edit.get("newText")?.as_str()?.to_string(),
                })
            })
            .collect(),
        deprecated,
        preselect: raw.get("preselect").and_then(Value::as_bool) == Some(true),
        label,
        server,
        raw: raw.clone(),
    })
}

/// A `CompletionList` or a bare array of items.
pub(crate) fn completions(
    converter: &mut Converter,
    path: &Path,
    result: &Value,
    server: ServerId,
) -> CompletionList {
    let (items, is_incomplete) = match result {
        Value::Array(items) => (items.as_slice(), false),
        Value::Object(fields) => (
            list(fields.get("items")),
            fields.get("isIncomplete").and_then(Value::as_bool) == Some(true),
        ),
        _ => (&[][..], false),
    };
    CompletionList {
        items: items
            .iter()
            .filter_map(|item| completion_item(converter, path, item, server))
            .collect(),
        is_incomplete,
    }
}

/// The bytes of `label` that `units` code units in `encoding` cover.
fn units_to_bytes(label: &str, units: u64, encoding: Encoding) -> usize {
    let index = LineIndex::new(label);
    let position = Position {
        line: 0,
        character: u32::try_from(units).unwrap_or(u32::MAX),
    };
    index.point_of(label, position, encoding).column as usize
}

pub(crate) fn signature_help(result: &Value, encoding: Encoding) -> Option<SignatureHelp> {
    let shared_active = result.get("activeParameter").and_then(Value::as_u64);
    let signatures: Vec<SignatureInfo> = list(result.get("signatures"))
        .iter()
        .filter_map(|signature| {
            let label = signature.get("label")?.as_str()?.to_string();
            let mut search_from = 0;
            let parameters: Vec<ParameterInfo> = list(signature.get("parameters"))
                .iter()
                .filter_map(|parameter| {
                    let (text, range) = match parameter.get("label")? {
                        Value::String(text) => {
                            // Named by its text: the first match after the
                            // previous parameter is the one meant.
                            let range = label[search_from..].find(text.as_str()).map(|found| {
                                let start = search_from + found;
                                (start, start + text.len())
                            });
                            (text.clone(), range)
                        }
                        Value::Array(ends) => {
                            let start = units_to_bytes(&label, ends.first()?.as_u64()?, encoding);
                            let end = units_to_bytes(&label, ends.get(1)?.as_u64()?, encoding);
                            let end = end.max(start);
                            (label[start..end].to_string(), Some((start, end)))
                        }
                        _ => return None,
                    };
                    if let Some((_, end)) = range {
                        search_from = end;
                    }
                    Some(ParameterInfo {
                        label: text,
                        label_range: range,
                        documentation: parameter.get("documentation").and_then(markup),
                    })
                })
                .collect();
            let active = signature
                .get("activeParameter")
                .and_then(Value::as_u64)
                .or(shared_active)
                .and_then(|active| usize::try_from(active).ok())
                .filter(|active| *active < parameters.len());
            Some(SignatureInfo {
                documentation: signature.get("documentation").and_then(markup),
                parameters,
                active_parameter: active,
                label,
            })
        })
        .collect();
    if signatures.is_empty() {
        return None;
    }
    let active_signature = result
        .get("activeSignature")
        .and_then(Value::as_u64)
        .and_then(|active| usize::try_from(active).ok())
        .filter(|active| *active < signatures.len())
        .unwrap_or(0);
    Some(SignatureHelp {
        signatures,
        active_signature,
    })
}

fn symbol(converter: &mut Converter, path: &Path, raw: &Value) -> Option<DocumentSymbol> {
    let name = raw.get("name")?.as_str()?.to_string();
    let kind = SymbolKind::from_code(raw.get("kind").and_then(Value::as_u64).unwrap_or(0));
    // A flat `SymbolInformation` carries a location instead of two ranges.
    let (range, selection_range) = match raw.get("location") {
        Some(location) => {
            let range = converter.span(path, location.get("range")?)?;
            (range, range)
        }
        None => {
            let range = converter.span(path, raw.get("range")?)?;
            let selection = raw
                .get("selectionRange")
                .and_then(|range| converter.span(path, range))
                .unwrap_or(range);
            (range, selection)
        }
    };
    Some(DocumentSymbol {
        name,
        detail: text_of(raw.get("detail")),
        kind,
        range,
        selection_range,
        container: text_of(raw.get("containerName")),
        children: list(raw.get("children"))
            .iter()
            .filter_map(|child| symbol(converter, path, child))
            .collect(),
    })
}

/// `DocumentSymbol[]` (a tree) or `SymbolInformation[]` (flat).
pub(crate) fn document_symbols(
    converter: &mut Converter,
    path: &Path,
    result: &Value,
) -> Vec<DocumentSymbol> {
    list(Some(result))
        .iter()
        .filter_map(|raw| symbol(converter, path, raw))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(l1: u32, c1: u32, l2: u32, c2: u32) -> Value {
        json!({ "start": { "line": l1, "character": c1 }, "end": { "line": l2, "character": c2 } })
    }

    fn span(l1: u32, c1: u32, l2: u32, c2: u32) -> Span {
        Span {
            start: Point {
                line: l1,
                column: c1,
            },
            end: Point {
                line: l2,
                column: c2,
            },
        }
    }

    /// A converter over two in-memory files, counting UTF-16.
    fn converter() -> Converter<'static> {
        Converter::new(Encoding::Utf16, |path: &Path| match path.to_str()? {
            "/w/a.py" => Some(Arc::from("x = '😀' + y\nz = 1\n")),
            "/w/b.py" => Some(Arc::from("def ş(): pass\n")),
            _ => None,
        })
    }

    #[test]
    fn positions_are_converted_per_file() {
        let mut converter = converter();
        let a = Path::new("/w/a.py");
        // UTF-16 character 10 on line 0 is after the two-unit emoji: byte 12.
        assert_eq!(
            converter.span(a, &range(0, 10, 1, 1)),
            Some(span(0, 12, 1, 1))
        );
        assert_eq!(
            converter.position(a, 12),
            json!({ "line": 0, "character": 10 })
        );
        // A file that cannot be read keeps the server's numbers.
        assert_eq!(
            converter.span(Path::new("/w/gone.py"), &range(0, 5, 0, 6)),
            Some(span(0, 5, 0, 6))
        );
        let mut direct = Converter::new(Encoding::Utf8, |_: &Path| -> Option<Arc<str>> {
            panic!("UTF-8 positions need no text")
        });
        assert_eq!(
            direct.span(a, &range(0, 12, 0, 13)),
            Some(span(0, 12, 0, 13))
        );
    }

    #[test]
    fn diagnostics_are_read_and_sorted() {
        let mut converter = converter();
        let items = [
            json!({
                "range": range(1, 0, 1, 1), "severity": 2, "message": "unused", "code": "F841",
                "source": "Ruff", "tags": [1],
                "codeDescription": { "href": "https://docs.astral.sh/ruff/rules/f841" },
            }),
            json!({
                "range": range(0, 10, 0, 11), "message": "bad type", "code": 7,
                "relatedInformation": [{
                    "location": { "uri": "file:///w/b.py", "range": range(0, 4, 0, 5) },
                    "message": "declared here",
                }],
            }),
            json!({ "message": "no range" }),
        ];
        let found = diagnostics(&mut converter, Path::new("/w/a.py"), &items, "ruff");
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].message, "bad type");
        assert_eq!(found[0].severity, Severity::Error);
        assert_eq!(found[0].code.as_deref(), Some("7"));
        assert_eq!(found[0].range, span(0, 12, 0, 13));
        assert_eq!(
            found[0].related,
            vec![(
                Location {
                    path: "/w/b.py".into(),
                    range: span(0, 4, 0, 6)
                },
                "declared here".to_string()
            )]
        );
        assert_eq!(found[1].severity, Severity::Warning);
        assert!(found[1].unnecessary && !found[1].deprecated);
        assert_eq!(found[1].source.as_deref(), Some("Ruff"));
        assert_eq!(found[1].server, "ruff");
        assert!(found[1].code_href.as_deref().unwrap().ends_with("f841"));
    }

    #[test]
    fn locations_come_in_three_shapes() {
        let mut converter = converter();
        let location = json!({ "uri": "file:///w/b.py", "range": range(0, 4, 0, 5) });
        let expected = vec![Location {
            path: "/w/b.py".into(),
            range: span(0, 4, 0, 6),
        }];
        assert_eq!(locations(&mut converter, &location), expected);
        assert_eq!(
            locations(&mut converter, &json!([location, location])),
            expected
        );
        let link = json!([{
            "originSelectionRange": range(0, 0, 0, 1),
            "targetUri": "file:///w/b.py",
            "targetRange": range(0, 0, 0, 13),
            "targetSelectionRange": range(0, 4, 0, 5),
        }]);
        assert_eq!(locations(&mut converter, &link), expected);
        assert_eq!(locations(&mut converter, &Value::Null), vec![]);
        assert_eq!(
            locations(
                &mut converter,
                &json!([{ "uri": "untitled:x", "range": range(0, 0, 0, 0) }])
            ),
            vec![]
        );
    }

    #[test]
    fn hover_contents_flatten_to_markdown() {
        let mut converter = converter();
        let a = Path::new("/w/a.py");
        let read = |converter: &mut Converter, contents: Value| {
            hover(converter, a, &json!({ "contents": contents })).map(|hover| hover.contents)
        };
        assert_eq!(
            read(
                &mut converter,
                json!({ "kind": "markdown", "value": "**x**: int\n" })
            ),
            Some("**x**: int".into())
        );
        assert_eq!(read(&mut converter, json!("plain")), Some("plain".into()));
        assert_eq!(
            read(
                &mut converter,
                json!([{ "language": "python", "value": "x: int" }, "A number."])
            ),
            Some("```python\nx: int\n```\n\nA number.".into())
        );
        assert_eq!(read(&mut converter, json!("")), None);
        assert_eq!(read(&mut converter, json!([])), None);
        assert_eq!(hover(&mut converter, a, &Value::Null), None);
        let ranged = hover(
            &mut converter,
            a,
            &json!({ "contents": "x", "range": range(1, 0, 1, 1) }),
        );
        assert_eq!(ranged.unwrap().range, Some(span(1, 0, 1, 1)));
    }

    #[test]
    fn snippets_become_plain_text() {
        assert_eq!(strip_snippet("print($1)$0"), "print()");
        assert_eq!(
            strip_snippet("for ${1:item} in ${2:items}:"),
            "for item in items:"
        );
        assert_eq!(strip_snippet("${1:a ${2:b} c}!"), "a b c!");
        assert_eq!(strip_snippet("${1|one,two,three|} x"), "one x");
        assert_eq!(strip_snippet("cost: \\$5 \\} \\\\"), "cost: $5 } \\");
        assert_eq!(
            strip_snippet("$TM_FILENAME|${TM_SELECTED_TEXT:none}|${3}"),
            "|none|"
        );
        assert_eq!(strip_snippet("${1/(.*)/${1:/upcase}/}done"), "/}done");
        assert_eq!(strip_snippet("a $ b } ${"), "a $ b } ${");
        assert_eq!(strip_snippet("plain"), "plain");
        // Nesting far deeper than a stack of calls could follow.
        let depth = 200_000;
        let deep = format!("{}x{}", "${1:".repeat(depth), "}".repeat(depth));
        assert_eq!(strip_snippet(&deep), "x");
        assert_eq!(strip_snippet(&"${1:".repeat(depth)), "");
    }

    #[test]
    fn completion_lists_and_arrays() {
        let mut converter = converter();
        let a = Path::new("/w/a.py");
        let server = ServerId(4);
        let result = json!({
            "isIncomplete": true,
            "items": [
                {
                    "label": "append", "kind": 2, "detail": "(x) -> None",
                    "sortText": "09.append", "filterText": "appendx",
                    "documentation": { "kind": "markdown", "value": "Adds." },
                    "insertTextFormat": 2,
                    "textEdit": { "range": range(1, 0, 1, 1), "newText": "append(${1:x})$0" },
                    "additionalTextEdits": [{ "range": range(0, 0, 0, 0), "newText": "import a\n" }],
                    "preselect": true, "tags": [1], "data": { "n": 1 },
                },
                {
                    "label": "clear", "insertText": "clear()",
                    "labelDetails": { "description": "list" },
                    "textEdit": {
                        "insert": range(0, 10, 0, 10), "replace": range(0, 10, 0, 11),
                        "newText": "clear",
                    },
                },
                { "label": "bare" },
                { "kind": 3 },
            ],
        });
        let list = completions(&mut converter, a, &result, server);
        assert!(list.is_incomplete);
        assert_eq!(list.items.len(), 3);
        let append = &list.items[0];
        assert_eq!(append.kind, CompletionKind::Method);
        assert_eq!(append.insert_text, "append(x)");
        assert_eq!(append.replace, Some(span(1, 0, 1, 1)));
        assert_eq!(append.sort_text, "09.append");
        assert_eq!(append.filter_text, "appendx");
        assert_eq!(append.documentation.as_deref(), Some("Adds."));
        assert_eq!(append.additional_edits[0].text, "import a\n");
        assert!(append.preselect && append.deprecated);
        assert_eq!(append.raw["data"]["n"], 1);
        assert_eq!(append.server, server);
        let clear = &list.items[1];
        assert_eq!(clear.insert_text, "clear");
        assert_eq!(clear.replace, Some(span(0, 12, 0, 12)));
        assert_eq!(clear.detail.as_deref(), Some("list"));
        let bare = &list.items[2];
        assert_eq!(
            (
                bare.insert_text.as_str(),
                bare.sort_text.as_str(),
                bare.filter_text.as_str()
            ),
            ("bare", "bare", "bare")
        );
        assert_eq!(bare.kind, CompletionKind::Other);
        assert_eq!(bare.replace, None);

        let array = completions(&mut converter, a, &json!([{ "label": "x" }]), server);
        assert!(!array.is_incomplete);
        assert_eq!(array.items.len(), 1);
        assert_eq!(
            completions(&mut converter, a, &Value::Null, server)
                .items
                .len(),
            0
        );
    }

    #[test]
    fn signature_help_finds_its_parameters() {
        let result = json!({
            "signatures": [
                {
                    "label": "f(a: int, a2: str) -> None",
                    "documentation": "Does f.",
                    "parameters": [
                        { "label": "a: int", "documentation": { "kind": "markdown", "value": "first" } },
                        { "label": "a2: str" },
                    ],
                    "activeParameter": 1,
                },
                {
                    "label": "ş(😀x, y)",
                    "parameters": [{ "label": [2, 5] }, { "label": [7, 8] }],
                },
            ],
            "activeSignature": 1,
            "activeParameter": 0,
        });
        let help = signature_help(&result, Encoding::Utf16).unwrap();
        assert_eq!(help.active_signature, 1);
        let first = &help.signatures[0];
        assert_eq!(first.active_parameter, Some(1));
        assert_eq!(first.parameters[0].label_range, Some((2, 8)));
        assert_eq!(first.parameters[1].label_range, Some((10, 17)));
        assert_eq!(first.parameters[0].documentation.as_deref(), Some("first"));
        assert_eq!(first.documentation.as_deref(), Some("Does f."));
        let second = &help.signatures[1];
        assert_eq!(second.active_parameter, Some(0));
        assert_eq!(second.parameters[0].label, "😀x");
        assert_eq!(second.parameters[1].label, "y");
        let range = second.parameters[1].label_range.unwrap();
        assert_eq!(&second.label[range.0..range.1], "y");

        assert_eq!(signature_help(&Value::Null, Encoding::Utf16), None);
        assert_eq!(
            signature_help(&json!({ "signatures": [] }), Encoding::Utf16),
            None
        );
        let out_of_range = json!({
            "signatures": [{ "label": "g()", "parameters": [] }],
            "activeSignature": 5, "activeParameter": 0,
        });
        let help = signature_help(&out_of_range, Encoding::Utf16).unwrap();
        assert_eq!(help.active_signature, 0);
        assert_eq!(help.signatures[0].active_parameter, None);
    }

    #[test]
    fn symbols_as_a_tree_or_flat() {
        let mut converter = converter();
        let a = Path::new("/w/a.py");
        let tree = json!([{
            "name": "Thing", "kind": 5, "detail": "class",
            "range": range(0, 0, 1, 5), "selectionRange": range(0, 6, 0, 11),
            "children": [{
                "name": "run", "kind": 6,
                "range": range(1, 0, 1, 5), "selectionRange": range(1, 0, 1, 3),
            }],
        }]);
        let symbols = document_symbols(&mut converter, a, &tree);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].kind, SymbolKind::Class);
        assert_eq!(symbols[0].detail.as_deref(), Some("class"));
        assert_eq!(symbols[0].children[0].name, "run");
        assert_eq!(symbols[0].children[0].kind, SymbolKind::Method);
        assert_eq!(symbols[0].children[0].selection_range, span(1, 0, 1, 3));

        let flat = json!([{
            "name": "run", "kind": 12, "containerName": "Thing",
            "location": { "uri": "file:///w/a.py", "range": range(1, 0, 1, 5) },
        }]);
        let symbols = document_symbols(&mut converter, a, &flat);
        assert_eq!(symbols[0].kind, SymbolKind::Function);
        assert_eq!(symbols[0].container.as_deref(), Some("Thing"));
        assert_eq!(symbols[0].range, symbols[0].selection_range);
        assert!(document_symbols(&mut converter, a, &Value::Null).is_empty());
    }
}
