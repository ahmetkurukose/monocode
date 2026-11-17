//! The Create Worktree dialog (spec §7.1): a name, where to start from, an optional
//! branch override and the repository's setup script. Submitting hands a
//! [`CreateRequest`] to the Shell's background creation service and closes at once.
use crate::{
    input::{self, Input},
    runtime::{Database, locked},
    theme::*,
};
use gpui::{
    AnyElement, Context, Entity, EventEmitter, FocusHandle, Focusable, Role, SharedString,
    Subscription, Task, Window, div, prelude::*, px, rgb,
};
use monocode_core::{
    git::Branch,
    worktree::{self, CreateRequest, CreatedBy, REMOTE_UNAVAILABLE, SetupPlan},
};
use std::path::{Path, PathBuf};

/// Start-from suggestions shown under the field.
const SUGGESTIONS: usize = 6;

pub(crate) enum DialogEvent {
    Submit {
        request: Box<CreateRequest>,
        run_setup: bool,
        /// Save this base as the repository's default (`main checkout`, base).
        default_base: Option<(PathBuf, String)>,
    },
    Cancel,
}

/// What the dialog reads from the repository off the main thread.
#[derive(Clone, Debug)]
struct Loaded {
    main: PathBuf,
    /// The repository's saved default base.
    saved_default: Option<String>,
    /// `base::detect_default`, used when nothing is saved.
    detected: Option<String>,
    branches: Vec<Branch>,
    setup: Option<SetupPlan>,
}

fn load(cwd: &Path, store: &Database) -> Result<Loaded, String> {
    let main = worktree::layout::main_checkout(cwd)?;
    let prefs = worktree::meta::repo_prefs(&*locked(store)?, &main).unwrap_or_default();
    let branches = crate::git_cache::load(cwd)
        .map(|s| s.branches)
        .unwrap_or_default();
    Ok(Loaded {
        detected: worktree::base::detect_default(&main),
        setup: worktree::setup::plan(&main, &main, "", prefs.setup_approved_hash.as_deref()),
        saved_default: prefs.default_base.filter(|b| !b.trim().is_empty()),
        branches,
        main,
    })
}

/// A readable placeholder name, different on each open.
fn suggestion() -> String {
    const FIRST: [&str; 8] = [
        "quiet", "bright", "swift", "calm", "bold", "fresh", "keen", "steady",
    ];
    const SECOND: [&str; 8] = [
        "river", "maple", "harbor", "comet", "meadow", "falcon", "summit", "lantern",
    ];
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as usize / 1000)
        .unwrap_or_default();
    format!("{}-{}", FIRST[n % 8], SECOND[(n / 8) % 8])
}

fn full_name(branch: &Branch) -> String {
    branch
        .remote
        .as_ref()
        .map_or_else(|| branch.name.clone(), |r| format!("{r}/{}", branch.name))
}

pub(crate) struct CreateWorktreeDialog {
    cwd: PathBuf,
    project_label: String,
    remote: bool,
    pub(crate) name: Entity<Input>,
    pub(crate) base: Entity<Input>,
    pub(crate) branch: Entity<Input>,
    loaded: Option<Result<Loaded, String>>,
    set_default: bool,
    advanced: bool,
    use_existing: bool,
    pub(crate) run_setup: bool,
    /// The live check of the branch override: `(text, valid?)`.
    branch_check: Option<(String, Result<(), String>)>,
    /// Submit was tried; required-field errors show from now on.
    attempted: bool,
    focus: FocusHandle,
    _check: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}
impl EventEmitter<DialogEvent> for CreateWorktreeDialog {}

impl CreateWorktreeDialog {
    pub(crate) fn new(
        project: String,
        prefill: String,
        store: Database,
        cx: &mut Context<Self>,
    ) -> Self {
        let field = |label: &str, placeholder: String, cx: &mut Context<Self>| {
            cx.new(|cx| {
                let mut input = Input::field(label.to_owned(), placeholder, content(0.35), cx);
                input.set_font_size(13.);
                input
            })
        };
        let name = field("Name", suggestion(), cx);
        name.update(cx, |input, cx| {
            input.set_text(prefill.trim().to_owned(), cx)
        });
        let base = field("Start from", "Default base".into(), cx);
        let branch = field("Branch name", "Same as the name".into(), cx);
        let mut subscriptions = Vec::new();
        for input in [&name, &base, &branch] {
            subscriptions
                .push(cx.subscribe(input, |this, _, _: &input::Submitted, cx| this.submit(cx)));
        }
        subscriptions.push(cx.subscribe(&name, |_, _, _: &input::Changed, cx| cx.notify()));
        subscriptions.push(cx.subscribe(&base, |_, _, _: &input::Changed, cx| cx.notify()));
        subscriptions.push(cx.subscribe(&branch, |this, _, _: &input::Changed, cx| {
            this.check_branch(cx)
        }));
        let remote = monocode_core::files::remote::is_remote(Path::new(&project));
        let cwd = PathBuf::from(&project);
        if !remote {
            let (folder, db) = (cwd.clone(), store.clone());
            let work = cx
                .background_executor()
                .spawn(async move { load(&folder, &db) });
            cx.spawn(async move |this, cx| {
                let loaded = work.await;
                let _ = this.update(cx, |this, cx| {
                    if let Ok(loaded) = &loaded {
                        let default = loaded
                            .saved_default
                            .clone()
                            .or_else(|| loaded.detected.clone())
                            .unwrap_or_else(|| "HEAD".into());
                        this.base.update(cx, |input, cx| {
                            input.set_placeholder(format!("Default · {default}"), cx)
                        });
                        // An approved script runs by default; a new one waits for a yes.
                        this.run_setup = loaded.setup.as_ref().is_some_and(|p| p.approved);
                    }
                    this.loaded = Some(loaded);
                    cx.notify();
                });
            })
            .detach();
        }
        Self {
            project_label: crate::workspace::project_name(&project).to_string(),
            cwd,
            remote,
            name,
            base,
            branch,
            loaded: None,
            set_default: false,
            advanced: false,
            use_existing: false,
            run_setup: false,
            branch_check: None,
            attempted: false,
            focus: cx.focus_handle(),
            _check: None,
            _subscriptions: subscriptions,
        }
    }

    /// Where focus goes when the dialog opens.
    pub(crate) fn initial_focus(&self, cx: &gpui::App) -> FocusHandle {
        self.name.read(cx).focus_handle(cx)
    }

    /// Where focus should return when it left the dialog (e.g. a closing menu restored
    /// its own after the dialog opened).
    pub(crate) fn lost_focus(&self, window: &Window, cx: &gpui::App) -> Option<FocusHandle> {
        let inside = self.focus.contains_focused(window, cx)
            || [&self.name, &self.base, &self.branch]
                .iter()
                .any(|input| input.read(cx).focus_handle(cx).is_focused(window));
        (!inside).then(|| self.initial_focus(cx))
    }

    fn text(input: &Entity<Input>, cx: &gpui::App) -> String {
        input.read(cx).content.trim().to_owned()
    }

    fn check_branch(&mut self, cx: &mut Context<Self>) {
        let text = Self::text(&self.branch, cx);
        if text.is_empty() || self.remote {
            self.branch_check = None;
            self._check = None;
            cx.notify();
            return;
        }
        let cwd = self.cwd.clone();
        let checked = text.clone();
        let work = cx
            .background_executor()
            .spawn(async move { worktree::naming::validate_branch(&cwd, &checked) });
        self._check = Some(cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                if Self::text(&this.branch, cx) == text {
                    this.branch_check = Some((text, result));
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }

    /// The typed branch override already exists locally or on origin.
    fn branch_exists(&self, branch: &str) -> bool {
        self.loaded
            .as_ref()
            .and_then(|l| l.as_ref().ok())
            .is_some_and(|l| {
                l.branches
                    .iter()
                    .any(|b| b.name == branch && b.remote.as_deref().is_none_or(|r| r == "origin"))
            })
    }

    /// Why the form cannot be submitted yet; `Err(None)` while a check is running.
    fn validate(&self, cx: &gpui::App) -> Result<(), Option<String>> {
        if self.remote {
            return Err(Some(REMOTE_UNAVAILABLE.into()));
        }
        match &self.loaded {
            None => return Err(None),
            Some(Err(error)) => return Err(Some(error.clone())),
            Some(Ok(_)) => {}
        }
        let name = Self::text(&self.name, cx);
        let branch = Self::text(&self.branch, cx);
        if !branch.is_empty() {
            match &self.branch_check {
                Some((checked, result)) if *checked == branch => {
                    result.clone().map_err(Some)?;
                }
                _ => return Err(None),
            }
            if self.branch_exists(&branch) && !self.use_existing {
                return Err(Some(format!(
                    "A branch named {branch} already exists. Use it, or choose another name."
                )));
            }
        }
        if !name.is_empty() {
            worktree::naming::slug(&name).map_err(Some)?;
        } else if branch.is_empty() {
            return Err(self
                .attempted
                .then(|| "Enter a name for the worktree.".to_owned()));
        }
        Ok(())
    }

    /// The error under the form, if any.
    pub(crate) fn problem(&self, cx: &gpui::App) -> Option<String> {
        self.validate(cx).err().flatten()
    }

    pub(crate) fn submit(&mut self, cx: &mut Context<Self>) {
        self.attempted = true;
        if self.validate(cx).is_err() {
            cx.notify();
            return;
        }
        let Some(Ok(loaded)) = &self.loaded else {
            return;
        };
        let name = Self::text(&self.name, cx);
        let base = Self::text(&self.base, cx);
        let branch = Self::text(&self.branch, cx);
        let existing = !branch.is_empty() && self.use_existing && self.branch_exists(&branch);
        let request = CreateRequest {
            base: (!base.is_empty() && !existing).then(|| base.clone()),
            branch: (!branch.is_empty()).then(|| branch.clone()),
            existing_branch: existing,
            created_by: CreatedBy::Dialog,
            ..CreateRequest::new(&self.cwd, &name)
        };
        let default_base = (self.set_default && !base.is_empty() && !existing)
            .then(|| (loaded.main.clone(), base));
        cx.emit(DialogEvent::Submit {
            request: Box::new(request),
            run_setup: self.run_setup && loaded.setup.is_some(),
            default_base,
        });
    }

    fn checkbox(
        id: &'static str,
        label: impl Into<SharedString>,
        on: bool,
        cx: &mut Context<Self>,
        toggle: impl Fn(&mut Self) + 'static,
    ) -> AnyElement {
        let label = label.into();
        div()
            .id(id)
            .debug_selector(move || id.to_owned())
            .role(Role::CheckBox)
            .aria_label(label.clone())
            .aria_toggled(if on {
                gpui::Toggled::True
            } else {
                gpui::Toggled::False
            })
            .flex()
            .items_center()
            .gap(px(8.))
            .cursor_pointer()
            .text_size(px(12.))
            .text_color(content(if on { 0.85 } else { 0.55 }))
            .hover(|s| s.text_color(content(0.9)))
            .child(
                div()
                    .size(px(14.))
                    .flex_shrink_0()
                    .rounded(px(4.))
                    .border_1()
                    .border_color(content(if on { 0.9 } else { 0.35 }))
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(on, |d| {
                        d.bg(content(0.9))
                            .child(icon("check", 10.).text_color(background_base()))
                    }),
            )
            .child(label)
            .on_click(cx.listener(move |this, _, _, cx| {
                toggle(this);
                cx.notify();
            }))
            .into_any_element()
    }

    fn field(
        label: &'static str,
        selector: &'static str,
        input: &Entity<Input>,
        disabled: bool,
    ) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(content(0.6))
                    .child(label),
            )
            .child(
                div()
                    .debug_selector(move || selector.to_owned())
                    .h(px(32.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(content(0.12))
                    .bg(content(0.04))
                    .when(disabled, |d| d.opacity(0.5))
                    .child(div().flex_1().min_w_0().child(input.clone())),
            )
            .into_any_element()
    }

    fn base_suggestions(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.base.read(cx).focus_handle(cx).is_focused(window) {
            return None;
        }
        let loaded = self.loaded.as_ref()?.as_ref().ok()?;
        let needle = Self::text(&self.base, cx).to_lowercase();
        let mut matches: Vec<String> = loaded
            .branches
            .iter()
            .filter(|b| b.remote.is_none())
            .chain(loaded.branches.iter().filter(|b| b.remote.is_some()))
            .map(full_name)
            .filter(|name| name.to_lowercase().contains(&needle) && name.to_lowercase() != needle)
            .collect();
        matches.dedup();
        matches.truncate(SUGGESTIONS);
        if matches.is_empty() {
            return None;
        }
        Some(
            div()
                .id("worktree-dialog-base-options")
                .debug_selector(|| "worktree-dialog-base-options".into())
                .role(Role::ListBox)
                .aria_label("Start from")
                .mt(px(4.))
                .p(px(4.))
                .rounded(px(8.))
                .border_1()
                .border_color(content(0.1))
                .bg(content(0.03))
                .flex()
                .flex_col()
                .children(matches.into_iter().enumerate().map(|(ix, name)| {
                    let picked = name.clone();
                    div()
                        .id(("worktree-dialog-base-option", ix))
                        .debug_selector(move || format!("worktree-dialog-base-option-{ix}"))
                        .role(Role::ListBoxOption)
                        .aria_label(name.clone())
                        .h(px(26.))
                        .px(px(8.))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .rounded(px(6.))
                        .cursor_pointer()
                        .text_size(px(12.))
                        .text_color(content(0.8))
                        .hover(|s| s.bg(content(0.07)))
                        .child(icon("git-branch", 12.).text_color(content(0.45)))
                        .child(div().truncate().child(name))
                        // A press would blur the field (and drop this list) before a click.
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.base
                                    .update(cx, |input, cx| input.set_text(picked.clone(), cx));
                                cx.notify();
                            }),
                        )
                }))
                .into_any_element(),
        )
    }

    fn setup_row(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let plan = self.loaded.as_ref()?.as_ref().ok()?.setup.clone()?;
        let run = self.run_setup;
        let choice = |id: &'static str,
                      label: &'static str,
                      on: bool,
                      value: bool,
                      cx: &mut Context<Self>| {
            div()
                .id(id)
                .debug_selector(move || id.to_owned())
                .role(Role::RadioButton)
                .aria_label(label)
                .aria_toggled(if on {
                    gpui::Toggled::True
                } else {
                    gpui::Toggled::False
                })
                .h(px(26.))
                .px(px(10.))
                .flex()
                .items_center()
                .rounded(px(6.))
                .cursor_pointer()
                .text_size(px(12.))
                .map(|d| {
                    if on {
                        d.bg(content(0.12)).text_color(content(1.))
                    } else {
                        d.text_color(content(0.55)).hover(|s| s.bg(content(0.06)))
                    }
                })
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.run_setup = value;
                    cx.notify();
                }))
        };
        Some(
            div()
                .debug_selector(|| "worktree-dialog-setup".into())
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_size(px(12.))
                                .text_color(content(0.6))
                                .child("Setup script"),
                        )
                        .child(
                            div()
                                .p(px(2.))
                                .flex()
                                .gap(px(2.))
                                .rounded(px(8.))
                                .bg(content(0.05))
                                .child(choice(
                                    "worktree-dialog-setup-run",
                                    "Run setup",
                                    run,
                                    true,
                                    cx,
                                ))
                                .child(choice(
                                    "worktree-dialog-setup-skip",
                                    "Skip",
                                    !run,
                                    false,
                                    cx,
                                )),
                        ),
                )
                .when(!plan.approved, |d| {
                    d.child(
                        div()
                            .id("worktree-dialog-script")
                            .debug_selector(|| "worktree-dialog-script".into())
                            .max_h(px(120.))
                            .overflow_y_scroll()
                            .p(px(8.))
                            .rounded(px(8.))
                            .bg(content(0.05))
                            .font_family(crate::fonts::mono())
                            .text_size(px(11.))
                            .line_height(px(16.))
                            .text_color(content(0.75))
                            .child(plan.script.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(content(0.45))
                            .child("This script is new. Running it trusts it for this repository."),
                    )
                })
                .into_any_element(),
        )
    }
}

impl Render for CreateWorktreeDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let disabled = self.remote || matches!(self.loaded, Some(Err(_)));
        for input in [&self.name, &self.base, &self.branch] {
            input.update(cx, |input, _| input.set_read_only(disabled));
        }
        let valid = self.validate(cx).is_ok();
        let problem = self.problem(cx);
        let base = Self::text(&self.base, cx);
        let branch = Self::text(&self.branch, cx);
        let exists = !branch.is_empty() && self.branch_exists(&branch);
        let advanced = self.advanced;
        let card = crate::modal::card().w(px(460.)).child(
            crate::modal::card_body()
                .gap(px(14.))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(crate::modal::title("New worktree"))
                        .child(
                            crate::modal::body(format!(
                                "A separate working copy of {} on its own branch.",
                                self.project_label
                            ))
                            .text_size(px(12.)),
                        ),
                )
                .child(Self::field(
                    "Name",
                    "worktree-dialog-name",
                    &self.name,
                    disabled,
                ))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .child(Self::field(
                            "Start from",
                            "worktree-dialog-base",
                            &self.base,
                            disabled,
                        ))
                        .children(self.base_suggestions(window, cx))
                        .when(!base.is_empty() && !disabled, |d| {
                            let on = self.set_default;
                            d.child(div().mt(px(8.)).child(Self::checkbox(
                                "worktree-dialog-set-default",
                                format!("Use {base} as this project's default"),
                                on,
                                cx,
                                |this| this.set_default = !this.set_default,
                            )))
                        }),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .child(
                            div()
                                .id("worktree-dialog-advanced")
                                .debug_selector(|| "worktree-dialog-advanced".into())
                                .role(Role::Button)
                                .aria_label("Advanced")
                                .aria_expanded(advanced)
                                .flex()
                                .items_center()
                                .gap(px(4.))
                                .cursor_pointer()
                                .text_size(px(12.))
                                .text_color(content(0.55))
                                .hover(|s| s.text_color(content(0.9)))
                                .child(icon(
                                    if advanced {
                                        "chevron-down"
                                    } else {
                                        "chevron-right"
                                    },
                                    12.,
                                ))
                                .child("Advanced")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.advanced = !this.advanced;
                                    if this.advanced {
                                        window.focus(&this.branch.read(cx).focus_handle(cx), cx);
                                    }
                                    cx.notify();
                                })),
                        )
                        .when(advanced, |d| {
                            let on = self.use_existing;
                            d.child(Self::field(
                                "Branch name",
                                "worktree-dialog-branch",
                                &self.branch,
                                disabled,
                            ))
                            .when(exists, |d| {
                                d.child(Self::checkbox(
                                    "worktree-dialog-use-existing",
                                    format!("Use the existing branch {branch}"),
                                    on,
                                    cx,
                                    |this| this.use_existing = !this.use_existing,
                                ))
                            })
                        }),
                )
                .children(self.setup_row(cx))
                .when(self.loaded.is_none() && !self.remote, |d| {
                    d.child(
                        div()
                            .text_size(px(12.))
                            .text_color(content(0.45))
                            .child("Reading the repository…"),
                    )
                })
                .when_some(problem, |d, problem| {
                    d.child(
                        div()
                            .id("worktree-dialog-error")
                            .debug_selector(|| "worktree-dialog-error".into())
                            .role(Role::Alert)
                            .text_size(px(12.))
                            .text_color(rgb(0xf87171))
                            .child(problem),
                    )
                })
                .child(
                    crate::modal::actions()
                        .mt(px(4.))
                        .child(
                            crate::modal::ghost_button("worktree-dialog-cancel", "Cancel")
                                .debug_selector(|| "worktree-dialog-cancel".into())
                                .cursor_pointer()
                                .on_click(cx.listener(|_, _, _, cx| cx.emit(DialogEvent::Cancel))),
                        )
                        .child(
                            crate::modal::disabled(
                                crate::modal::primary_button("worktree-dialog-create", "Create"),
                                !valid,
                            )
                            .debug_selector(|| "worktree-dialog-create".into())
                            .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
                        ),
                ),
        );
        let entity = cx.weak_entity();
        div()
            .id("worktree-dialog")
            .debug_selector(|| "worktree-dialog".into())
            .role(Role::Dialog)
            .aria_label("New worktree")
            .absolute()
            .inset_0()
            .track_focus(&self.focus)
            .key_context("WorktreeDialog")
            .on_key_down(cx.listener(|_, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    cx.emit(DialogEvent::Cancel);
                }
            }))
            .on_action(cx.listener(|_, _: &crate::popover::Cancel, _, cx| {
                cx.stop_propagation();
                cx.emit(DialogEvent::Cancel);
            }))
            // The Shell binds Escape to DismissMenu, which would close Settings behind us.
            .on_action(cx.listener(|_, _: &crate::shell::DismissMenu, _, cx| {
                cx.stop_propagation();
                cx.emit(DialogEvent::Cancel);
            }))
            .child(crate::modal::overlay(
                "worktree-dialog",
                card,
                move |_, cx| {
                    let _ = entity.update(cx, |_, cx| cx.emit(DialogEvent::Cancel));
                },
            ))
    }
}
