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
use monocode_core::db::{Connection, DataSource, runtime::runtime};

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
    source: DataSource,
    editing: bool,
    has_saved_password: bool,
    name: Entity<Input>,
    host: Entity<Input>,
    port: Entity<Input>,
    database: Entity<Input>,
    user: Entity<Input>,
    password: Entity<Input>,
    comment: Entity<Input>,
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
        cx: &mut Context<Self>,
    ) -> Self {
        let name = field("Name", cx);
        let host = field("Host", cx);
        let port = field("Port", cx);
        let database = field("Database", cx);
        let user = field("User", cx);
        let password = field("Password", cx);
        let comment = field("Comment", cx);
        password.update(cx, |input, _| input.set_secret(true));
        set(&name, &source.name, cx);
        set(&host, &source.host, cx);
        set(&port, &source.port.to_string(), cx);
        set(&database, &source.database, cx);
        set(&user, &source.user, cx);
        set(&comment, &source.comment, cx);
        let subscriptions = [&name, &host, &port, &database, &user, &password, &comment]
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
            editing,
            has_saved_password,
            name,
            host,
            port,
            database,
            user,
            password,
            comment,
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
        source.port = Self::text(&self.port, cx)
            .parse()
            .map_err(|_| "The port must be a number")?;
        if source.name.is_empty() {
            return Err("Give the data source a name");
        }
        if source.host.is_empty() {
            return Err("Enter a host");
        }
        Ok(source)
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
        let password = self.typed_password(cx).or_else(|| {
            self.has_saved_password
                .then(|| monocode_core::db::source::saved_password(&source.id))
                .flatten()
        });
        self.test = Test::Running;
        let work = runtime().spawn(async move {
            let conn = Connection::connect(&source, password.as_deref())
                .await
                .map_err(|e| e.message)?;
            let result = conn
                .execute("select version()", 1)
                .await
                .map_err(|e| e.message)?;
            Ok::<_, String>(
                result
                    .rows
                    .first()
                    .and_then(|r| r.first().cloned().flatten())
                    .unwrap_or_default(),
            )
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

    fn switch(
        id: &'static str,
        label: &'static str,
        on: bool,
        cx: &mut Context<Self>,
        toggle: impl Fn(&mut DataSource) + 'static,
    ) -> AnyElement {
        div()
            .id(id)
            .role(Role::Switch)
            .aria_label(label)
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
        let card = modal::card().w(px(520.)).child(
            modal::card_body()
                .gap(px(12.))
                .child(modal::title(if self.editing {
                    "Edit data source"
                } else {
                    "New PostgreSQL data source"
                }))
                .child(Self::labeled("Name", &self.name, None))
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
                .child(Self::labeled("Database", &self.database, None))
                .child(Self::labeled("Comment", &self.comment, None))
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
                        )),
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
