//! The footer's Databases button: the project's data sources at a glance,
//! which are connected, whose tunnel is open, and what agents may do with
//! each through the built-in `monocode_db` MCP server. Its menu connects,
//! opens tunnels, changes agent access, and finds connections that already
//! exist elsewhere (dbports, SOCket, the shell's aliases, running proxies).
//!
//! The state is polled, not pushed: tunnels are other processes, so only a
//! look at the listening ports says whether they are up. The look is cheap
//! (one `lsof`) and the footer redraws only when what it shows changed.
use super::*;
use crate::database::LinkState;
use monocode_core::db::{
    DataSource, DataSourceStore, McpAccess,
    discover::{self, Found},
    tunnel::{self, TunnelState},
};
use monocode_core::db_bridge::{self, Phase, SourceStats};
use std::time::Duration;

/// How long a tunnel may take to open from the menu.
const TUNNEL_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Default)]
pub(super) struct DbFooter {
    /// Every saved data source; the button lists the project's.
    sources: Vec<DataSource>,
    /// Tunnel states by local port.
    tunnels: HashMap<u16, TunnelState>,
    stats: HashMap<String, SourceStats>,
    /// What "Find existing connections" found, and which are ticked.
    found: Vec<Found>,
    chosen: HashSet<usize>,
    scanning: bool,
    /// Ports whose tunnel the menu is opening.
    opening: HashSet<u16>,
    /// What the button and menu last showed, to redraw only on a change.
    shown: String,
}

/// One data source's state, the way the footer says it.
#[derive(Clone, Debug, PartialEq)]
enum Status {
    Connected,
    Connecting,
    TunnelOpen,
    TunnelOpening,
    Failed(String),
    PortBusy(String),
    Idle,
}

impl Status {
    fn color(&self) -> gpui::Hsla {
        match self {
            Self::Connected => rgb(0x34d399).into(),
            Self::Connecting | Self::TunnelOpening => rgb(0xfbbf24).into(),
            Self::TunnelOpen => rgb(0x60a5fa).into(),
            Self::Failed(_) => rgb(0xf87171).into(),
            Self::PortBusy(_) => rgb(0xfb923c).into(),
            Self::Idle => content(0.3),
        }
    }

    fn text(&self) -> String {
        match self {
            Self::Connected => "Connected".into(),
            Self::Connecting => "Connecting…".into(),
            Self::TunnelOpen => "Tunnel open".into(),
            Self::TunnelOpening => "Opening tunnel…".into(),
            Self::Failed(error) => first_line(error),
            Self::PortBusy(by) => format!("Port used by {by}"),
            Self::Idle => "Not connected".into(),
        }
    }

    fn settling(&self) -> bool {
        matches!(self, Self::Connecting | Self::TunnelOpening)
    }
}

fn first_line(text: &str) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let mut short: String = line.chars().take(90).collect();
    if line.chars().count() > 90 {
        short.push('…');
    }
    short
}

fn home() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_default()
}

impl Shell {
    fn tunnel_dir(&self) -> std::path::PathBuf {
        self.database_dir.join("tunnels")
    }

    /// The data sources offered in this project, in saved order.
    fn footer_sources(&self) -> Vec<&DataSource> {
        let project = &self.workspace.project;
        self.db_footer
            .sources
            .iter()
            .filter(|s| s.in_project(project))
            .collect()
    }

    fn source_status(&self, source: &DataSource, cx: &App) -> Status {
        let link = self
            .database
            .as_ref()
            .map_or(LinkState::Idle, |view| view.read(cx).link_state(&source.id));
        match link {
            LinkState::Connected => return Status::Connected,
            LinkState::Connecting => return Status::Connecting,
            LinkState::Failed(error) => return Status::Failed(error),
            LinkState::Idle => {}
        }
        if source.tunnel.is_none() {
            return Status::Idle;
        }
        if self.db_footer.opening.contains(&source.port) {
            return Status::TunnelOpening;
        }
        match self.db_footer.tunnels.get(&source.port) {
            Some(TunnelState::Running { .. }) => Status::TunnelOpen,
            Some(TunnelState::Starting) => Status::TunnelOpening,
            Some(TunnelState::Failed(error)) => Status::Failed(error.clone()),
            Some(TunnelState::Busy { command, pid }) => {
                Status::PortBusy(format!("{command} (pid {pid})"))
            }
            Some(TunnelState::Stopped) | None => Status::Idle,
        }
    }

    /// What the footer shows, as text: when it changes, the footer redraws.
    fn footer_signature(&self, cx: &App) -> String {
        let mut signature = String::new();
        for source in self.footer_sources() {
            let stats = self.db_footer.stats.get(&source.id);
            signature.push_str(&format!(
                "{}|{}|{:?}|{:?}|{}|{};",
                source.id,
                source.name,
                self.source_status(source, cx),
                source.mcp,
                stats.map_or(0, |s| s.calls),
                stats.map_or(0, |s| s.running),
            ));
        }
        signature.push_str(&format!("{}", self.db_footer.scanning));
        signature
    }

    /// Starts the footer's look at data sources and tunnels, and listens to
    /// agents' calls. Once, with the window.
    pub(super) fn start_database_footer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        monocode_core::db::conn::set_tunnel_dir(self.tunnel_dir());
        cx.spawn_in(window, async move |this, cx| {
            loop {
                let Ok((store, tunnel_dir, ports)) = this.update(cx, |this, _| {
                    let ports: Vec<(u16, monocode_core::db::Tunnel)> = this
                        .db_footer
                        .sources
                        .iter()
                        .filter_map(|s| s.tunnel.clone().map(|t| (s.port, t)))
                        .collect();
                    (this.database_dir.clone(), this.tunnel_dir(), ports)
                }) else {
                    break;
                };
                let (sources, tunnels) = cx
                    .background_executor()
                    .spawn(async move {
                        let sources = DataSourceStore::new(&store).load();
                        let mut tunnels = HashMap::new();
                        // Tunnels are looked at only when some source has one.
                        let wanted: Vec<_> = sources
                            .iter()
                            .filter_map(|s| s.tunnel.clone().map(|t| (s.port, t)))
                            .chain(ports)
                            .collect();
                        if !wanted.is_empty() {
                            let listeners = tunnel::listeners();
                            for (port, spec) in wanted {
                                tunnels
                                    .entry(port)
                                    .or_insert_with(|| tunnel::state(&spec, port, &listeners, &tunnel_dir));
                            }
                        }
                        (sources, tunnels)
                    })
                    .await;
                let Ok(wait) = this.update(cx, |this, cx| {
                    this.db_footer.sources = sources;
                    this.db_footer.tunnels = tunnels;
                    this.db_footer.stats = db_bridge::stats();
                    this.database_footer_changed(cx);
                    let settling = this
                        .footer_sources()
                        .iter()
                        .any(|s| this.source_status(s, cx).settling());
                    let menu_open = this.menu_open(&MenuTarget::Databases);
                    let has_tunnels = this.db_footer.sources.iter().any(|s| s.tunnel.is_some());
                    if settling || menu_open {
                        1
                    } else if has_tunnels {
                        3
                    } else {
                        10
                    }
                }) else {
                    break;
                };
                cx.background_executor()
                    .timer(Duration::from_secs(wait))
                    .await;
            }
        })
        .detach();
        let Ok(activity) = db_bridge::activity() else {
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            while let Ok(event) = activity.recv().await {
                let alive = this.update_in(cx, |this, window, cx| {
                    this.db_footer.stats = db_bridge::stats();
                    if matches!(event.phase, Phase::Finished)
                        && let Some(statement) = event.statement.as_deref()
                    {
                        let view = this.ensure_database(window, cx);
                        view.update(cx, |view, cx| {
                            view.show_agent_statement(
                                &event.source_id,
                                statement,
                                &event.tool,
                                event.result.clone(),
                                event.error.clone(),
                                cx,
                            )
                        });
                    }
                    this.database_footer_changed(cx);
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Redraws the button, and the open menu, when what they show changed.
    fn database_footer_changed(&mut self, cx: &mut Context<Self>) {
        let signature = self.footer_signature(cx);
        if signature == self.db_footer.shown {
            return;
        }
        self.db_footer.shown = signature;
        if let Some(open) = self
            .menu
            .as_ref()
            .filter(|m| m.target == MenuTarget::Databases)
        {
            let menu = open.menu.clone();
            let items = self.databases_menu(cx);
            menu.update(cx, |menu, cx| menu.replace_items(items, cx));
        }
        cx.notify();
    }

    /// Reads the saved sources again now, after the menu changed them.
    fn reload_footer_sources(&mut self, cx: &mut Context<Self>) {
        self.db_footer.sources = DataSourceStore::new(&self.database_dir).load();
        self.database_footer_changed(cx);
    }

    /// The footer's button: a dot for the best state among the project's
    /// sources, and what is connected.
    pub(super) fn databases_button(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let project = &self.workspace.project;
        if project.is_empty() || project.starts_with("remote://") {
            return None;
        }
        let sources = self.footer_sources();
        let statuses: Vec<(&DataSource, Status)> = sources
            .iter()
            .map(|s| (*s, self.source_status(s, cx)))
            .collect();
        let connected: Vec<&str> = statuses
            .iter()
            .filter(|(_, s)| *s == Status::Connected)
            .map(|(source, _)| source.name.as_str())
            .collect();
        let tunnels = statuses
            .iter()
            .filter(|(_, s)| *s == Status::TunnelOpen)
            .count();
        let label: SharedString = match (connected.as_slice(), tunnels) {
            ([one], _) => (*one).to_owned().into(),
            ([], 0) => "Databases".into(),
            ([], n) => format!("{n} tunnel{}", if n == 1 { "" } else { "s" }).into(),
            (many, _) => format!("{} connected", many.len()).into(),
        };
        // The dot: the best state there is, so "something is up" shows.
        let dot = [
            Status::Connected,
            Status::Connecting,
            Status::TunnelOpening,
            Status::TunnelOpen,
        ]
        .into_iter()
        .find(|wanted| statuses.iter().any(|(_, s)| s == wanted))
        .or_else(|| {
            statuses
                .iter()
                .find(|(_, s)| matches!(s, Status::Failed(_) | Status::PortBusy(_)))
                .map(|(_, s)| s.clone())
        });
        let agents_busy = sources
            .iter()
            .any(|s| self.db_footer.stats.get(&s.id).is_some_and(|s| s.running > 0));
        let open = self.menu_open(&MenuTarget::Databases);
        let classic = self.workspace.boxy_layout();
        let description: SharedString = statuses
            .iter()
            .map(|(source, status)| format!("{}: {}", source.name, status.text()))
            .collect::<Vec<_>>()
            .join(", ")
            .into();
        Some(
            div()
                .id("footer-databases")
                .debug_selector(|| "footer-databases".into())
                .role(Role::Button)
                .aria_label(SharedString::from(format!("Databases. {label}")))
                .aria_description(description)
                .aria_expanded(open)
                .relative()
                .h(px(20.))
                .px(px(if classic { 6. } else { 8. }))
                .map(|d| {
                    if classic {
                        d.rounded_md()
                    } else {
                        d.rounded_full()
                    }
                })
                .max_w(px(220.))
                .min_w_0()
                .mr(px(terminal_dock::FOOTER_GAP))
                .flex()
                .flex_shrink_0()
                .items_center()
                .gap(px(6.))
                .whitespace_nowrap()
                .text_size(px(11.))
                .cursor_pointer()
                .text_color(content(if open { 1. } else { 0.55 }))
                .when(open, |d| d.bg(content(0.08)))
                .hover(|s| s.bg(content(0.08)).text_color(content(1.)))
                .child(self.databases_anchor.probe())
                .child(icon("database", 12.).text_color(content(0.55)))
                .child(div().min_w_0().truncate().child(label))
                .when_some(dot, |d, status| {
                    d.child(div().size(px(6.)).rounded_full().bg(status.color()))
                })
                .when(agents_busy, |d| {
                    d.child(icon("bot", 11.).text_color(accent_color()))
                })
                .child(icon("chevron-down", 12.).text_color(content(0.45)))
                .on_click(cx.listener(|this, _, window, cx| {
                    if this.menu_open(&MenuTarget::Databases) {
                        this.close_menu(cx);
                    } else {
                        let anchor = Anchor::Slot(this.databases_anchor.clone());
                        this.open_menu(MenuTarget::Databases, anchor, window, cx);
                    }
                }))
                .into_any_element(),
        )
    }

    fn source_actions(&self, source: &DataSource, status: &Status) -> Vec<MenuAction> {
        let id = &source.id;
        let mut items = Vec::new();
        items.push(match status {
            Status::Connected | Status::Connecting => {
                MenuAction::new(format!("disconnect:{id}"), "Disconnect").icon("x")
            }
            _ => MenuAction::new(format!("connect:{id}"), "Connect").icon("play"),
        });
        items.push(MenuAction::new(format!("open:{id}"), "Open in Database").icon("database"));
        if source.tunnel.is_some() {
            let running = matches!(
                self.db_footer.tunnels.get(&source.port),
                Some(TunnelState::Running { .. } | TunnelState::Starting)
            );
            items.push(if running {
                MenuAction::new(format!("tunnel-stop:{id}"), "Stop tunnel").icon("square")
            } else {
                MenuAction::new(format!("tunnel-start:{id}"), "Open tunnel")
                    .icon("git-branch-dot")
            });
            items.push(
                MenuAction::new(format!("tunnel-log:{id}"), "Show tunnel log").icon("file-script"),
            );
            items.push(
                MenuAction::new(format!("tunnel-copy:{id}"), "Copy tunnel command")
                    .icon("response-copy"),
            );
        }
        for access in McpAccess::ALL {
            items.push(
                MenuAction::new(
                    format!("mcp:{}:{id}", access.badge()),
                    format!("Agents: {}", access.label()),
                )
                .icon("bot")
                .checked(source.mcp == access),
            );
        }
        items.push(MenuAction::new(format!("edit:{id}"), "Edit…").icon("pencil-edit"));
        items
    }

    pub(super) fn databases_menu(&self, cx: &App) -> Vec<MenuItem> {
        let sources = self.footer_sources();
        let mut items: Vec<MenuItem> = Vec::new();
        if sources.is_empty() {
            items.push(MenuItem::Note(
                "No data sources in this project yet. Find the ones dbports, SOCket or your \
                 shell already know, or add one."
                    .into(),
            ));
        } else {
            items.push(MenuItem::Header("Data sources".into()));
        }
        for source in &sources {
            let status = self.source_status(source, cx);
            let stats = self.db_footer.stats.get(&source.id);
            let mut detail = vec![status.text(), format!(":{}", source.port)];
            if let Some(stats) = stats.filter(|s| s.calls > 0) {
                detail.push(format!(
                    "{} agent call{}",
                    stats.calls,
                    if stats.calls == 1 { "" } else { "s" }
                ));
            }
            items.push(
                MenuAction::new(format!("source:{}", source.id), source.name.clone())
                    .icon("circle-dot")
                    .icon_color(status.color())
                    .description(detail.join(" · "))
                    .badge(source.mcp.badge())
                    .submenu(self.source_actions(source, &status))
                    .into(),
            );
        }
        items.push(MenuItem::Separator);
        items.push(
            MenuAction::new("find", "Find existing connections…")
                .icon("search-01")
                .description("dbports, SOCket, ~/.zshrc aliases, running proxies")
                .disabled(self.db_footer.scanning)
                .into(),
        );
        items.push(
            MenuAction::new("add", "Add data source…")
                .icon("add-01")
                .into(),
        );
        items.push(
            MenuAction::new("open-db", "Open Database")
                .icon("database")
                .into(),
        );
        let visible = sources.iter().filter(|s| s.mcp != McpAccess::Off).count();
        let writable = sources.iter().filter(|s| s.mcp == McpAccess::Write).count();
        let calls: u64 = sources
            .iter()
            .filter_map(|s| self.db_footer.stats.get(&s.id))
            .map(|s| s.calls)
            .sum();
        if !sources.is_empty() {
            items.push(MenuItem::Note(
                format!(
                    "Agents (monocode_db): {visible} of {} source{} visible, {writable} writable · \
                     {calls} call{} this run.",
                    sources.len(),
                    if sources.len() == 1 { "" } else { "s" },
                    if calls == 1 { "" } else { "s" },
                )
                .into(),
            ));
        }
        items
    }

    fn footer_source(&self, id: &str) -> Option<DataSource> {
        self.db_footer.sources.iter().find(|s| s.id == id).cloned()
    }

    fn database_notice(&mut self, message: String, cx: &mut Context<Self>) {
        self.notification_state
            .database_sent
            .update(cx, |notice, cx| notice.show(message, cx));
        cx.notify();
    }

    fn database_error(&mut self, message: String, cx: &mut Context<Self>) {
        self.notification_state
            .file_error
            .update(cx, |notice, cx| notice.set_message(Some(message), cx));
        cx.notify();
    }

    pub(super) fn run_databases_menu(
        &mut self,
        action: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (verb, rest) = action.split_once(':').unwrap_or((action, ""));
        match verb {
            "find" => self.find_connections(window, cx),
            "add" => {
                self.open_database(window, cx);
                if let Some(view) = self.database.clone() {
                    view.update(cx, |view, cx| view.edit_source(None, window, cx));
                }
            }
            "open-db" => self.open_database(window, cx),
            "source" | "open" => {
                self.open_database(window, cx);
                if let Some(view) = self.database.clone() {
                    view.update(cx, |view, cx| view.reveal(rest, cx));
                }
            }
            "edit" => {
                self.open_database(window, cx);
                if let Some(view) = self.database.clone() {
                    view.update(cx, |view, cx| {
                        view.edit_source(Some(rest.to_owned()), window, cx)
                    });
                }
            }
            "connect" => {
                let view = self.ensure_database(window, cx);
                view.update(cx, |view, cx| view.connect(rest, cx));
                self.database_footer_changed(cx);
            }
            "disconnect" => {
                if let Some(view) = self.database.clone() {
                    view.update(cx, |view, cx| view.disconnect(rest, cx));
                }
                db_bridge::forget_source(rest);
                self.database_footer_changed(cx);
            }
            "tunnel-start" => self.open_tunnel(rest, cx),
            "tunnel-stop" => self.stop_tunnel(rest, window, cx),
            "tunnel-log" => {
                if let Some(source) = self.footer_source(rest) {
                    let lines = tunnel::log_tail(&self.tunnel_dir(), source.port, 8);
                    let message = if lines.is_empty() {
                        format!("The tunnel of {} has written nothing yet.", source.name)
                    } else {
                        format!("{} tunnel log:\n{}", source.name, lines.join("\n"))
                    };
                    self.database_notice(message, cx);
                }
            }
            "tunnel-copy" => {
                if let Some(source) = self.footer_source(rest)
                    && let Some(spec) = &source.tunnel
                {
                    cx.write_to_clipboard(ClipboardItem::new_string(tunnel::display_command(
                        spec,
                        source.port,
                    )));
                }
            }
            "mcp" => {
                let Some((badge, id)) = rest.split_once(':') else {
                    return;
                };
                let Some(access) = McpAccess::ALL.into_iter().find(|a| a.badge() == badge) else {
                    return;
                };
                let view = self.ensure_database(window, cx);
                view.update(cx, |view, cx| view.set_mcp_access(id, access, cx));
                self.reload_footer_sources(cx);
            }
            _ => {}
        }
    }

    fn open_tunnel(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(source) = self.footer_source(id) else {
            return;
        };
        let Some(spec) = source.tunnel.clone() else {
            return;
        };
        let port = source.port;
        if !self.db_footer.opening.insert(port) {
            return;
        }
        let dir = self.tunnel_dir();
        let name = source.name.clone();
        let work = cx
            .background_executor()
            .spawn(async move { tunnel::ensure(&spec, port, &dir, TUNNEL_TIMEOUT) });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                this.db_footer.opening.remove(&port);
                if let Err(error) = result {
                    this.database_error(format!("The tunnel of {name} did not open: {error}"), cx);
                }
                this.database_footer_changed(cx);
            });
        })
        .detach();
        self.database_footer_changed(cx);
    }

    fn stop_tunnel(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(source) = self.footer_source(id) else {
            return;
        };
        // Everything that went through the tunnel goes with it.
        let sharing: Vec<String> = self
            .db_footer
            .sources
            .iter()
            .filter(|s| s.port == source.port && s.tunnel.is_some())
            .map(|s| s.id.clone())
            .collect();
        if let Some(view) = self.database.clone() {
            view.update(cx, |view, cx| {
                for id in &sharing {
                    view.disconnect(id, cx);
                }
            });
        }
        for id in &sharing {
            db_bridge::forget_source(id);
        }
        let dir = self.tunnel_dir();
        let port = source.port;
        let name = source.name.clone();
        let work = cx
            .background_executor()
            .spawn(async move { tunnel::stop(port, &dir) });
        cx.spawn_in(window, async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                if let Err(error) = result {
                    this.database_error(format!("The tunnel of {name} did not stop: {error}"), cx);
                }
                this.database_footer_changed(cx);
            });
        })
        .detach();
    }

    /// Looks for connections other tools already know, then offers them.
    fn find_connections(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.db_footer.scanning {
            return;
        }
        self.db_footer.scanning = true;
        let existing = DataSourceStore::new(&self.database_dir).load();
        let work = cx
            .background_executor()
            .spawn(async move { discover::discover(&home(), &existing) });
        cx.spawn_in(window, async move |this, cx| {
            let found = work.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.db_footer.scanning = false;
                if found.is_empty() {
                    this.database_notice(
                        "Nothing new: every connection dbports, SOCket, your shell aliases and \
                         the running proxies know is already a data source."
                            .into(),
                        cx,
                    );
                    return;
                }
                this.db_footer.chosen = (0..found.len()).collect();
                this.db_footer.found = found;
                let anchor = Anchor::Slot(this.databases_anchor.clone());
                this.open_menu(MenuTarget::DatabaseImport, anchor, window, cx);
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn database_import_menu(&self) -> Vec<MenuItem> {
        let found = &self.db_footer.found;
        let mut items: Vec<MenuItem> = vec![MenuItem::Header(
            format!(
                "Found {} connection{}",
                found.len(),
                if found.len() == 1 { "" } else { "s" }
            )
            .into(),
        )];
        for (i, entry) in found.iter().enumerate() {
            let mut detail = vec![entry.origin.clone()];
            if let Some(spec) = &entry.source.tunnel {
                detail.push(spec.kind.label().to_owned());
            }
            detail.push(format!(":{}", entry.source.port));
            if !entry.source.database.is_empty() {
                detail.push(entry.source.database.clone());
            }
            if entry.password.is_some() {
                detail.push("with password".into());
            }
            items.push(
                MenuAction::new(format!("pick:{i}"), entry.source.name.clone())
                    .description(detail.join(" · "))
                    .toggle(self.db_footer.chosen.contains(&i))
                    .into(),
            );
        }
        items.push(MenuItem::Separator);
        let count = self.db_footer.chosen.len();
        items.push(
            MenuAction::new(
                "import",
                format!(
                    "Add {count} data source{}",
                    if count == 1 { "" } else { "s" }
                ),
            )
            .icon("add-01")
            .disabled(count == 0)
            .into(),
        );
        items.push(MenuItem::Note(
            "Passwords found go to the keychain. Sources without a user or password can be \
             completed with Edit."
                .into(),
        ));
        items
    }

    /// Ticks a found connection on or off; the menu stays open.
    pub(super) fn toggle_found(&mut self, id: &str) -> bool {
        let Some(i) = id.strip_prefix("pick:").and_then(|i| i.parse::<usize>().ok()) else {
            return false;
        };
        if !self.db_footer.chosen.remove(&i) {
            self.db_footer.chosen.insert(i);
        }
        true
    }

    pub(super) fn run_database_import_menu(
        &mut self,
        action: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if action != "import" {
            return;
        }
        let chosen = std::mem::take(&mut self.db_footer.chosen);
        let found = std::mem::take(&mut self.db_footer.found);
        let mut picked: Vec<(DataSource, Option<String>)> = found
            .into_iter()
            .enumerate()
            .filter(|(i, _)| chosen.contains(i))
            .map(|(_, f)| (f.source, f.password))
            .collect();
        // Found here, they belong to every project unless the user says otherwise.
        for (source, _) in &mut picked {
            source.projects.clear();
        }
        let view = self.ensure_database(window, cx);
        let added = view.update(cx, |view, cx| view.import(picked, cx));
        self.reload_footer_sources(cx);
        self.database_notice(
            format!(
                "Added {added} data source{}. Connect from the Databases menu below.",
                if added == 1 { "" } else { "s" }
            ),
            cx,
        );
    }
}
