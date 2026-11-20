//! What the rest of the window asks of the Database view: the footer's
//! data source menu (connect, reveal, agent access, import) and agents'
//! statements, which show in an "Agent" console of their data source.
use super::*;
use monocode_core::db::McpAccess;

/// The name of the console agents' statements are written to.
const AGENT_CONSOLE: &str = "Agent";

/// How a data source's own connection stands, for the footer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LinkState {
    Idle,
    Connecting,
    Connected,
    Failed(String),
}

impl DatabaseView {
    pub(crate) fn link_state(&self, id: &str) -> LinkState {
        if self.is_connected(id) {
            return LinkState::Connected;
        }
        match self.nodes.get(id).map(|n| &n.link) {
            Some(Link::Connecting) => LinkState::Connecting,
            Some(Link::Failed(error)) => LinkState::Failed(error.clone()),
            _ => LinkState::Idle,
        }
    }

    /// Opens the source's connection without showing the view.
    pub(crate) fn connect(&mut self, id: &str, cx: &mut Context<Self>) {
        // A failed attempt is not "connecting": try again.
        if let Some(node) = self.nodes.get_mut(id)
            && matches!(node.link, Link::Failed(_))
        {
            node.link = Link::Idle;
        }
        self.connect_source(id, cx);
    }

    pub(crate) fn disconnect(&mut self, id: &str, cx: &mut Context<Self>) {
        self.disconnect_source(id, cx);
    }

    /// Selects the source, opens its tree and connects it, for a view that
    /// is about to be shown.
    pub(crate) fn reveal(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.source(id).is_none() {
            return;
        }
        self.select_source(id, cx);
        self.expanded.insert(format!("s:{id}"));
        self.connect(id, cx);
        self.save_state();
        cx.notify();
    }

    /// The data source dialog: a source to edit, or a new one.
    pub(crate) fn edit_source(
        &mut self,
        id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = &id {
            self.select_source(id, cx);
        }
        self.open_dialog(id, window, cx);
    }

    pub(crate) fn set_mcp_access(&mut self, id: &str, access: McpAccess, cx: &mut Context<Self>) {
        let Some(source) = self.sources.iter_mut().find(|s| s.id == id) else {
            return;
        };
        if source.mcp == access {
            return;
        }
        source.mcp = access;
        if let Err(error) = self.store.save(&self.sources) {
            self.notice = Some(format!("Data sources were not saved: {error}"));
        }
        monocode_core::db_bridge::forget_source(id);
        cx.notify();
    }

    /// Adds sources found elsewhere (dbports, SOCket, the shell), with the
    /// passwords that came with them. Returns how many were added.
    pub(crate) fn import(
        &mut self,
        found: Vec<(DataSource, Option<String>)>,
        cx: &mut Context<Self>,
    ) -> usize {
        let mut added = 0;
        for (mut data_source, password) in found {
            if let Some(password) = password.filter(|p| !p.is_empty()) {
                match source::save_password(&data_source.id, &password) {
                    Ok(()) => data_source.save_password = true,
                    Err(error) => {
                        self.notice = Some(format!("A password was not saved: {error}"));
                        self.passwords.insert(data_source.id.clone(), password);
                    }
                }
            }
            self.sources.push(data_source);
            added += 1;
        }
        if added > 0 {
            if let Err(error) = self.store.save(&self.sources) {
                self.notice = Some(format!("Data sources were not saved: {error}"));
            }
            self.reselect_shown(cx);
            cx.notify();
        }
        added
    }

    /// The console agents' statements go to, made the first time.
    fn agent_console(&mut self, source: &str, cx: &mut Context<Self>) -> Option<usize> {
        self.source(source)?;
        let saved = self
            .console_store
            .list(source)
            .into_iter()
            .find(|id| self.console_store.name(source, *id).as_deref() == Some(AGENT_CONSOLE));
        if let Some(id) = saved {
            if self.console_index(source, id).is_none() {
                let text = self.console_store.load(source, id);
                self.add_console(source, id, text, cx);
                self.remember_open(source);
            }
            return self.console_index(source, id);
        }
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
        let _ = self.console_store.save(source, id, "");
        self.console_store.set_name(source, id, Some(AGENT_CONSOLE));
        self.add_console(source, id, String::new(), cx);
        self.refresh_saved(source);
        self.remember_open(source);
        self.save_state();
        self.console_index(source, id)
    }

    /// An agent ran `statement` on `source`: it is added to the source's
    /// Agent console with what came back, so the user sees what agents do
    /// and can run it again themselves.
    pub(crate) fn show_agent_statement(
        &mut self,
        source: &str,
        statement: &str,
        tool: &str,
        result: Option<QueryResult>,
        error: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self.agent_console(source, cx) else {
            return;
        };
        let comment = self.kind_of(source).line_comment();
        let time = chrono::Local::now().format("%H:%M:%S");
        let mut text = format!("{comment} {tool} · {time}\n{}", statement.trim());
        if self.kind_of(source) == DbKind::Postgres && !text.ends_with(';') {
            text.push(';');
        }
        self.append_to_console(ix, &text, cx);
        let content = self.consoles[ix].editor.read(cx).content.clone();
        let _ = self
            .console_store
            .save(source, self.consoles[ix].id, &content);
        let console = &mut self.consoles[ix];
        match (result, error) {
            (_, Some(error)) => {
                console.error = Some((DbError::plain(error), statement.to_owned()));
            }
            (Some(result), None) => {
                console.error = None;
                console.all = vec![Arc::new(result)];
                console.select_result(0);
                console.note = Some("Run by an agent".into());
                console.paging = None;
                console.export_sql = None;
            }
            (None, None) => {}
        }
        // The user's own console stays in front; a source with no console
        // shown yet shows this one.
        if !self.active.contains_key(source) {
            self.active.insert(source.to_owned(), self.consoles[ix].id);
        }
        cx.notify();
    }
}
