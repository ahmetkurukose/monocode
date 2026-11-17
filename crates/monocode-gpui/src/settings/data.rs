//! Settings discovery runs on a worker; rendering never probes CLIs or Git.
use monocode_core::{
    git,
    provider::{self, Kind},
    worktree::{self, Ownership},
};
use std::path::{Path, PathBuf};
#[derive(Clone, Debug)]
pub(super) struct Skill {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    pub scope: String,
    pub provider: String,
}
#[derive(Clone, Debug)]
pub(super) struct McpServer {
    pub name: String,
    pub provider: String,
    pub scope: String,
    pub path: PathBuf,
    pub transport: String,
    pub status: String,
}
#[derive(Default)]
pub(super) struct Discovery {
    pub binaries: Vec<(Kind, Option<PathBuf>)>,
    pub skills: Vec<Skill>,
    pub mcp: Vec<McpServer>,
    pub mcp_error: Option<String>,
    pub worktrees: ResultTrees,
}
#[derive(Clone, Debug)]
pub(super) struct Tree {
    pub tree: git::Worktree,
    pub locked: bool,
    pub dirty: Option<bool>,
    pub unpushed: usize,
    /// `None` for remote projects, which have no ownership metadata.
    pub ownership: Option<Ownership>,
    /// The managed worktree's name, when it has one.
    pub display_name: Option<String>,
}
impl Tree {
    pub(super) fn ownership_label(&self) -> Option<&'static str> {
        self.ownership.and_then(ownership_label)
    }
}
pub(super) fn ownership_label(ownership: Ownership) -> Option<&'static str> {
    match ownership {
        Ownership::Main => None,
        Ownership::Managed => Some("Managed"),
        Ownership::Legacy => Some("Legacy"),
        Ownership::External => Some("External"),
    }
}
#[derive(Default)]
pub(super) struct ResultTrees {
    pub trees: Vec<Tree>,
    pub branches: Vec<git::Branch>,
    /// Where new worktrees of this project are created.
    pub default_root: PathBuf,
    pub main: String,
    pub error: Option<String>,
    /// Global worktree preferences and this project's, for local projects.
    pub prefs: Option<worktree::Prefs>,
    pub repo: Option<worktree::RepoPrefs>,
}
pub(super) fn discover(cwd: &str, store: Option<&crate::runtime::Database>) -> Discovery {
    let mut data = Discovery {
        binaries: Kind::ALL
            .into_iter()
            .map(|k| (k, provider::resolve(k)))
            .collect(),
        ..Default::default()
    };
    data.skills = monocode_core::skills::list_skills(cwd.into(), None)
        .unwrap_or_default()
        .into_iter()
        .map(|s| {
            let path = PathBuf::from(s.path);

            Skill {
                name: s.name,
                description: s.description,
                path,
                scope: s.scope,
                provider: s.source,
            }
        })
        .collect();
    match monocode_core::mcp::mcp_discover(cwd.into()) {
        Ok(servers) => {
            data.mcp = servers
                .into_iter()
                .map(|s| McpServer {
                    name: s.name,
                    provider: s.provider,
                    scope: s.scope,
                    path: s.config_path.into(),
                    transport: s.transport,
                    status: if s.enabled { "Configured" } else { "Disabled" }.into(),
                })
                .collect()
        }
        Err(e) => data.mcp_error = Some(e),
    }
    if !cwd.is_empty() {
        let local = store.filter(|_| !monocode_core::files::remote::is_remote(Path::new(cwd)));
        // Metadata of worktrees deleted outside the app is dropped first.
        if let Some(store) = local {
            let _ = monocode_core::worktree_callers::reconcile(Path::new(cwd), store);
        }
        let entries = local
            .and_then(|store| monocode_core::worktree_callers::list(Path::new(cwd), store).ok())
            .unwrap_or_default();
        match git::snapshot(Path::new(cwd)) {
            Ok(s) => {
                if let Some(main) = s.worktrees.first() {
                    data.worktrees.main = main.path.clone();
                    data.worktrees.default_root =
                        monocode_core::worktree_management::default_root(Path::new(&main.path));
                    if let Some(store) = local
                        && let Ok(db) = crate::runtime::locked(store)
                    {
                        let main = Path::new(&main.path);
                        let prefs = worktree::meta::prefs(&db).unwrap_or_default();
                        data.worktrees.default_root =
                            worktree::layout::repo_dir(&prefs.root_or_default(), main);
                        data.worktrees.repo = worktree::meta::repo_prefs(&db, main).ok();
                        data.worktrees.prefs = Some(prefs);
                    }
                }
                data.worktrees.branches = s.branches;
                data.worktrees.trees = s
                    .worktrees
                    .into_iter()
                    .filter(|t| !t.main)
                    .map(|tree| {
                        let status =
                            monocode_core::worktree_management::status(Path::new(cwd), &tree);
                        let key = worktree::meta::key(Path::new(&tree.path));
                        let entry = entries.iter().find(|e| e.key == key);
                        Tree {
                            tree,
                            locked: status.0,
                            dirty: status.1,
                            unpushed: status.2,
                            ownership: entry.map(|e| e.ownership),
                            display_name: entry
                                .and_then(|e| e.meta.as_ref())
                                .map(|m| m.display_name.clone())
                                .filter(|n| !n.is_empty()),
                        }
                    })
                    .collect();
            }
            Err(e) => data.worktrees.error = Some(e),
        }
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;
    use monocode_core::store::Store;
    use std::sync::{Arc, Mutex};

    fn git(cwd: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .current_dir(cwd)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn worktrees_page_data_labels_ownership_and_uses_the_configured_root() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let main = base.join("project");
        std::fs::create_dir(&main).unwrap();
        git(&main, &["init", "-q", "-b", "main"]);
        git(&main, &["config", "user.name", "Test"]);
        git(&main, &["config", "user.email", "test@example.invalid"]);
        git(&main, &["config", "commit.gpgsign", "false"]);
        git(&main, &["commit", "-q", "--allow-empty", "-m", "Base"]);
        let root = base.join("workspaces");
        let store: crate::runtime::Database =
            Arc::new(Mutex::new(Store::open(Path::new(":memory:")).unwrap()));
        worktree::meta::save_prefs(
            &crate::runtime::locked(&store).unwrap(),
            &worktree::Prefs {
                root: Some(root.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        let managed = monocode_core::worktree_callers::named(
            &main,
            "managed",
            None,
            false,
            worktree::CreatedBy::Dialog,
            &store,
        )
        .unwrap();
        let legacy = base.join("project-worktrees/mc-native-1-2");
        git(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "mc/native-1-2",
                "--",
                legacy.to_str().unwrap(),
            ],
        );
        let external = base.join("elsewhere");
        git(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "ext",
                "--",
                external.to_str().unwrap(),
            ],
        );

        let cwd = main.to_string_lossy().into_owned();
        let data = discover(&cwd, Some(&store)).worktrees;
        assert!(data.error.is_none());
        assert_eq!(data.default_root, root.join("project"));
        assert!(data.prefs.is_some() && data.repo.is_some());
        let label = |path: &Path| {
            data.trees
                .iter()
                .find(|t| {
                    Path::new(&t.tree.path).canonicalize().unwrap() == path.canonicalize().unwrap()
                })
                .and_then(Tree::ownership_label)
        };
        assert_eq!(label(Path::new(&managed.path)), Some("Managed"));
        assert_eq!(label(&legacy), Some("Legacy"));
        assert_eq!(label(&external), Some("External"));
        let tree = data
            .trees
            .iter()
            .find(|t| t.ownership == Some(Ownership::Managed))
            .unwrap();
        assert_eq!(tree.display_name.as_deref(), Some("managed"));

        // Without the store (remote projects, tests), the page keeps the old data.
        let plain = discover(&cwd, None).worktrees;
        assert!(plain.prefs.is_none());
        assert!(plain.trees.iter().all(|t| t.ownership.is_none()));
    }
}
