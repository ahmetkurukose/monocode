//! The data sources from the code editor: sending the selection to a
//! source's console, and running or explaining the SQL at the caret.
use super::{ExecuteSql, ExplainSql, FileEditor, FileEvent, selection::db_dot};
use crate::database::{RunMode, Target};
use crate::popover::{MenuAction, MenuItem};
use gpui::{App, Context, Window};
use monocode_core::db::{DbKind, split};
use std::ops::Range;

/// Whether `path` holds SQL, by its extension.
pub(super) fn is_sql_path(path: &str) -> bool {
    let name = path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
        .to_lowercase();
    [".sql", ".pgsql", ".psql"]
        .iter()
        .any(|ext| name.ends_with(ext))
}

/// The statement around `cursor`, split as the console splits it: semicolons
/// in strings, comments and dollar-quoted bodies do not end one.
pub(super) fn statement_at(text: &str, cursor: usize) -> Option<(Range<usize>, String)> {
    split::at_cursor(text, cursor).map(|s| (s.from..s.to, s.text))
}

/// The 1-based lines `range` covers in `text`.
fn lines_of(text: &str, range: &Range<usize>) -> (usize, usize) {
    let line = |at: usize| {
        text.as_bytes()[..at]
            .iter()
            .filter(|&&b| b == b'\n')
            .count()
            + 1
    };
    let first = line(range.start);
    let last = if range.end > range.start {
        line(range.end - 1)
    } else {
        first
    };
    (first, last.max(first))
}

fn shortcut() -> &'static str {
    if cfg!(target_os = "macos") {
        "⌘↩"
    } else {
        "Ctrl+Enter"
    }
}

/// A source row: its colour, and whether it accepts writes.
fn source_row(id: String, source: &Target) -> MenuAction {
    let row = MenuAction::new(id, source.name.clone())
        .icon("dot")
        .icon_color(db_dot(source.color));
    if source.read_only {
        row
    } else {
        row.value("writable")
    }
}

impl FileEditor {
    /// The sources SQL in this file can run on, with their place in `sources`;
    /// none when the file is not SQL.
    fn sql_sources<'a>(&self, sources: &'a [Target]) -> Vec<(usize, &'a Target)> {
        if !is_sql_path(&self.path) {
            return Vec::new();
        }
        sources
            .iter()
            .enumerate()
            .filter(|(_, s)| s.kind == DbKind::Postgres)
            .collect()
    }

    /// What Execute runs: the selection, else the statement at the caret.
    fn sql_to_run(&self, cx: &App) -> Option<(Range<usize>, String)> {
        let input = self.input.read(cx);
        let text = &input.content;
        let range = input.selection_range();
        if !range.is_empty()
            && range.end <= text.len()
            && text.is_char_boundary(range.start)
            && text.is_char_boundary(range.end)
            && !text[range.clone()].trim().is_empty()
        {
            return Some((range.clone(), text[range].to_string()));
        }
        statement_at(text, range.start.min(text.len()))
    }

    /// Sends the statement to run to `source`'s console for this file, asking
    /// the console to run it as `mode`.
    fn run_sql(&mut self, source: &Target, mode: RunMode, cx: &mut Context<Self>) {
        let Some((range, text)) = self.sql_to_run(cx) else {
            return;
        };
        let (first_line, last_line) = lines_of(&self.input.read(cx).content, &range);
        cx.emit(FileEvent::SendToDatabase {
            source: source.id.clone(),
            text,
            first_line,
            last_line,
            run: Some(mode),
        });
    }

    /// The code menu's data source rows.
    pub(super) fn sql_menu_items(&self, sources: &[Target], cx: &App) -> Vec<MenuItem> {
        let mut items = Vec::new();
        let runnable = self.sql_sources(sources);
        if !runnable.is_empty() {
            let nothing = self.sql_to_run(cx).is_none();
            items.extend([
                MenuAction::new("sql-execute", "Execute")
                    .icon("play")
                    .shortcut(shortcut())
                    .disabled(nothing)
                    .into(),
                MenuAction::new("sql-execute-in", "Execute in")
                    .disabled(nothing)
                    .submenu(
                        runnable
                            .iter()
                            .map(|(ix, s)| source_row(format!("sql-exec:{ix}"), s))
                            .collect(),
                    )
                    .into(),
                MenuAction::new("sql-explain", "Explain Plan")
                    .disabled(nothing)
                    .submenu(vec![
                        MenuAction::new("sql-explain:plan", "Explain"),
                        MenuAction::new("sql-explain:analyze", "Explain Analyze"),
                    ])
                    .into(),
            ]);
        }
        let selected = !self.input.read(cx).selection_range().is_empty();
        if selected && !sources.is_empty() {
            items.push(
                MenuAction::new("database", "Send to Database")
                    .icon("database")
                    .submenu(
                        sources
                            .iter()
                            .enumerate()
                            .map(|(ix, source)| source_row(format!("db:{ix}"), source))
                            .collect(),
                    )
                    .into(),
            );
        }
        items
    }
    /// Takes a picked row of `sql_menu_items`; `false` when it is not one.
    pub(super) fn sql_menu_pick(
        &mut self,
        id: &str,
        target: &super::selection::Target,
        sources: &[Target],
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let first = || self.sql_sources(sources).first().map(|(_, s)| (*s).clone());
        let run = match id {
            "sql-execute" => first().map(|s| (s, RunMode::Execute)),
            "sql-explain:plan" => first().map(|s| (s, RunMode::Explain)),
            "sql-explain:analyze" => first().map(|s| (s, RunMode::ExplainAnalyze)),
            _ => match id.strip_prefix("sql-exec:") {
                Some(ix) => ix
                    .parse::<usize>()
                    .ok()
                    .and_then(|ix| sources.get(ix))
                    .map(|s| (s.clone(), RunMode::Execute)),
                None => {
                    let Some(ix) = id
                        .strip_prefix("db:")
                        .and_then(|ix| ix.parse::<usize>().ok())
                    else {
                        return false;
                    };
                    if let (Some(source), Some(text)) =
                        (sources.get(ix), self.selected_text(&target.range, cx))
                    {
                        cx.emit(FileEvent::SendToDatabase {
                            source: source.id.clone(),
                            text,
                            first_line: target.lines.0,
                            last_line: target.lines.1,
                            run: None,
                        });
                    }
                    return true;
                }
            },
        };
        if let Some((source, mode)) = run {
            self.run_sql(&source, mode, cx);
        }
        true
    }
    /// Execute (⌘↩ / Ctrl+Enter): the statement at the caret, on the source used last.
    pub(super) fn execute_sql(&mut self, _: &ExecuteSql, _: &mut Window, cx: &mut Context<Self>) {
        self.run_first(RunMode::Execute, cx);
    }
    /// The plan of the statement at the caret, on the source used last.
    pub(super) fn explain_sql(&mut self, _: &ExplainSql, _: &mut Window, cx: &mut Context<Self>) {
        self.run_first(RunMode::Explain, cx);
    }
    fn run_first(&mut self, mode: RunMode, cx: &mut Context<Self>) {
        let sources = crate::database::send_targets(&self.database_dir, &self.cwd);
        match self.sql_sources(&sources).first() {
            Some((_, source)) => {
                let source = (*source).clone();
                self.run_sql(&source, mode, cx);
            }
            None => cx.propagate(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::DatabaseView;
    use gpui::{AppContext, Focusable, TestAppContext, WindowHandle};
    use std::{cell::RefCell, rc::Rc};

    #[test]
    fn the_statement_at_the_caret_ignores_semicolons_in_strings_comments_and_bodies() {
        let text = "select 'a;b' as x; -- not; here\n\
                    create function f() returns int as $$ begin; return 1; end $$ language plpgsql;\n\
                    /* ; */ select 2";
        let at = |needle: &str| statement_at(text, text.find(needle).unwrap()).unwrap().1;
        assert_eq!(at("'a;b'"), "select 'a;b' as x");
        assert_eq!(
            at("return 1"),
            "-- not; here\ncreate function f() returns int as $$ begin; return 1; end $$ language plpgsql"
        );
        assert!(at("select 2").ends_with("select 2"));
        // Just after a semicolon is still the statement it ends.
        assert_eq!(
            statement_at("select 1;\n\nselect 2;", 9).unwrap().1,
            "select 1"
        );
        assert_eq!(statement_at("  \n", 1), None);
        assert_eq!(lines_of("a\nb\nc\n", &(2..6)), (2, 3));
        assert!(is_sql_path("/p/seed/One.SQL") && is_sql_path("q.pgsql"));
        assert!(!is_sql_path("/p/sql/main.rs"));
    }

    /// What the file sent: the source, the text and the run it asked for.
    type Sent = (String, String, Option<RunMode>);

    struct Setup {
        window: WindowHandle<FileEditor>,
        events: Rc<RefCell<Vec<Sent>>>,
        /// The source used last, then the other one.
        sources: (String, String),
        db: tempfile::TempDir,
        _dir: tempfile::TempDir,
    }

    fn setup(cx: &mut TestAppContext, name: &str, text: &str) -> Setup {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(name);
        std::fs::write(&file, text).unwrap();
        let db = tempfile::tempdir().unwrap();
        let (a, b) = crate::database::write_sources(db.path(), &["alpha", "beta"]);
        std::fs::write(
            db.path().join("state.json"),
            serde_json::json!({ "send_target": b }).to_string(),
        )
        .unwrap();
        cx.update(|cx| {
            cx.set_global(crate::settings::Preferences::default());
            crate::popover::bind(cx);
            crate::input::bind(cx);
            super::super::bind(cx);
        });
        let path = file.to_string_lossy().into_owned();
        let database_dir = db.path().to_owned();
        let root = dir.path().to_string_lossy().into_owned();
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |_, cx| {
                let git = cx.new(|cx| crate::git_status::ProjectGit::new(root, cx));
                cx.new(|cx| {
                    let mut editor = FileEditor::new(path, git, cx);
                    editor.database_dir = database_dir;
                    editor
                })
            })
            .unwrap()
        });
        let events = Rc::new(RefCell::new(Vec::new()));
        let log = events.clone();
        window
            .update(cx, |editor, _, cx| {
                cx.subscribe(&cx.entity(), move |_, _, event: &FileEvent, _| {
                    if let FileEvent::SendToDatabase {
                        source, text, run, ..
                    } = event
                    {
                        log.borrow_mut().push((source.clone(), text.clone(), *run));
                    }
                })
                .detach();
                editor.set_visible(true, cx);
            })
            .unwrap();
        cx.run_until_parked();
        Setup {
            window,
            events,
            sources: (b, a),
            db,
            _dir: dir,
        }
    }

    fn row_ids(editor: &FileEditor, cx: &App) -> Vec<String> {
        let sources = crate::database::send_targets(&editor.database_dir, &editor.cwd);
        editor
            .sql_menu_items(&sources, cx)
            .into_iter()
            .filter_map(|item| match item {
                MenuItem::Action(a) => Some(a.id.to_string()),
                _ => None,
            })
            .collect()
    }

    const SCRIPT: &str = "select 1;\nupdate t set a = 1 where id = 2;\n";

    #[gpui::test]
    fn sql_files_offer_execute_and_explain_and_other_files_only_send(cx: &mut TestAppContext) {
        let sql = setup(cx, "q.sql", SCRIPT);
        sql.window
            .update(cx, |editor, _, cx| {
                assert_eq!(
                    row_ids(editor, cx),
                    ["sql-execute", "sql-execute-in", "sql-explain"]
                );
                editor
                    .input
                    .update(cx, |input, cx| input.select_range(0..8, cx));
                assert_eq!(
                    row_ids(editor, cx),
                    ["sql-execute", "sql-execute-in", "sql-explain", "database"]
                );
            })
            .unwrap();
        let rust = setup(cx, "main.rs", "fn main() {}\n");
        rust.window
            .update(cx, |editor, _, cx| {
                assert!(row_ids(editor, cx).is_empty());
                editor
                    .input
                    .update(cx, |input, cx| input.select_range(0..2, cx));
                assert_eq!(row_ids(editor, cx), ["database"]);
            })
            .unwrap();
    }

    #[gpui::test]
    fn cmd_enter_executes_the_statement_at_the_caret_on_the_source_used_last(
        cx: &mut TestAppContext,
    ) {
        let s = setup(cx, "q.sql", SCRIPT);
        s.window
            .update(cx, |editor, window, cx| {
                window.focus(&editor.input.focus_handle(cx), cx);
                editor
                    .input
                    .update(cx, |input, cx| input.select_range(15..15, cx));
            })
            .unwrap();
        cx.run_until_parked();
        let key = if cfg!(target_os = "macos") {
            "cmd-enter"
        } else {
            "ctrl-enter"
        };
        cx.simulate_keystrokes(s.window.into(), key);
        cx.run_until_parked();
        assert_eq!(
            *s.events.borrow(),
            [(
                s.sources.0.clone(),
                "update t set a = 1 where id = 2".to_owned(),
                Some(RunMode::Execute)
            )]
        );
        let text = s
            .window
            .read_with(cx, |editor, cx| editor.input.read(cx).content.to_string())
            .unwrap();
        assert_eq!(text, SCRIPT, "the key did not type into the file");
    }

    fn database(cx: &mut TestAppContext, dir: &std::path::Path) -> WindowHandle<DatabaseView> {
        let dir = dir.to_owned();
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|cx| DatabaseView::new(dir, cx))
            })
            .unwrap()
        });
        cx.run_until_parked();
        window
    }

    /// Delivers what the file sent, as the shell does, then checks the view
    /// before anything it started can finish.
    fn deliver(
        cx: &mut TestAppContext,
        view: WindowHandle<DatabaseView>,
        events: &Rc<RefCell<Vec<Sent>>>,
        check: impl FnOnce(&mut DatabaseView, &mut Context<DatabaseView>),
    ) {
        let sent: Vec<_> = events.borrow_mut().drain(..).collect();
        view.update(cx, |view, _, cx| {
            for (source, text, run) in sent {
                let origin = crate::database::Origin {
                    path: "/p/q.sql".into(),
                    label: "q.sql".into(),
                    name: "q.sql".into(),
                };
                view.send_text(&source, &origin, &text, run, cx).unwrap();
            }
            check(view, cx);
        })
        .unwrap();
    }

    #[gpui::test]
    fn execute_and_explain_reach_the_console_and_read_only_sources_refuse_writes(
        cx: &mut TestAppContext,
    ) {
        let s = setup(cx, "q.sql", SCRIPT);
        // Nothing listens on port 1, so a run fails without a server.
        let mut sources = monocode_core::db::DataSourceStore::new(s.db.path()).load();
        for source in &mut sources {
            source.port = 1;
        }
        monocode_core::db::DataSourceStore::new(s.db.path())
            .save(&sources)
            .unwrap();
        let view = database(cx, s.db.path());
        let (last, other) = s.sources.clone();
        // The write at the caret, on the source used last: read-only, so refused.
        s.window
            .update(cx, |editor, window, cx| {
                editor
                    .input
                    .update(cx, |input, cx| input.select_range(15..15, cx));
                editor.execute_sql(&ExecuteSql, window, cx);
            })
            .unwrap();
        deliver(cx, view, &s.events, |view, cx| {
            assert!(view.has_run_to_show());
            view.show_run(cx);
            assert!(!view.has_run_to_show());
            let (error, statement) = view.console_error(&last).expect("refused");
            assert!(error.contains("read-only"), "{error}");
            assert_eq!(statement, "update t set a = 1 where id = 2");
            assert!(!view.console_running(&last), "it never reached the server");
        });
        cx.run_until_parked();
        // Explain the selected read: plain EXPLAIN runs nothing, so read-only is fine.
        s.window
            .update(cx, |editor, window, cx| {
                editor
                    .input
                    .update(cx, |input, cx| input.select_range(0..8, cx));
                let target = editor.menu_target(cx);
                let sources = crate::database::send_targets(&editor.database_dir, &editor.cwd);
                assert!(editor.sql_menu_pick("sql-explain:plan", &target, &sources, window, cx));
            })
            .unwrap();
        assert_eq!(
            s.events.borrow()[0].2,
            Some(RunMode::Explain),
            "the run rides on the event"
        );
        deliver(cx, view, &s.events, |view, cx| {
            assert!(view.console_running(&last), "the plan was asked for");
            assert_eq!(view.console_selection(&last, cx), "select 1");
        });
        cx.run_until_parked();
        // Execute in another source, then a plain send: only the run asked for runs.
        s.window
            .update(cx, |editor, window, cx| {
                let target = editor.menu_target(cx);
                let sources = crate::database::send_targets(&editor.database_dir, &editor.cwd);
                assert_eq!(sources[1].id, other);
                editor.sql_menu_pick("sql-exec:1", &target, &sources, window, cx);
                editor.sql_menu_pick("db:1", &target, &sources, window, cx);
            })
            .unwrap();
        let runs: Vec<_> = s.events.borrow().iter().map(|e| e.2).collect();
        assert_eq!(
            runs,
            [Some(RunMode::Execute), None],
            "a plain send asks for no run"
        );
        deliver(cx, view, &s.events, |view, cx| {
            assert!(view.console_running(&other), "Execute in started on beta");
            assert_eq!(
                view.console_texts(&other, cx)
                    .last()
                    .unwrap()
                    .matches("select 1")
                    .count(),
                2,
                "both sends landed"
            );
        });
    }
}
