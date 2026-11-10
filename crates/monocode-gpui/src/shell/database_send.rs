//! Sending text, or a whole file, from the workspace to a data source's
//! console. The Database view is made if it was never opened but is not
//! shown: the user stays where they were, and the notice says where it went.
use super::*;
use crate::database::{MAX_SEND_BYTES, Origin};

pub(super) enum DbSend {
    /// Text selected in a file; `lines` are 1-based when it is part of one.
    Text {
        path: String,
        source: String,
        text: String,
        lines: Option<(usize, usize)>,
    },
    /// A file, read when it is sent.
    File { path: String, source: String },
}

impl Shell {
    /// Where the Database view is told a file came from.
    fn database_origin(&self, path: &str, lines: Option<(usize, usize)>) -> Origin {
        let shown = std::path::Path::new(path)
            .strip_prefix(&self.workspace.project)
            .unwrap_or_else(|_| std::path::Path::new(path))
            .to_string_lossy()
            .into_owned();
        let label = match lines {
            Some((a, b)) if a == b => format!("{shown} (line {a})"),
            Some((a, b)) => format!("{shown} (lines {a}-{b})"),
            None => shown,
        };
        Origin {
            path: path.to_owned(),
            label,
            name: std::path::Path::new(path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.to_owned()),
        }
    }

    /// The file a detached file tab is showing, when it is one.
    pub(super) fn tab_file(&self, pane: i64) -> Option<String> {
        if !self.workspace.review_owners.contains_key(&pane) {
            return None;
        }
        let state = self.workspace.reviews.get(&pane)?;
        match state.tabs.get(state.active)? {
            crate::review::ReviewTab::File { path, .. } => Some(path.clone()),
            _ => None,
        }
    }

    /// Queues a whole file for a data source.
    pub(super) fn send_file_to_database(
        &mut self,
        path: String,
        source: String,
        cx: &mut Context<Self>,
    ) {
        self.pending_database_sends
            .push(DbSend::File { path, source });
        cx.notify();
    }

    fn database_send_failed(&mut self, message: String, cx: &mut Context<Self>) {
        self.notification_state
            .file_error
            .update(cx, |notice, cx| notice.set_message(Some(message), cx));
        cx.notify();
    }

    pub(super) fn flush_database_sends(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for send in std::mem::take(&mut self.pending_database_sends) {
            match send {
                DbSend::Text {
                    path,
                    source,
                    text,
                    lines,
                } => self.deliver_to_database(path, source, text, lines, window, cx),
                DbSend::File { path, source } => {
                    let read = {
                        let path = path.clone();
                        cx.background_executor().spawn(async move {
                            let meta = std::fs::metadata(&path).map_err(|e| e.to_string())?;
                            if meta.len() as usize > MAX_SEND_BYTES {
                                return Err("That file is too large to send (over 1 MB)".to_owned());
                            }
                            let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
                            String::from_utf8(bytes)
                                .map_err(|_| "Only text files can be sent".to_owned())
                        })
                    };
                    cx.spawn_in(window, async move |this, cx| {
                        let text = read.await;
                        let _ = this.update_in(cx, |this, window, cx| match text {
                            Ok(text) => {
                                this.deliver_to_database(path, source, text, None, window, cx)
                            }
                            Err(error) => this.database_send_failed(error, cx),
                        });
                    })
                    .detach();
                }
            }
        }
    }

    fn deliver_to_database(
        &mut self,
        path: String,
        source: String,
        text: String,
        lines: Option<(usize, usize)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let origin = self.database_origin(&path, lines);
        let view = self.ensure_database(window, cx);
        match view.update(cx, |view, cx| view.send_text(&source, &origin, &text, cx)) {
            Ok(message) => self
                .notification_state
                .database_sent
                .update(cx, |notice, cx| notice.show(message, cx)),
            Err(error) => self.database_send_failed(error, cx),
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("seed")).unwrap();
        std::fs::write(
            dir.path().join("seed/one.sql"),
            "insert into t values (1);\n",
        )
        .unwrap();
        dir
    }

    fn settle(cx: &mut TestAppContext) {
        for _ in 0..4 {
            cx.refresh().unwrap();
            cx.run_until_parked();
        }
    }

    #[gpui::test]
    fn text_and_files_reach_a_console_while_the_view_stays_closed(cx: &mut TestAppContext) {
        let db = tempfile::tempdir().unwrap();
        let (source, _) = crate::database::write_sources(db.path(), &["alpha", "beta"]);
        let dir = project();
        let cwd = dir.path().to_string_lossy().to_string();
        let file = dir
            .path()
            .join("seed/one.sql")
            .to_string_lossy()
            .to_string();
        let (window, _, _) = super::super::tests::window(cx);
        window
            .update(cx, |shell, _, cx| {
                shell.database_dir = db.path().to_owned();
                shell.workspace.project = cwd.clone();
                assert!(shell.database.is_none(), "never opened");
                shell.pending_database_sends.push(DbSend::Text {
                    path: file.clone(),
                    source: source.clone(),
                    text: "insert into t values (7);".into(),
                    lines: Some((3, 4)),
                });
                shell.send_file_to_database(file.clone(), source.clone(), cx);
            })
            .unwrap();
        settle(cx);
        window
            .update(cx, |shell, _, cx| {
                assert!(!shell.app_view_open(), "the user is still where they were");
                let view = shell.database.clone().expect("made without being shown");
                let texts: Vec<String> = view.read(cx).console_texts(&source, cx);
                assert_eq!(
                    texts.len(),
                    2,
                    "the empty console it starts with, and the file's"
                );
                assert!(texts[0].is_empty());
                let sent = &texts[1];
                assert!(
                    sent.starts_with(
                        "-- from seed/one.sql (lines 3-4)\ninsert into t values (7);\n"
                    ),
                    "{sent}"
                );
                assert!(
                    sent.ends_with("-- from seed/one.sql\ninsert into t values (1);\n"),
                    "the whole file follows in the same console: {sent}"
                );
                assert!(shell.notification_state.database_sent.read(cx).is_visible());
                assert!(!shell.notification_state.file_error.read(cx).is_visible());
            })
            .unwrap();
    }

    #[gpui::test]
    fn a_missing_source_or_unreadable_file_is_an_error_not_a_send(cx: &mut TestAppContext) {
        let db = tempfile::tempdir().unwrap();
        let (source, _) = crate::database::write_sources(db.path(), &["alpha", "beta"]);
        let dir = project();
        let (window, _, _) = super::super::tests::window(cx);
        window
            .update(cx, |shell, _, cx| {
                shell.database_dir = db.path().to_owned();
                shell.workspace.project = dir.path().to_string_lossy().to_string();
                shell.send_file_to_database(
                    dir.path()
                        .join("seed/one.sql")
                        .to_string_lossy()
                        .to_string(),
                    "gone".into(),
                    cx,
                );
            })
            .unwrap();
        settle(cx);
        window
            .update(cx, |shell, _, cx| {
                assert!(shell.notification_state.file_error.read(cx).is_visible());
                assert!(!shell.notification_state.database_sent.read(cx).is_visible());
                shell
                    .notification_state
                    .file_error
                    .update(cx, |n, cx| n.set_message(None, cx));
                shell.send_file_to_database(
                    dir.path()
                        .join("seed/nothing.sql")
                        .to_string_lossy()
                        .to_string(),
                    source.clone(),
                    cx,
                );
            })
            .unwrap();
        settle(cx);
        window
            .update(cx, |shell, _, cx| {
                assert!(shell.notification_state.file_error.read(cx).is_visible());
            })
            .unwrap();
    }

    #[gpui::test]
    fn a_detached_file_tab_names_its_file(cx: &mut TestAppContext) {
        let (window, _, owner) = super::super::tests::window(cx);
        window
            .update(cx, |shell, _, _| {
                let pane = shell.workspace.next_pane_id();
                shell.workspace.review_owners.insert(pane, owner);
                shell.workspace.reviews.insert(
                    pane,
                    crate::review::ReviewState {
                        tabs: vec![crate::review::ReviewTab::File {
                            cwd: "/p".into(),
                            path: "/p/a.sql".into(),
                        }],
                        ..Default::default()
                    },
                );
                assert_eq!(shell.tab_file(pane).as_deref(), Some("/p/a.sql"));
                assert_eq!(shell.tab_file(owner), None, "a session is no file");
            })
            .unwrap();
    }
}
