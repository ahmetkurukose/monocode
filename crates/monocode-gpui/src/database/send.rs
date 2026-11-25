//! Text sent from a file to a data source's console, without leaving the file.
//! Each file gets a console of its own, named after it, and later text from
//! the same file goes to that console; consoles that were already there are
//! never touched.
use super::*;

/// The most one send may carry.
pub(crate) const MAX_SEND_BYTES: usize = 1024 * 1024;

/// A data source offered as a destination.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Target {
    pub id: String,
    pub name: String,
    pub color: Option<monocode_core::db::DbColor>,
    pub read_only: bool,
    pub kind: DbKind,
}

/// What a statement sent from a file does once it is in its console.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RunMode {
    Execute,
    Explain,
    ExplainAnalyze,
}

/// The saved data sources, the one used last first, read from `dir` so a file
/// editor can offer them without the Database view being open. Sources that
/// belong to other projects than `place`'s are left out.
pub(crate) fn send_targets(dir: &std::path::Path, place: &str) -> Vec<Target> {
    let mut sources = DataSourceStore::new(dir).load();
    sources.retain(|source| source.in_project(place));
    let last: Option<String> = std::fs::read(dir.join("state.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<SavedState>(&bytes).ok())
        .and_then(|state| state.send_target);
    let mut targets: Vec<Target> = sources
        .into_iter()
        .map(|s| Target {
            id: s.id,
            name: s.name,
            color: s.color,
            read_only: s.read_only,
            kind: s.kind,
        })
        .collect();
    targets.sort_by_key(|t| (Some(&t.id) != last.as_ref(), t.name.to_lowercase()));
    targets
}

/// What text came from, for the comment above it and the console's name.
#[derive(Clone, Debug)]
pub(crate) struct Origin {
    /// The file, as a key (its full path).
    pub path: String,
    /// What the comment says: the file as the project knows it, and the lines.
    pub label: String,
    /// The console's name.
    pub name: String,
}

impl DatabaseView {
    /// What each open console of a source holds, in tab order.
    #[cfg(test)]
    pub(crate) fn console_texts(&self, source: &str, cx: &gpui::App) -> Vec<String> {
        self.consoles
            .iter()
            .filter(|c| c.source == source)
            .map(|c| c.editor.read(cx).content.to_string())
            .collect()
    }

    /// The active console of `source`: its error and the statement it names.
    #[cfg(test)]
    pub(crate) fn console_error(&self, source: &str) -> Option<(String, String)> {
        let id = self.active.get(source)?;
        let ix = self.console_index(source, *id)?;
        let (error, statement) = self.consoles[ix].error.clone()?;
        Some((error.message, statement))
    }

    /// Whether the active console of `source` is running something.
    #[cfg(test)]
    pub(crate) fn console_running(&self, source: &str) -> bool {
        self.active
            .get(source)
            .and_then(|id| self.console_index(source, *id))
            .is_some_and(|ix| self.consoles[ix].running)
    }

    /// The selected text of the active console of `source`.
    #[cfg(test)]
    pub(crate) fn console_selection(&self, source: &str, cx: &gpui::App) -> String {
        let Some(ix) = self
            .active
            .get(source)
            .and_then(|id| self.console_index(source, *id))
        else {
            return String::new();
        };
        let input = self.consoles[ix].editor.read(cx);
        input.content[input.selection_range()].to_string()
    }

    /// Puts `text` in the console of `origin`'s file under `source`, making it
    /// when this file has none yet. Nothing runs unless `run` asks for it,
    /// and the view stays as it is apart from remembering the console as the
    /// one to show.
    pub(crate) fn send_text(
        &mut self,
        source: &str,
        origin: &Origin,
        text: &str,
        run: Option<RunMode>,
        cx: &mut Context<Self>,
    ) -> Result<String, String> {
        if self.source(source).is_none() {
            return Err("That data source no longer exists".into());
        }
        if text.trim().is_empty() {
            return Err("There is nothing to send".into());
        }
        if text.len() > MAX_SEND_BYTES {
            return Err("That is too large to send (over 1 MB)".into());
        }
        self.ensure_consoles(source, cx);
        let head = format!(
            "{} from {}\n",
            self.kind_of(source).line_comment(),
            origin.label
        );
        let block = format!(
            "{head}{text}{}",
            if text.ends_with('\n') { "" } else { "\n" }
        );
        let block_len = block.len();
        let key = format!("{source}\0{}", origin.path);
        let existing = self
            .sent
            .get(&key)
            .copied()
            .filter(|id| self.console_store.list(source).contains(id));
        let id = match existing {
            Some(id) => {
                let mut full = match self.console_index(source, id) {
                    Some(ix) => self.consoles[ix].editor.read(cx).content.to_string(),
                    None => self.console_store.load(source, id),
                };
                if !full.is_empty() && !full.ends_with('\n') {
                    full.push('\n');
                }
                if !full.trim().is_empty() {
                    full.push('\n');
                }
                full.push_str(&block);
                match self.console_index(source, id) {
                    Some(ix) => {
                        // The console saves what it holds a moment after any edit.
                        self.consoles[ix]
                            .editor
                            .update(cx, |input, cx| input.set_text(full, cx));
                    }
                    None => {
                        self.console_store.save(source, id, &full)?;
                        self.add_console(source, id, full, cx);
                    }
                }
                id
            }
            None => {
                let id = self
                    .console_store
                    .list(source)
                    .into_iter()
                    .chain(
                        self.consoles
                            .iter()
                            .filter(|c| c.source == source)
                            .map(|c| c.id),
                    )
                    .max()
                    .unwrap_or(0)
                    + 1;
                self.console_store.save(source, id, &block)?;
                self.console_store.set_name(source, id, Some(&origin.name));
                self.add_console(source, id, block, cx);
                self.sent.insert(key, id);
                id
            }
        };
        self.remember_open(source);
        self.refresh_saved(source);
        // The next time the view opens it shows this console.
        self.selected = Some(source.to_owned());
        self.active.insert(source.to_owned(), id);
        self.send_target = Some(source.to_owned());
        self.save_state();
        if let Some(mode) = run {
            self.run_sent(source, id, head.len(), block_len, text.len(), mode, cx);
        }
        cx.notify();
        let name = self
            .source(source)
            .map(|s| s.name.clone())
            .unwrap_or_default();
        Ok(format!("{name} · {}", origin.name))
    }
}

impl DatabaseView {
    /// Runs or explains what was just sent: the text `len` bytes long, `head`
    /// bytes into the `block` that ends the console.
    #[allow(clippy::too_many_arguments)]
    fn run_sent(
        &mut self,
        source: &str,
        id: u32,
        head: usize,
        block: usize,
        len: usize,
        mode: RunMode,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self.console_index(source, id) else {
            return;
        };
        if self.consoles[ix].running {
            self.notice = Some("A statement is still running in that console".into());
            return;
        }
        let editor = self.consoles[ix].editor.clone();
        let start = editor.read(cx).content.len() - block + head;
        editor.update(cx, |input, cx| input.select_range(start..start + len, cx));
        match mode {
            RunMode::Execute => self.run(source, id, false, cx),
            RunMode::Explain => self.explain(source, id, false, cx),
            RunMode::ExplainAnalyze => self.explain(source, id, true, cx),
        }
        self.ran = Some((source.to_owned(), id));
    }

    /// Whether a file's run is waiting to be shown.
    pub(crate) fn has_run_to_show(&self) -> bool {
        self.ran.is_some()
    }

    /// Shows the console a file's statement last ran in; call it once the
    /// view is open, as opening may pick another source.
    pub(crate) fn show_run(&mut self, cx: &mut Context<Self>) {
        let Some((source, id)) = self.ran.take() else {
            return;
        };
        if self.source(&source).is_none() {
            return;
        }
        self.selected = Some(source.clone());
        self.ensure_consoles(&source, cx);
        self.active.insert(source, id);
        self.save_state();
        cx.notify();
    }
}

/// Saves data sources with these names under `dir`, for tests elsewhere; their ids.
#[cfg(test)]
pub(crate) fn write_sources(dir: &std::path::Path, names: &[&str]) -> (String, String) {
    let sources: Vec<DataSource> = names.iter().map(|n| DataSource::new(*n)).collect();
    DataSourceStore::new(dir).save(&sources).unwrap();
    (sources[0].id.clone(), sources[1].id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    fn view(cx: &mut TestAppContext, dir: &std::path::Path) -> gpui::WindowHandle<DatabaseView> {
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

    fn source(name: &str) -> DataSource {
        let mut source = DataSource::new(name);
        source.host = "127.0.0.1".into();
        source
    }

    fn origin(path: &str, label: &str, name: &str) -> Origin {
        Origin {
            path: path.into(),
            label: label.into(),
            name: name.into(),
        }
    }

    #[gpui::test]
    fn each_file_gets_its_own_console_and_existing_consoles_stay_untouched(
        cx: &mut TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let window = view(cx, dir.path());
        window
            .update(cx, |view, _, cx| {
                let a = source("a");
                let id = a.id.clone();
                view.save_source(a, None, cx);
                // A console the user already had, with text in it.
                let existing = view.console_index(&id, 1).expect("the first console");
                view.consoles[existing]
                    .editor
                    .update(cx, |input, cx| input.set_text("select 1;".into(), cx));
                let first = view
                    .send_text(&id, &origin("/p/one.sql", "seed/one.sql (lines 1–2)", "one.sql"), "insert into t values (1);\n", None, cx)
                    .unwrap();
                assert!(first.ends_with("one.sql"), "{first}");
                let second = view
                    .send_text(&id, &origin("/p/two.sql", "seed/two.sql", "two.sql"), "insert into t values (2);", None, cx)
                    .unwrap();
                assert!(second.ends_with("two.sql"));
                let ids: Vec<u32> = view.consoles.iter().map(|c| c.id).collect();
                assert_eq!(ids.len(), 3, "the user's console and one per file: {ids:?}");
                let text = |view: &DatabaseView, id: u32, cx: &gpui::App| {
                    let ix = view.console_index(&view.selected.clone().unwrap(), id).unwrap();
                    view.consoles[ix].editor.read(cx).content.to_string()
                };
                let source_id = view.selected.clone().unwrap();
                assert_eq!(text(view, 1, cx), "select 1;", "left alone");
                assert_eq!(
                    text(view, 2, cx),
                    "-- from seed/one.sql (lines 1–2)\ninsert into t values (1);\n"
                );
                assert_eq!(
                    text(view, 3, cx),
                    "-- from seed/two.sql\ninsert into t values (2);\n"
                );
                assert_eq!(view.active.get(&source_id), Some(&3), "the last sent is the one to show");
                assert_eq!(view.console_store.name(&source_id, 2).as_deref(), Some("one.sql"));
                // The same file again goes to its own console, below what is there.
                view.send_text(&id, &origin("/p/one.sql", "seed/one.sql (line 9)", "one.sql"), "insert into t values (9);", None, cx)
                    .unwrap();
                assert_eq!(view.consoles.len(), 3, "no new console for the same file");
                assert_eq!(
                    text(view, 2, cx),
                    "-- from seed/one.sql (lines 1–2)\ninsert into t values (1);\n\n-- from seed/one.sql (line 9)\ninsert into t values (9);\n"
                );
                assert_eq!(view.send_target.as_deref(), Some(id.as_str()));
            })
            .unwrap();
    }

    #[gpui::test]
    fn nothing_empty_oversized_or_for_a_missing_source_is_sent(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let window = view(cx, dir.path());
        window
            .update(cx, |view, _, cx| {
                let a = source("a");
                let id = a.id.clone();
                view.save_source(a, None, cx);
                let o = origin("/p/x.sql", "x.sql", "x.sql");
                assert!(view.send_text(&id, &o, "   \n", None, cx).is_err());
                assert!(view.send_text("missing", &o, "select 1", None, cx).is_err());
                let huge = "x".repeat(MAX_SEND_BYTES + 1);
                assert!(view.send_text(&id, &o, &huge, None, cx).is_err());
                assert_eq!(view.consoles.len(), 1, "nothing was made");
            })
            .unwrap();
    }

    #[test]
    fn targets_list_the_last_used_first_without_the_view_open() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataSourceStore::new(dir.path());
        let (b, a) = (source("b"), source("a"));
        store.save(&[b.clone(), a.clone()]).unwrap();
        let names = |dir: &std::path::Path| {
            send_targets(dir, "/work/a")
                .into_iter()
                .map(|t| t.name)
                .collect::<Vec<_>>()
        };
        assert_eq!(names(dir.path()), ["a", "b"], "by name until one is used");
        std::fs::write(
            dir.path().join("state.json"),
            serde_json::json!({ "send_target": b.id }).to_string(),
        )
        .unwrap();
        assert_eq!(names(dir.path()), ["b", "a"]);
        assert!(send_targets(&dir.path().join("none"), "/work/a").is_empty());
        // A source of another project is not offered from this one's files.
        let mut b = b;
        b.projects = vec!["/work/b".into()];
        store.save(&[b, a]).unwrap();
        assert_eq!(names(dir.path()), ["a"]);
    }
}
