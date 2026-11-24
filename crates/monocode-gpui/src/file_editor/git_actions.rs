//! Git from the code editor: the file's history, its changes against the
//! last commit, blame, a line's history and the file on its remote, and
//! comparing the editor's text with the clipboard.
use super::{
    CompareWithClipboard, FileEditor, FileEvent, NextChange, OpenOnRemote, PreviousChange,
    RevertChange, ShowLineHistory, ToggleBlame,
};
use crate::{
    code_editor::{self, DocumentEvent, InlineNote, NoteTone},
    file_history::Request,
    git_status::{GitChanged, ProjectGit},
    popover::MenuAction,
};
use gpui::{
    Action, App, AppContext, ClipboardItem, Context, Entity, Focusable, Global, SharedString,
    Subscription, Task, Window,
};
use monocode_core::scm::{self, Blame, BlameCommit, RemoteWeb};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

/// The hub source blame's notes are stored under.
const BLAME_SOURCE: &str = "git-blame";

/// A shortcut as this platform writes it.
fn keys(mac: &'static str, other: &'static str) -> &'static str {
    if cfg!(target_os = "macos") {
        mac
    } else {
        other
    }
}

/// What the Git rows can do with this file, from what is on disk and the
/// project's last status read, without running git.
struct GitState {
    repo: bool,
    /// Committed: not untracked or only added.
    tracked: bool,
    remote: Option<RemoteWeb>,
}

/// One file's blame, shown in every editor of it while it is on.
struct FileBlame {
    blame: Option<Arc<Blame>>,
    /// For each buffer line, its line in `blame` while it is unchanged.
    rows: Arc<Vec<Option<usize>>>,
    /// The buffer text `rows` will describe once they are worked out.
    text: SharedString,
    load: Option<Task<()>>,
    align: Option<Task<()>>,
    _head: Option<Subscription>,
}

/// The files blame is on for. Blame is the file's, as the hub's notes are:
/// two editors of one file show it together.
#[derive(Default)]
pub(crate) struct BlameStore {
    files: HashMap<PathBuf, FileBlame>,
    _hub: Option<Subscription>,
}
struct Blames(Entity<BlameStore>);
impl Global for Blames {}

fn blames(cx: &mut App) -> Entity<BlameStore> {
    if let Some(blames) = cx.try_global::<Blames>() {
        return blames.0.clone();
    }
    let hub = code_editor::hub(cx);
    let store = cx.new(|cx| BlameStore {
        files: HashMap::new(),
        _hub: Some(cx.subscribe(
            &hub,
            |this: &mut BlameStore, hub, event: &DocumentEvent, cx| {
                this.document_event(&hub, event, cx)
            },
        )),
    });
    cx.set_global(Blames(store.clone()));
    store
}
fn blames_ref(cx: &App) -> Option<&BlameStore> {
    cx.try_global::<Blames>().map(|blames| blames.0.read(cx))
}

/// The text of a blame's note: who and when, and the commit.
fn note_text(commit: &BlameCommit, now_ms: u64) -> String {
    let when = monocode_core::time::relative(commit.time * 1000, now_ms);
    [
        commit.author.as_str(),
        when.as_str(),
        commit.short_sha.as_str(),
    ]
    .into_iter()
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join(" · ")
}

/// The notes for a buffer whose lines sit at `rows` of `blame`: unchanged
/// lines name their commit, changed ones show nothing.
fn blame_notes(blame: &Blame, rows: &[Option<usize>], now_ms: u64) -> Vec<InlineNote> {
    let texts: Vec<SharedString> = blame
        .commits
        .iter()
        .map(|commit| note_text(commit, now_ms).into())
        .collect();
    rows.iter()
        .enumerate()
        .filter_map(|(row, line)| {
            let commit = blame.lines.get((*line)?)?.commit;
            Some(InlineNote {
                row,
                text: texts.get(commit)?.clone(),
                tone: NoteTone::Dim,
            })
        })
        .collect()
}

impl BlameStore {
    fn is_on(&self, path: &Path) -> bool {
        self.files.contains_key(path)
    }
    /// The commit that last changed a buffer row, as of the last line-up.
    fn commit_at(&self, path: &Path, row: usize) -> Option<&BlameCommit> {
        let file = self.files.get(path)?;
        let blame = file.blame.as_ref()?;
        let line = (*file.rows.get(row)?)?;
        blame.commits.get(blame.lines.get(line)?.commit)
    }
    fn turn_on(
        &mut self,
        path: PathBuf,
        text: SharedString,
        git: &Entity<ProjectGit>,
        cx: &mut Context<Self>,
    ) {
        let head = {
            let path = path.clone();
            cx.subscribe(git, move |this, _, _: &GitChanged, cx| {
                this.refresh(&path, cx)
            })
        };
        self.files.insert(
            path.clone(),
            FileBlame {
                blame: None,
                rows: Arc::default(),
                text,
                load: None,
                align: None,
                _head: Some(head),
            },
        );
        self.load(&path, cx);
    }
    fn turn_off(&mut self, path: &Path, cx: &mut Context<Self>) {
        if self.files.remove(path).is_some() {
            let path = path.to_owned();
            code_editor::hub(cx).update(cx, |hub, cx| {
                hub.set_inline_notes(&path, BLAME_SOURCE, Vec::new(), cx)
            });
        }
    }
    /// Blames HEAD's version of the file again, off the UI thread.
    fn load(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Some(file) = self.files.get_mut(path) else {
            return;
        };
        let read = path.to_owned();
        let work = cx
            .background_executor()
            .spawn(async move { scm::blame_file(&read) });
        let path = path.to_owned();
        file.load = Some(cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(blame) => {
                    if let Some(file) = this.files.get_mut(&path) {
                        file.blame = Some(Arc::new(blame));
                        let text = file.text.clone();
                        this.align(&path, text, cx);
                    }
                }
                // A file that stopped being blamable (its commit is gone,
                // or it was never committed) shows nothing.
                Err(error) => {
                    eprintln!("monocode: git blame failed: {error}");
                    this.turn_off(&path, cx);
                }
            });
        }));
    }
    /// Blames again when HEAD moved; otherwise lines the buffer up again.
    fn refresh(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Some(file) = self.files.get(path) else {
            return;
        };
        let blamed = file.blame.as_ref().map(|blame| blame.head.clone());
        let read = path.to_owned();
        let work = cx
            .background_executor()
            .spawn(async move { scm::head_revision(&read) });
        let path = path.to_owned();
        cx.spawn(async move |this, cx| {
            let head = work.await;
            let _ = this.update(cx, |this, cx| {
                if head.is_some() && head != blamed {
                    this.load(&path, cx);
                }
            });
        })
        .detach();
    }
    /// Works out which buffer lines are still HEAD's, then shows their notes.
    fn align(&mut self, path: &Path, text: SharedString, cx: &mut Context<Self>) {
        let Some(file) = self.files.get_mut(path) else {
            return;
        };
        file.text = text.clone();
        let Some(blame) = file.blame.clone() else {
            return;
        };
        let lined = blame.clone();
        let work = cx.background_executor().spawn(async move {
            let base: Vec<&str> = lined.lines.iter().map(|l| l.text.as_str()).collect();
            scm::align_lines(&base, &text)
        });
        let path = path.to_owned();
        file.align = Some(cx.spawn(async move |this, cx| {
            let rows = Arc::new(work.await);
            let _ = this.update(cx, |this, cx| {
                let Some(file) = this.files.get_mut(&path) else {
                    return;
                };
                if !file.blame.as_ref().is_some_and(|b| Arc::ptr_eq(b, &blame)) {
                    return;
                }
                file.rows = rows.clone();
                let notes = blame_notes(&blame, &rows, monocode_core::time::now_ms());
                code_editor::hub(cx).update(cx, |hub, cx| {
                    hub.set_inline_notes(&path, BLAME_SOURCE, notes, cx)
                });
                cx.notify();
            });
        }));
    }
    fn document_event(
        &mut self,
        hub: &Entity<code_editor::Hub>,
        event: &DocumentEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            DocumentEvent::Changed { doc, text, .. } if self.is_on(&doc.path) => {
                self.align(&doc.path, text.clone(), cx)
            }
            DocumentEvent::Saved { doc, text, .. } if self.is_on(&doc.path) => {
                self.align(&doc.path, text.clone(), cx);
                self.refresh(&doc.path, cx);
            }
            DocumentEvent::Closed { doc } if hub.read(cx).document(&doc.path).is_none() => {
                self.turn_off(&doc.path, cx)
            }
            _ => {}
        }
    }
}

impl FileEditor {
    fn git_state(&self, cx: &App) -> GitState {
        let Some(repo) = scm::local_repo(Path::new(&self.path)) else {
            return GitState {
                repo: false,
                tracked: false,
                remote: None,
            };
        };
        let status = self.git.read(cx).statuses.files.get(&self.path);
        GitState {
            repo: true,
            tracked: self.document.is_some()
                && !matches!(status.map(String::as_str), Some("untracked" | "added")),
            remote: repo.remote,
        }
    }
    fn blame_on(&self, cx: &App) -> bool {
        blames_ref(cx).is_some_and(|store| store.is_on(&self.extensions.doc.path))
    }
    /// The commit that last changed the caret's line, while blame is on.
    fn caret_commit(&self, cx: &App) -> Option<BlameCommit> {
        let input = self.input.read(cx);
        let row = code_editor::row_of(&input.content, input.caret());
        blames_ref(cx)?
            .commit_at(&self.extensions.doc.path, row)
            .cloned()
    }
    /// The rows of the code menu's Git submenu.
    pub(super) fn git_menu_items(&self, cx: &App) -> Vec<MenuAction> {
        let input = self.input.read(cx);
        let changes = !input.editor_analysis().changes.is_empty();
        let writable = !input.editing_caps().read_only;
        let selected = !input.selection_range().is_empty();
        let state = self.git_state(cx);
        let committed = state.repo && state.tracked;
        let blame_on = self.blame_on(cx);
        let host = state.remote.as_ref().map(|remote| remote.name().to_owned());
        let mut items = vec![
            MenuAction::new("blame", "Annotate with Git Blame")
                .checked(blame_on)
                .disabled(!committed && !blame_on),
        ];
        if let Some(commit) = self.caret_commit(cx) {
            items.push(
                MenuAction::new("blame-commit", format!("Show Commit {}", commit.short_sha))
                    .description(commit.subject),
            );
        }
        items.extend([
            MenuAction::new("history", "Show History").disabled(!state.repo),
            MenuAction::new(
                "line-history",
                if selected {
                    "Show History for Selection"
                } else {
                    "Show History for Line"
                },
            )
            .disabled(!committed),
            MenuAction::new(
                "open-remote",
                match &host {
                    Some(host) => format!("Open on {host}"),
                    None => "Open on Remote".into(),
                },
            )
            .icon("external-link")
            .disabled(!committed || host.is_none()),
            MenuAction::new(
                "copy-remote-link",
                match &host {
                    Some(host) => format!("Copy {host} Link"),
                    None => "Copy Link".into(),
                },
            )
            .disabled(!committed || host.is_none()),
            MenuAction::new("previous-change", "Previous Change")
                .shortcut(keys("⌃⌥⇧↑", "Ctrl+Alt+Shift+Up"))
                .disabled(!changes),
            MenuAction::new("next-change", "Next Change")
                .shortcut(keys("⌃⌥⇧↓", "Ctrl+Alt+Shift+Down"))
                .disabled(!changes),
            MenuAction::new("revert-change", "Revert Change").disabled(!changes || !writable),
        ]);
        items
    }
    /// Takes a picked Git row; `false` when it is not one.
    pub(super) fn git_menu_pick(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let action: Box<dyn Action> = match id {
            "history" => {
                self.open_history(None, cx);
                return true;
            }
            "blame-commit" => {
                if let Some(commit) = self.caret_commit(cx) {
                    self.open_history(Some(Request::Commit(commit.sha)), cx);
                }
                return true;
            }
            "copy-remote-link" => {
                self.remote_link(false, window, cx);
                return true;
            }
            "blame" => Box::new(ToggleBlame),
            "line-history" => Box::new(ShowLineHistory),
            "open-remote" => Box::new(OpenOnRemote),
            "previous-change" => Box::new(PreviousChange),
            "next-change" => Box::new(NextChange),
            "revert-change" => Box::new(RevertChange),
            _ => return false,
        };
        window.focus(&self.input.focus_handle(cx), cx);
        window.dispatch_action(action, cx);
        true
    }
    /// Opens the history window for this file, showing `request`.
    fn open_history(&mut self, request: Option<Request>, cx: &mut Context<Self>) {
        cx.emit(FileEvent::FileHistory {
            cwd: self.cwd.clone(),
            path: self.path.clone(),
            request,
        });
    }
    /// The selected lines (one-based), or `None` without a selection.
    fn selected_lines(&self, cx: &App) -> Option<(usize, usize)> {
        let selected = !self.input.read(cx).selection_range().is_empty();
        selected.then(|| self.menu_target(cx).lines)
    }

    pub(super) fn toggle_blame(&mut self, _: &ToggleBlame, _: &mut Window, cx: &mut Context<Self>) {
        let path = self.extensions.doc.path.clone();
        let store = blames(cx);
        if store.read(cx).is_on(&path) {
            store.update(cx, |store, cx| store.turn_off(&path, cx));
            cx.notify();
            return;
        }
        let state = self.git_state(cx);
        if !state.repo || !state.tracked {
            let why = match state.repo {
                false => "This file is not in a Git repository.",
                true => "Git has no committed version of this file to annotate.",
            };
            self.show_notice(why.into(), cx);
            return;
        }
        let text = self.input.read(cx).content.clone();
        let git = self.git.clone();
        store.update(cx, |store, cx| store.turn_on(path, text, &git, cx));
        cx.notify();
    }
    pub(super) fn show_line_history(
        &mut self,
        _: &ShowLineHistory,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.document.is_none() {
            return;
        }
        let (first, last) = self.menu_target(cx).lines;
        let text = self.input.read(cx).content.to_string();
        self.open_history(
            Some(Request::Lines {
                first,
                last,
                text: Some(text),
            }),
            cx,
        );
    }
    pub(super) fn open_on_remote(
        &mut self,
        _: &OpenOnRemote,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.remote_link(true, window, cx);
    }
    /// Opens the file's page on its remote at HEAD, with the selected lines
    /// marked, or copies its address.
    fn remote_link(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.document.is_none() {
            return;
        }
        let path = PathBuf::from(&self.path);
        let lines = self.selected_lines(cx);
        let text = self.input.read(cx).content.to_string();
        let work = cx
            .background_executor()
            .spawn(async move { scm::remote_file_link(&path, lines, Some(&text)) });
        cx.spawn_in(window, async move |this, cx| {
            let result = work.await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(url) if open => crate::shell::browser_links::open_link(&url, false, window, cx),
                Ok(url) => {
                    cx.write_to_clipboard(ClipboardItem::new_string(url));
                    this.show_notice("Link copied".into(), cx);
                }
                Err(error) => this.show_notice(error, cx),
            });
        })
        .detach();
    }
    pub(super) fn compare_with_clipboard(
        &mut self,
        _: &CompareWithClipboard,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.document.is_none() {
            return;
        }
        let Some(clipboard) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            self.show_notice("The clipboard has no text to compare.".into(), cx);
            return;
        };
        let input = self.input.read(cx);
        let range = input.selection_range();
        let content = input.content.clone();
        let name = self.relative_path();
        let (title, right) = match self.selected_lines(cx) {
            Some((first, last)) if range.end <= content.len() => (
                match first == last {
                    true => format!("Clipboard ↔ {name}: line {first}"),
                    false => format!("Clipboard ↔ {name}: lines {first}–{last}"),
                },
                content[range].to_owned(),
            ),
            _ => (format!("Clipboard ↔ {name}"), content.to_string()),
        };
        self.open_history(
            Some(Request::Compare {
                title,
                left: clipboard,
                right,
            }),
            cx,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, WindowHandle};
    use std::{cell::RefCell, rc::Rc};

    fn git(dir: &Path, args: &[&str]) {
        let done = std::process::Command::new("git")
            .args(["-c", "user.name=Ann", "-c", "user.email=ann@example.com"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            done.status.success(),
            "{}",
            String::from_utf8_lossy(&done.stderr)
        );
    }

    struct Setup {
        window: WindowHandle<FileEditor>,
        history: Rc<RefCell<Vec<(String, Option<Request>)>>>,
        dir: tempfile::TempDir,
        path: String,
    }

    /// An editor on `a.txt`, committed twice when `repo`, else outside Git.
    fn setup(cx: &mut TestAppContext, repo: bool) -> Setup {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a.txt"), "one\ntwo\n").unwrap();
        if repo {
            git(root, &["init"]);
            git(root, &["add", "-A"]);
            git(root, &["commit", "-m", "first"]);
            std::fs::write(root.join("a.txt"), "one\ntwo\nthree\n").unwrap();
            git(root, &["commit", "-am", "second"]);
        }
        cx.update(|cx| {
            cx.set_global(crate::settings::Preferences::default());
            crate::popover::bind(cx);
        });
        let path = root.join("a.txt").to_string_lossy().into_owned();
        let project = root.to_string_lossy().into_owned();
        let opened = path.clone();
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |_, cx| {
                let git = cx.new(|cx| ProjectGit::new(project, cx));
                cx.new(|cx| FileEditor::new(opened, git, cx))
            })
            .unwrap()
        });
        let history = Rc::new(RefCell::new(Vec::new()));
        let log = history.clone();
        window
            .update(cx, |editor, _, cx| {
                cx.subscribe(&cx.entity(), move |_, _, event: &FileEvent, _| {
                    if let FileEvent::FileHistory { path, request, .. } = event {
                        log.borrow_mut().push((path.clone(), request.clone()));
                    }
                })
                .detach();
                editor.set_visible(true, cx);
            })
            .unwrap();
        cx.run_until_parked();
        Setup {
            window,
            history,
            dir,
            path,
        }
    }

    fn rows(editor: &FileEditor, cx: &App) -> Vec<(String, String, bool, bool)> {
        editor
            .git_menu_items(cx)
            .into_iter()
            .map(|a| (a.id.to_string(), a.label.to_string(), a.disabled, a.checked))
            .collect()
    }
    fn row(rows: &[(String, String, bool, bool)], id: &str) -> (String, bool, bool) {
        let row = rows.iter().find(|r| r.0 == id).expect(id);
        (row.1.clone(), row.2, row.3)
    }
    fn notes(cx: &mut TestAppContext, path: &str) -> Vec<(usize, String)> {
        cx.update(|cx| {
            code_editor::hub(cx)
                .read(cx)
                .inline_notes(Path::new(path))
                .into_iter()
                .map(|note| (note.row, note.text.to_string()))
                .collect()
        })
    }

    #[test]
    fn blame_notes_skip_changed_lines_and_name_who_and_when() {
        let blame = Blame {
            head: "h".into(),
            path: "a.txt".into(),
            commits: vec![BlameCommit {
                sha: "a".repeat(40),
                short_sha: "aaaaaaa".into(),
                author: "Ann".into(),
                time: 1_000,
                subject: "First".into(),
            }],
            lines: vec![
                scm::BlameLine {
                    commit: 0,
                    text: "one".into(),
                },
                scm::BlameLine {
                    commit: 0,
                    text: "two".into(),
                },
            ],
        };
        let notes = blame_notes(&blame, &[Some(0), None, Some(1)], 1_000_000 + 3_600_000);
        let shown: Vec<_> = notes.iter().map(|n| (n.row, n.text.as_ref())).collect();
        assert_eq!(
            shown,
            [(0, "Ann · 1h · aaaaaaa"), (2, "Ann · 1h · aaaaaaa")]
        );
    }

    #[gpui::test]
    fn outside_a_repository_the_git_rows_are_disabled(cx: &mut TestAppContext) {
        let s = setup(cx, false);
        s.window
            .update(cx, |editor, _, cx| {
                let rows = rows(editor, cx);
                for id in [
                    "blame",
                    "history",
                    "line-history",
                    "open-remote",
                    "copy-remote-link",
                ] {
                    assert!(row(&rows, id).1, "{id} is disabled");
                }
                assert_eq!(row(&rows, "open-remote").0, "Open on Remote");
            })
            .unwrap();
    }

    #[gpui::test]
    fn the_rows_follow_the_files_state_and_remote(cx: &mut TestAppContext) {
        let s = setup(cx, true);
        s.window
            .update(cx, |editor, _, cx| {
                let rows = rows(editor, cx);
                assert_eq!(
                    row(&rows, "blame"),
                    ("Annotate with Git Blame".into(), false, false)
                );
                assert!(!row(&rows, "line-history").1);
                assert_eq!(row(&rows, "line-history").0, "Show History for Line");
                assert!(row(&rows, "open-remote").1, "no remote yet");
            })
            .unwrap();
        git(
            s.dir.path(),
            &["remote", "add", "origin", "https://gitlab.com/acme/app.git"],
        );
        s.window
            .update(cx, |editor, _, cx| {
                editor
                    .input
                    .update(cx, |input, cx| input.select_range(0..5, cx));
                let rows = rows(editor, cx);
                assert_eq!(
                    row(&rows, "open-remote"),
                    ("Open on GitLab".into(), false, false)
                );
                assert_eq!(row(&rows, "copy-remote-link").0, "Copy GitLab Link");
                assert_eq!(row(&rows, "line-history").0, "Show History for Selection");
            })
            .unwrap();
        // An untracked file has no history, blame or page to offer.
        let new = s.dir.path().join("new.txt");
        std::fs::write(&new, "x\n").unwrap();
        let project = s.dir.path().to_string_lossy().into_owned();
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |_, cx| {
                let git = cx.new(|cx| ProjectGit::new(project, cx));
                cx.new(|cx| FileEditor::new(new.to_string_lossy().into_owned(), git, cx))
            })
            .unwrap()
        });
        cx.run_until_parked();
        window
            .update(cx, |editor, _, cx| {
                let rows = rows(editor, cx);
                for id in ["blame", "line-history", "open-remote"] {
                    assert!(row(&rows, id).1, "{id} is disabled");
                }
                assert!(!row(&rows, "history").1);
            })
            .unwrap();
    }

    #[gpui::test]
    fn blame_annotates_committed_lines_follows_edits_and_turns_off(cx: &mut TestAppContext) {
        let s = setup(cx, true);
        s.window
            .update(cx, |editor, window, cx| {
                editor.toggle_blame(&ToggleBlame, window, cx)
            })
            .unwrap();
        cx.run_until_parked();
        let shown = notes(cx, &s.path);
        assert_eq!(shown.len(), 3, "{shown:?}");
        assert!(shown[0].1.starts_with("Ann · "), "{shown:?}");
        assert_ne!(shown[0].1, shown[2].1, "two commits");
        s.window
            .update(cx, |editor, _, cx| {
                assert_eq!(row(&rows(editor, cx), "blame").2, true, "checked");
                let commit = editor.caret_commit(cx).expect("the caret's line has one");
                assert_eq!(commit.subject, "first");
                assert_eq!(
                    row(&rows(editor, cx), "blame-commit").0,
                    format!("Show Commit {}", commit.short_sha)
                );
                // A line typed above the first moves the notes down and has none.
                editor.input.update(cx, |input, cx| {
                    input.replace_all("zero\none\ntwo\nthree\n".into(), cx)
                });
                editor.sync_document(cx);
            })
            .unwrap();
        cx.run_until_parked();
        let rows_shown: Vec<_> = notes(cx, &s.path).into_iter().map(|n| n.0).collect();
        assert_eq!(rows_shown, [1, 2, 3]);
        // Picking the commit row opens the history at that commit.
        s.window
            .update(cx, |editor, window, cx| {
                editor
                    .input
                    .update(cx, |input, cx| input.select_range(6..6, cx));
                assert!(editor.git_menu_pick("blame-commit", window, cx));
            })
            .unwrap();
        let asked = s.history.borrow().clone();
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0].0, s.path);
        assert!(matches!(asked[0].1, Some(Request::Commit(_))));
        s.window
            .update(cx, |editor, window, cx| {
                editor.toggle_blame(&ToggleBlame, window, cx)
            })
            .unwrap();
        cx.run_until_parked();
        assert!(notes(cx, &s.path).is_empty());
    }

    #[gpui::test]
    fn line_history_compare_and_link_requests_carry_the_selection(cx: &mut TestAppContext) {
        let s = setup(cx, true);
        git(
            s.dir.path(),
            &["remote", "add", "origin", "git@github.com:acme/app.git"],
        );
        let last = |s: &Setup| s.history.borrow().last().and_then(|(_, r)| r.clone());
        s.window
            .update(cx, |editor, window, cx| {
                // "two\nthree" is lines 2 and 3.
                editor
                    .input
                    .update(cx, |input, cx| input.select_range(4..13, cx));
                editor.show_line_history(&ShowLineHistory, window, cx);
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(
            last(&s),
            Some(Request::Lines {
                first: 2,
                last: 3,
                text: Some("one\ntwo\nthree\n".into()),
            })
        );
        s.window
            .update(cx, |editor, window, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string("two\n3".into()));
                editor.compare_with_clipboard(&CompareWithClipboard, window, cx);
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(
            last(&s),
            Some(Request::Compare {
                title: "Clipboard ↔ a.txt: lines 2–3".into(),
                left: "two\n3".into(),
                right: "two\nthree".into(),
            })
        );
        s.window
            .update(cx, |editor, window, cx| {
                editor.git_menu_pick("copy-remote-link", window, cx);
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(s.history.borrow().len(), 2);
        let head = scm::head_revision(Path::new(&s.path)).unwrap();
        let copied = cx.update(|cx| cx.read_from_clipboard().and_then(|c| c.text()));
        assert_eq!(
            copied.as_deref(),
            Some(format!("https://github.com/acme/app/blob/{head}/a.txt#L2-L3").as_str())
        );
    }
}
