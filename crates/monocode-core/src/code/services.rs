//! The one home of the shared code services: the language server manager,
//! the debug manager with its breakpoints, and the registry of runs. The
//! panes and the agents' tools both go through `services()`, so they see the
//! same servers, sessions and runs.
//!
//! It also answers where a path belongs. What a project is, which terminal
//! setup it has and which run configurations are the app's knowledge, so
//! the app registers a resolver; until it does, a path belongs to the git
//! checkout around it and has no setup.
use super::dap::{self, BreakpointStore};
use super::lsp::{self, DocumentContext, ManagerOptions};
use super::run::{self, ProjectRuns, RunContext};
use super::toolchain::Toolchain;
use crate::terminal_setup::TerminalSetup;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, PoisonError, RwLock};

/// Where a file or folder belongs and what its project has set up.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CodeContext {
    /// The worktree the path is in: tools run here and roots are found here.
    pub checkout: PathBuf,
    /// The project the checkout belongs to; the checkout itself for the
    /// project's own folder.
    pub project: PathBuf,
    /// The project's terminal setup, which names its virtual environment.
    pub setup: TerminalSetup,
    /// The project's run configurations, last run and test runner.
    pub runs: ProjectRuns,
}

impl CodeContext {
    /// The checkout's interpreter and tools. Looks at the disk.
    pub fn toolchain(&self) -> Toolchain {
        Toolchain::for_folder(&self.setup, &self.checkout, &self.project)
    }

    /// What `lsp::Manager::open` and `with_file` take. Looks at the disk.
    pub fn document(&self) -> DocumentContext {
        DocumentContext {
            checkout: self.checkout.clone(),
            project: self.project.clone(),
            toolchain: self.toolchain(),
        }
    }

    /// A run context with no focused file.
    pub fn run_context(&self) -> RunContext {
        RunContext::new(&self.checkout, &self.project)
    }
}

/// Answers `Services::context`. Called on any thread, often the interface's:
/// it must not wait for anything.
pub type Resolver = Arc<dyn Fn(&Path) -> Option<CodeContext> + Send + Sync>;

pub struct Services {
    lsp: lsp::Manager,
    dap: dap::Manager,
    runs: run::Registry,
    resolver: RwLock<Option<Resolver>>,
}

impl Services {
    /// Services of their own, for tests; the app uses `services()`.
    pub fn new(runs: run::Registry, options: ManagerOptions) -> Self {
        Self {
            lsp: lsp::Manager::new(options),
            dap: dap::Manager::new(BreakpointStore::new()),
            runs,
            resolver: RwLock::new(None),
        }
    }

    pub fn lsp(&self) -> &lsp::Manager {
        &self.lsp
    }

    pub fn dap(&self) -> &dap::Manager {
        &self.dap
    }

    /// The breakpoints every debug session uses; the same as `dap().breakpoints()`.
    pub fn breakpoints(&self) -> &BreakpointStore {
        self.dap.breakpoints()
    }

    pub fn runs(&self) -> &run::Registry {
        &self.runs
    }

    /// Replaces the resolver. The app calls this once at startup.
    pub fn set_resolver(
        &self,
        resolver: impl Fn(&Path) -> Option<CodeContext> + Send + Sync + 'static,
    ) {
        *self
            .resolver
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(resolver));
    }

    /// Where `path` (absolute; a file or a folder) belongs. Never fails:
    /// what the resolver does not know falls back to `locate` and a default
    /// setup. Cheap enough for the interface thread.
    pub fn context(&self, path: &Path) -> CodeContext {
        let resolver = self
            .resolver
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        resolver
            .and_then(|resolver| resolver(path))
            .unwrap_or_else(|| {
                let (checkout, project) = locate(path);
                CodeContext {
                    checkout,
                    project,
                    ..CodeContext::default()
                }
            })
    }

    /// Stops the language servers, the debug sessions and the headless
    /// runs. Blocks briefly; for when the app quits.
    pub fn shutdown(&self) {
        self.dap.stop_all();
        self.lsp.stop_all();
        self.runs.shutdown();
    }
}

static SERVICES: OnceLock<Services> = OnceLock::new();

/// The app's services, made on first use. Any thread.
pub fn services() -> &'static Services {
    SERVICES.get_or_init(|| Services::new(run::registry().clone(), ManagerOptions::default()))
}

/// `Services::shutdown` for the app's services, if anything ever used them.
pub fn shutdown() {
    match SERVICES.get() {
        Some(services) => services.shutdown(),
        None => run::registry().shutdown(),
    }
}

/// The checkout and the project of `path` as the disk alone tells them:
/// the checkout is the nearest folder at or above the path that holds
/// `.git` (else the path's own folder), and the project is that checkout,
/// or for a linked git worktree the repository it was added to.
pub fn locate(path: &Path) -> (PathBuf, PathBuf) {
    let folder = match path.is_dir() {
        true => path,
        false => path.parent().unwrap_or(path),
    };
    let Some(checkout) = folder
        .ancestors()
        .find(|folder| folder.join(".git").exists())
    else {
        return (folder.to_path_buf(), folder.to_path_buf());
    };
    let project = main_repository(checkout).unwrap_or_else(|| checkout.to_path_buf());
    (checkout.to_path_buf(), project)
}

/// A linked worktree's `.git` is a file naming a folder under the main
/// repository's `.git/worktrees`.
fn main_repository(checkout: &Path) -> Option<PathBuf> {
    let pointer = std::fs::read_to_string(checkout.join(".git")).ok()?;
    let gitdir = Path::new(pointer.lines().next()?.strip_prefix("gitdir:")?.trim());
    let gitdir = match gitdir.is_absolute() {
        true => gitdir.to_path_buf(),
        false => checkout.join(gitdir),
    };
    let worktrees = gitdir.parent()?;
    let git = worktrees.parent()?;
    (worktrees.file_name()? == "worktrees" && git.file_name()? == ".git")
        .then(|| git.parent().map(Path::to_path_buf))
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal_setup::Venv;

    #[test]
    fn a_path_belongs_to_the_checkout_around_it_and_a_worktree_to_its_repository() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let repository = root.join("repo");
        let worktree = root.join("trees/feature");
        std::fs::create_dir_all(repository.join(".git/worktrees/feature")).unwrap();
        std::fs::create_dir_all(repository.join("src")).unwrap();
        std::fs::create_dir_all(worktree.join("src")).unwrap();
        std::fs::write(repository.join("src/a.py"), "").unwrap();
        std::fs::write(
            worktree.join(".git"),
            format!(
                "gitdir: {}\n",
                repository.join(".git/worktrees/feature").display()
            ),
        )
        .unwrap();
        assert_eq!(
            locate(&repository.join("src/a.py")),
            (repository.clone(), repository.clone())
        );
        // A file that does not exist yet still belongs to its folder's checkout.
        assert_eq!(
            locate(&worktree.join("src/new.py")),
            (worktree.clone(), repository.clone())
        );
        assert_eq!(locate(&worktree), (worktree.clone(), repository.clone()));
        // Outside any checkout the folder stands for itself.
        let loose = root.join("loose");
        std::fs::create_dir_all(&loose).unwrap();
        assert_eq!(locate(&loose.join("x.py")), (loose.clone(), loose));
    }

    #[test]
    fn the_resolver_answers_first_and_the_disk_when_it_does_not_know() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("known/.git")).unwrap();
        std::fs::create_dir_all(root.join("other/.git")).unwrap();
        let services = Services::new(run::Registry::new(), ManagerOptions::default());
        assert_eq!(
            services.context(&root.join("known")).checkout,
            root.join("known")
        );
        let known = root.join("known");
        services.set_resolver(move |path| {
            path.starts_with(&known).then(|| CodeContext {
                checkout: known.clone(),
                project: known.clone(),
                setup: TerminalSetup {
                    venv: Venv::Auto,
                    ..TerminalSetup::default()
                },
                runs: ProjectRuns::default(),
            })
        });
        assert_eq!(
            services.context(&root.join("known/a.py")).setup.venv,
            Venv::Auto
        );
        let other = services.context(&root.join("other/a.py"));
        assert_eq!(other.checkout, root.join("other"));
        assert_eq!(other.setup, TerminalSetup::default());
        assert_eq!(other.run_context().checkout, root.join("other"));
        assert!(std::ptr::eq(
            services.breakpoints(),
            services.dap().breakpoints()
        ));
    }
}
