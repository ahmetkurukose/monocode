//! Values of the paused program shown in the editor's text: the short notes
//! after the lines of the stopped function that name a local, and the
//! expression under the mouse to evaluate for a hover. Both work on the text
//! alone, cheaply; neither asks a language server.
use crate::code_editor::{InlineNote, NoteTone, links};
use monocode_core::code::dap::Variable;
use std::ops::Range;

/// How far above the stop a function's first line is looked for.
const REACH: usize = 80;
const VALUE_LENGTH: usize = 48;
const PER_LINE: usize = 3;

const KEYWORDS: &[&str] = &[
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue",
    "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import",
    "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while",
    "with", "yield",
];

fn identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_alphabetic() || first == '_')
        && chars.all(|c| c.is_alphanumeric() || c == '_')
}

fn indent(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// The names a line of code mentions, in order, without attributes
/// (`self.count` names `self`) and without what follows a `#`.
fn names(line: &str) -> Vec<&str> {
    let code = line.split('#').next().unwrap_or("");
    let mut found = Vec::new();
    let mut start = None;
    for (at, c) in code.char_indices().chain([(code.len(), ' ')]) {
        let word = c.is_alphanumeric() || c == '_';
        match (start, word) {
            (None, true) => start = Some(at),
            (Some(from), false) => {
                let name = &code[from..at];
                let attribute = code[..from].ends_with('.');
                if identifier(name) && !attribute && !found.contains(&name) {
                    found.push(name);
                }
                start = None;
            }
            _ => {}
        }
    }
    found
}

fn shorten(value: &str) -> String {
    let value = value.replace('\n', " ");
    if value.chars().count() <= VALUE_LENGTH {
        return value;
    }
    let cut: String = value.chars().take(VALUE_LENGTH - 1).collect();
    format!("{cut}…")
}

/// Notes for the lines of the function stopped at `line` (1-based), from
/// its `def` down to the stop: each line gets the values of the locals it
/// names. Lines below the stop have not run, so they get none.
pub(super) fn inline_notes(text: &str, line: u32, locals: &[Variable]) -> Vec<InlineNote> {
    let lines: Vec<&str> = text.split('\n').collect();
    let stop = (line.saturating_sub(1) as usize).min(lines.len().saturating_sub(1));
    let Some(stop_line) = lines.get(stop) else {
        return Vec::new();
    };
    let depth = indent(stop_line);
    let first = (stop.saturating_sub(REACH)..stop)
        .rev()
        .find(|row| {
            let code = lines[*row].trim_start();
            (code.starts_with("def ") || code.starts_with("async def "))
                && indent(lines[*row]) < depth
        })
        .unwrap_or(if depth == 0 {
            stop.saturating_sub(REACH / 4)
        } else {
            stop
        });
    let values: Vec<(&str, String)> = (locals.iter())
        .filter(|local| identifier(&local.name) && !local.name.starts_with("__"))
        .map(|local| (local.name.as_str(), shorten(&local.value)))
        .collect();
    let mut notes = Vec::new();
    for (row, code) in lines.iter().enumerate().take(stop + 1).skip(first) {
        let shown: Vec<String> = (names(code).into_iter())
            .filter(|name| !KEYWORDS.contains(name))
            .filter_map(|name| {
                let (_, value) = values.iter().find(|(known, _)| *known == name)?;
                Some(format!("{name} = {value}"))
            })
            .take(PER_LINE)
            .collect();
        if !shown.is_empty() {
            notes.push(InlineNote {
                row,
                text: shown.join(", ").into(),
                tone: NoteTone::Dim,
            });
        }
    }
    notes
}

/// The expression to evaluate for a hover at `offset`: the name there with
/// the names it is an attribute of (`self.items` when on `items`). `None`
/// on a keyword, a number or blank space.
pub(super) fn expression_at(text: &str, offset: usize) -> Option<Range<usize>> {
    let word = links::word_at(text, offset)?;
    if !identifier(&text[word.clone()]) || KEYWORDS.contains(&&text[word.clone()]) {
        return None;
    }
    let mut start = word.start;
    while text[..start].ends_with('.') {
        let Some(owner) = links::word_at(text, start - 1).filter(|owner| owner.end == start - 1)
        else {
            break;
        };
        if !identifier(&text[owner.clone()]) {
            break;
        }
        start = owner.start;
    }
    Some(start..word.end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(name: &str, value: &str) -> Variable {
        Variable {
            name: name.into(),
            value: value.into(),
            type_name: None,
            variables_reference: 0,
            named_children: None,
            indexed_children: None,
            evaluate_name: None,
        }
    }

    #[test]
    fn locals_are_noted_on_the_lines_of_the_function_that_name_them() {
        let text = "import os\n\ndef work(n, items):\n    total = 0  # total so far\n    for item in items:\n        total += item.size\n    return total\n\nwork(1, [])\n";
        let locals = [
            local("n", "1"),
            local("items", &"x".repeat(80)),
            local("total", "7"),
            local("special variables", ""),
        ];
        // Stopped on `total += item.size`, line 6.
        let notes = inline_notes(text, 6, &locals);
        let shown: Vec<(usize, &str)> = notes
            .iter()
            .map(|note| (note.row, note.text.as_ref()))
            .collect();
        assert_eq!(shown.len(), 4);
        assert!(shown[0].1.starts_with("n = 1, items = xxx"));
        assert!(shown[0].1.ends_with('…'), "a long value is cut");
        // The value is the one the local has now, at the stop.
        assert_eq!(shown[1], (3, "total = 7"));
        assert_eq!(shown[2].0, 4);
        // `item.size` names `item`, which is not a known local here.
        assert_eq!(shown[3], (5, "total = 7"));
        // At module level only the lines just above the stop are read.
        let top = inline_notes(text, 9, &[local("os", "<module>")]);
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].row, 0);
        assert!(inline_notes("", 4, &locals).is_empty());
    }

    #[test]
    fn a_hover_evaluates_the_name_with_its_owners() {
        let text = "value = self.items.count + 12\nreturn value";
        let at = |needle: &str| text.find(needle).unwrap() + 1;
        let expression = |offset| expression_at(text, offset).map(|range| &text[range]);
        assert_eq!(expression(at("items")), Some("self.items"));
        assert_eq!(expression(at("count")), Some("self.items.count"));
        assert_eq!(expression(at("self")), Some("self"));
        assert_eq!(expression(at("12")), None);
        assert_eq!(expression(at("return")), None);
        assert_eq!(expression(at(" + ")), None);
    }
}
