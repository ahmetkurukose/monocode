//! The code insight tools: what the language servers say about files as
//! they are on disk. Each question opens the file with the shared manager
//! (or joins the editor's copy), asks, and closes it; the servers stay up
//! for their idle period, so a run of questions pays for the start once.
use super::tools::{head, line_of, one_line, untrusted, Call};
use crate::code::lsp::{
    self, CancelToken, DocumentSymbol, Language, LineIndex, Location, LspError, Point, Severity,
};
use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

const MAX_FILES: usize = 20;
const MAX_PROBLEMS_PER_FILE: usize = 60;
const MAX_LOCATIONS: usize = 120;
const MAX_SYMBOLS: usize = 400;
const MAX_HOVER_BYTES: usize = 6_000;

/// The servers meant for `path` that are not installed, each as a sentence
/// naming what was looked for and what to install.
fn missing_servers(call: &Call, path: &Path) -> Vec<String> {
    let Some(language) = Language::of(path) else {
        return Vec::new();
    };
    let toolchain = call.context.toolchain();
    lsp::Registry::builtin()
        .for_language(language)
        .filter(|spec| spec.resolve(&toolchain, None).is_none())
        .map(|spec| {
            format!(
                "{} is not installed (looked for {} in the project's virtual environment and \
                 on the PATH). {}",
                spec.id,
                spec.executables().join(", "),
                spec.install_hint
            )
        })
        .collect()
}

/// Why a file cannot be asked about at all, if so.
fn unserved(call: &Call, path: &Path) -> Option<String> {
    if Language::of(path).is_none() {
        return Some(format!(
            "No language server is known for {}: code insight covers Python, TypeScript and \
             JavaScript, Go and Rust files.",
            call.shown(path)
        ));
    }
    None
}

fn failure(call: &Call, path: &Path, error: LspError) -> String {
    let missing = missing_servers(call, path);
    match (&error, missing.is_empty()) {
        (LspError::NoServer, false) => missing.join("\n"),
        (_, true) => error.to_string(),
        (_, false) => format!("{error}\n{}", missing.join("\n")),
    }
}

fn counted(count: usize, what: &str) -> String {
    match count {
        1 => format!("1 {what}"),
        _ => format!("{count} {what}s"),
    }
}

pub(super) fn diagnostics(call: &Call) -> Result<String, String> {
    let mut names: Vec<&str> = Vec::new();
    for value in call.list("files")? {
        names.push(value.as_str().ok_or("`files` must be a list of paths.")?);
    }
    if let Some(file) = call.text("file")? {
        names.push(file);
    }
    if names.is_empty() {
        return Err("`files` is required: the files to check.".into());
    }
    if names.len() > MAX_FILES {
        return Err(format!("At most {MAX_FILES} files at a time."));
    }
    let wait = call.wait("timeout_s", 20, Duration::from_secs(120))?;
    let files = names
        .into_iter()
        .map(|name| call.file(name))
        .collect::<Result<Vec<_>, _>>()?;
    let deadline = Instant::now() + wait;
    let document = call.context.document();
    let manager = call.services.lsp();
    let mut report = String::new();
    let mut notes: Vec<String> = Vec::new();
    for path in &files {
        let shown = call.shown(path);
        if let Some(reason) = unserved(call, path) {
            report.push_str(&format!("{shown}: not checked. {reason}\n"));
            continue;
        }
        for note in missing_servers(call, path) {
            if !notes.contains(&note) {
                notes.push(note);
            }
        }
        // Every file gets at least a moment, even when an earlier one used
        // the time up waiting for a server to start.
        let left = deadline
            .saturating_duration_since(Instant::now())
            .max(Duration::from_secs(1));
        let found = manager
            .with_file(path, &document, |manager| {
                manager.diagnostics_settled(path, left)
            })
            .and_then(|found| found);
        let found = match found {
            Ok(found) => found,
            Err(error) => {
                report.push_str(&format!("{shown}: not checked. {error}\n"));
                continue;
            }
        };
        let count = |severity| found.iter().filter(|d| d.severity == severity).count();
        let (errors, warnings) = (count(Severity::Error), count(Severity::Warning));
        let lesser = found.len() - errors - warnings;
        if errors + warnings == 0 {
            report.push_str(&format!("{shown}: no errors or warnings"));
        } else {
            report.push_str(&format!(
                "{shown}: {}, {}",
                counted(errors, "error"),
                counted(warnings, "warning")
            ));
        }
        if lesser > 0 {
            report.push_str(&format!(" ({} not shown)", counted(lesser, "hint")));
        }
        report.push('\n');
        let mut listed = 0;
        for problem in &found {
            let word = match problem.severity {
                Severity::Error => "error",
                Severity::Warning => "warning",
                _ => continue,
            };
            if listed == MAX_PROBLEMS_PER_FILE {
                report.push_str(&format!(
                    "  … and {} more\n",
                    errors + warnings - MAX_PROBLEMS_PER_FILE
                ));
                break;
            }
            listed += 1;
            let source = match &problem.code {
                Some(code) => format!("{} {code}", problem.server),
                None => problem.server.clone(),
            };
            report.push_str(&format!(
                "  {}:{} {word}: {} [{source}]\n",
                problem.range.start.line + 1,
                problem.range.start.column + 1,
                one_line(&problem.message, 400),
            ));
        }
    }
    for note in notes {
        report.push_str(&format!("Note: {note}\n"));
    }
    Ok(report.trim_end().to_owned())
}

/// The byte offset in `text` an agent means by a line and a column or a
/// name on that line, and the word there for the answer's first line.
fn place(call: &Call, text: &str) -> Result<(usize, String), String> {
    let line = call
        .ordinal("line")?
        .ok_or("`line` is required (from 1).")?;
    let index = LineIndex::new(text);
    if line as usize > index.line_count() {
        return Err(format!(
            "The file has {} lines; there is no line {line}.",
            index.line_count()
        ));
    }
    let start = index.offset_of(
        text,
        Point {
            line: line - 1,
            column: 0,
        },
    );
    let end = index.offset_of(
        text,
        Point {
            line: line - 1,
            column: u32::MAX,
        },
    );
    let content = &text[start..end];
    let word = |character: char| character.is_alphanumeric() || character == '_';
    if let Some(symbol) = call.text("symbol")? {
        let found = content.match_indices(symbol).find(|(at, _)| {
            let before = content[..*at].chars().next_back();
            let after = content[at + symbol.len()..].chars().next();
            !before.is_some_and(word) && !after.is_some_and(word)
        });
        let Some((at, _)) = found else {
            return Err(format!(
                "`{symbol}` is not on line {line}, which reads: {}",
                one_line(content, 200)
            ));
        };
        // In `shapes.Circle` it is the last name that is meant.
        let last = symbol.rfind(|character| !word(character)).map_or(0, |i| {
            i + symbol[i..].chars().next().map_or(1, char::len_utf8)
        });
        return Ok((start + at + last, symbol[last..].to_owned()));
    }
    let Some(column) = call.ordinal("column")? else {
        return Err("Give `column` (from 1) or `symbol`, a name on that line.".into());
    };
    let offset = content
        .char_indices()
        .nth(column as usize - 1)
        .map_or(content.len(), |(at, _)| at);
    let name: String = content[offset..]
        .chars()
        .take_while(|character| word(*character))
        .collect();
    Ok((start + offset, name))
}

/// Reads the file and asks one positional question about it.
fn ask<T>(
    call: &Call,
    question: impl FnOnce(&lsp::Manager, &Path, usize) -> Result<T, LspError>,
) -> Result<(T, String, std::path::PathBuf), String> {
    let path = call.required_file("file")?;
    if let Some(reason) = unserved(call, &path) {
        return Err(reason);
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|error| format!("{} could not be read: {error}", call.shown(&path)))?;
    let (offset, name) = place(call, &text)?;
    let document = call.context.document();
    let answer = call
        .services
        .lsp()
        .with_file(&path, &document, |manager| question(manager, &path, offset))
        .and_then(|answer| answer)
        .map_err(|error| failure(call, &path, error))?;
    Ok((answer, name, path))
}

fn locations(call: &Call, found: Vec<Location>) -> String {
    let mut texts = HashMap::new();
    let mut lines: Vec<String> = Vec::new();
    for location in found.iter().take(MAX_LOCATIONS) {
        let line = location.range.start.line + 1;
        lines.push(format!(
            "{}:{line}:{}  {}",
            call.shown(&location.path),
            location.range.start.column + 1,
            line_of(&mut texts, &location.path, line)
        ));
    }
    if found.len() > MAX_LOCATIONS {
        lines.push(format!("… and {} more", found.len() - MAX_LOCATIONS));
    }
    lines.join("\n")
}

pub(super) fn definition(call: &Call) -> Result<String, String> {
    let (found, name, _) = ask(call, |manager, path, offset| {
        manager.definition(path, offset, &CancelToken::new())
    })?;
    if found.is_empty() {
        return Ok(format!("No definition was found for `{name}` there."));
    }
    Ok(format!(
        "Definition of `{name}`:\n{}",
        locations(call, found)
    ))
}

pub(super) fn references(call: &Call) -> Result<String, String> {
    let declaration = call.flag("include_declaration")?.unwrap_or(true);
    let (found, name, _) = ask(call, |manager, path, offset| {
        manager.references(path, offset, declaration, &CancelToken::new())
    })?;
    if found.is_empty() {
        return Ok(format!("No uses of `{name}` were found."));
    }
    Ok(format!(
        "{} uses of `{name}`:\n{}",
        found.len(),
        locations(call, found)
    ))
}

pub(super) fn hover(call: &Call) -> Result<String, String> {
    let (found, name, _) = ask(call, |manager, path, offset| {
        manager.hover(path, offset, &CancelToken::new())
    })?;
    match found.filter(|hover| !hover.contents.trim().is_empty()) {
        Some(hover) => Ok(untrusted(
            &format!("What the language server says about `{name}`"),
            &head(hover.contents.trim(), MAX_HOVER_BYTES),
        )),
        None => Ok(format!(
            "The language server has nothing on `{name}` there."
        )),
    }
}

fn outline(symbols: &[DocumentSymbol], depth: usize, lines: &mut Vec<String>) {
    for symbol in symbols {
        let kind = format!("{:?}", symbol.kind).to_lowercase();
        let (first, last) = (symbol.range.start.line + 1, symbol.range.end.line + 1);
        let span = match first == last {
            true => format!("{first}"),
            false => format!("{first}-{last}"),
        };
        let detail = match symbol.detail.as_deref().map(str::trim) {
            Some(detail) if !detail.is_empty() => format!(" {}", one_line(detail, 120)),
            _ => String::new(),
        };
        lines.push(format!(
            "{}{kind} {}{detail}  (line {span})",
            "  ".repeat(depth),
            one_line(&symbol.name, 120)
        ));
        outline(&symbol.children, depth + 1, lines);
    }
}

pub(super) fn symbols(call: &Call) -> Result<String, String> {
    let path = call.required_file("file")?;
    if let Some(reason) = unserved(call, &path) {
        return Err(reason);
    }
    let document = call.context.document();
    let found = call
        .services
        .lsp()
        .with_file(&path, &document, |manager| {
            manager.document_symbols(&path, &CancelToken::new())
        })
        .and_then(|found| found)
        .map_err(|error| failure(call, &path, error))?;
    if found.is_empty() {
        return Ok(format!("{} has no symbols.", call.shown(&path)));
    }
    let mut lines = Vec::new();
    outline(&found, 0, &mut lines);
    let more = lines.len().saturating_sub(MAX_SYMBOLS);
    lines.truncate(MAX_SYMBOLS);
    if more > 0 {
        lines.push(format!("… and {more} more"));
    }
    Ok(format!("{}:\n{}", call.shown(&path), lines.join("\n")))
}
