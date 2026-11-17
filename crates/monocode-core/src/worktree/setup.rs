//! The optional setup script (`scripts.setup`) and its trust hash (spec §5).
use super::config::{self, Config};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// What the caller needs to offer or run setup for a new worktree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetupPlan {
    pub script: String,
    pub hash: String,
    /// The hash matches the repo's `setup_approved_hash`.
    pub approved: bool,
    pub env: Vec<(String, String)>,
}

pub fn script(config: &Config) -> Option<&str> {
    config
        .setup
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// sha256 hex of the trimmed script.
pub fn hash(script: &str) -> String {
    Sha256::digest(script.trim().as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `MONOCODE_*` variables plus the Orca aliases.
pub fn env(main: &Path, worktree: &Path, name: &str) -> Vec<(String, String)> {
    let main = main.to_string_lossy().into_owned();
    let worktree = worktree.to_string_lossy().into_owned();
    let mut out = Vec::new();
    for prefix in ["MONOCODE", "ORCA"] {
        out.push((format!("{prefix}_ROOT_PATH"), main.clone()));
        out.push((format!("{prefix}_WORKTREE_PATH"), worktree.clone()));
        out.push((format!("{prefix}_WORKSPACE_NAME"), name.to_owned()));
    }
    out
}

/// The setup script from the new worktree's config, falling back to the main checkout's.
pub fn plan(
    main: &Path,
    worktree: &Path,
    name: &str,
    approved_hash: Option<&str>,
) -> Option<SetupPlan> {
    let own = config::load(worktree);
    let script = match script(&own) {
        Some(s) => s.to_owned(),
        None => script(&config::load(main))?.to_owned(),
    };
    let hash = hash(&script);
    Some(SetupPlan {
        approved: approved_hash == Some(hash.as_str()),
        hash,
        env: env(main, worktree, name),
        script,
    })
}

/// Write `$(git rev-parse --git-path monocode/setup-runner.sh)` (bash, `set -e`, 0755).
pub fn write_runner(worktree: &Path, script: &str) -> Result<PathBuf, String> {
    let rel = super::git(
        worktree,
        &["rev-parse", "--git-path", "monocode/setup-runner.sh"],
    )?;
    let rel = PathBuf::from(rel.trim());
    let path = if rel.is_absolute() {
        rel
    } else {
        worktree.join(rel)
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let body = format!(
        "#!/usr/bin/env bash\nset -e\n{}\n",
        script.trim().replace("\r\n", "\n")
    );
    std::fs::write(&path, body).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::super::fixture;
    use super::*;

    #[test]
    fn worktree_setup_hash_runner_env() {
        assert_eq!(hash("echo hi"), hash("  echo hi \n"));
        assert_ne!(hash("echo hi"), hash("echo bye"));
        assert_eq!(hash("x").len(), 64);
        assert_eq!(script(&Config::default()), None);

        let (_t, main, _root) = fixture::repo();
        let wt = main.with_file_name("wt");
        fixture::run(
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
        assert_eq!(plan(&main, &wt, "wt", None), None);
        std::fs::write(main.join("orca.yaml"), "scripts:\n  setup: echo main\n").unwrap();
        let p = plan(&main, &wt, "wt", None).unwrap();
        assert_eq!((p.script.as_str(), p.approved), ("echo main", false));
        // The worktree's own config wins.
        let script =
            "echo \"$MONOCODE_WORKSPACE_NAME|$ORCA_ROOT_PATH|$MONOCODE_WORKTREE_PATH\" > out.txt";
        std::fs::write(
            wt.join("monocode.yaml"),
            format!("scripts:\n  setup: |\n    {script}\n"),
        )
        .unwrap();
        let p = plan(&main, &wt, "wt", Some(&hash(script))).unwrap();
        assert_eq!(p.script, script);
        assert!(p.approved);
        let keys: Vec<&str> = p.env.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            [
                "MONOCODE_ROOT_PATH",
                "MONOCODE_WORKTREE_PATH",
                "MONOCODE_WORKSPACE_NAME",
                "ORCA_ROOT_PATH",
                "ORCA_WORKTREE_PATH",
                "ORCA_WORKSPACE_NAME"
            ]
        );

        let runner = write_runner(&wt, &p.script).unwrap();
        assert!(runner.ends_with("monocode/setup-runner.sh"));
        assert!(
            !runner.starts_with(&wt),
            "runner lives in the git dir: {runner:?}"
        );
        let body = std::fs::read_to_string(&runner).unwrap();
        assert!(body.starts_with("#!/usr/bin/env bash\nset -e\n"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&runner).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o755);
            let status = std::process::Command::new(&runner)
                .current_dir(&wt)
                .envs(p.env.iter().map(|(k, v)| (k, v)))
                .status()
                .unwrap();
            assert!(status.success());
            assert_eq!(
                std::fs::read_to_string(wt.join("out.txt")).unwrap().trim(),
                format!("wt|{}|{}", main.display(), wt.display())
            );
            // set -e stops at the first failure.
            let failing = write_runner(&wt, "false\necho after > after.txt").unwrap();
            assert!(!std::process::Command::new(&failing)
                .current_dir(&wt)
                .status()
                .unwrap()
                .success());
            assert!(!wt.join("after.txt").exists());
        }
    }
}
