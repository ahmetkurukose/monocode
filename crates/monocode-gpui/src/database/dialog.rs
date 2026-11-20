//! The data source dialog: connection fields, "Test Connection", Save.

use crate::{
    input::{self, Input},
    modal,
    theme::*,
};
use gpui::{
    AnyElement, Context, Entity, EventEmitter, FocusHandle, Focusable, Role, SharedString,
    Subscription, Task, Window, div, prelude::*, px, rgb,
};
use monocode_core::db::{
    Connection, DataSource, DbKind, McpAccess, Tunnel, TunnelKind, runtime::runtime,
};

pub(crate) enum DialogEvent {
    Close,
    Save {
        source: Box<DataSource>,
        /// `Some` when the password field was filled in; it replaces the saved one.
        password: Option<String>,
    },
}

enum Test {
    Idle,
    Running,
    Passed(String),
    Failed(String),
}

pub(crate) struct SourceDialog {
    pub(super) source: DataSource,
    /// The workspace's projects, offered in the Projects row.
    projects: Vec<String>,
    editing: bool,
    has_saved_password: bool,
    name: Entity<Input>,
    host: Entity<Input>,
    port: Entity<Input>,
    database: Entity<Input>,
    user: Entity<Input>,
    password: Entity<Input>,
    comment: Entity<Input>,
    /// MongoDB: a connection string, replacing host and port.
    uri: Entity<Input>,
    /// MongoDB: the database users authenticate against.
    auth_source: Entity<Input>,
    /// The tunnel's instance, kubectl target, or custom command.
    tunnel_target: Entity<Input>,
    /// kubectl: the target's port, namespace and context.
    tunnel_remote_port: Entity<Input>,
    tunnel_namespace: Entity<Input>,
    tunnel_context: Entity<Input>,
    /// Extra arguments, split like a shell would.
    tunnel_args: Entity<Input>,
    test: Test,
    task: Option<Task<()>>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DialogEvent> for SourceDialog {}

impl Focusable for SourceDialog {
    fn focus_handle(&self, cx: &gpui::App) -> FocusHandle {
        self.name.focus_handle(cx)
    }
}

fn field(label: &'static str, cx: &mut Context<SourceDialog>) -> Entity<Input> {
    cx.new(|cx| {
        let mut input = Input::field(label, label, content(0.35), cx);
        input.set_font_size(13.);
        input
    })
}

fn set(input: &Entity<Input>, text: &str, cx: &mut Context<SourceDialog>) {
    let text = text.to_owned();
    input.update(cx, |input, cx| input.set_text(text, cx));
}

impl SourceDialog {
    pub(crate) fn new(
        source: DataSource,
        editing: bool,
        has_saved_password: bool,
        projects: Vec<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        let name = field("Name", cx);
        let host = field("Host", cx);
        let port = field("Port", cx);
        let database = field("Database", cx);
        let user = field("User", cx);
        let password = field("Password", cx);
        let comment = field("Comment", cx);
        let uri = field("mongodb://host:27017/?replicaSet=rs0", cx);
        let auth_source = field("admin", cx);
        let tunnel_target = field("Instance", cx);
        let tunnel_remote_port = field("Same as port", cx);
        let tunnel_namespace = field("default", cx);
        let tunnel_context = field("current", cx);
        let tunnel_args = field("--auto-iam-authn", cx);
        if let Some(tunnel) = &source.tunnel {
            let target = if tunnel.kind == TunnelKind::Custom {
                &tunnel.command
            } else {
                &tunnel.instance
            };
            set(&tunnel_target, target, cx);
            if let Some(port) = tunnel.remote_port {
                set(&tunnel_remote_port, &port.to_string(), cx);
            }
            set(&tunnel_namespace, &tunnel.namespace, cx);
            set(&tunnel_context, &tunnel.context, cx);
            set(
                &tunnel_args,
                &shlex::try_join(tunnel.args.iter().map(String::as_str)).unwrap_or_default(),
                cx,
            );
        }
        password.update(cx, |input, _| input.set_secret(true));
        set(&name, &source.name, cx);
        set(&host, &source.host, cx);
        set(&port, &source.port.to_string(), cx);
        set(&database, &source.database, cx);
        set(&user, &source.user, cx);
        set(&comment, &source.comment, cx);
        set(&uri, &source.uri, cx);
        set(&auth_source, &source.auth_source, cx);
        let subscriptions = [
            &name,
            &host,
            &port,
            &database,
            &user,
            &password,
            &comment,
            &uri,
            &auth_source,
            &tunnel_target,
            &tunnel_remote_port,
            &tunnel_namespace,
            &tunnel_context,
            &tunnel_args,
        ]
        .into_iter()
        .map(|input| {
            cx.subscribe(input, |this, _, _: &input::Changed, cx| {
                this.test = Test::Idle;
                cx.notify();
            })
        })
        .collect();
        Self {
            source,
            projects,
            editing,
            has_saved_password,
            name,
            host,
            port,
            database,
            user,
            password,
            comment,
            uri,
            auth_source,
            tunnel_target,
            tunnel_remote_port,
            tunnel_namespace,
            tunnel_context,
            tunnel_args,
            test: Test::Idle,
            task: None,
            focus: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    fn text(input: &Entity<Input>, cx: &Context<Self>) -> String {
        input.read(cx).content.trim().to_owned()
    }

    /// The source as the fields describe it, or why it is not valid yet.
    fn collect(&self, cx: &Context<Self>) -> Result<DataSource, &'static str> {
        let mut source = self.source.clone();
        source.name = Self::text(&self.name, cx);
        source.host = Self::text(&self.host, cx);
        source.database = Self::text(&self.database, cx);
        source.user = Self::text(&self.user, cx);
        source.comment = Self::text(&self.comment, cx);
        let mongo = source.kind == DbKind::MongoDb;
        source.uri = if mongo {
            Self::text(&self.uri, cx)
        } else {
            String::new()
        };
        source.auth_source = if mongo {
            Self::text(&self.auth_source, cx)
        } else {
            String::new()
        };
        if !mongo {
            source.tls = false;
        }
        if let Some(tunnel) = &mut source.tunnel {
            let target = Self::text(&self.tunnel_target, cx);
            if tunnel.kind == TunnelKind::Custom {
                tunnel.command = target;
                tunnel.instance.clear();
            } else {
                tunnel.instance = target;
                tunnel.command.clear();
            }
            let kubectl = tunnel.kind == TunnelKind::Kubectl;
            let remote = Self::text(&self.tunnel_remote_port, cx);
            tunnel.remote_port = if kubectl && !remote.is_empty() {
                Some(
                    remote
                        .parse()
                        .map_err(|_| "The remote port must be a number")?,
                )
            } else {
                None
            };
            tunnel.namespace = if kubectl {
                Self::text(&self.tunnel_namespace, cx)
            } else {
                String::new()
            };
            tunnel.context = if kubectl {
                Self::text(&self.tunnel_context, cx)
            } else {
                String::new()
            };
            if tunnel.kind != TunnelKind::AlloyDb {
                tunnel.public_ip = false;
            }
            let args = Self::text(&self.tunnel_args, cx);
            tunnel.args = if tunnel.kind == TunnelKind::Custom {
                Vec::new()
            } else {
                shlex::split(&args).ok_or("The extra arguments have an unclosed quote")?
            };
            if tunnel.instance.is_empty() && tunnel.command.is_empty() {
                return Err(match tunnel.kind {
                    TunnelKind::Custom => "Enter the tunnel's command",
                    TunnelKind::Kubectl => "Enter what to forward (svc/name, pod/name)",
                    _ => "Enter the proxy's instance",
                });
            }
            // A tunnel listens on the loopback.
            if source.host.is_empty() {
                source.host = "127.0.0.1".into();
            }
        }
        let port = Self::text(&self.port, cx);
        source.port = if port.is_empty() && !source.uri.is_empty() {
            source.kind.default_port()
        } else {
            port.parse().map_err(|_| "The port must be a number")?
        };
        if source.name.is_empty() {
            return Err("Give the data source a name");
        }
        if !source.uri.is_empty()
            && !(source.uri.starts_with("mongodb://") || source.uri.starts_with("mongodb+srv://"))
        {
            return Err("A connection string starts with mongodb:// or mongodb+srv://");
        }
        if source.host.is_empty() && source.uri.is_empty() {
            return Err(if mongo {
                "Enter a host or a connection string"
            } else {
                "Enter a host"
            });
        }
        Ok(source)
    }

    /// Switches a new source between PostgreSQL and MongoDB; a port left
    /// at one kind's default moves to the other's.
    fn set_kind(&mut self, kind: DbKind, cx: &mut Context<Self>) {
        if self.source.kind == kind {
            return;
        }
        let old = self.source.kind;
        self.source.kind = kind;
        let port = Self::text(&self.port, cx);
        if port.is_empty() || port == old.default_port().to_string() {
            set(&self.port, &kind.default_port().to_string(), cx);
        }
        self.test = Test::Idle;
        cx.notify();
    }

    /// The PostgreSQL / MongoDB choice, for a source being made.
    fn kind_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let current = self.source.kind;
        div()
            .flex()
            .gap(px(6.))
            .children(DbKind::ALL.into_iter().map(|kind| {
                let on = kind == current;
                div()
                    .id(SharedString::from(format!(
                        "database-kind-{}",
                        kind.label()
                    )))
                    .role(Role::RadioButton)
                    .aria_label(kind.label())
                    .aria_selected(on)
                    .h(px(28.))
                    .px(px(12.))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(content(if on { 0.5 } else { 0.12 }))
                    .when(on, |d| d.bg(content(0.1)))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .cursor_pointer()
                    .text_size(px(12.))
                    .text_color(content(if on { 1. } else { 0.7 }))
                    .hover(|s| s.bg(content(0.08)))
                    .child(icon("database", 13.))
                    .child(kind.label())
                    .on_click(cx.listener(move |this, _, _, cx| this.set_kind(kind, cx)))
            }))
            .into_any_element()
    }

    /// No tunnel, or the program that opens the source's port.
    fn tunnel_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let current = self.source.tunnel.as_ref().map(|t| t.kind);
        let choices =
            std::iter::once((None, "None")).chain(TunnelKind::ALL.into_iter().map(|kind| {
                let label = match kind {
                    TunnelKind::AlloyDb => "AlloyDB",
                    TunnelKind::CloudSql => "Cloud SQL",
                    TunnelKind::Kubectl => "kubectl",
                    TunnelKind::Custom => "Command",
                };
                (Some(kind), label)
            }));
        Self::chips(
            "Tunnel",
            choices.map(|(kind, label)| {
                (
                    SharedString::from(format!("database-tunnel-{label}")),
                    label,
                    kind == current,
                    Box::new(move |s: &mut DataSource| {
                        s.tunnel = kind.map(|kind| Tunnel {
                            kind,
                            public_ip: kind == TunnelKind::AlloyDb,
                            ..s.tunnel.clone().unwrap_or_default()
                        });
                        if kind.is_some() && s.host.is_empty() {
                            s.host = "127.0.0.1".into();
                        }
                    }) as Box<dyn Fn(&mut DataSource)>,
                )
            }),
            cx,
        )
    }

    /// What agents in sessions may do with the source.
    fn agents_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let current = self.source.mcp;
        Self::chips(
            "Agents (monocode_db MCP)",
            McpAccess::ALL.into_iter().map(|access| {
                (
                    SharedString::from(format!("database-agents-{}", access.badge())),
                    access.label(),
                    access == current,
                    Box::new(move |s: &mut DataSource| s.mcp = access)
                        as Box<dyn Fn(&mut DataSource)>,
                )
            }),
            cx,
        )
    }

    /// A labelled row of choices, one of them on.
    fn chips(
        label: &'static str,
        choices: impl Iterator<
            Item = (
                SharedString,
                &'static str,
                bool,
                Box<dyn Fn(&mut DataSource)>,
            ),
        >,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let row =
            div()
                .flex()
                .flex_wrap()
                .gap(px(6.))
                .children(choices.map(|(id, text, on, choose)| {
                    div()
                        .id(id)
                        .role(Role::RadioButton)
                        .aria_label(text)
                        .aria_selected(on)
                        .h(px(24.))
                        .px(px(10.))
                        .rounded(px(6.))
                        .border_1()
                        .border_color(content(if on { 0.5 } else { 0.12 }))
                        .when(on, |d| d.bg(content(0.1)))
                        .flex()
                        .items_center()
                        .cursor_pointer()
                        .text_size(px(12.))
                        .text_color(content(if on { 1. } else { 0.7 }))
                        .hover(|s| s.bg(content(0.08)))
                        .child(text)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            choose(&mut this.source);
                            this.test = Test::Idle;
                            cx.notify();
                        }))
                }));
        div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(content(0.5))
                    .child(label),
            )
            .child(row)
            .into_any_element()
    }

    /// The tunnel's own fields, by kind.
    fn tunnel_fields(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let tunnel = self.source.tunnel.as_ref()?;
        let target = match tunnel.kind {
            TunnelKind::AlloyDb => "Instance (projects/…/clusters/…/instances/…)",
            TunnelKind::CloudSql => "Instance (project:region:instance)",
            TunnelKind::Kubectl => "Forward (svc/name, pod/name, deploy/name)",
            TunnelKind::Custom => "Command (run with sh -c; listens on the port above)",
        };
        let mut column = div().flex().flex_col().gap(px(10.)).child(Self::labeled(
            target,
            &self.tunnel_target,
            None,
        ));
        match tunnel.kind {
            TunnelKind::Kubectl => {
                column = column.child(
                    div()
                        .flex()
                        .gap(px(10.))
                        .child(Self::labeled(
                            "Remote port",
                            &self.tunnel_remote_port,
                            Some(110.),
                        ))
                        .child(Self::labeled("Namespace", &self.tunnel_namespace, None))
                        .child(Self::labeled("Context", &self.tunnel_context, None)),
                );
            }
            TunnelKind::Custom => {}
            _ => {}
        }
        if tunnel.kind != TunnelKind::Custom {
            column = column.child(Self::labeled("Extra arguments", &self.tunnel_args, None));
        }
        if tunnel.kind == TunnelKind::AlloyDb {
            column = column.child(Self::switch(
                "database-public-ip",
                "Public IP (--public-ip)",
                tunnel.public_ip,
                cx,
                |s| {
                    if let Some(t) = &mut s.tunnel {
                        t.public_ip = !t.public_ip;
                    }
                },
            ));
        }
        Some(column.into_any_element())
    }

    fn typed_password(&self, cx: &Context<Self>) -> Option<String> {
        let text = self.password.read(cx).content.to_string();
        (!text.is_empty()).then_some(text)
    }

    fn run_test(&mut self, cx: &mut Context<Self>) {
        let source = match self.collect(cx) {
            Ok(source) => source,
            Err(message) => {
                self.test = Test::Failed(message.into());
                cx.notify();
                return;
            }
        };
        // The saved password is fetched with the connection, off the UI thread.
        let typed = self.typed_password(cx);
        let saved_id = self.has_saved_password.then(|| source.id.clone());
        self.test = Test::Running;
        let work = runtime().spawn(async move {
            let password = match typed {
                Some(password) => Some(password),
                None => match saved_id {
                    Some(id) => {
                        monocode_core::db::source::saved_password_checked(&id).map_err(|error| {
                            format!(
                                "The saved password could not be read from the keychain ({error})."
                            )
                        })?
                    }
                    None => None,
                },
            };
            let conn = Connection::connect(&source, password.as_deref())
                .await
                .map_err(|e| e.message)?;
            conn.version().await.map_err(|e| e.message)
        });
        self.task = Some(cx.spawn(async move |this, cx| {
            let outcome = work.await.unwrap_or_else(|e| Err(e.to_string()));
            let _ = this.update(cx, |this, cx| {
                this.test = match outcome {
                    Ok(version) => Test::Passed(version),
                    Err(message) => Test::Failed(message),
                };
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        match self.collect(cx) {
            Ok(source) => cx.emit(DialogEvent::Save {
                source: Box::new(source),
                password: self.typed_password(cx),
            }),
            Err(message) => {
                self.test = Test::Failed(message.into());
                cx.notify();
            }
        }
    }

    fn labeled(label: &'static str, input: &Entity<Input>, width: Option<f32>) -> AnyElement {
        div()
            .when_some(width, |d, w| d.w(px(w)).flex_shrink_0())
            .when(width.is_none(), |d| d.flex_1().min_w_0())
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(content(0.5))
                    .child(label),
            )
            .child(
                div()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(content(0.12))
                    .bg(content(0.04))
                    .px_2()
                    .py(px(6.))
                    .child(input.clone()),
            )
            .into_any_element()
    }

    /// "All projects", and when that is off one switch per project. The
    /// last project cannot be switched off: no projects means all of them.
    fn projects_row(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.projects.is_empty() {
            return None;
        }
        let all = self.source.projects.is_empty();
        let first = self.projects[0].clone();
        let mut row = div().flex().flex_col().gap(px(6.)).child(
            div()
                .text_size(px(11.))
                .text_color(content(0.5))
                .child("Projects"),
        );
        row = row.child(Self::switch(
            "database-all-projects",
            "All projects",
            all,
            cx,
            move |s| {
                if s.projects.is_empty() {
                    s.projects = vec![first.clone()];
                } else {
                    s.projects.clear();
                }
            },
        ));
        if !all {
            let list = div()
                .flex()
                .flex_wrap()
                .gap_x(px(16.))
                .gap_y(px(6.))
                .pl(px(24.));
            let list = list.children(self.projects.iter().enumerate().map(|(ix, project)| {
                let on = self.source.projects.contains(project);
                let path = project.clone();
                Self::switch(
                    SharedString::from(format!("database-project-{ix}")),
                    crate::workspace::project_name(project),
                    on,
                    cx,
                    move |s| {
                        if !s.projects.contains(&path) {
                            s.projects.push(path.clone());
                        } else if s.projects.len() > 1 {
                            s.projects.retain(|p| p != &path);
                        }
                    },
                )
            }));
            row = row.child(list);
        }
        Some(row.into_any_element())
    }

    fn switch(
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        on: bool,
        cx: &mut Context<Self>,
        toggle: impl Fn(&mut DataSource) + 'static,
    ) -> AnyElement {
        let id: SharedString = id.into();
        let label: SharedString = label.into();
        let selector = id.clone();
        div()
            .id(id)
            .debug_selector(move || selector.to_string())
            .role(Role::Switch)
            .aria_label(label.clone())
            .aria_selected(on)
            .cursor_pointer()
            .flex()
            .items_center()
            .gap_2()
            .text_size(px(12.))
            .text_color(content(0.8))
            .child(
                div()
                    .size(px(16.))
                    .rounded(px(4.))
                    .border_1()
                    .border_color(content(if on { 0.7 } else { 0.25 }))
                    .bg(content(if on { 0.9 } else { 0.0 }))
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(on, |d| {
                        let mut ink = modal::scrim_color();
                        ink.a = 1.;
                        d.child(icon("check", 12.).text_color(ink))
                    }),
            )
            .child(label)
            .on_click(cx.listener(move |this, _, _, cx| {
                toggle(&mut this.source);
                cx.notify();
            }))
            .into_any_element()
    }
}

impl gpui::Render for SourceDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (status, color): (SharedString, _) = match &self.test {
            Test::Idle => ("".into(), content(0.5)),
            Test::Running => ("Connecting…".into(), content(0.5)),
            Test::Passed(version) => (
                format!(
                    "Connected · {}",
                    version.split(" on ").next().unwrap_or(version)
                )
                .into(),
                rgb(0x4ade80).into(),
            ),
            Test::Failed(message) => (message.clone().into(), rgb(0xf87171).into()),
        };
        let mongo = self.source.kind == DbKind::MongoDb;
        let title: SharedString = if self.editing {
            format!("Edit {} data source", self.source.kind.label()).into()
        } else {
            "New data source".into()
        };
        let card = modal::card().w(px(520.)).child(
            modal::card_body()
                .gap(px(12.))
                .child(modal::title(title))
                .when(!self.editing, |d| d.child(self.kind_row(cx)))
                .child(Self::labeled("Name", &self.name, None))
                .when(mongo, |d| {
                    d.child(Self::labeled(
                        "Connection string (optional — replaces host and port)",
                        &self.uri,
                        None,
                    ))
                })
                .child(
                    div()
                        .flex()
                        .gap(px(10.))
                        .child(Self::labeled("Host", &self.host, None))
                        .child(Self::labeled("Port", &self.port, Some(90.))),
                )
                .child(
                    div()
                        .flex()
                        .gap(px(10.))
                        .child(Self::labeled("User", &self.user, None))
                        .child(Self::labeled(
                            if self.has_saved_password {
                                "Password (saved — leave empty to keep)"
                            } else {
                                "Password"
                            },
                            &self.password,
                            None,
                        )),
                )
                .child(if mongo {
                    div()
                        .flex()
                        .gap(px(10.))
                        .child(Self::labeled("Default database", &self.database, None))
                        .child(Self::labeled(
                            "Auth database",
                            &self.auth_source,
                            Some(160.),
                        ))
                        .into_any_element()
                } else {
                    Self::labeled("Database", &self.database, None)
                })
                .child(Self::labeled("Comment", &self.comment, None))
                .child(self.tunnel_row(cx))
                .children(self.tunnel_fields(cx))
                .child(self.agents_row(cx))
                .children(self.projects_row(cx))
                .child(
                    div()
                        .flex()
                        .gap(px(20.))
                        .child(Self::switch(
                            "database-save-password",
                            "Save password",
                            self.source.save_password,
                            cx,
                            |s| s.save_password = !s.save_password,
                        ))
                        .child(Self::switch(
                            "database-read-only",
                            "Read-only",
                            self.source.read_only,
                            cx,
                            |s| s.read_only = !s.read_only,
                        ))
                        .when(mongo, |d| {
                            d.child(Self::switch(
                                "database-tls",
                                "TLS",
                                self.source.tls,
                                cx,
                                |s| s.tls = !s.tls,
                            ))
                        }),
                )
                .child(
                    div()
                        .min_h(px(18.))
                        .text_size(px(12.))
                        .line_height(px(18.))
                        .text_color(color)
                        .child(status),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(
                            crate::project_settings::button("database-test", "Test Connection")
                                .on_click(cx.listener(|this, _, _, cx| this.run_test(cx))),
                        )
                        .child(div().flex_1())
                        .child(
                            crate::project_settings::button("database-cancel", "Cancel")
                                .on_click(cx.listener(|_, _, _, cx| cx.emit(DialogEvent::Close))),
                        )
                        .child(
                            modal::primary_button("database-save", "Save")
                                .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                        ),
                ),
        );
        div()
            .size_full()
            .track_focus(&self.focus)
            .key_context("DataSourceDialog")
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                match event.keystroke.key.as_str() {
                    "escape" => cx.emit(DialogEvent::Close),
                    "enter" if event.keystroke.modifiers.platform => this.save(cx),
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .child(modal::overlay("database-dialog", card, {
                let weak = cx.weak_entity();
                move |_, cx| {
                    let _ = weak.update(cx, |_, cx| cx.emit(DialogEvent::Close));
                }
            }))
    }
}
