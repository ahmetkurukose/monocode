//! The code insight tools: what the language servers say about files as
//! they are on disk. Each question opens the file with the shared manager
//! (or joins the editor's copy), asks, and closes it.
//!
//! These tools are let through ahead of the permission mode, and a language
//! server is a program out of the project's own folders (its virtual
//! environment), so they never start one: they join servers that already
//! run, the editor's or those `code_servers_start` started, and otherwise
//! say that tool has to be called. That one asks first.
use super::tools::{char_column, head, line_of, one_line, untrusted, Call};
use crate::code::lsp::{
    self, CancelToken, DocumentSymbol, Language, LineIndex, Location, LspError, Point, ServerState,
    Severity,
};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
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
    if error == LspError::NoServer {
        return not_running(call, path);
    }
    let missing = missing_servers(call, path);
    match missing.is_empty() {
        true => error.to_string(),
        false => format!("{error}\n{}", missing.join("\n")),
    }
}

/// What to say when no language server runs for a file.
fn not_running(call: &Call, path: &Path) -> String {
    let mut text = format!(
        "No language server is running for {}. Call code_servers_start with this file to start \
         them (it asks first, since they run programs from the project's environment), then \
         ask again.",
        call.shown(path)
    );
    for note in missing_servers(call, path) {
        text.push('\n');
        text.push_str(&note);
    }
    text
}

/// The servers of a file that do not run, for a note under an answer that
/// the others gave.
fn absent_note(call: &Call, path: &Path) -> Option<String> {
    let absent = call
        .services
        .lsp()
        .coverage(path, &call.context.document())
        .absent;
    (!absent.is_empty()).then(|| {
        format!(
            "{} {} not running for {}, so what it would report is missing; \
             code_servers_start starts {}.",
            absent.join(", "),
            if absent.len() == 1 { "is" } else { "are" },
            call.shown(path),
            if absent.len() == 1 { "it" } else { "them" }
        )
    })
}

fn owner(call: &Call) -> String {
    call.scope.session.to_string()
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
        let mut file_notes = missing_servers(call, path);
        file_notes.extend(absent_note(call, path));
        for note in file_notes {
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
            .with_running_file(path, &document, &owner(call), |manager| {
                let found = manager.diagnostics_settled(path, left)?;
                Ok((found, manager.document_text(path)))
            })
            .and_then(|found| found);
        let (found, text) = match found {
            Ok(found) => found,
            Err(LspError::NoServer) => {
                report.push_str(&format!(
                    "{shown}: not checked. {}\n",
                    not_running(call, path)
                ));
                continue;
            }
            Err(error) => {
                report.push_str(&format!("{shown}: not checked. {error}\n"));
                continue;
            }
        };
        let text = text.unwrap_or_else(|| Arc::from(""));
        let index = LineIndex::new(&text);
        let count = |severity| found.iter().filter(|d| d.severity == severity).count();
        let (errors, warnings) = (count(Severity::Error), count(Severity::Warning));
        let lesser = found.len().saturating_sub(errors + warnings);
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
                    (errors + warnings).saturating_sub(MAX_PROBLEMS_PER_FILE)
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
                problem.range.start.line.saturating_add(1),
                characters(&text, &index, problem.range.start),
                one_line(&problem.message, 400),
            ));
        }
    }
    for note in notes {
        report.push_str(&format!("Note: {note}\n"));
    }
    Ok(report.trim_end().to_owned())
}

/// The column an agent reads for `point` in `text`: from 1, in characters.
pub(super) fn characters(text: &str, index: &LineIndex, point: Point) -> u32 {
    if index.line_count() <= point.line as usize {
        return point.column.saturating_add(1);
    }
    let start = index.offset_of(
        text,
        Point {
            line: point.line,
            column: 0,
        },
    );
    let at = index.offset_of(text, point).max(start);
    let count = text[start..at].chars().count();
    u32::try_from(count).unwrap_or(u32::MAX).saturating_add(1)
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
        .nth((column as usize).saturating_sub(1))
        .map_or(content.len(), |(at, _)| at);
    let name: String = content[offset..]
        .chars()
        .take_while(|character| word(*character))
        .collect();
    Ok((start + offset, name))
}

/// Why a positional question could not be asked.
enum Refusal {
    /// The agent's own mistake: a line that is not there, a name that is not on it.
    Message(String),
    Lsp(LspError),
}

/// Opens the file with the servers that run, works out the place the agent
/// named in the text the manager holds (the editor's, when it has the file
/// open with unsaved changes), and asks one question about it.
fn ask<T>(
    call: &Call,
    question: impl FnOnce(&lsp::Manager, &Path, usize) -> Result<T, LspError>,
) -> Result<(T, String, std::path::PathBuf), String> {
    let path = call.required_file("file")?;
    if let Some(reason) = unserved(call, &path) {
        return Err(reason);
    }
    let document = call.context.document();
    let answer = call
        .services
        .lsp()
        .with_running_file(&path, &document, &owner(call), |manager| {
            let text = manager
                .document_text(&path)
                .ok_or(Refusal::Lsp(LspError::NotOpen))?;
            let (offset, name) = place(call, &text).map_err(Refusal::Message)?;
            let answer = question(manager, &path, offset).map_err(Refusal::Lsp)?;
            Ok((answer, name))
        })
        .map_err(Refusal::Lsp)
        .and_then(|answer| answer);
    match answer {
        Ok((answer, name)) => Ok((answer, name, path)),
        Err(Refusal::Message(message)) => Err(message),
        Err(Refusal::Lsp(LspError::NoServer)) => {
            // Without a server there is no text of the manager's: what is on
            // disk is as good for telling the agent what it got wrong first.
            if let Ok(text) = std::fs::read_to_string(&path) {
                place(call, &text)?;
            }
            Err(failure(call, &path, LspError::NoServer))
        }
        Err(Refusal::Lsp(error)) => Err(failure(call, &path, error)),
    }
}

fn locations(call: &Call, found: Vec<Location>) -> String {
    let mut texts = HashMap::new();
    let mut lines: Vec<String> = Vec::new();
    for location in found.iter().take(MAX_LOCATIONS) {
        let line = location.range.start.line.saturating_add(1);
        lines.push(format!(
            "{}:{line}:{}  {}",
            call.shown(&location.path),
            char_column(&mut texts, &location.path, location.range.start),
            line_of(&mut texts, &location.path, line)
        ));
    }
    if found.len() > MAX_LOCATIONS {
        lines.push(format!(
            "… and {} more",
            found.len().saturating_sub(MAX_LOCATIONS)
        ));
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
        let (first, last) = (
            symbol.range.start.line.saturating_add(1),
            symbol.range.end.line.saturating_add(1),
        );
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
        .with_running_file(&path, &document, &owner(call), |manager| {
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

/// Starts the language servers for a file's project, which runs programs
/// from the project's environment, and says how each one is doing.
pub(super) fn servers_start(call: &Call) -> Result<String, String> {
    let path = call.required_file("file")?;
    if let Some(reason) = unserved(call, &path) {
        return Err(reason);
    }
    let wait = call.wait("timeout_s", 45, Duration::from_secs(120))?;
    let lsp = call.services.lsp();
    let document = call.context.document();
    if lsp.project_settings(&document.project).disabled {
        return Err("Language servers are switched off for this project in its settings.".into());
    }
    let statuses = lsp
        .start_servers(&path, &document, &owner(call), wait)
        .map_err(|error| error.to_string())?;
    if statuses.is_empty() {
        return Ok(format!(
            "No language server applies to {}.",
            call.shown(&path)
        ));
    }
    let mut lines = Vec::new();
    for status in statuses {
        let name = one_line(&status.server, 80);
        lines.push(match status.state {
            ServerState::Ready => format!("{name}: ready"),
            ServerState::Indexing { title } => format!(
                "{name}: running, still working on {}; answers may be incomplete until it is done",
                one_line(title.as_deref().unwrap_or("its index"), 80)
            ),
            ServerState::Starting => format!(
                "{name}: still starting after {} s; it carries on, ask again in a moment",
                wait.as_secs()
            ),
            ServerState::Stopped => format!("{name}: stopped"),
            ServerState::ToolNotFound { looked_for, hint } => format!(
                "{}: not installed (looked for {}). {hint}",
                status.server,
                looked_for.join(", ")
            ),
            ServerState::Failed { message, log_tail } => {
                let mut line = format!("{name}: failed. {}", one_line(&message, 300));
                if !log_tail.trim().is_empty() {
                    line.push('\n');
                    line.push_str(&untrusted(
                        "What it wrote",
                        &head(&super::tools::tail(&log_tail, 800), 800),
                    ));
                }
                line
            }
        });
    }
    Ok(format!(
        "Language servers for {}:\n{}\nThey stay up while you use them (code_diagnostics, \
         code_definition, ...).",
        call.shown(&path),
        lines.join("\n")
    ))
}
