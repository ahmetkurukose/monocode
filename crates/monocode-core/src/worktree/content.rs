//! Content paths for a new worktree (spec §5): `sharedDirectories` become symlinks to the
//! main checkout, `.worktreeinclude` entries are copied. Only existing, Git-ignored paths
//! are touched, nothing is ever overwritten, and failures are warnings.
use super::config::{safe_relative, Config};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

pub const INCLUDE_FILE: &str = ".worktreeinclude";
const INCLUDE_MAX_BYTES: u64 = 256 * 1024;
const INCLUDE_MAX_ENTRIES: usize = 1000;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Budget {
    pub bytes: u64,
    pub entries: u64,
}
pub(crate) const BUDGET: Budget = Budget {
    bytes: 2 * 1024 * 1024 * 1024,
    entries: 50_000,
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Relative paths linked to the main checkout.
    pub shared: Vec<String>,
    /// Relative paths copied from the main checkout.
    pub copied: Vec<String>,
    pub warnings: Vec<String>,
}

/// The subset of `paths` Git ignores in `cwd` (tracked paths are never reported).
pub(crate) fn ignored(cwd: &Path, paths: &[String]) -> HashSet<String> {
    if paths.is_empty() {
        return HashSet::new();
    }
    let mut input = Vec::new();
    for p in paths {
        input.extend_from_slice(p.as_bytes());
        input.push(0);
    }
    match super::git_input(cwd, &["check-ignore", "-z", "--stdin"], &input) {
        Ok((0, out)) => out
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(|s| s.trim_end_matches('/').to_owned())
            .collect(),
        _ => HashSet::new(),
    }
}

/// Configured shared directories that exist as directories in `main` and are ignored.
pub fn shared_directories(main: &Path, config: &Config, warnings: &mut Vec<String>) -> Vec<String> {
    let mut existing = Vec::new();
    for rel in &config.shared_directories {
        match std::fs::metadata(main.join(rel)) {
            Ok(m) if m.is_dir() => existing.push(rel.clone()),
            Ok(_) => warnings.push(format!(
                "Skipping shared directory {rel}: it is not a directory."
            )),
            Err(_) => {}
        }
    }
    let ignored = ignored(main, &existing);
    existing.retain(|rel| {
        let keep = ignored.contains(rel);
        if !keep {
            warnings.push(format!(
                "Skipping shared directory {rel}: only Git-ignored directories can be shared."
            ));
        }
        keep
    });
    existing
}

fn link(target: &Path, at: &Path) -> std::io::Result<()> {
    if let Some(parent) = at.parent() {
        std::fs::create_dir_all(parent)?;
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, at)
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_dir(target, at)
    }
}

/// Literal entries of a `.worktreeinclude` file, with per-entry warnings for skipped lines.
pub(crate) fn parse_include(text: &str, warnings: &mut Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if out.len() >= INCLUDE_MAX_ENTRIES {
            warnings.push(format!(
                "{INCLUDE_FILE} lists more than {INCLUDE_MAX_ENTRIES} entries; the rest were ignored."
            ));
            break;
        }
        if line.starts_with('!') || line.contains(['*', '?', '[']) {
            warnings.push(format!(
                "Skipping {INCLUDE_FILE} pattern \"{line}\": only literal paths are supported."
            ));
            continue;
        }
        let Some(rel) = safe_relative(line) else {
            warnings.push(format!("Skipping unsafe {INCLUDE_FILE} path \"{line}\"."));
            continue;
        };
        if seen.insert(rel.clone()) {
            out.push(rel);
        }
    }
    out
}

/// `(bytes, entries)` under `path`, without following symlinks; stops once over `budget`.
fn measure(path: &Path, budget: Budget) -> (u64, u64) {
    let mut bytes = 0;
    let mut entries = 0;
    let mut stack = vec![path.to_path_buf()];
    while let Some(p) = stack.pop() {
        let Ok(meta) = p.symlink_metadata() else {
            continue;
        };
        entries += 1;
        if meta.is_dir() {
            if let Ok(dir) = std::fs::read_dir(&p) {
                stack.extend(dir.flatten().map(|e| e.path()));
            }
        } else {
            bytes += meta.len();
        }
        if bytes > budget.bytes || entries > budget.entries {
            break;
        }
    }
    (bytes, entries)
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    let meta = from.symlink_metadata()?;
    if to.symlink_metadata().is_ok() {
        return Ok(()); // never overwrite
    }
    if meta.file_type().is_symlink() {
        let target = std::fs::read_link(from)?;
        #[cfg(unix)]
        return std::os::unix::fs::symlink(target, to);
        #[cfg(windows)]
        return if from.is_dir() {
            std::os::windows::fs::symlink_dir(target, to)
        } else {
            std::os::windows::fs::symlink_file(target, to)
        };
    }
    if meta.is_dir() {
        std::fs::create_dir(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            copy_tree(&entry.path(), &to.join(entry.file_name()))?;
        }
        return Ok(());
    }
    std::fs::copy(from, to).map(|_| ())
}

/// macOS: APFS clone via `cp -c -R`; everywhere else (or on failure) a plain copy.
fn copy(from: &Path, to: &Path) -> std::io::Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    #[cfg(target_os = "macos")]
    {
        let cloned = std::process::Command::new("cp")
            .args(["-c", "-R", "-n", "--"])
            .arg(from)
            .arg(to)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if cloned && to.symlink_metadata().is_ok() {
            return Ok(());
        }
        // Retry without cloning; a partial clone is completed without overwriting.
    }
    copy_tree(from, to)
}

fn read_include(main: &Path) -> Option<String> {
    let path = main.join(INCLUDE_FILE);
    let meta = path.symlink_metadata().ok()?;
    if !meta.is_file() || meta.len() > INCLUDE_MAX_BYTES {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

/// Link shared directories and copy `.worktreeinclude` paths from `main` into `worktree`.
pub fn materialize(main: &Path, worktree: &Path, config: &Config) -> Report {
    materialize_with(main, worktree, config, BUDGET)
}

pub(crate) fn materialize_with(
    main: &Path,
    worktree: &Path,
    config: &Config,
    budget: Budget,
) -> Report {
    let mut report = Report::default();
    let shared = shared_directories(main, config, &mut report.warnings);
    for rel in &shared {
        let at = worktree.join(rel);
        if at.symlink_metadata().is_ok() {
            continue;
        }
        match link(&main.join(rel), &at) {
            Ok(()) => report.shared.push(rel.clone()),
            Err(e) => report.warnings.push(format!("Could not share {rel}: {e}")),
        }
    }

    let Some(text) = read_include(main) else {
        return report;
    };
    let under_shared = |rel: &str| {
        shared
            .iter()
            .any(|s| rel == s || rel.starts_with(&format!("{s}/")))
    };
    let candidates: Vec<String> = parse_include(&text, &mut report.warnings)
        .into_iter()
        .filter(|rel| !under_shared(rel))
        .filter(|rel| main.join(rel).symlink_metadata().is_ok())
        .collect();
    let ignored = ignored(main, &candidates);
    let mut used = Budget {
        bytes: 0,
        entries: 0,
    };
    let mut sorted: Vec<&String> = candidates.iter().filter(|r| ignored.contains(*r)).collect();
    sorted.sort();
    for rel in sorted {
        let from = main.join(rel);
        let to = worktree.join(rel);
        if to.symlink_metadata().is_ok() {
            continue;
        }
        let (bytes, entries) = measure(&from, budget);
        if used.bytes + bytes > budget.bytes || used.entries + entries > budget.entries {
            report.warnings.push(format!(
                "Skipping {rel}: copying it would exceed the {INCLUDE_FILE} size budget."
            ));
            continue;
        }
        match copy(&from, &to) {
            Ok(()) => {
                used.bytes += bytes;
                used.entries += entries;
                report.copied.push(rel.clone());
            }
            Err(e) => report.warnings.push(format!("Could not copy {rel}: {e}")),
        }
    }
    report
}

/// Configured shared paths that are symlinks inside `worktree` (ours to unlink).
pub(crate) fn shared_links(main: &Path, worktree: &Path) -> Vec<PathBuf> {
    super::config::load(main)
        .shared_directories
        .iter()
        .map(|rel| worktree.join(rel))
        .filter(|p| {
            p.symlink_metadata()
                .is_ok_and(|m| m.file_type().is_symlink())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::fixture::{self, run};
    use super::*;

    fn setup() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let (t, main, _root) = fixture::repo();
        std::fs::write(
            main.join(".gitignore"),
            "node_modules/\n.cache/\n.env\n.env.local\nsecrets/\nbig/\nlinked\n",
        )
        .unwrap();
        fixture::commit(&main, "tracked.txt", "t\n", "Tracked");
        run(&main, &["add", ".gitignore"]);
        run(&main, &["commit", "-q", "-m", "Ignore"]);
        let wt = t.path().canonicalize().unwrap().join("wt");
        run(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "wt",
                "--",
                &wt.to_string_lossy(),
            ],
        );
        (t, main, wt)
    }

    #[test]
    fn worktree_shared_directories_symlink_only_ignored_dirs() {
        let (_t, main, wt) = setup();
        std::fs::create_dir_all(main.join("node_modules/pkg")).unwrap();
        std::fs::write(main.join("node_modules/pkg/index.js"), "x").unwrap();
        std::fs::create_dir_all(main.join("src")).unwrap();
        std::fs::write(main.join("src/a.rs"), "a").unwrap();
        std::fs::write(main.join(".env"), "A=1").unwrap();
        std::fs::create_dir_all(main.join(".cache")).unwrap();
        std::fs::create_dir_all(wt.join(".cache")).unwrap();
        let config = Config {
            shared_directories: vec![
                "node_modules".into(),
                "src".into(),     // not ignored
                ".env".into(),    // not a directory
                "missing".into(), // does not exist
                ".cache".into(),  // destination exists
            ],
            ..Config::default()
        };
        let report = materialize(&main, &wt, &config);
        assert_eq!(report.shared, ["node_modules"]);
        assert_eq!(report.warnings.len(), 2, "{:?}", report.warnings);
        let link = wt.join("node_modules");
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            main.join("node_modules")
        );
        assert!(wt.join("node_modules/pkg/index.js").exists());
        assert!(!wt.join("src").exists());
        assert!(!wt
            .join(".cache")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            shared_links(&main, &wt),
            Vec::<PathBuf>::new(),
            "main has no config file"
        );
        std::fs::write(
            main.join("monocode.yaml"),
            "worktree:\n  sharedDirectories: [node_modules, .cache]\n",
        )
        .unwrap();
        assert_eq!(shared_links(&main, &wt), [wt.join("node_modules")]);
    }

    #[test]
    fn worktree_include_copy_rules() {
        let (_t, main, wt) = setup();
        std::fs::write(main.join(".env"), "SECRET=1").unwrap();
        std::fs::write(main.join(".env.local"), "LOCAL=main").unwrap();
        std::fs::write(wt.join(".env.local"), "LOCAL=mine").unwrap();
        std::fs::create_dir_all(main.join("secrets/nested")).unwrap();
        std::fs::write(main.join("secrets/nested/key"), "k").unwrap();
        std::fs::create_dir_all(main.join("node_modules")).unwrap();
        std::fs::write(main.join("node_modules/x"), "x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("tracked.txt", main.join("linked")).unwrap();
        std::fs::write(
            main.join(INCLUDE_FILE),
            "# comment\n\n.env\n./.env.local\nsecrets/\n*.pem\n!keep\nfoo?\n../escape\n/abs\n.git/config\ntracked.txt\nmissing.txt\nnode_modules\n.env\nlinked\n",
        )
        .unwrap();
        let config = Config {
            shared_directories: vec!["node_modules".into()],
            ..Config::default()
        };
        let report = materialize(&main, &wt, &config);
        assert_eq!(report.shared, ["node_modules"]);
        #[cfg(unix)]
        assert_eq!(report.copied, [".env", "linked", "secrets"]);
        assert_eq!(report.warnings.len(), 6, "{:?}", report.warnings);
        assert_eq!(
            std::fs::read_to_string(wt.join(".env")).unwrap(),
            "SECRET=1"
        );
        // Never overwrite.
        assert_eq!(
            std::fs::read_to_string(wt.join(".env.local")).unwrap(),
            "LOCAL=mine"
        );
        assert_eq!(
            std::fs::read_to_string(wt.join("secrets/nested/key")).unwrap(),
            "k"
        );
        // Tracked file untouched (same content as checkout), not reported as copied.
        assert_eq!(
            std::fs::read_to_string(wt.join("tracked.txt")).unwrap(),
            "t\n"
        );
        #[cfg(unix)]
        assert!(wt
            .join("linked")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink());

        // Budget overflow is skipped with a warning.
        let wt2 = wt.with_file_name("wt2");
        run(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "wt2",
                "--",
                &wt2.to_string_lossy(),
            ],
        );
        let report = materialize_with(
            &main,
            &wt2,
            &Config::default(),
            Budget {
                bytes: 10,
                entries: 3,
            },
        );
        assert!(report.copied.contains(&".env".to_owned()));
        assert!(!report.copied.contains(&"secrets".to_owned()));
        assert!(report.warnings.iter().any(|w| w.contains("budget")));
    }
}
