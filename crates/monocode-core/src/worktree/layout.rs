//! Where managed worktrees live: `<root>/<repo>/<slug>`.
use std::path::{Path, PathBuf};

/// `~/monocode/workspaces` (falls back to the temp dir when no home is known).
pub fn default_root() -> PathBuf {
    crate::accounts::home()
        .unwrap_or_else(std::env::temp_dir)
        .join("monocode")
        .join("workspaces")
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
