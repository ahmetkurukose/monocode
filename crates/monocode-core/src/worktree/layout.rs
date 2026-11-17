//! Where managed worktrees live: `<root>/<repo>/<slug>`.
use std::path::{Path, PathBuf};

/// Overrides [`default_root`] when set and non-empty (tests point it at a temp dir).
pub const ROOT_ENV: &str = "MONOCODE_WORKTREE_ROOT";

/// `$MONOCODE_WORKTREE_ROOT`, else `~/monocode/workspaces` (falls back to the temp dir when
/// no home is known).
pub fn default_root() -> PathBuf {
    #[cfg(test)]
    isolate_for_tests();
    if let Some(root) = std::env::var_os(ROOT_ENV).filter(|r| !r.is_empty()) {
        return PathBuf::from(root);
    }
    crate::accounts::home()
        .unwrap_or_else(std::env::temp_dir)
        .join("monocode")
        .join("workspaces")
}

/// Points [`ROOT_ENV`] at a process-wide temp dir unless the environment already sets it,
/// so no test ever writes to the real `~/monocode`. Idempotent.
pub fn isolate_for_tests() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        if std::env::var_os(ROOT_ENV).is_some_and(|r| !r.is_empty()) {
            return;
        }
        let dir = std::env::temp_dir()
            .join(format!("monocode-test-{}", std::process::id()))
            .join("monocode")
            .join("workspaces");
        // Set once, before this process creates any worktree.
        std::env::set_var(ROOT_ENV, dir);
    });
}

/// `root/<basename(main) minus a trailing .git>`.
pub fn repo_dir(root: &Path, main: &Path) -> PathBuf {
    let name = main
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = name.strip_suffix(".git").unwrap_or(&name);
    root.join(if name.is_empty() || name == "." || name == ".." {
        "repo"
    } else {
        name
    })
}

/// The primary checkout of the repository containing `cwd`.
pub fn main_checkout(cwd: &Path) -> Result<PathBuf, String> {
    super::ensure_local(cwd)?;
    let trees = crate::git::worktrees(cwd)?;
    let main = trees.first().ok_or("No working copies found")?;
    Ok(super::canonical(Path::new(&main.path)))
}

/// The pre-Orca sibling root (`<main>-worktrees`) used by legacy `mc-native-*` copies.
pub fn legacy_root(main: &Path) -> PathBuf {
    crate::worktree_management::default_root(main)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn worktree_layout_paths() {
        let root = Path::new("/r");
        assert_eq!(repo_dir(root, Path::new("/src/app")), Path::new("/r/app"));
        assert_eq!(
            repo_dir(root, Path::new("/src/app.git")),
            Path::new("/r/app")
        );
        assert_eq!(repo_dir(root, Path::new("/")), Path::new("/r/repo"));
        assert!(default_root().ends_with("monocode/workspaces"));
        assert_eq!(
            legacy_root(Path::new("/src/app")),
            Path::new("/src/app-worktrees")
        );
    }
}
