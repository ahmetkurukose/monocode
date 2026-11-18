//! The project's "Code insight" settings: whether language servers run for
//! it at all, which of them, where each one's executable is when it is not
//! found by itself, and the two limits the whole app shares (how long an
//! unused server lives and how many may be alive).
use crate::{
    code_shell::{
        context,
        settings::{self as pages, PageContext, PageHost, SettingsPage},
    },
    input::{self, Input},
    project_settings::button,
    theme::*,
};
use gpui::{
    App, AppContext, Context, Entity, Role, SharedString, Subscription, Window, div, prelude::*, px,
};
use monocode_core::code::lsp::{self, Language, ManagerOptions, Registry, ServerSpec};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf, rc::Rc, time::Duration};

pub(super) const PAGE: &str = "code-insight";
const MAX_IDLE_MINUTES: u64 = 240;
const MAX_SERVERS: usize = 32;

/// What a project saved, as it is stored with the workspace.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct InsightSettings {
    pub disabled: bool,
    /// Registry ids of the servers switched off.
    pub disabled_servers: Vec<String>,
    /// Executables by registry id.
    pub tool_paths: BTreeMap<String, String>,
    /// Registry ids of the servers of the other languages switched on.
    /// Only Python has been run against real servers, so the rest stay off
    /// until a project asks for them.
    pub languages: Vec<String>,
    pub idle_minutes: Option<u64>,
    pub max_servers: Option<usize>,
}

impl InsightSettings {
    /// Whether a server may run for this project: Python's unless switched
    /// off one by one, another language's only once it was switched on.
    pub(super) fn runs(&self, spec: &ServerSpec) -> bool {
        !unverified(spec) || self.languages.iter().any(|on| on == spec.id)
    }

    pub(super) fn for_manager(&self) -> lsp::ProjectSettings {
        // A language that is off is off for an agent's tools too.
        let off = (super::sync::REGISTRY.servers.iter())
            .filter(|spec| !self.runs(spec))
            .map(|spec| spec.id.to_owned());
        lsp::ProjectSettings {
            disabled: self.disabled,
            disabled_servers: (self.disabled_servers.iter().cloned()).chain(off).collect(),
            tool_paths: (self.tool_paths.iter())
                .map(|(id, path)| (id.clone(), PathBuf::from(path)))
                .collect(),
        }
    }

    fn limits(&self) -> Option<(Duration, usize)> {
        if self.idle_minutes.is_none() && self.max_servers.is_none() {
            return None;
        }
        let defaults = ManagerOptions::default();
        Some((
            (self.idle_minutes).map_or(defaults.idle_period, |minutes| {
                Duration::from_secs(minutes * 60)
            }),
            self.max_servers.unwrap_or(defaults.max_servers),
        ))
    }
}

/// Whether a server is for a language nobody has run it for yet.
fn unverified(spec: &ServerSpec) -> bool {
    !spec.languages.contains(&Language::Python)
}

/// What `project` saved; the defaults when it saved nothing.
pub(super) fn load(project: &str, cx: &App) -> InsightSettings {
    pages::value(project, PAGE, cx)
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default()
}

/// The limits for the whole app: every project's page shows and saves the
/// same two numbers, so the first project that has them speaks for all.
pub(super) fn limits(cx: &App) -> Option<(Duration, usize)> {
    (context::snapshot(cx).projects.iter()).find_map(|project| load(project, cx).limits())
}

pub(super) fn register(cx: &mut App) {
    pages::register(
        SettingsPage {
            id: PAGE,
            title: "Code insight",
            icon: "code",
            build: Rc::new(|page, window, cx| {
                let view = cx.new(|cx| Page::new(page, window, cx));
                #[cfg(test)]
                cx.set_global(LastPage(view.clone()));
                view.into()
            }),
        },
        cx,
    );
}

/// The page built last, where the tests find it.
#[cfg(test)]
struct LastPage(Entity<Page>);
#[cfg(test)]
impl gpui::Global for LastPage {}

#[cfg(test)]
pub(super) fn last_page(cx: &App) -> Option<Entity<Page>> {
    cx.try_global::<LastPage>().map(|page| page.0.clone())
}

struct ServerRow {
    id: &'static str,
    /// What the server is for, after its name: "Python".
    languages: String,
    off: bool,
    /// One of the other languages' servers: off unless switched on.
    unverified: bool,
    path: Entity<Input>,
}

pub(super) struct Page {
    project: String,
    host: PageHost,
    disabled: bool,
    servers: Vec<ServerRow>,
    idle: Entity<Input>,
    cap: Entity<Input>,
    /// Why the last save was refused.
    problem: Option<SharedString>,
    _events: Vec<Subscription>,
}

fn line(label: &str, placeholder: &str, text: &str, cx: &mut App) -> Entity<Input> {
    cx.new(|cx| {
        let mut field = Input::field(label.to_owned(), placeholder.to_owned(), content(0.35), cx);
        field.set_text(text.to_owned(), cx);
        field
    })
}

impl Page {
    fn new(page: PageContext, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let saved: InsightSettings = (page.value)
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default();
        let defaults = ManagerOptions::default();
        let servers: Vec<ServerRow> = (Registry::builtin().servers.iter())
            .map(|spec| ServerRow {
                id: spec.id,
                languages: {
                    let mut names: Vec<&str> = Vec::new();
                    for language in spec.languages {
                        let name = match language.id() {
                            "python" => "Python",
                            "go" => "Go",
                            "rust" => "Rust",
                            _ => "TypeScript and JavaScript",
                        };
                        if !names.contains(&name) {
                            names.push(name);
                        }
                    }
                    names.join(", ")
                },
                off: if unverified(spec) {
                    !saved.runs(spec)
                } else {
                    saved.disabled_servers.iter().any(|off| off == spec.id)
                },
                unverified: unverified(spec),
                path: line(
                    &format!("Executable of {}", spec.id),
                    "Found in the virtual environment or on PATH",
                    saved.tool_paths.get(spec.id).map_or("", String::as_str),
                    cx,
                ),
            })
            .collect();
        let number = |value: Option<String>| value.unwrap_or_default();
        let idle = line(
            "Minutes an unused server is kept",
            &(defaults.idle_period.as_secs() / 60).to_string(),
            &number(saved.idle_minutes.map(|minutes| minutes.to_string())),
            cx,
        );
        let cap = line(
            "Servers alive at most",
            &defaults.max_servers.to_string(),
            &number(saved.max_servers.map(|servers| servers.to_string())),
            cx,
        );
        let mut events = Vec::new();
        for field in (servers.iter().map(|row| &row.path)).chain([&idle, &cap]) {
            events.push(cx.subscribe(field, |this, _, _: &input::Changed, cx| {
                this.problem = None;
                cx.notify();
            }));
            // Enter in a field saves the form.
            events.push(cx.subscribe_in(
                field,
                window,
                |this, _, _: &input::Submitted, window, cx| this.save(window, cx),
            ));
        }
        Self {
            project: page.project,
            host: page.host,
            disabled: saved.disabled,
            servers,
            idle,
            cap,
            problem: None,
            _events: events,
        }
    }

    /// What the form says now, or why it cannot be saved.
    fn settings(&self, cx: &App) -> Result<InsightSettings, SharedString> {
        let number = |field: &Entity<Input>, most: u64, what: &str| {
            let typed = field.read(cx).content.trim().to_owned();
            if typed.is_empty() {
                return Ok(None);
            }
            match typed.parse::<u64>() {
                Ok(value) if (1..=most).contains(&value) => Ok(Some(value)),
                _ => Err(SharedString::from(format!(
                    "{what} is a whole number from 1 to {most}."
                ))),
            }
        };
        Ok(InsightSettings {
            disabled: self.disabled,
            disabled_servers: (self.servers.iter())
                .filter(|row| row.off && !row.unverified)
                .map(|row| row.id.to_owned())
                .collect(),
            languages: (self.servers.iter())
                .filter(|row| row.unverified && !row.off)
                .map(|row| row.id.to_owned())
                .collect(),
            tool_paths: (self.servers.iter())
                .filter_map(|row| {
                    let path = row.path.read(cx).content.trim().to_owned();
                    (!path.is_empty()).then(|| (row.id.to_owned(), path))
                })
                .collect(),
            idle_minutes: number(&self.idle, MAX_IDLE_MINUTES, "The idle time in minutes")?,
            max_servers: number(&self.cap, MAX_SERVERS as u64, "The number of servers")?
                .map(|servers| servers as usize),
        })
    }

    /// Stores the form and applies it: the project's documents are reopened
    /// under the new settings.
    pub(super) fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let chosen = match self.settings(cx) {
            Ok(chosen) => chosen,
            Err(problem) => {
                self.problem = Some(problem);
                cx.notify();
                return;
            }
        };
        self.problem = None;
        let value = |settings: &InsightSettings| {
            (*settings != InsightSettings::default())
                .then(|| serde_json::to_value(settings).ok())
                .flatten()
        };
        self.host.save(value(&chosen), window, cx);
        // The two limits are the app's: every project's page keeps them.
        for other in context::snapshot(cx).projects {
            let mut theirs = load(&other, cx);
            let same = (theirs.idle_minutes, theirs.max_servers)
                == (chosen.idle_minutes, chosen.max_servers);
            if other != self.project && !same {
                theirs.idle_minutes = chosen.idle_minutes;
                theirs.max_servers = chosen.max_servers;
                let host = PageHost {
                    project: other,
                    page: PAGE,
                };
                host.save(value(&theirs), window, cx);
            }
        }
        if let Some(sync) = super::sync(cx) {
            let project = self.project.clone();
            sync.update(cx, |sync, cx| sync.settings_changed(&project, cx));
        }
        cx.notify();
    }

    pub(super) fn set_disabled(&mut self, off: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.disabled = off;
        self.save(window, cx);
    }

    /// Switches one of the other languages' servers on or off.
    #[cfg(test)]
    pub(super) fn set_language(
        &mut self,
        server: &str,
        on: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(row) = (self.servers.iter_mut()).find(|row| row.unverified && row.id == server)
        {
            row.off = !on;
        }
        self.save(window, cx);
    }

    #[cfg(test)]
    pub(super) fn set_tool_path(
        &mut self,
        server: &str,
        path: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(row) = self.servers.iter().find(|row| row.id == server) {
            (row.path).update(cx, |field, cx| field.set_text(path.to_owned(), cx));
        }
        self.save(window, cx);
    }
}

fn check(
    id: impl Into<gpui::ElementId>,
    label: SharedString,
    on: bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .role(Role::CheckBox)
        .aria_label(label.clone())
        .cursor_pointer()
        .flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .size(px(14.))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(3.))
                .border_1()
                .border_color(content(if on { 0.6 } else { 0.25 }))
                .when(on, |d| {
                    d.bg(content(0.85))
                        .child(icon("check", 10.).text_color(pane_fill()))
                }),
        )
        .child(
            div()
                .text_size(px(13.))
                .text_color(content(0.9))
                .child(label),
        )
}

fn hint(text: impl Into<SharedString>) -> gpui::Div {
    div()
        .text_size(px(11.))
        .line_height(px(16.5))
        .text_color(content(0.45))
        .child(text.into())
}

fn boxed(field: &Entity<Input>) -> gpui::Div {
    div()
        .h(px(30.))
        .px(px(10.))
        .flex()
        .items_center()
        .rounded_md()
        .border_1()
        .border_color(content(0.1))
        .bg(content(0.04))
        .child(div().flex_1().min_w_0().child(field.clone()))
}

impl Render for Page {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let enabled = !self.disabled;
        let mut servers = Vec::new();
        // The languages that are off until asked for, under their own label.
        let mut others = Vec::new();
        for (n, row) in self.servers.iter().enumerate() {
            let label = if row.unverified {
                format!("{} ({})", row.languages, row.id)
            } else {
                format!("{} ({})", row.id, row.languages)
            };
            let element = div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    check(("code-insight-server", n), label.into(), !row.off).on_click(
                        cx.listener(move |this, _, window, cx| {
                            this.servers[n].off = !this.servers[n].off;
                            this.save(window, cx);
                        }),
                    ),
                )
                .when(!row.off || !row.unverified, |d| {
                    d.child(boxed(&row.path).ml(px(22.)))
                });
            if row.unverified {
                others.push(element);
            } else {
                servers.push(element);
            }
        }
        div()
            .id("code-insight-settings")
            .debug_selector(|| "code-insight-settings".into())
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        check(
                            "code-insight-enabled",
                            "Language servers for this project".into(),
                            enabled,
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            let off = !this.disabled;
                            this.set_disabled(off, window, cx);
                        })),
                    )
                    .child(hint(
                        "Off: no server is started for this project's files, by the editor or by an agent.",
                    )),
            )
            .when(enabled, |d| {
                d.child(section_label("Servers"))
                    .children(servers)
                    .child(hint(
                        "A path set here is used as it is; when it is not there, nothing else is tried.",
                    ))
                    .child(section_label(
                        "Other languages \u{2014} not yet verified",
                    ))
                    .children(others)
                    .child(hint(
                        "Off: files of the language open as they did before code insight, with no server started or looked for.",
                    ))
            })
            .child(section_label("Limits, for every project"))
            .child(
                div()
                    .flex()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(hint("Minutes an unused server is kept"))
                            .child(boxed(&self.idle)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(hint("Servers alive at most"))
                            .child(boxed(&self.cap)),
                    ),
            )
            .when_some(self.problem.clone(), |d, problem| {
                d.child(
                    div()
                        .text_size(px(12.))
                        .text_color(gpui::rgb(0xf87171))
                        .child(problem),
                )
            })
            .child(
                div().flex().justify_end().child(
                    button("code-insight-save", "Save")
                        .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
                ),
            )
    }
}
