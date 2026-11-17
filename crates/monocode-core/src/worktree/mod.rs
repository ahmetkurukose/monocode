//! Orca-style worktrees: first-class checkouts owned by a project, created under a
//! central root (`~/monocode/workspaces/<repo>/<slug>`). Git is the source of truth for
//! structure; Monocode only stores overlay metadata in the native settings table.
//! See `docs/native/worktrees.md`. Local projects only: every entry point that touches
//! a repository returns [`REMOTE_UNAVAILABLE`] for `remote://` paths.
//!
//! Public API (all errors are user-facing `String`s):
//!
//! ```text
//! layout::default_root() -> PathBuf                                   // ~/monocode/workspaces
//! layout::repo_dir(root: &Path, main: &Path) -> PathBuf               // root/<basename(main) minus .git>
//! layout::main_checkout(cwd: &Path) -> Result<PathBuf, String>        // first `worktree list` entry
//!
//! naming::slug(name: &str) -> Result<String, String>
//! naming::from_prompt(prompt: &str) -> String                         // first ~6 words, fallback "worktree"
//! naming::BranchPrefix { None, GitUsername, Custom(String) }          // serde: "none" | "git-username" | "custom:<p>"
//! naming::resolve_prefix(prefix: &BranchPrefix, cwd: &Path) -> Option<String>
//! naming::branch_for(slug: &str, prefix: &BranchPrefix, cwd: &Path) -> String
//! naming::claim(path: &Path) -> Option<naming::Claim>                 // RAII, process-global
//! naming::pick(&naming::PickRequest) -> Result<naming::Picked, String> // collision loop slug, slug-2 … slug-100
//!
//! base::detect_default(cwd: &Path) -> Option<String>                  // origin/HEAD → origin/main → origin/master → main → master
//! base::resolve(cwd, explicit: Option<&str>, repo_default: Option<&str>) -> Result<base::Resolved, String>
//! base::fetch(cwd, remote: &str, branch: &str, timeout: Duration) -> Result<(), String>
//!
//! create::create(req: &CreateRequest, store: &Mutex<Store>, progress: &dyn Fn(Phase)) -> Result<Created, String>
//!
//! content::materialize(main: &Path, worktree: &Path, config: &Config) -> content::Report
//! content::shared_directories(main: &Path, config: &Config, warnings: &mut Vec<String>) -> Vec<String>
//!
//! config::load(dir: &Path) -> Config                                  // monocode.yaml → orca.yaml
//! config::parse(text: &str) -> Result<Config, String>
//!
//! setup::script(config: &Config) -> Option<&str>
//! setup::hash(script: &str) -> String                                 // sha256 hex of the trimmed script
//! setup::env(main: &Path, worktree: &Path, name: &str) -> Vec<(String, String)>
//! setup::plan(main, worktree, name, approved_hash: Option<&str>) -> Option<SetupPlan>
//! setup::write_runner(worktree: &Path, script: &str) -> Result<PathBuf, String>
//!
//! catalog::list(cwd: &Path, store: &Store) -> Result<Vec<Entry>, String>
//! catalog::list_with(cwd: &Path, metas: &[WorktreeMeta]) -> Result<Vec<Entry>, String>
//! catalog::reconcile(cwd: &Path, store: &Store) -> Result<Vec<String>, String> // removed meta keys
//!
//! remove::preflight(cwd: &Path, path: &Path) -> Result<Preflight, String>
//! remove::remove(cwd, path, opts: &RemoveOptions, store: &Mutex<Store>) -> Result<Removed, String>
//! remove::force_delete_branch(cwd: &Path, branch: &str) -> Result<(), String>
//!
//! meta::key(path: &Path) -> String                                    // files::path_key(canonical path)
//! meta::{load, save, delete, all}(store, …)                           // `worktree:<key>`
//! meta::{repo_prefs, save_repo_prefs}(store, main, …)                 // `worktree-repo:<key>`
//! meta::{prefs, save_prefs}(store, …)                                 // `worktree-prefs`
//! ```
//!
//! `create` and `remove` take the store behind a `Mutex` (the GPUI app's
//! `Database = Arc<Mutex<Store>>` derefs to it) and lock it only briefly, so they can run
//! on a background thread without holding the database during Git work.
pub mod base;
pub mod catalog;
pub mod config;
pub mod content;
pub mod create;
pub mod layout;
pub mod meta;
pub mod naming;
pub mod remove;
pub mod setup;

pub use catalog::{Entry, Ownership};
pub use config::Config;
pub use create::{CreateRequest, Created};
pub use meta::{CreatedBy, Prefs, RepoPrefs, StartKind, WorktreeMeta};
pub use remove::{Preflight, RemoveOptions, Removed};
pub use setup::SetupPlan;

use std::{
    io::{Read, Write},
    path::Path,
    process::{Command, Stdio},
    sync::Mutex,
    time::{Duration, Instant},
};

/// Error returned for `remote://` projects; callers keep the old path for those.
pub const REMOTE_UNAVAILABLE: &str = "Worktrees are not available for remote projects yet.";

/// Creation progress, reported in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Resolving,
    Fetching,
    CheckingOut { percent: Option<u8> },
    CopyingFiles,
    Done,
}

pub(crate) fn ensure_local(path: &Path) -> Result<(), String> {
    if crate::files::remote::is_remote(path) {
        Err(REMOTE_UNAVAILABLE.into())
    } else {
        Ok(())
    }
}

pub(crate) fn lock(
    store: &Mutex<crate::store::Store>,
) -> std::sync::MutexGuard<'_, crate::store::Store> {
    store.lock().unwrap_or_else(|e| e.into_inner())
}

fn command(cwd: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(cwd)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .stdin(Stdio::null());
    crate::process::configure_child(&mut command);
    command
}

fn failure(out: &std::process::Output) -> String {
    let err = String::from_utf8_lossy(&out.stderr).trim().to_owned();
    if err.is_empty() {
        format!("Git exited with {}", out.status)
    } else {
        err
    }
}

/// Run Git and return stdout, or stderr as the error.
pub(crate) fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    ensure_local(cwd)?;
    let out = command(cwd, args)
        .output()
        .map_err(|e| format!("Could not run Git: {e}"))?;
    if !out.status.success() {
        return Err(failure(&out));
    }
    String::from_utf8(out.stdout).map_err(|e| e.to_string())
}

/// Whether Git exits successfully (used for `--quiet`/`--verify` probes).
pub(crate) fn git_ok(cwd: &Path, args: &[&str]) -> bool {
    !crate::files::remote::is_remote(cwd)
        && command(cwd, args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
}

/// Run Git with `input` on stdin; returns `(exit code, stdout)`.
pub(crate) fn git_input(cwd: &Path, args: &[&str], input: &[u8]) -> Result<(i32, String), String> {
    ensure_local(cwd)?;
    let mut child = command(cwd, args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Could not run Git: {e}"))?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    let input = input.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let out = child
        .wait_with_output()
        .map_err(|e| format!("Could not run Git: {e}"))?;
    let _ = writer.join();
    Ok((
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    ))
}

/// Run Git, killing it after `timeout`. Stderr chunks (split on `\r`/`\n`) go to
/// `on_stderr` as they arrive, so progress meters can be parsed live.
pub(crate) fn git_live(
    cwd: &Path,
    args: &[&str],
    timeout: Option<Duration>,
    on_stderr: &mut dyn FnMut(&str),
) -> Result<String, String> {
    ensure_local(cwd)?;
    let mut child = command(cwd, args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Could not run Git: {e}"))?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let out_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    let err_thread = std::thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        while let Ok(n) = stderr.read(&mut chunk) {
            if n == 0 || tx.send(chunk[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    let deadline = timeout.map(|t| Instant::now() + t);
    let mut err_all = Vec::new();
    let mut pending = Vec::new();
    let mut emit = |bytes: &[u8], pending: &mut Vec<u8>, flush: bool| {
        pending.extend_from_slice(bytes);
        while let Some(at) = pending.iter().position(|b| *b == b'\r' || *b == b'\n') {
            let line: Vec<u8> = pending.drain(..=at).collect();
            let line = String::from_utf8_lossy(&line[..line.len() - 1]).into_owned();
            if !line.is_empty() {
                on_stderr(&line);
            }
        }
        if flush && !pending.is_empty() {
            on_stderr(&String::from_utf8_lossy(pending));
            pending.clear();
        }
    };
    let status = loop {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(bytes) => {
                err_all.extend_from_slice(&bytes);
                emit(&bytes, &mut pending, false);
                continue;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                break child
                    .wait()
                    .map_err(|e| format!("Could not run Git: {e}"))?;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            // Drain what the reader still has buffered.
            while let Ok(bytes) = rx.recv_timeout(Duration::from_millis(200)) {
                err_all.extend_from_slice(&bytes);
                emit(&bytes, &mut pending, false);
            }
            break status;
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            let _ = child.kill();
            let _ = child.wait();
            // Grandchildren (ssh, credential helpers) may keep the pipes open, so the
            // reader threads are left to finish on their own.
            return Err(format!(
                "Git timed out after {} s",
                timeout.unwrap_or_default().as_secs()
            ));
        }
    };
    emit(&[], &mut pending, true);
    let _ = err_thread.join();
    let stdout = out_thread.join().unwrap_or_default();
    if !status.success() {
        let err = String::from_utf8_lossy(&err_all).trim().to_owned();
        return Err(if err.is_empty() {
            format!("Git exited with {status}")
        } else {
            err
        });
    }
    String::from_utf8(stdout).map_err(|e| e.to_string())
}

/// Canonical path when it exists, the path itself otherwise.
pub(crate) fn canonical(path: &Path) -> std::path::PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
pub(crate) mod fixture {
    //! Real-Git fixtures shared by the worktree tests.
    use super::git;
    use std::path::{Path, PathBuf};

    pub fn run(cwd: &Path, args: &[&str]) -> String {
        git(cwd, args).unwrap_or_else(|e| panic!("git {args:?} failed: {e}"))
    }

    pub fn configure(cwd: &Path) {
        for (k, v) in [
            ("user.name", "Fixture"),
            ("user.email", "fixture@example.invalid"),
            ("commit.gpgsign", "false"),
            ("tag.gpgsign", "false"),
            ("core.autocrlf", "false"),
            ("core.hooksPath", ".git/no-hooks"),
        ] {
            run(cwd, &["config", "--local", k, v]);
        }
    }

    pub fn commit(cwd: &Path, file: &str, body: &str, message: &str) {
        let path = cwd.join(file);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, body).unwrap();
        run(cwd, &["add", "--", file]);
        run(cwd, &["commit", "-q", "-m", message]);
    }

    /// `<tmp>/project` with one commit on `main`; returns (tempdir, main, root).
    pub fn repo() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let main = base.join("project");
        std::fs::create_dir(&main).unwrap();
        run(&main, &["init", "-q", "-b", "main"]);
        configure(&main);
        commit(&main, "README.md", "hello\n", "Initial");
        let root = base.join("workspaces");
        (temp, main, root)
    }

    /// `repo()` cloned from a bare `origin` with `main` and `feature` branches.
    pub fn with_origin() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let seed = base.join("seed");
        std::fs::create_dir(&seed).unwrap();
        run(&seed, &["init", "-q", "-b", "main"]);
        configure(&seed);
        commit(&seed, "README.md", "hello\n", "Initial");
        run(&seed, &["branch", "feature"]);
        let origin = base.join("origin.git");
        run(
            &base,
            &["clone", "-q", "--bare", "--", "seed", "origin.git"],
        );
        run(&base, &["clone", "-q", "--", "origin.git", "project"]);
        let main = base.join("project");
        configure(&main);
        let root = base.join("workspaces");
        (temp, main, root, origin)
    }

    pub fn store() -> std::sync::Mutex<crate::store::Store> {
        std::sync::Mutex::new(crate::store::Store::open(Path::new(":memory:")).unwrap())
    }

    pub fn has_branch(cwd: &Path, branch: &str) -> bool {
        super::git_ok(
            cwd,
            &[
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ],
        )
    }
}
