//! The app's side of the shared code services (docs/native/code.md §7).
//! `monocode-core` runs the language servers, runs and debug sessions and
//! answers the agents' tools on its own threads; what it cannot know is
//! which folders are projects and what each has set up. This keeps a copy
//! of that part of the workspace where any thread can read it, registers
//! the resolver that answers from it, and ends the services at quit.
use crate::shell::Shell;
use crate::workspace::Workspace;
use gpui::{App, BackgroundExecutor, Entity, Task};
use monocode_core::code::run::ProjectRuns;
use monocode_core::code::services::{self, CodeContext};
use monocode_core::files::path_key;
use monocode_core::terminal_setup::TerminalSetup;
use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, PoisonError, RwLock};
use std::time::Duration;

/// How long quitting waits for the services to stop. A server that will not
/// die must not keep the app open.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(2);
static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);

/// Stops the language servers, the debug sessions and the headless runs off
/// the UI thread, and finishes once they have stopped or `SHUTDOWN_WAIT` is
/// over. Quitting waits for it before the app exits (the framework's own
/// quit hooks only get a fraction of a second), and the hook below asks again
/// for the quits that do not come through there; the second ask is free.
pub(crate) fn shutdown(cx: &App) -> Task<()> {
    if SHUTTING_DOWN.swap(true, Ordering::SeqCst) {
        return Task::ready(());
    }
    let executor = cx.background_executor();
    let stopping = executor.spawn(async { services::shutdown() });
    bounded(executor, stopping, SHUTDOWN_WAIT)
}

/// Finishes when `work` does or after `limit`, whichever is first. The work
/// is left to finish by itself when the limit comes first.
fn bounded(
    executor: &BackgroundExecutor,
    work: impl Future<Output = ()> + Send + 'static,
    limit: Duration,
) -> Task<()> {
    let (done, finished) = async_channel::bounded::<()>(2);
    let (by_work, by_timer) = (done.clone(), done);
    executor
        .spawn(async move {
            work.await;
            let _ = by_work.send(()).await;
        })
        .detach();
    let timer = executor.timer(limit);
    executor
        .spawn(async move {
            timer.await;
            let _ = by_timer.send(()).await;
        })
        .detach();
    executor.spawn(async move {
        let _ = finished.recv().await;
    })
}

/// What the resolver needs of the workspace.
#[derive(Clone, Default, PartialEq)]
struct Known {
    projects: Vec<String>,
    setups: HashMap<String, TerminalSetup>,
    runs: HashMap<String, ProjectRuns>,
}

impl Known {
    fn of(workspace: &Workspace) -> Self {
        Self {
            projects: workspace.projects.clone(),
            setups: workspace.project_terminal_setups.clone(),
            runs: workspace.project_runs.clone(),
        }
    }

    /// Whether `workspace` still says what this copy says, without copying it.
    fn matches(&self, workspace: &Workspace) -> bool {
        self.projects == workspace.projects
            && self.setups == workspace.project_terminal_setups
            && self.runs == workspace.project_runs
    }

    /// A project's entry in a map keyed by project path, however the path
    /// is written.
    fn entry<T: Clone + Default>(map: &HashMap<String, T>, project: &str) -> T {
        let key = path_key(project);
        map.iter()
            .find(|(path, _)| path_key(path) == key)
            .map(|(_, value)| value.clone())
            .unwrap_or_default()
    }

    /// Where `path` belongs: the project folder that holds it, or the
    /// project whose linked worktree it is in. `None` for a path of no
    /// project, which the services then place by the disk alone.
    fn context(&self, path: &Path) -> Option<CodeContext> {
        let (checkout, repository) = services::locate(path);
        let local = |project: &&String| monocode_core::remote::parse_remote_path(project).is_none();
        let holding = (self.projects.iter().filter(local))
            .filter(|project| path.starts_with(Path::new(project.as_str())))
            .max_by_key(|project| project.len());
        let project = holding.or_else(|| {
            let repository = path_key(&repository.to_string_lossy());
            (self.projects.iter().filter(local)).find(|project| path_key(project) == repository)
        })?;
        let folder = PathBuf::from(project);
        // A project that is a folder inside a larger repository is its own
        // checkout: tools and roots stay inside what the user opened.
        let checkout = match holding.is_some() && !checkout.starts_with(&folder) {
            true => folder.clone(),
            false => checkout,
        };
        Some(CodeContext {
            checkout,
            project: folder,
            setup: Self::entry(&self.setups, project),
            runs: Self::entry(&self.runs, project),
        })
    }
}

/// Starts answering for the app: once, when the shell exists. The copy of
/// the workspace follows the shell from then on.
pub(crate) fn install(shell: &Entity<Shell>, cx: &mut App) {
    let known = Arc::new(RwLock::new(Known::default()));
    let follow = known.clone();
    let refresh = move |shell: &Shell| {
        let workspace = &shell.workspace;
        monocode_core::code::bridge::set_enabled(workspace.preferences.code_agents);
        let current = follow
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .matches(workspace);
        if !current {
            *follow.write().unwrap_or_else(PoisonError::into_inner) = Known::of(workspace);
        }
    };
    refresh(shell.read(cx));
    // The shell notifies on every change of the workspace; most of them
    // change nothing here and cost a comparison.
    cx.observe(shell, move |shell, cx| refresh(shell.read(cx)))
        .detach();
    services::services().set_resolver(move |path| {
        known
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .context(path)
    });
    cx.on_app_quit(|cx| shutdown(cx)).detach();
}

#[cfg(test)]
mod tests {
    use super::*;
    use monocode_core::code::run::RunConfig;
    use monocode_core::terminal_setup::Venv;

    #[gpui::test]
    fn quitting_waits_for_the_work_but_only_so_long(cx: &mut gpui::TestAppContext) {
        let executor = cx.executor();
        let ended = |waited: Task<()>, cx: &mut gpui::TestAppContext| {
            let flag = std::rc::Rc::new(std::cell::Cell::new(false));
            let seen = flag.clone();
            cx.update(|cx| {
                cx.spawn(async move |_| {
                    waited.await;
                    seen.set(true);
                })
                .detach()
            });
            cx.run_until_parked();
            flag
        };
        // Work that is done at once ends the wait before the limit.
        let quick = ended(bounded(&executor, async {}, Duration::from_secs(60)), cx);
        assert!(quick.get());
        // Work that never ends is left behind when the limit passes.
        let stuck = ended(
            bounded(&executor, std::future::pending(), Duration::from_secs(2)),
            cx,
        );
        assert!(!stuck.get());
        executor.advance_clock(Duration::from_secs(3));
        cx.run_until_parked();
        assert!(stuck.get());
    }

    #[test]
    fn a_path_is_placed_in_its_project_with_the_projects_setup_and_runs() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let project = root.join("repo/services/api");
        let worktree = root.join("trees/feature");
        std::fs::create_dir_all(root.join("repo/.git/worktrees/feature")).unwrap();
        std::fs::create_dir_all(project.join("src")).unwrap();
        std::fs::create_dir_all(project.join("vendored/.git")).unwrap();
        std::fs::create_dir_all(worktree.join("src")).unwrap();
        std::fs::write(
            worktree.join(".git"),
            format!(
                "gitdir: {}\n",
                root.join("repo/.git/worktrees/feature").display()
            ),
        )
        .unwrap();
        let api = project.to_string_lossy().into_owned();
        let repo = root.join("repo").to_string_lossy().into_owned();
        let mut runs = ProjectRuns::default();
        runs.add(RunConfig {
            name: "Serve".into(),
            command: "python -m api".into(),
            ..RunConfig::default()
        });
        let known = Known {
            projects: vec![repo.clone(), api.clone(), "remote://box/home/me/app".into()],
            // Saved with a trailing slash: still the same project.
            setups: HashMap::from([(
                format!("{api}/"),
                TerminalSetup {
                    venv: Venv::Auto,
                    ..TerminalSetup::default()
                },
            )]),
            runs: HashMap::from([(repo.clone(), runs.clone())]),
        };

        // The innermost project wins, and is its own checkout although the
        // repository's `.git` is further up.
        let inner = known.context(&project.join("src/app.py")).unwrap();
        assert_eq!(inner.project, project);
        assert_eq!(inner.checkout, project);
        assert_eq!(inner.setup.venv, Venv::Auto);
        assert!(inner.runs.is_empty());
        // A checkout inside the project is the checkout.
        let nested = known.context(&project.join("vendored/x.py")).unwrap();
        assert_eq!(nested.project, project);
        assert_eq!(nested.checkout, project.join("vendored"));
        // A linked worktree elsewhere belongs to the repository's project.
        let linked = known.context(&worktree.join("src/app.py")).unwrap();
        assert_eq!(linked.project, root.join("repo"));
        assert_eq!(linked.checkout, worktree);
        assert_eq!(linked.runs, runs);
        assert_eq!(linked.setup, TerminalSetup::default());
        // A folder of no project is left to the services' own answer.
        assert!(known.context(&root.join("elsewhere/x.py")).is_none());
    }
}
