//! The project's "Run configurations" page: the ordered list with add,
//! duplicate, remove and move, a form for the selected one with the command
//! line it comes to for the focused file, the variables it may use, and the
//! project's test runner. What it saves is `Workspace::project_runs`, the
//! same list the agents' tools read.
mod groups;
#[cfg(any(test, feature = "selftest"))]
#[allow(unused_imports)]
pub(crate) use groups::MemberField;
#[allow(unused_imports)]
pub(crate) use groups::WaitKind;

use super::{Marks, header::PAGE, targets};
use crate::{
    code_shell::{
        context::{self, Checkout},
        settings::{self, PageContext, SettingsPage},
    },
    input::{self, Input},
    project_settings::button,
    theme::*,
};
use gpui::{
    App, Context, Entity, FocusHandle, Focusable, SharedString, Subscription, Task, Window, div,
    prelude::*, px, rgb,
};
use monocode_core::{
    code::{
        run::{self, ProjectRuns, RunConfig, TestRunner, Variable},
        toolchain::Toolchain,
    },
    files::path_key,
    terminal_setup::invalid_lines,
};
use std::{path::Path, rc::Rc};

pub(super) fn init(marks: Entity<Marks>, cx: &mut App) {
    settings::register(
        SettingsPage {
            id: PAGE,
            title: "Run configurations",
            icon: "play",
            build: Rc::new(move |context: PageContext, _, cx| {
                let marks = marks.clone();
                cx.new(|cx| RunsPage::new(context.project, Some(marks), cx))
                    .into()
            }),
        },
        cx,
    );
}

/// The fields of the selected configuration's form, for a test to type
/// into.
#[cfg(any(test, feature = "selftest"))]
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Field {
    Name,
    Command,
    Arguments,
    Folder,
    Environment,
}

pub(crate) struct RunsPage {
    project: String,
    /// The list as it is being edited. A configuration without a command
    /// yet stays here and is left out of what is saved.
    runs: ProjectRuns,
    selected: Option<String>,
    name: Entity<Input>,
    command: Entity<Input>,
    arguments: Entity<Input>,
    folder: Entity<Input>,
    environment: Entity<Input>,
    /// The form differs from what is saved.
    unsaved: bool,
    /// The form is being filled from the list, not typed into.
    filling: bool,
    /// Told when the test runner changes, since the gutter's marks depend
    /// on it.
    marks: Option<Entity<Marks>>,
    /// The group being edited, below the configurations.
    group: groups::GroupForm,
    /// The command line shown under the form, worked out off the UI thread
    /// for what it was asked about.
    preview: Option<Preview>,
    /// How often the preview was worked out, for the tests.
    #[cfg(test)]
    previews: usize,
    _events: Vec<Subscription>,
}

/// What the preview depends on: the configuration and the focused file's
/// place and text version.
#[derive(PartialEq)]
struct PreviewKey {
    config: RunConfig,
    file: Option<targets::FileKey>,
}

struct Preview {
    key: PreviewKey,
    /// The answer for `key`, or for the key before while the new one is
    /// being worked out.
    shown: Option<Result<String, String>>,
    _task: Task<()>,
}

impl Focusable for RunsPage {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.name.focus_handle(cx)
    }
}

/// One argument as a shell would need it typed.
fn quoted(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_alphanumeric() || "-_=+,./:@%^".contains(c));
    match plain {
        true => word.to_owned(),
        false => format!("'{}'", word.replace('\'', "'\\''")),
    }
}

impl RunsPage {
    pub(crate) fn new(
        project: String,
        marks: Option<Entity<Marks>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let line = |label: &str, placeholder: &str, cx: &mut Context<Self>| {
            cx.new(|cx| Input::field(label.to_owned(), placeholder.to_owned(), content(0.35), cx))
        };
        let name = line("Name", "Serve", cx);
        let command = line("Command", "$PYTHON manage.py runserver", cx);
        let arguments = line("Arguments", "--port 8000", cx);
        let folder = line("Working folder", "The checkout", cx);
        let environment = cx.new(|cx| {
            let mut field = Input::multiline_field("Environment", "DEBUG=1\nPORT=8000", cx);
            field.set_min_height(58.);
            field.set_max_height(58.);
            field
        });
        let mut events: Vec<Subscription> = [&name, &command, &arguments, &folder, &environment]
            .map(|field| cx.subscribe(field, |this, _, _: &input::Changed, cx| this.typed(cx)))
            .into();
        let (group, group_typed) = groups::GroupForm::new(cx);
        events.push(group_typed);
        let runs = settings::runs(&project, cx);
        let mut this = Self {
            project,
            selected: None,
            runs,
            name,
            command,
            arguments,
            folder,
            environment,
            unsaved: false,
            filling: false,
            group,
            marks,
            preview: None,
            #[cfg(test)]
            previews: 0,
            _events: events,
        };
        let first = this.runs.configs.first().map(|config| config.id.clone());
        this.select(first, cx);
        let first = this.runs.groups.first().map(|group| group.id.clone());
        this.select_group(first, cx);
        this
    }

    #[cfg(test)]
    pub(crate) fn runs(&self) -> &ProjectRuns {
        &self.runs
    }
    pub(crate) fn selected(&self) -> Option<&RunConfig> {
        self.runs.get(self.selected.as_deref()?)
    }
    #[cfg(any(test, feature = "selftest"))]
    fn field(&self, field: Field) -> &Entity<Input> {
        match field {
            Field::Name => &self.name,
            Field::Command => &self.command,
            Field::Arguments => &self.arguments,
            Field::Folder => &self.folder,
            Field::Environment => &self.environment,
        }
    }
    /// Types `text` into a field of the form, as the keyboard would.
    #[cfg(any(test, feature = "selftest"))]
    pub(crate) fn set_field(&mut self, field: Field, text: &str, cx: &mut Context<Self>) {
        let input = self.field(field).clone();
        input.update(cx, |input, cx| input.set_text(text.to_owned(), cx));
        self.typed(cx);
    }

    /// Shows a configuration in the form.
    pub(crate) fn select(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        self.selected = id;
        let config = self.selected().cloned().unwrap_or_default();
        self.filling = true;
        for (field, text) in [
            (&self.name, config.name),
            (&self.command, config.command),
            (&self.arguments, config.args),
            (&self.folder, config.cwd),
            (&self.environment, config.env),
        ] {
            field.update(cx, |field, cx| field.set_text(text, cx));
        }
        self.filling = false;
        cx.notify();
    }

    /// The form as a configuration.
    fn form(&self, id: String, cx: &App) -> RunConfig {
        let text = |field: &Entity<Input>| field.read(cx).content.to_string();
        RunConfig {
            id,
            name: text(&self.name),
            command: text(&self.command),
            args: text(&self.arguments),
            cwd: text(&self.folder),
            env: text(&self.environment),
        }
    }

    /// Something was typed: the list follows the form.
    fn typed(&mut self, cx: &mut Context<Self>) {
        if self.filling {
            return;
        }
        let Some(id) = self.selected.clone() else {
            return;
        };
        let config = self.form(id, cx);
        if self.selected() != Some(&config) {
            self.runs.update(config);
            self.unsaved = true;
        }
        cx.notify();
    }

    /// Stores the list. Configurations without a command are kept here and
    /// not written, since nothing could run them.
    pub(crate) fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let before = settings::runs(&self.project, cx).test_runner;
        let runs = self.runs.clone();
        settings::save_runs(&self.project, runs, window, cx);
        self.unsaved = false;
        if before != self.runs.test_runner
            && let Some(marks) = &self.marks
        {
            marks.update(cx, |marks, cx| marks.refresh(cx));
        }
        cx.notify();
    }

    pub(crate) fn add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let taken = |name: &str| self.runs.configs.iter().any(|config| config.name == name);
        let mut name = "New configuration".to_owned();
        let mut n = 2;
        while taken(&name) {
            name = format!("New configuration {n}");
            n += 1;
        }
        let id = self.runs.add(RunConfig {
            name,
            ..RunConfig::default()
        });
        self.select(Some(id), cx);
        window.focus(&self.command.focus_handle(cx), cx);
        self.save(window, cx);
    }

    pub(crate) fn duplicate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mut copy) = self.selected().cloned() else {
            return;
        };
        let at = (self.runs.configs.iter()).position(|config| config.id == copy.id);
        copy.name = format!("{} copy", copy.label());
        let id = self.runs.add(copy);
        if let Some(at) = at {
            self.runs.move_to(&id, at + 1);
        }
        self.select(Some(id), cx);
        self.save(window, cx);
    }

    pub(crate) fn remove(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        let at = (self.runs.configs.iter()).position(|config| config.id == id);
        self.runs.remove(&id);
        // Groups lose the configuration too.
        self.fill_group(cx);
        let next = at
            .and_then(|at| {
                let configs = &self.runs.configs;
                configs.get(at).or_else(|| configs.last())
            })
            .map(|config| config.id.clone());
        self.select(next, cx);
        self.save(window, cx);
    }

    /// Moves the selected configuration one place up or down.
    pub(crate) fn shift(&mut self, down: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        let Some(at) = (self.runs.configs.iter()).position(|config| config.id == id) else {
            return;
        };
        let to = match down {
            true => at + 1,
            false => at.saturating_sub(1),
        };
        if to != at && to < self.runs.configs.len() {
            self.runs.move_to(&id, to);
            self.save(window, cx);
        }
    }

    pub(crate) fn set_runner(
        &mut self,
        runner: TestRunner,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.runs.test_runner != runner {
            self.runs.test_runner = runner;
            self.save(window, cx);
        }
    }

    /// The command line the selected configuration comes to for the focused
    /// file, or the sentence saying what it lacks. Nothing until the first
    /// answer is in.
    pub(crate) fn preview(&self) -> Option<Result<String, String>> {
        self.preview.as_ref()?.shown.clone()
    }

    #[cfg(test)]
    pub(super) fn previews(&self) -> usize {
        self.previews
    }

    /// Works the preview out again when what it depends on has changed.
    /// Finding the interpreter reads the disk, so that runs on a background
    /// thread; drawing only compares keys.
    pub(super) fn refresh_preview(&mut self, cx: &mut Context<Self>) {
        let Some(config) = self.selected().cloned() else {
            self.preview = None;
            return;
        };
        let project = path_key(&self.project);
        let focused =
            context::focused_file(cx).filter(|file| path_key(&file.checkout.project) == project);
        let key = PreviewKey {
            config,
            file: focused
                .as_ref()
                .and_then(|file| targets::file_key(file, cx)),
        };
        if self.preview.as_ref().is_some_and(|known| known.key == key) {
            return;
        }
        let (checkout, context) = match focused {
            Some(file) => {
                let context =
                    targets::editor_context(Path::new(&file.path), &file.checkout, None, cx);
                (file.checkout, context)
            }
            None => {
                let checkout = Checkout {
                    project: self.project.clone(),
                    folder: self.project.clone(),
                };
                let context = targets::bare_context(&checkout);
                (checkout, context)
            }
        };
        #[cfg(test)]
        {
            self.previews += 1;
        }
        let setup = context::terminal_setup(&checkout.project, cx);
        let config = key.config.clone();
        let asked = cx.background_executor().spawn(async move {
            let toolchain = Toolchain::for_folder(
                &setup,
                Path::new(&checkout.folder),
                Path::new(&checkout.project),
            );
            match run::resolve(&config, &context, &toolchain) {
                Ok(spawn) => Ok(std::iter::once(&spawn.command)
                    .chain(&spawn.args)
                    .map(|word| quoted(word))
                    .collect::<Vec<_>>()
                    .join(" ")),
                Err(unresolved) => Err(unresolved.to_string()),
            }
        });
        // What was shown stays until the new answer is in.
        let shown = self.preview.take().and_then(|old| old.shown);
        let (config, file) = (key.config.clone(), key.file.clone());
        self.preview = Some(Preview {
            key,
            shown,
            _task: cx.spawn(async move |this, cx| {
                let answer = asked.await;
                let _ = this.update(cx, |this, cx| {
                    if let Some(known) = &mut this.preview
                        && known.key.config == config
                        && known.key.file == file
                    {
                        known.shown = Some(answer);
                        cx.notify();
                    }
                });
            }),
        });
    }
}

fn label(text: &'static str) -> gpui::Div {
    div()
        .text_size(px(12.))
        .line_height(px(16.5))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(content(0.8))
        .child(text)
}

fn hint(text: impl Into<SharedString>) -> gpui::Div {
    div()
        .text_size(px(11.))
        .line_height(px(16.5))
        .text_color(content(0.45))
        .child(text.into())
}

fn boxed(field: &Entity<Input>, multiline: bool) -> gpui::Div {
    div()
        .px(px(10.))
        .flex()
        .items_center()
        .rounded_md()
        .border_1()
        .border_color(content(0.1))
        .bg(content(0.04))
        .map(|d| match multiline {
            true => d.py(px(6.)),
            false => d.h(px(32.)),
        })
        .child(div().flex_1().min_w_0().child(field.clone()))
}

fn labelled(text: &'static str, field: &Entity<Input>, multiline: bool) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(label(text))
        .child(boxed(field, multiline))
}

impl RunsPage {
    fn list(&self, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let count = self.runs.configs.len();
        let rows = self.runs.configs.iter().enumerate().map(|(ix, config)| {
            let selected = self.selected.as_deref() == Some(config.id.as_str());
            let id = config.id.clone();
            let name = match config.label() {
                label if label.is_empty() => "Unnamed".to_owned(),
                label => label,
            };
            div()
                .id(("run-configuration", ix))
                .role(gpui::Role::ListItem)
                .aria_label(name.clone())
                .aria_selected(selected)
                .h(px(30.))
                .px(px(8.))
                .flex()
                .items_center()
                .gap(px(2.))
                .rounded_md()
                .cursor_pointer()
                .text_size(px(12.))
                .map(|d| match selected {
                    true => d.bg(content(0.1)).text_color(content(1.)),
                    false => d.text_color(content(0.7)).hover(|s| s.bg(content(0.05))),
                })
                .child(div().flex_1().min_w_0().truncate().child(name))
                .when(config.command.trim().is_empty(), |d| {
                    d.child(hint("no command yet"))
                })
                .when(selected, |d| {
                    d.child(
                        icon_button_enabled(
                            "run-configuration-up",
                            "Move up",
                            "chevron-up",
                            ix > 0,
                        )
                        .on_click(cx.listener(|this, _, window, cx| this.shift(false, window, cx))),
                    )
                    .child(
                        icon_button_enabled(
                            "run-configuration-down",
                            "Move down",
                            "chevron-down",
                            ix + 1 < count,
                        )
                        .on_click(cx.listener(|this, _, window, cx| this.shift(true, window, cx))),
                    )
                    .child(
                        icon_button("run-configuration-copy", "Duplicate", "response-copy")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.duplicate(window, cx)),
                            ),
                    )
                    .child(
                        icon_button("run-configuration-remove", "Remove", "trash")
                            .on_click(cx.listener(|this, _, window, cx| this.remove(window, cx))),
                    )
                })
                .on_click(cx.listener(move |this, _, _, cx| this.select(Some(id.clone()), cx)))
        });
        div()
            .id("run-configurations")
            .debug_selector(|| "run-configurations".into())
            .role(gpui::Role::List)
            .aria_label("Run configurations")
            .flex()
            .flex_col()
            .gap(px(2.))
            .p(px(4.))
            .rounded_md()
            .border_1()
            .border_color(content(0.1))
            .children(rows)
            .when(count == 0, |d| {
                d.child(div().px(px(8.)).py(px(6.)).child(hint(
                    "No configurations yet. Files still offer their own run and test tasks.",
                )))
            })
    }

    fn form_fields(&self) -> gpui::Div {
        let config = self.selected().cloned().unwrap_or_default();
        let invalid = invalid_lines(&config.env);
        let preview = self.preview().map(|preview| {
            let (text, color): (String, gpui::Hsla) = match preview {
                Ok(line) => (line, content(0.85)),
                Err(sentence) => (sentence, rgb(0xfbbf24).into()),
            };
            div()
                .id("run-configuration-preview")
                .debug_selector(|| "run-configuration-preview".into())
                .px(px(10.))
                .py(px(6.))
                .rounded_md()
                .bg(content(0.04))
                .font_family(crate::fonts::mono())
                .text_size(px(12.))
                .line_height(px(17.))
                .text_color(color)
                .child(text)
        });
        let used = run::variables_used(&config);
        let variables = Variable::ALL.into_iter().map(|variable| {
            div()
                .flex()
                .gap_2()
                .text_size(px(11.))
                .line_height(px(16.5))
                .child(
                    div()
                        .w(px(118.))
                        .flex_shrink_0()
                        .font_family(crate::fonts::mono())
                        .text_color(match used.contains(&variable) {
                            true => content(0.95),
                            false => content(0.6),
                        })
                        .child(format!("${}", variable.name())),
                )
                .child(
                    div()
                        .min_w_0()
                        .text_color(content(0.45))
                        .child(variable.description()),
                )
        });
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(labelled("Name", &self.name, false))
            .child(labelled("Command", &self.command, false).child(hint(
                "Split like a shell would, but no shell runs it: pipes need sh -c '\u{2026}'.",
            )))
            .child(labelled("Arguments", &self.arguments, false))
            .child(labelled("Working folder", &self.folder, false).child(hint(
                "Empty for the checkout; otherwise from the checkout, or absolute.",
            )))
            .child(labelled("Environment", &self.environment, true).when(
                !invalid.is_empty(),
                |d| {
                    d.child(
                        hint(format!(
                            "Not KEY=VALUE: line {}",
                            invalid
                                .iter()
                                .map(|line| line.to_string())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ))
                        .text_color(rgb(0xfbbf24)),
                    )
                },
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(label("Runs as"))
                    .children(preview)
                    .child(hint("For the focused file, in its checkout.")),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(label("Variables"))
                    .children(variables),
            )
    }
}

impl Render for RunsPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.refresh_preview(cx);
        let runner = self.runs.test_runner;
        let choice = |id: &'static str, text: &'static str, value: TestRunner| {
            button(id, text)
                .when(runner == value, |b| {
                    b.bg(content(0.1)).text_color(content(1.))
                })
                .on_click(
                    cx.listener(move |this, _, window, cx| this.set_runner(value, window, cx)),
                )
        };
        let runners = div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(label("Python tests run with"))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(choice("run-tests-pytest", "pytest", TestRunner::Pytest))
                    .child(choice(
                        "run-tests-unittest",
                        "unittest",
                        TestRunner::Unittest,
                    )),
            );
        let add = button("run-configuration-add", "Add configuration")
            .on_click(cx.listener(|this, _, window, cx| this.add(window, cx)));
        let save = button("run-configurations-save", "Save")
            .when(!self.unsaved, |b| b.opacity(0.5))
            .on_click(cx.listener(|this, _, window, cx| this.save(window, cx)));
        let body = div()
            .id("run-configurations-page")
            .debug_selector(|| "run-configurations-page".into())
            .flex()
            .flex_col()
            .gap_4()
            .child(self.list(cx))
            .child(div().flex().child(add))
            .when(self.selected().is_some(), |d| d.child(self.form_fields()))
            .child(runners)
            .child(self.groups_section(cx));
        // Under the scrolling body, so it is there however long the form is.
        let editing = self.selected().is_some() || self.selected_group().is_some();
        let footer = editing.then(|| {
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(save)
                .when(self.unsaved, |d| d.child(hint("Not saved yet.")))
        });
        crate::code_shell::settings::page_frame("run-configurations-scroll", body, footer)
    }
}
