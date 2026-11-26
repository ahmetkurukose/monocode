//! The Changes panel's "All open projects" view: every project open in the
//! rail as a fold with its count of changed files. The open fold holds that
//! project's own full Changes panel (commit box, file tree with staging and
//! rollback, branch, update, push and pull requests), so a project is worked
//! on without leaving the list. Only the open fold watches its checkout live;
//! the counts of the others are read off-thread while the view shows.
use crate::{
    changes::{Changes, ChangesEvent},
    git_status::{GitChanged, ProjectGit},
    right_panel::{icon_action, spinner, stroke, tabular},
    theme::*,
};
use gpui::{
    AnyElement, Context, Entity, EventEmitter, Role, ScrollHandle, SharedString, Subscription,
    Task, Window, div, prelude::*, px,
};
use monocode_core::{
    provider::Kind,
    scm::{self, DiffIndex},
};
use std::{collections::HashMap, path::Path, time::Duration};

/// How often the view reads every project's status again while it shows.
const POLL: Duration = Duration::from_secs(10);

/// What a project's own Changes panel asks of the shell, passed up.
pub(crate) enum CrossChangesEvent {
    OpenDiff {
        cwd: String,
        path: Option<String>,
        staged: Option<bool>,
    },
    ShowPullRequest(crate::inbox::RevealPr),
    Overlays,
    OpenProject(String),
    AllProjects(bool),
}

/// A project's own Git status and Changes panel, kept while its fold has been opened.
struct Surface {
    git: Entity<ProjectGit>,
    changes: Entity<Changes>,
    live: bool,
    _subscriptions: Vec<Subscription>,
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
    /// The project whose Changes panel shows; None folds them all.
    expanded: Option<String>,
    surfaces: HashMap<String, Surface>,
    /// Why the current project's branch actions are off (an agent works in it).
    blocked: Option<String>,
    provider: Option<Kind>,
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
            expanded: (!current.is_empty()).then(|| current.clone()),
            current,
            enabled: false,
            projects: Vec::new(),
            loading: false,
            surfaces: HashMap::new(),
            blocked: None,
            provider: None,
            generation: 0,
            load: None,
            poll: None,
            scroll: ScrollHandle::new(),
            _subscriptions: subscriptions,
        }
    }

    pub(crate) fn set_current(&mut self, current: String, cx: &mut Context<Self>) {
        if self.current != current {
            if self.expanded.is_none() || self.expanded.as_ref() == Some(&self.current) {
                self.expanded = (!current.is_empty()).then(|| current.clone());
            }
            self.current = current;
            self.sync_surfaces(cx);
            self.refresh(cx);
        }
    }

    pub(crate) fn set_blocked(&mut self, reason: Option<String>, cx: &mut Context<Self>) {
        self.blocked = reason;
        if let Some(surface) = self.surfaces.get(&self.current) {
            let reason = self.blocked.clone();
            surface
                .changes
                .update(cx, |changes, cx| changes.set_blocked(reason, cx));
        }
    }

    pub(crate) fn set_text_provider(&mut self, provider: Kind, cx: &mut Context<Self>) {
        self.provider = Some(provider);
        for surface in self.surfaces.values() {
            surface
                .changes
                .update(cx, |changes, cx| changes.set_text_provider(provider, cx));
        }
    }

    fn is_current(&self, root: &str) -> bool {
        monocode_core::files::path_key(root) == monocode_core::files::path_key(&self.current)
    }

    /// The open fold's panel, made the first time it opens.
    fn ensure_surface(&mut self, root: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.surfaces.contains_key(root) {
            return;
        }
        let git = cx.new(|cx| ProjectGit::new(root.to_owned(), cx));
        let changes = cx.new(|cx| {
            let mut changes = Changes::new(git.clone(), window, cx);
            changes.in_all_projects = true;
            changes
        });
        if let Some(provider) = self.provider {
            changes.update(cx, |changes, cx| changes.set_text_provider(provider, cx));
        }
        if self.is_current(root) {
            let reason = self.blocked.clone();
            changes.update(cx, |changes, cx| changes.set_blocked(reason, cx));
        }
        let subscriptions = vec![
            cx.subscribe(&changes, |_, _, event: &ChangesEvent, cx| {
                cx.emit(match event {
                    ChangesEvent::OpenDiff { cwd, path, staged } => CrossChangesEvent::OpenDiff {
                        cwd: cwd.clone(),
                        path: path.clone(),
                        staged: *staged,
                    },
                    ChangesEvent::ShowPullRequest(pr) => {
                        CrossChangesEvent::ShowPullRequest(pr.clone())
                    }
                    ChangesEvent::Overlays => CrossChangesEvent::Overlays,
                    ChangesEvent::OpenProject(path) => CrossChangesEvent::OpenProject(path.clone()),
                    ChangesEvent::AllProjects(on) => CrossChangesEvent::AllProjects(*on),
                })
            }),
            // A commit, stage or branch switch there updates its count here.
            cx.subscribe(&git, |this, _, _: &GitChanged, cx| {
                if this.load.is_none() {
                    this.refresh(cx);
                }
            }),
        ];
        self.surfaces.insert(
            root.to_owned(),
            Surface {
                git,
                changes,
                live: false,
                _subscriptions: subscriptions,
            },
        );
        self.sync_surfaces(cx);
    }

    /// Only the open fold's panel reads Git, and only while the view shows.
    fn sync_surfaces(&mut self, cx: &mut Context<Self>) {
        for (root, surface) in &mut self.surfaces {
            let live = self.enabled && self.expanded.as_ref() == Some(root);
            if surface.live != live {
                surface.live = live;
                surface.git.update(cx, |git, cx| git.set_polling(live, cx));
                surface
                    .changes
                    .update(cx, |changes, cx| changes.set_enabled(live, cx));
            }
        }
    }

    /// Reads only while shown; a hidden view keeps its last lists.
    pub(crate) fn set_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.enabled == enabled {
            return;
        }
        self.enabled = enabled;
        self.sync_surfaces(cx);
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
        // Projects closed in the rail let go of their panels.
        let open: Vec<String> = self.projects.iter().map(|p| p.root.clone()).collect();
        self.surfaces.retain(|root, _| open.contains(root));
        if self
            .expanded
            .as_ref()
            .is_some_and(|root| !open.contains(root))
        {
            self.expanded = None;
        }
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
        self.expanded = if self.expanded.as_deref() == Some(root) {
            None
        } else {
            Some(root.to_owned())
        };
        self.sync_surfaces(cx);
        cx.notify();
    }

    fn project_header(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let project = &self.projects[ix];
        let root = project.root.clone();
        let open = self.expanded.as_ref() == Some(&root);
        let current = self.is_current(&root);
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
            .when_some(branch.filter(|_| !open), |d, branch| {
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
}

impl Render for CrossChanges {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(root) = self.expanded.clone() {
            self.ensure_surface(&root, window, cx);
        }
        let open = self
            .expanded
            .as_ref()
            .and_then(|root| self.projects.iter().position(|p| &p.root == root));
        let headers: Vec<AnyElement> = (0..self.projects.len())
            .map(|ix| self.project_header(ix, cx))
            .collect();
        let total = self.total();
        let errors = self
            .projects
            .iter()
            .filter(|p| matches!(p.index, Some(Err(_))))
            .count();
        let repos = self.projects.len() - errors;
        let summary = format!(
            "{total} changed file{} in {repos} project{}",
            if total == 1 { "" } else { "s" },
            if repos == 1 { "" } else { "s" }
        );
        let bar = div()
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
            // With no project open there is no commit box to leave from.
            .when(open.is_none(), |d| {
                d.child(
                    crate::open_projects::toggle("cross-changes-all-projects", true, 0).on_click(
                        cx.listener(|_, _, _, cx| cx.emit(CrossChangesEvent::AllProjects(false))),
                    ),
                )
            })
            .child(div().min_w_0().flex_1().truncate().child(summary))
            .child(
                icon_action("cross-changes-refresh", "Refresh", "refresh-cw", true)
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
            );
        let root = div()
            .id("cross-changes")
            .debug_selector(|| "cross-changes".into())
            .role(Role::List)
            .aria_label("Changes in all open projects")
            .flex_1()
            .min_h_0()
            .min_w_0()
            .flex()
            .flex_col()
            .child(bar)
            .when(self.projects.is_empty(), |d| {
                d.child(
                    div()
                        .px_3()
                        .py_2()
                        .text_size(px(12.))
                        .text_color(content(0.45))
                        .child("No open projects"),
                )
            });
        let surface = open.and_then(|ix| self.surfaces.get(&self.projects[ix].root));
        match (open, surface) {
            // The open project's panel takes the room between the folds above
            // and below it.
            (Some(ix), Some(surface)) => {
                let mut headers = headers.into_iter();
                let above: Vec<_> = headers.by_ref().take(ix + 1).collect();
                let below: Vec<_> = headers.collect();
                root.child(div().flex_shrink_0().flex().flex_col().children(above))
                    .child(
                        div()
                            .id("cross-changes-surface")
                            .debug_selector(|| "cross-changes-surface".into())
                            .flex_1()
                            .min_h_0()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .border_t_1()
                            .border_b_1()
                            .border_color(stroke())
                            .bg(content(0.015))
                            .child(surface.changes.clone()),
                    )
                    .child(
                        div()
                            .id("cross-changes-below")
                            .flex_shrink_0()
                            .max_h(px(200.))
                            .overflow_y_scroll()
                            .flex()
                            .flex_col()
                            .children(below),
                    )
            }
            _ => root.child(
                div()
                    .id("cross-changes-list")
                    .min_h_0()
                    .flex_1()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .flex()
                    .flex_col()
                    .py_1()
                    .children(headers),
            ),
        }
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

    #[gpui::test]
    fn the_open_project_has_its_own_working_changes_panel(cx: &mut TestAppContext) {
        let (_a, a) = repo("fold-a", &["base.txt"]);
        let (_b, b) = repo("fold-b", &["base.txt", "new.txt"]);
        cx.update(|cx| {
            crate::input::bind(cx);
            crate::open_projects::publish(&[a.clone(), b.clone()], cx);
        });
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|cx| {
                    let mut view = CrossChanges::new(a.clone(), cx);
                    view.set_enabled(true, cx);
                    view
                })
            })
            .unwrap()
        });
        let view = window.root(cx).unwrap();
        let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();
        // The current project opens first, with the Changes panel's actions.
        assert!(visual.debug_bounds("cross-changes-surface").is_some());
        assert!(visual.debug_bounds("changes-header").is_some());
        let live = |cx: &mut gpui::VisualTestContext| -> Vec<String> {
            cx.update(|_, cx| {
                let view = view.read(cx);
                let mut live: Vec<String> = view
                    .surfaces
                    .iter()
                    .filter(|(_, s)| s.live)
                    .map(|(root, _)| root.clone())
                    .collect();
                live.sort();
                live
            })
        };
        assert_eq!(live(visual), [a.clone()]);
        // Opening the other project folds this one and stops its reads.
        visual.update(|_, cx| view.update(cx, |v, cx| v.toggle_fold(&b.clone(), cx)));
        visual.run_until_parked();
        assert_eq!(live(visual), [b.clone()]);
        assert_eq!(visual.update(|_, cx| view.read(cx).surfaces.len()), 2);
        // Folding it shows only the list.
        visual.update(|_, cx| view.update(cx, |v, cx| v.toggle_fold(&b.clone(), cx)));
        visual.run_until_parked();
        assert!(visual.debug_bounds("cross-changes-surface").is_none());
        assert!(live(visual).is_empty());
    }
}
