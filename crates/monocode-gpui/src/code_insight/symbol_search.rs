//! Quick open's `@` mode: the symbols of the focused file, and once
//! something is typed, the symbols of the checkout's other files of the same
//! language. Both come from the syntax tree, so no language server is
//! needed; the walk over the checkout is bounded in files and in time.
use super::outline::kind_name;
use crate::{
    code_editor::symbols::{self, Symbol},
    code_shell::{
        commands::{self, Item, Mode, Scope},
        context,
    },
    editor_syntax,
};
use gpui::{App, Task};
use std::{
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant},
};

const MAX_FILES: usize = 1500;
const MAX_FILE_BYTES: u64 = 512 * 1024;
const MAX_TIME: Duration = Duration::from_millis(700);
const MAX_ITEMS: usize = 200;
/// Folders that hold other people's code or build output.
const SKIPPED: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    "venv",
    "__pycache__",
    "site-packages",
    "vendor",
];

pub(super) fn register(cx: &mut App) {
    commands::register_mode(
        Mode {
            id: "code-symbols",
            prefix: '@',
            hint: "Type to search symbols",
            search: Rc::new(search),
        },
        cx,
    );
}

/// How well a name answers a query: lower is better, `None` is no match.
/// A prefix beats a word inside the name, which beats letters in order.
fn score(name: &str, query: &str) -> Option<usize> {
    if query.is_empty() {
        return Some(0);
    }
    let name = name.to_lowercase();
    if name.starts_with(query) {
        return Some(0);
    }
    if name.contains(query) {
        return Some(1);
    }
    let mut rest = name.chars();
    (query.chars().all(|typed| rest.any(|c| c == typed))).then_some(2)
}

fn item(path: &Path, shown: &Path, text: &str, symbol: &Symbol) -> Item {
    let line_start = text[..symbol.name_range.start.min(text.len())]
        .rfind('\n')
        .map_or(0, |at| at + 1);
    let column = text
        .get(line_start..symbol.name_range.start)
        .map_or(0, |before| before.chars().count());
    Item {
        title: symbol.name.clone().into(),
        detail: format!(
            "{} · {}:{}",
            kind_name(symbol.kind),
            shown.display(),
            symbol.row + 1
        )
        .into(),
        path: Some(path.to_string_lossy().into_owned()),
        line: symbol.row as u32 + 1,
        column: column as u32 + 1,
        icon: None,
    }
}

/// The matches of one file, each with its score and name length to order
/// by. With nothing typed the file's own order stands.
fn matches(
    path: &Path,
    folder: &Path,
    text: &str,
    outline: &[Symbol],
    query: &str,
) -> Vec<(usize, usize, Item)> {
    let shown = path.strip_prefix(folder).unwrap_or(path);
    (outline.iter())
        .filter_map(|symbol| {
            let score = score(&symbol.name, query)?;
            let length = match query.is_empty() {
                true => 0,
                false => symbol.name.len(),
            };
            Some((score, length, item(path, shown, text, symbol)))
        })
        .collect()
}

/// The files of `folder` in `language`, breadth first so the files near the
/// top are found before the limits run out.
fn files(folder: &Path, language: &str, skip: &Path, deadline: Instant) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut folders = std::collections::VecDeque::from([folder.to_path_buf()]);
    while let Some(at) = folders.pop_front() {
        if found.len() >= MAX_FILES || Instant::now() >= deadline {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&at) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if kind.is_dir() {
                if !name.starts_with('.') && !SKIPPED.contains(&name.as_ref()) {
                    folders.push_back(path);
                }
            } else if kind.is_file()
                && path != skip
                && editor_syntax::language_id(&name) == Some(language)
                && entry
                    .metadata()
                    .is_ok_and(|meta| meta.len() <= MAX_FILE_BYTES)
            {
                found.push(path);
            }
        }
    }
    found
}

fn search(query: String, scope: Scope, cx: &mut App) -> Task<Vec<Item>> {
    let query = query.to_lowercase();
    let folder = PathBuf::from(&scope.folder);
    // The focused file first, from the text and outline its editor has.
    let focused = context::focused_file(cx).and_then(|file| {
        let editor = file.editor.upgrade()?;
        let editor = editor.read(cx);
        let text = editor.input.read(cx).content.clone();
        Some((PathBuf::from(file.path), text, editor.outline()))
    });
    let mut own = Vec::new();
    let mut language = None;
    let mut skip = PathBuf::new();
    if let Some((path, text, outline)) = &focused {
        own = matches(path, &folder, text, outline, &query);
        language = editor_syntax::language_id(&path.to_string_lossy());
        skip = path.clone();
    }
    cx.background_executor().spawn(async move {
        own.sort_by_key(|(score, length, _)| (*score, *length));
        let mut items: Vec<Item> = own.into_iter().map(|(.., item)| item).collect();
        let Some(language) = language.filter(|_| !query.is_empty()) else {
            return items;
        };
        let deadline = Instant::now() + MAX_TIME;
        let mut others = Vec::new();
        for path in files(&folder, language, &skip, deadline) {
            if Instant::now() >= deadline {
                break;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let outline = symbols::outline(&path.to_string_lossy(), &text);
            others.extend(matches(&path, &folder, &text, &outline, &query));
        }
        others.sort_by_key(|(score, length, _)| (*score, *length));
        items.extend(others.into_iter().map(|(.., item)| item));
        items.truncate(MAX_ITEMS);
        items
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_match_by_prefix_then_inside_then_in_order() {
        assert_eq!(score("load_user", "load"), Some(0));
        assert_eq!(score("reload", "load"), Some(1));
        assert_eq!(score("LinkedOrderAdapter", "loa"), Some(2));
        assert_eq!(score("save", "load"), None);
        assert_eq!(score("anything", ""), Some(0));
    }

    #[test]
    fn the_scan_finds_same_language_files_and_skips_vendored_folders() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("app/models")).unwrap();
        std::fs::create_dir_all(root.join("node_modules/x")).unwrap();
        std::fs::create_dir_all(root.join(".venv/lib")).unwrap();
        std::fs::write(root.join("app/models/user.py"), "class User:\n    pass\n").unwrap();
        std::fs::write(root.join("app/main.py"), "def main():\n    pass\n").unwrap();
        std::fs::write(root.join("app/readme.md"), "# no\n").unwrap();
        std::fs::write(root.join("node_modules/x/a.py"), "def hidden(): pass\n").unwrap();
        std::fs::write(root.join(".venv/lib/b.py"), "def hidden(): pass\n").unwrap();
        let later = Instant::now() + Duration::from_secs(5);
        let mut found = files(root, "python", &root.join("app/main.py"), later);
        found.sort();
        assert_eq!(found, [root.join("app/models/user.py")]);
        // Out of time, the walk stops rather than finishing.
        assert!(files(root, "python", Path::new(""), Instant::now()).is_empty());
        let text = "class User:\n    def name(self):\n        pass\n";
        let outline = symbols::outline("/p/user.py", text);
        let rows = matches(
            Path::new("/p/user.py"),
            Path::new("/p"),
            text,
            &outline,
            "na",
        );
        assert_eq!(rows.len(), 1);
        let item = &rows[0].2;
        assert_eq!(item.title.as_ref(), "name");
        assert_eq!(item.detail.as_ref(), "method · user.py:2");
        assert_eq!((item.line, item.column), (2, 9));
    }
}
