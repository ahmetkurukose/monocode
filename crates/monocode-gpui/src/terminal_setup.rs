//! A project's terminal setup: the dialog that edits it, and the setups
//! every conversation can read to give its agent the same environment.
use crate::{
    input::{self, Input},
    popover::AnchorSlot,
    project_settings::button,
    theme::*,
};
use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, Global, Role, Subscription, Window,
    div, prelude::*, px, rgb,
};
use monocode_core::{
    files::path_key,
    project_appearance::ProjectAppearance,
    terminal_setup::{TerminalSetup, Venv, find_venv, invalid_lines, parse_environment},
};
use std::{collections::HashMap, path::Path};

/// The saved setups by project (`path_key`), published by the shell.
#[derive(Default)]
pub(crate) struct Setups(pub(crate) HashMap<String, TerminalSetup>);
impl Global for Setups {}

/// What an agent working in `folder` of `project` is given: the project's
/// variables and its virtual environment, when the setup includes agents.
pub(crate) fn agent_environment(project: &str, folder: &str, cx: &App) -> Vec<(String, String)> {
    if project.starts_with("remote://") {
        return Vec::new();
    }
    let Some(setup) = cx
        .try_global::<Setups>()
        .and_then(|setups| setups.0.get(&path_key(project)))
        .filter(|setup| setup.agents)
    else {
        return Vec::new();
    };
    let launch = setup.launch(Path::new(folder), Path::new(project));
    if launch.environment.is_empty() && launch.venv.is_none() {
        return Vec::new();
    }
    launch.process_environment(&monocode_core::scm::terminal_search_path())
}

pub(crate) enum Event {
    /// `None` when nothing is asked for any more.
    Save(Option<TerminalSetup>),
    Close,
}

pub(crate) struct TerminalSetupDialog {
    project: String,
    appearance: ProjectAppearance,
    environment: Entity<Input>,
    env_file: Entity<Input>,
    venv_path: Entity<Input>,
    commands: Entity<Input>,
    venv: VenvChoice,
    agents: bool,
    /// Saved values stay out of sight until asked for: the dialog may be
    /// opened with someone looking on.
    revealed: bool,
    has_saved: bool,
    bounds: AnchorSlot,
    _events: Vec<Subscription>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum VenvChoice {
    Off,
    Auto,
    Custom,
}

impl EventEmitter<Event> for TerminalSetupDialog {}

impl Focusable for TerminalSetupDialog {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        if self.revealed {
            self.environment.focus_handle(cx)
        } else {
            self.env_file.focus_handle(cx)
        }
    }
}

impl TerminalSetupDialog {
    pub(crate) fn new(
        project: String,
        appearance: ProjectAppearance,
        current: Option<TerminalSetup>,
        cx: &mut Context<Self>,
    ) -> Self {
        let has_saved = current.is_some();
        let current = current.unwrap_or_default();
        let multiline = |label: &str, placeholder: &str, text: &str, cx: &mut Context<Self>| {
            cx.new(|cx| {
                let mut field = Input::multiline_field(label, placeholder, cx);
                field.set_min_height(76.);
                field.set_max_height(76.);
                field.set_text(text.to_owned(), cx);
                field
            })
        };
        let line = |label: &str, placeholder: &str, text: &str, cx: &mut Context<Self>| {
            cx.new(|cx| {
                let mut field = Input::field(label, placeholder, content(0.35), cx);
                field.set_text(text.to_owned(), cx);
                field
            })
        };
        let environment = multiline(
            "Environment variables",
            "DATABASE_URL=postgres://localhost/app\nDEBUG=1",
            &current.environment,
            cx,
        );
        let env_file = line("Environment file", ".env", &current.env_file, cx);
        let (venv, path) = match &current.venv {
            Venv::Off => (VenvChoice::Off, ""),
            Venv::Auto => (VenvChoice::Auto, ""),
            Venv::Path(path) => (VenvChoice::Custom, path.as_str()),
        };
        let venv_path = line("Virtual environment folder", ".venv", path, cx);
        let commands = multiline(
            "Startup commands",
            "nvm use\nsource scripts/env.sh",
            &current.commands,
            cx,
        );
        let mut events = Vec::new();
        for field in [&environment, &env_file, &venv_path, &commands] {
            // Validation and the found folder follow what is typed.
            events.push(cx.subscribe(field, |_, _, _: &input::Changed, cx| cx.notify()));
        }
        for field in [&env_file, &venv_path] {
            events.push(cx.subscribe(field, |this, _, _: &input::Submitted, cx| {
                this.save(cx);
            }));
        }
        Self {
            project,
            appearance,
            revealed: current.environment.trim().is_empty(),
            environment,
            env_file,
            venv_path,
            commands,
            venv,
            agents: current.agents,
            has_saved,
            bounds: AnchorSlot::default(),
            _events: events,
        }
    }

    /// What is typed for the variables, and whether the keyboard is there.
    #[cfg(test)]
    pub(crate) fn typed_environment(&self, window: &Window, cx: &App) -> (String, bool) {
        (
            self.environment.read(cx).content.to_string(),
            self.environment.focus_handle(cx).is_focused(window),
        )
    }

    pub(crate) fn measured_height(&self) -> Option<f32> {
        self.bounds.get().map(|bounds| bounds.size.height.as_f32())
    }

    fn setup(&self, cx: &App) -> TerminalSetup {
        TerminalSetup {
            environment: self.environment.read(cx).content.to_string(),
            env_file: self.env_file.read(cx).content.to_string(),
            venv: match self.venv {
                VenvChoice::Off => Venv::Off,
                VenvChoice::Auto => Venv::Auto,
                VenvChoice::Custom => Venv::Path(self.venv_path.read(cx).content.to_string()),
            },
            commands: self.commands.read(cx).content.to_string(),
            agents: self.agents,
        }
        .normalized()
    }

    fn save(&mut self, cx: &mut Context<Self>) -> bool {
        let setup = self.setup(cx);
        if !invalid_lines(&setup.environment).is_empty() {
            // The lines are named under the field; nothing is saved.
            self.revealed = true;
            cx.notify();
            return false;
        }
        cx.emit(Event::Save((!setup.is_empty()).then_some(setup)));
        true
    }

    fn choose(&mut self, choice: VenvChoice, window: &mut Window, cx: &mut Context<Self>) {
        self.venv = choice;
        if choice == VenvChoice::Custom {
            window.focus(&self.venv_path.focus_handle(cx), cx);
        }
        cx.notify();
    }

    /// What the chosen virtual environment comes to in the project's folder.
    fn venv_status(&self, setup: &TerminalSetup) -> Option<(String, bool)> {
        if setup.venv == Venv::Off {
            return None;
        }
        let project = Path::new(&self.project);
        Some(match find_venv(&setup.venv, project, project) {
            Some(found) => {
                let shown = found.strip_prefix(project).unwrap_or(&found);
                (format!("Found: {}", shown.display()), true)
            }
            None if self.venv == VenvChoice::Auto => (
                "No .venv or venv in the project yet. Terminals say so and start without one."
                    .into(),
                false,
            ),
            None => ("This folder is not a virtual environment.".into(), false),
        })
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

fn hint(text: impl Into<gpui::SharedString>) -> gpui::Div {
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

impl Render for TerminalSetupDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let setup = self.setup(cx);
        let invalid = invalid_lines(&setup.environment);
        let names: Vec<String> = parse_environment(&setup.environment)
            .into_iter()
            .map(|(key, _)| key)
            .collect();
        let venv_status = self.venv_status(&setup);
        let venv = self.venv;
        let choice = |id: &'static str, text: &'static str, value: VenvChoice| {
            button(id, text)
                .when(venv == value, |b| {
                    b.bg(content(0.1)).text_color(content(1.))
                })
                .on_click(cx.listener(move |this, _, window, cx| this.choose(value, window, cx)))
        };
        div()
            .id("project-terminal-setup-dialog")
            .debug_selector(|| "project-terminal-setup-dialog".into())
            .role(Role::Dialog)
            .aria_label("Terminal Setup")
            .relative()
            .min_h_0()
            .max_h(px(window.viewport_size().height.as_f32() - 34.))
            .flex()
            .flex_col()
            .text_color(content(1.))
            .child(self.bounds.layout_probe())
            .on_key_down(cx.listener(|_, event: &gpui::KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    cx.emit(Event::Close);
                    window.prevent_default();
                    cx.stop_propagation();
                }
            }))
            .child(
                div()
                    .flex_shrink_0()
                    .flex()
                    .items_start()
                    .gap_2()
                    .px_4()
                    .pt_3()
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .pt(px(2.))
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(px(16.))
                                    .line_height(px(20.))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child("Terminal Setup"),
                            )
                            .child(
                                div()
                                    .mt(px(2.))
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(12.))
                                    .line_height(px(16.5))
                                    .text_color(content(0.5))
                                    .child(format!(
                                        "What new terminals in {} start with",
                                        self.appearance.label(&self.project)
                                    )),
                            ),
                    )
                    .child(
                        icon_button("close-project-terminal-setup", "Close", "close")
                            .size(px(28.))
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(Event::Close))),
                    ),
            )
            .child(
                div()
                    .id("project-terminal-setup-body")
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_4()
                    .child(label("Environment variables"))
                    .map(|d| {
                        if self.revealed {
                            d.child(boxed(&self.environment, true))
                        } else {
                            d.child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .id("terminal-setup-variable-names")
                                            .debug_selector(|| {
                                                "terminal-setup-variable-names".into()
                                            })
                                            .min_w_0()
                                            .flex_1()
                                            .truncate()
                                            .text_size(px(12.))
                                            .text_color(content(0.7))
                                            .child(names.join(", ")),
                                    )
                                    .child(
                                        button("terminal-setup-show-variables", "Show and edit")
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.revealed = true;
                                                window.focus(
                                                    &this.environment.focus_handle(cx),
                                                    cx,
                                                );
                                                cx.notify();
                                            })),
                                    ),
                            )
                        }
                    })
                    .map(|d| match invalid.is_empty() {
                        true => d.child(hint("One NAME=value per line. Kept on this Mac, as plain text.")),
                        false => d.child(
                            div()
                                .id("terminal-setup-invalid-lines")
                                .debug_selector(|| "terminal-setup-invalid-lines".into())
                                .text_size(px(11.))
                                .line_height(px(16.5))
                                .text_color(rgb(0xff6467))
                                .child(format!(
                                    "Not NAME=value: line {}",
                                    invalid
                                        .iter()
                                        .map(usize::to_string)
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                )),
                        ),
                    })
                    .child(label("Environment file").mt_2())
                    .child(boxed(&self.env_file, false))
                    .child(hint(
                        "Read each time a terminal starts; secrets can stay in it. \
                         Variables above win over the file.",
                    ))
                    .child(label("Python virtual environment").mt_2())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(choice("terminal-setup-venv-off", "Off", VenvChoice::Off))
                            .child(choice("terminal-setup-venv-auto", "Automatic", VenvChoice::Auto))
                            .child(choice("terminal-setup-venv-custom", "Folder", VenvChoice::Custom)),
                    )
                    .when(venv == VenvChoice::Custom, |d| {
                        d.child(boxed(&self.venv_path, false))
                    })
                    .when_some(venv_status, |d, (text, found)| {
                        d.child(
                            div()
                                .id("terminal-setup-venv-status")
                                .debug_selector(|| "terminal-setup-venv-status".into())
                                .text_size(px(11.))
                                .line_height(px(16.5))
                                .text_color(if found { content(0.7) } else { content(0.45) })
                                .child(text),
                        )
                    })
                    .child(label("Startup commands").mt_2())
                    .child(boxed(&self.commands, true))
                    .child(hint(
                        "One per line, run at the first prompt after the virtual environment.",
                    ))
                    .child(
                        div()
                            .mt_2()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                button(
                                    "terminal-setup-agents",
                                    if self.agents { "Agents: included" } else { "Agents: not included" },
                                )
                                .when(self.agents, |b| b.bg(content(0.1)).text_color(content(1.)))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.agents = !this.agents;
                                    cx.notify();
                                })),
                            )
                            .child(hint(
                                "Agents get the variables and the virtual environment, not the commands.",
                            ).min_w_0().flex_1()),
                    ),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .px_4()
                    .pb_4()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(div().when(self.has_saved, |d| {
                        d.child(
                            button("clear-project-terminal-setup", "Clear")
                                .text_color(rgb(0xff6467))
                                .on_click(cx.listener(|_, _, _, cx| cx.emit(Event::Save(None)))),
                        )
                    }))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(hint("Applies to terminals opened from now on."))
                            .child(
                                button("cancel-project-terminal-setup", "Cancel")
                                    .on_click(cx.listener(|_, _, _, cx| cx.emit(Event::Close))),
                            )
                            .child(
                                button("save-project-terminal-setup", "Save")
                                    .text_color(content(1.))
                                    .bg(content(0.1))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        // Back to the lines that need fixing.
                                        if !this.save(cx) {
                                            window.focus(&this.environment.focus_handle(cx), cx);
                                        }
                                    })),
                            ),
                    ),
            )
    }
}
