//! The Changes panel's "All open projects" view: every project open in the
//! rail with its uncommitted files, read-only. A file opens its diff; a
//! project's arrow switches to it for committing. Each checkout's status is
//! read off-thread, again while the view shows.
use crate::{
    diff_view::diff_counts,
    file_icons::file_type_icon,
    right_panel::{DIFF_ADD, DIFF_DEL, icon_action, spinner, stroke, tabular},
    theme::*,
};
use gpui::{
    AnyElement, Context, EventEmitter, Role, ScrollHandle, SharedString, Task, Window, div,
    prelude::*, px, rgb,
};
use monocode_core::scm::{self, ChangedFile, DiffIndex};
use std::{collections::HashSet, path::Path, time::Duration};

/// How often the view reads every project's status again while it shows.
const POLL: Duration = Duration::from_secs(10);

pub(crate) enum CrossChangesEvent {
    OpenDiff { cwd: String, path: String },
    OpenProject(String),
}

struct ProjectChanges {
    root: String,
    name: String,
    /// None until the first read finishes.
    index: Option<Result<DiffIndex, String>>,
}

pub(crate) struct CrossChanges {
    current: String,
    enabled: bool,
    projects: Vec<ProjectChanges>,
    loading: bool,
    collapsed: HashSet<String>,
    generation: u64,
    load: Option<Task<()>>,
    poll: Option<Task<()>>,
    scroll: ScrollHandle,
    _subscriptions: Vec<gpui::Subscription>,
}
impl EventEmitter<CrossChangesEvent> for CrossChanges {}

impl CrossChanges {
    pub(crate) fn new(current: String, cx: &mut Context<Self>) -> Self {
        let subscriptions =
            vec![
                cx.observe_global::<crate::open_projects::OpenProjects>(|this: &mut Self, cx| {
                    this.refresh(cx)
                }),
            ];
        Self {
            current,
            enabled: false,
            projects: Vec::new(),
            loading: false,
            collapsed: HashSet::new(),
            generation: 0,
            load: None,
            poll: None,
            scroll: ScrollHandle::new(),
            _subscriptions: subscriptions,
        }
    }

    pub(crate) fn set_current(&mut self, current: String, cx: &mut Context<Self>) {
        if self.current != current {
            self.current = current;
            self.refresh(cx);
        }
    }

    /// Reads only while shown; a hidden view keeps its last lists.
    pub(crate) fn set_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.enabled == enabled {
            return;
        }
        self.enabled = enabled;
        if enabled {
            self.refresh(cx);
            self.start_poll(cx);
        } else {
            self.poll = None;
            self.load = None;
            self.loading = false;
        }
    }

    fn start_poll(&mut self, cx: &mut Context<Self>) {
        self.poll = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL).await;
                let Ok(()) = this.update(cx, |this, cx| {
                    if this.load.is_none() {
                        this.refresh(cx);
                    }
                }) else {
                    return;
                };
            }
        }));
    }

    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        if !self.enabled {
            return;
        }
        let current = (!self.current.is_empty()).then_some(self.current.as_str());
        let roots = crate::open_projects::local_projects(cx, current);
        // Keep what was read for projects still open, so the list does not
        // blink back to "Loading…" on every pass.
        let mut previous = std::mem::take(&mut self.projects);
        self.projects = roots
            .iter()
            .map(|root| {
                let index = previous
                    .iter()
                    .position(|p| &p.root == root)
                    .and_then(|ix| previous.swap_remove(ix).index);
                ProjectChanges {
                    root: root.clone(),
                    name: crate::open_projects::project_name(root),
                    index,
                }
            })
            .collect();
        self.generation += 1;
        let generation = self.generation;
        self.loading = true;
        let work = cx.background_executor().spawn(async move {
            roots
                .into_iter()
                .map(|root| {
                    let index = if Path::new(&root).is_dir() {
                        scm::diff_index(Path::new(&root))
                    } else {
                        Err("Folder not found".to_owned())
                    };
                    (root, index)
                })
                .collect::<Vec<_>>()
        });
        self.load = Some(cx.spawn(async move |this, cx| {
            let results = work.await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                for (root, index) in results {
                    if let Some(project) = this.projects.iter_mut().find(|p| p.root == root) {
                        project.index = Some(index);
                    }
                }
                this.loading = false;
                this.load = None;
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub(crate) fn total(&self) -> usize {
        self.projects
            .iter()
            .filter_map(|p| p.index.as_ref()?.as_ref().ok())
            .map(|i| i.files.len())
            .sum()
    }

    fn toggle_fold(&mut self, root: &str, cx: &mut Context<Self>) {
        if !self.collapsed.remove(root) {
            self.collapsed.insert(root.to_owned());
        }
        cx.notify();
    }

    fn project_header(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let project = &self.projects[ix];
        let root = project.root.clone();
        let open = !self.collapsed.contains(&root);
        let current =
            monocode_core::files::path_key(&root) == monocode_core::files::path_key(&self.current);
        let (branch, count, detail) = match &project.index {
            None => (None, None, Some("Loading…".to_owned())),
            Some(Err(_)) => (None, None, Some("Not a Git repository".to_owned())),
            Some(Ok(index)) => (
                index.branch.clone(),
                Some(index.files.len()),
                index.files.is_empty().then(|| "No changes".to_owned()),
            ),
        };
        let fold_root = root.clone();
        let open_root = root.clone();
        div()
            .id(("cross-changes-project", ix))
            .debug_selector(move || format!("cross-changes-project-{ix}"))
            .role(Role::Button)
            .aria_label(SharedString::from(project.name.clone()))
            .aria_expanded(open)
            .group(SharedString::from(format!("cross-changes-project-{ix}")))
            .h(px(28.))
            .flex_shrink_0()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(6.))
            .px_2()
            .cursor_pointer()
            .text_size(px(12.))
            .hover(|s| s.bg(content(0.05)))
            .child(
                icon(
                    if open {
                        "chevron-down"
                    } else {
                        "chevron-right"
                    },
                    12.,
                )
                .text_color(content(0.45)),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(content(0.9))
                    .child(project.name.clone()),
            )
            .when(current, |d| {
                d.child(
                    div()
                        .flex_shrink_0()
                        .rounded(px(4.))
                        .bg(content(0.08))
                        .px(px(5.))
                        .text_size(px(10.))
                        .text_color(content(0.55))
                        .child("current"),
                )
            })
            .when_some(branch, |d, branch| {
                d.child(
                    div()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap(px(3.))
                        .text_size(px(11.))
                        .text_color(content(0.45))
                        .child(icon("git-branch-hi", 11.).text_color(content(0.4)))
                        .child(div().min_w_0().truncate().child(branch)),
                )
            })
            .child(div().flex_1())
            .when_some(detail, |d, detail| {
                d.child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(11.))
                        .text_color(content(0.4))
                        .child(detail),
                )
            })
            .when_some(count.filter(|n| *n > 0), |d, count| {
                d.child(
                    div()
                        .flex_shrink_0()
                        .rounded_full()
                        .bg(content(0.1))
                        .px(px(6.))
                        .text_size(px(10.))
                        .font_features(tabular())
                        .text_color(content(0.75))
                        .child(count.to_string()),
                )
            })
            .when(!current, |d| {
                d.child(
                    icon_action(
                        ("cross-changes-open-project", ix),
                        "Switch to this project",
                        "external-link",
                        true,
                    )
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.stop_propagation();
                        cx.emit(CrossChangesEvent::OpenProject(open_root.clone()));
                    })),
                )
            })
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_fold(&fold_root, cx)))
            .into_any_element()
    }

    fn file_row(
        &self,
        project: usize,
        ix: usize,
        file: &ChangedFile,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let root = self.projects[project].root.clone();
        let name = file
            .relative
            .rsplit('/')
            .next()
            .unwrap_or(&file.relative)
            .to_owned();
        let dir = file
            .relative
            .rsplit_once('/')
            .map(|(dir, _)| dir.to_owned())
            .unwrap_or_default();
        let tint = match file.status.as_str() {
            "deleted" => Some(rgb(DIFF_DEL)),
            "added" | "untracked" => Some(rgb(DIFF_ADD)),
            _ => None,
        };
        let deleted = file.status == "deleted";
        let path = file.path.clone();
        div()
            .id(SharedString::from(format!(
                "cross-changes-file-{project}-{ix}"
            )))
            .debug_selector(move || format!("cross-changes-file-{project}-{ix}"))
            .role(Role::Button)
            .aria_label(SharedString::from(file.relative.clone()))
            .h(px(24.))
            .flex_shrink_0()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(6.))
            .pl(px(26.))
            .pr_2()
            .text_size(px(12.))
            .text_color(content(0.8))
            .when(!deleted, |d| d.cursor_pointer())
            .hover(|s| s.bg(content(0.06)))
            .child(file_type_icon(&name, false, false, 14.))
            .child(
                div()
                    .flex_shrink_0()
                    .max_w(px(180.))
                    .truncate()
                    .when_some(tint, |d, tint| d.text_color(tint))
                    .child(name),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .text_size(px(10.))
                    .text_color(content(0.4))
                    .child(dir),
            )
            .when(file.status != "untracked", |d| {
                d.child(diff_counts(
                    file.additions.max(0) as u32,
                    file.deletions.max(0) as u32,
                ))
            })
            .when(!deleted, |d| {
                d.on_click(cx.listener(move |_, _, _, cx| {
                    cx.emit(CrossChangesEvent::OpenDiff {
                        cwd: root.clone(),
                        path: path.clone(),
                    })
                }))
            })
            .into_any_element()
    }
}

impl Render for CrossChanges {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut rows: Vec<AnyElement> = Vec::new();
        for ix in 0..self.projects.len() {
            rows.push(self.project_header(ix, cx));
            if self.collapsed.contains(&self.projects[ix].root) {
                continue;
            }
            let files = match &self.projects[ix].index {
                Some(Ok(index)) => index.files.clone(),
                _ => Vec::new(),
            };
            for (file_ix, file) in files.iter().enumerate() {
                rows.push(self.file_row(ix, file_ix, file, cx));
            }
        }
        let total = self.total();
        let errors = self
            .projects
            .iter()
            .filter(|p| matches!(p.index, Some(Err(_))))
            .count();
        let summary = format!(
            "{total} changed file{} in {} project{}",
            if total == 1 { "" } else { "s" },
            self.projects.len() - errors,
            if self.projects.len() - errors == 1 {
                ""
            } else {
                "s"
            }
        );
        div()
            .id("cross-changes")
            .debug_selector(|| "cross-changes".into())
            .role(Role::List)
            .aria_label("Changes in all open projects")
            .flex_1()
            .min_h_0()
            .min_w_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(28.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .border_b_1()
                    .border_color(stroke())
                    .text_size(px(11.))
                    .text_color(content(0.45))
                    .when(self.loading, |d| {
                        d.child(spinner("cross-changes-spinner", "loader", 12.))
                    })
                    .child(div().min_w_0().flex_1().truncate().child(summary))
                    .child(
                        icon_action("cross-changes-refresh", "Refresh", "refresh-cw", true)
                            .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                    ),
            )
            .when(self.projects.is_empty(), |d| {
                d.child(
                    div()
                        .px_3()
                        .py_2()
                        .text_size(px(12.))
                        .text_color(content(0.45))
                        .child("No open projects"),
                )
            })
            .child(
                div()
                    .id("cross-changes-list")
                    .min_h_0()
                    .flex_1()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .flex()
                    .flex_col()
                    .py_1()
                    .children(rows),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    fn git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap()
            .status;
        assert!(status.success(), "git {args:?}");
    }

    fn repo(name: &str, changed: &[&str]) -> (tempfile::TempDir, String) {
        let dir = tempfile::Builder::new().prefix(name).tempdir().unwrap();
        git(dir.path(), &["init", "-q"]);
        std::fs::write(dir.path().join("base.txt"), "one\n").unwrap();
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-qm", "base"]);
        for file in changed {
            std::fs::write(dir.path().join(file), "two\n").unwrap();
        }
        let root = dir
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        (dir, root)
    }

    #[gpui::test]
    fn every_open_project_lists_its_changes(cx: &mut TestAppContext) {
        let (_a, a) = repo("cross-a", &["base.txt", "new.txt"]);
        let (_b, b) = repo("cross-b", &[]);
        let (_c, c) = repo("cross-c", &["base.txt"]);
        cx.update(|cx| crate::open_projects::publish(&[a.clone(), b.clone(), c.clone()], cx));
        let view = cx.new(|cx| CrossChanges::new(b.clone(), cx));
        cx.update(|cx| view.update(cx, |view, cx| view.set_enabled(true, cx)));
        cx.run_until_parked();
        // Reading Git happens on real threads; wait for it to land.
        for _ in 0..200 {
            if cx.update(|cx| !view.read(cx).loading) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
            cx.run_until_parked();
        }
        cx.update(|cx| {
            let view = view.read(cx);
            let roots: Vec<_> = view.projects.iter().map(|p| p.root.clone()).collect();
            assert_eq!(
                roots,
                [b.clone(), a.clone(), c.clone()],
                "the current project leads"
            );
            let counts: Vec<_> = view
                .projects
                .iter()
                .map(|p| p.index.as_ref().unwrap().as_ref().unwrap().files.len())
                .collect();
            assert_eq!(counts, [0, 2, 1]);
            assert_eq!(view.total(), 3);
        });
    }
}
