use crate::{
    input::{self, Input},
    settings::{
        SettingsEvent, SettingsView,
        widgets::{note, secondary_button},
    },
    theme::*,
};
use gpui::{AnyElement, Context, Entity, SharedString, div, prelude::*, px};
use monocode_core::worktree::{self, naming::BranchPrefix};

const ROOT: &str = "worktree-root";
const PREFIX: &str = "worktree-prefix";
const BASE: &str = "worktree-default-base";

impl SettingsView {
    fn worktree_field(&mut self, key: &'static str, cx: &mut Context<Self>) -> Entity<Input> {
        if let Some(input) = self.fields.get(key) {
            return input.clone();
        }
        let (label, placeholder) = match key {
            ROOT => (
                "Worktree location",
                worktree::layout::default_root().display().to_string(),
            ),
            PREFIX => ("Custom branch prefix", "me".to_owned()),
            _ => ("Default base", "Detected (origin/main, main, …)".to_owned()),
        };
        let input = cx.new(|cx| {
            let mut input = Input::field(label, placeholder, content(0.35), cx);
            input.set_font_size(12.);
            input
        });
        self._subscriptions
            .push(cx.subscribe(&input, |_, _, _: &input::Changed, cx| cx.notify()));
        self._subscriptions.push(
            cx.subscribe(&input, move |this, _, _: &input::Submitted, cx| {
                this.save_worktree_field(key, cx)
            }),
        );
        self.fields.insert(key, input.clone());
        input
    }

    /// Discovery landed: show what is saved.
    pub(in crate::settings) fn sync_worktree_fields(&mut self, cx: &mut Context<Self>) {
        let trees = &self.discovery.worktrees;
        let Some(prefs) = trees.prefs.clone() else {
            return;
        };
        let base = trees
            .repo
            .as_ref()
            .and_then(|r| r.default_base.clone())
            .unwrap_or_default();
        let root = prefs
            .root
            .as_ref()
            .map(|r| r.display().to_string())
            .unwrap_or_default();
        let prefix = match &prefs.branch_prefix {
            BranchPrefix::Custom(p) => p.clone(),
            _ => String::new(),
        };
        for (key, text) in [(ROOT, root), (PREFIX, prefix), (BASE, base)] {
            let input = self.worktree_field(key, cx);
            input.update(cx, |input, cx| input.set_text(text, cx));
        }
    }

    fn worktree_store(&self) -> Option<crate::runtime::Database> {
        self.ctx
            .store
            .clone()
            .map(|s| s.0)
            .filter(|_| self.discovery.worktrees.prefs.is_some())
    }

    pub(in crate::settings) fn save_worktree_prefs(
        &mut self,
        change: impl FnOnce(&mut worktree::Prefs),
        cx: &mut Context<Self>,
    ) {
        let Some(store) = self.worktree_store() else {
            return;
        };
        let Some(prefs) = self.discovery.worktrees.prefs.as_mut() else {
            return;
        };
        change(prefs);
        let result =
            crate::runtime::locked(&store).and_then(|db| worktree::meta::save_prefs(&db, prefs));
        let main = std::path::Path::new(&self.discovery.worktrees.main);
        self.discovery.worktrees.default_root =
            worktree::layout::repo_dir(&prefs.root_or_default(), main);
        self.discovery.worktrees.error = result.err();
        cx.notify();
    }

    pub(in crate::settings) fn save_repo_worktree_prefs(
        &mut self,
        change: impl FnOnce(&mut worktree::RepoPrefs),
        cx: &mut Context<Self>,
    ) {
        let Some(store) = self.worktree_store() else {
            return;
        };
        let repo = self.discovery.worktrees.repo.get_or_insert_default();
        change(repo);
        let main = std::path::Path::new(&self.discovery.worktrees.main);
        let result = crate::runtime::locked(&store)
            .and_then(|db| worktree::meta::save_repo_prefs(&db, main, repo));
        self.discovery.worktrees.error = result.err();
        cx.notify();
    }

    fn save_worktree_field(&mut self, key: &'static str, cx: &mut Context<Self>) {
        let text = self
            .fields
            .get(key)
            .map(|i| i.read(cx).content.trim().to_owned())
            .unwrap_or_default();
        match key {
            ROOT => {
                let root = std::path::PathBuf::from(&text);
                if !text.is_empty() && !root.is_absolute() {
                    self.discovery.worktrees.error =
                        Some("The worktree location must be an absolute path".into());
                    cx.notify();
                    return;
                }
                self.save_worktree_prefs(|p| p.root = (!text.is_empty()).then_some(root), cx);
            }
            PREFIX => self.save_worktree_prefs(
                |p| p.branch_prefix = BranchPrefix::from(format!("custom:{text}")),
                cx,
            ),
            _ => self.save_repo_worktree_prefs(
                |r| r.default_base = (!text.is_empty()).then_some(text),
                cx,
            ),
        }
    }

    fn choose_worktree_root(&mut self, cx: &mut Context<Self>) {
        let prompt = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose worktree location".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = prompt.await
                && let Some(path) = paths.into_iter().next()
            {
                let _ = this.update(cx, |this, cx| {
                    let text = path.display().to_string();
                    this.worktree_field(ROOT, cx)
                        .update(cx, |input, cx| input.set_text(text, cx));
                    this.save_worktree_prefs(|p| p.root = Some(path), cx);
                });
            }
        })
        .detach();
    }

    fn worktree_text_control(
        &mut self,
        key: &'static str,
        width: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let input = self.worktree_field(key, cx);
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(
                div()
                    .w(px(width))
                    .h(px(28.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .rounded_md()
                    .border_1()
                    .border_color(content(0.1))
                    .bg(content(0.04))
                    .child(input),
            )
            .child(
                secondary_button(
                    SharedString::from(format!("save-{key}")),
                    "Save",
                    false,
                    false,
                )
                .child("Save")
                .on_click(cx.listener(move |this, _, _, cx| this.save_worktree_field(key, cx))),
            )
            .into_any_element()
    }

    /// Location, branch prefix and this project's default base (spec §7.6).
    fn worktree_preferences(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let prefs = self.discovery.worktrees.prefs.clone()?;
        let repo = self.discovery.worktrees.repo.clone().unwrap_or_default();
        let root = prefs.root_or_default();
        let location = self.worktree_text_control(ROOT, 220., cx);
        let reveal = root.clone();
        let mut rows = vec![self.row(
            Some("worktree-location"),
            true,
            "Location",
            Some("New worktrees go in a folder per project under this one.".into()),
            vec![
                    location,
                    secondary_button(
                        "choose-worktree-root",
                        "Choose worktree location",
                        false,
                        false,
                    )
                    .child("Choose…")
                    .on_click(cx.listener(|this, _, _, cx| this.choose_worktree_root(cx)))
                    .into_any_element(),
                    secondary_button(
                        "reveal-worktree-root",
                        "Reveal worktree location",
                        false,
                        !root.is_dir(),
                    )
                    .child(icon("folder-open", 14.))
                    .on_click(move |_, _, cx| cx.reveal_path(&reveal))
                    .into_any_element(),
                ],
        )];
        let kind = match &prefs.branch_prefix {
            BranchPrefix::None => "none",
            BranchPrefix::GitUsername => "git-username",
            BranchPrefix::Custom(_) => "custom",
        };
        let mut prefix_controls = vec![self.segmented(
            "worktree-branch-prefix",
            "Branch prefix",
            kind,
            &[
                ("none", "None"),
                ("git-username", "Git user name"),
                ("custom", "Custom"),
            ],
            cx,
            |this, option, cx| match option {
                "none" => this.save_worktree_prefs(|p| p.branch_prefix = BranchPrefix::None, cx),
                "git-username" => {
                    this.save_worktree_prefs(|p| p.branch_prefix = BranchPrefix::GitUsername, cx)
                }
                _ => this.save_worktree_field(PREFIX, cx),
            },
        )];
        if kind == "custom"
            || self
                .fields
                .get(PREFIX)
                .is_some_and(|i| !i.read(cx).content.trim().is_empty())
        {
            prefix_controls.push(self.worktree_text_control(PREFIX, 120., cx));
        }
        rows.push(self.row(
            Some("worktree-branch-prefix"),
            false,
            "Branch prefix",
            Some("Put before new branch names, as in prefix/name.".into()),
            prefix_controls,
        ));
        let base = self.worktree_text_control(BASE, 180., cx);
        rows.push(self.row(
            Some("worktree-default-base"),
            false,
            "Default base",
            Some("What this project's new worktrees start from.".into()),
            vec![base],
        ));
        let toggle = self.toggle(
            "worktree-show-external",
            "Show other worktrees",
            repo.show_external,
            false,
            cx,
            |this, on, cx| this.save_repo_worktree_prefs(|r| r.show_external = on, cx),
        );
        rows.push(self.row(
            Some("worktree-show-external"),
            false,
            "Show other worktrees",
            Some("List worktrees made outside Monocode with this project's.".into()),
            vec![toggle],
        ));
        Some(self.group(
            Some("worktree-preferences"),
            false,
            "New worktrees",
            None,
            None,
            rows,
        ))
    }

    /// Reload the page after a worktree was created or changed elsewhere.
    pub(crate) fn refresh_worktrees(&mut self, cx: &mut Context<Self>) {
        self.refresh(cx);
    }

    /// A worktree was deleted: leave it if it was the selected project, and reload.
    pub(crate) fn worktree_removed(&mut self, path: &str, cx: &mut Context<Self>) {
        if !self.discovery.worktrees.main.is_empty()
            && monocode_core::files::is_equal_or_inside(&self.selected_project, path)
        {
            self.selected_project = self.discovery.worktrees.main.clone();
        }
        self.refresh(cx);
    }

    pub(super) fn worktrees_page(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let project = self.project_picker(cx);
        let ready = !self.discovery_loading
            && self.discovery.worktrees.error.is_none()
            && !self.discovery.worktrees.main.is_empty();
        let mut page=vec![div().relative().child(self.reveal_probe("project-worktrees")).flex().items_center().gap_1().child(project).child(secondary_button("create-settings-worktree","Create worktree",false,!ready).debug_selector(||"create-settings-worktree".into()).child(icon("plus",12.)).child("Create worktree").on_click(cx.listener(move|this,_,_,cx|{if ready{cx.emit(SettingsEvent::CreateWorktree(this.selected_project.clone()));}}))).into_any_element(),div().mt_4().flex().items_center().justify_between().gap_3().child(div().min_w_0().flex_1().text_size(px(12.)).line_height(px(18.)).text_color(content(0.5)).child("Sessions can share a worktree. Deleting one keeps its sessions by default. A managed worktree's merged branch is deleted with it; other branches are kept.")).child(self.refresh_button("refresh-settings-worktrees","Refresh",cx)).into_any_element()];
        if self.selected_project.is_empty() {
            page.push(note("Add a project to manage its worktrees."));
        } else if self.discovery_loading {
            page.push(note("Loading worktrees…"));
        } else if let Some(error) = &self.discovery.worktrees.error {
            page.push(
                div()
                    .id("settings-page-error")
                    .mt_4()
                    .role(gpui::Role::Alert)
                    .text_size(px(12.))
                    .text_color(gpui::rgb(0xf87171))
                    .child(error.clone())
                    .into_any_element(),
            );
        } else if self.discovery.worktrees.trees.is_empty() {
            page.push(
                div()
                    .mt_4()
                    .rounded_xl()
                    .border_1()
                    .border_color(content(0.1))
                    .px_4()
                    .py_8()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_2()
                    .child(icon("folder-tree", 20.))
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child("No additional worktrees"),
                    )
                    .child(
                        div().text_size(px(12.)).text_color(content(0.5)).child(
                            "Create a worktree to work on another branch in a separate folder.",
                        ),
                    )
                    .into_any_element(),
            );
        } else {
            let trees = self.discovery.worktrees.trees.clone();
            page.push(
                div()
                    .mt_4()
                    .rounded_xl()
                    .border_1()
                    .border_color(content(0.1))
                    .overflow_hidden()
                    .children(trees.into_iter().enumerate().map(|(ix, tree)| {
                        let t = &tree.tree;
                        let path = std::path::PathBuf::from(&t.path);
                        let count = self
                            .ctx
                            .sessions
                            .iter()
                            .filter(|(_, p, _)| {
                                monocode_core::files::is_equal_or_inside(p, &t.path)
                            })
                            .count();
                        let status = if t.missing {
                            "Missing folder"
                        } else if tree.dirty == Some(true) {
                            "Uncommitted changes"
                        } else if tree.dirty == Some(false) {
                            "Clean"
                        } else {
                            "Status unavailable"
                        };
                        let blocked = tree.locked || t.branch.is_none();
                        div()
                            .p_4()
                            .flex()
                            .items_start()
                            .gap_3()
                            .border_b_1()
                            .border_color(content(0.1))
                            .child(icon("folder-tree", 16.))
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .child(
                                        div()
                                            .text_size(px(13.))
                                            .font_weight(gpui::FontWeight::MEDIUM)
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .child(tree.display_name.clone().unwrap_or_else(|| {
                                                path.file_name()
                                                    .unwrap_or_default()
                                                    .to_string_lossy()
                                                    .into_owned()
                                            }))
                                            .when_some(tree.ownership_label(), |d, label| {
                                                d.child(
                                                    div()
                                                        .id(SharedString::from(format!(
                                                            "tree-ownership-{ix}"
                                                        )))
                                                        .debug_selector(move || {
                                                            format!(
                                                                "worktree-ownership-{ix}-{label}"
                                                            )
                                                        })
                                                        .px(px(6.))
                                                        .rounded_md()
                                                        .bg(content(0.08))
                                                        .text_size(px(10.))
                                                        .text_color(content(0.6))
                                                        .child(label),
                                                )
                                            }),
                                    )
                                    .child(
                                        div()
                                            .mt_1()
                                            .text_size(px(11.))
                                            .text_color(content(0.4))
                                            .child(t.path.clone()),
                                    )
                                    .child(
                                        div()
                                            .mt_2()
                                            .flex()
                                            .items_center()
                                            .gap_1()
                                            .text_size(px(11.))
                                            .text_color(content(0.55))
                                            .child(icon("git-branch", 12.))
                                            .child(
                                                t.branch
                                                    .as_ref()
                                                    .map(|b| format!("Current branch: {b}"))
                                                    .unwrap_or_else(|| {
                                                        format!(
                                                            "Detached at {}",
                                                            t.head
                                                                .chars()
                                                                .take(7)
                                                                .collect::<String>()
                                                        )
                                                    }),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .mt_2()
                                            .text_size(px(11.))
                                            .text_color(content(0.55))
                                            .child(format!(
                                                "{count} {} in this worktree · {status}{}{}",
                                                if count == 1 { "session" } else { "sessions" },
                                                if tree.unpushed > 0 {
                                                    format!(
                                                        " · {} unpublished commits",
                                                        tree.unpushed
                                                    )
                                                } else {
                                                    String::new()
                                                },
                                                if tree.locked { " · Locked" } else { "" }
                                            )),
                                    ),
                            )
                            .child(
                                secondary_button(
                                    SharedString::from(format!("reveal-tree-{ix}")),
                                    format!("Reveal {}", t.branch.as_deref().unwrap_or("worktree")),
                                    false,
                                    t.missing,
                                )
                                .child(icon("folder-open", 16.))
                                .on_click(move |_, _, cx| cx.reveal_path(&path)),
                            )
                            .child(
                                secondary_button(
                                    SharedString::from(format!("delete-tree-{ix}")),
                                    format!("Delete {}", t.branch.as_deref().unwrap_or("worktree")),
                                    false,
                                    blocked,
                                )
                                .debug_selector(move || format!("delete-tree-{ix}"))
                                .child(icon("trash", 16.))
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        if !blocked {
                                            cx.emit(SettingsEvent::DeleteWorktree {
                                                cwd: this.selected_project.clone(),
                                                path: tree.tree.path.clone(),
                                            });
                                        }
                                    },
                                )),
                            )
                    }))
                    .into_any_element(),
            );
        }
        if ready {
            page.push(note(format!(
                "New worktrees are created in {}.",
                self.discovery.worktrees.default_root.display()
            )));
            if let Some(group) = self.worktree_preferences(cx) {
                page.push(group);
            }
        }
        page
    }
}
