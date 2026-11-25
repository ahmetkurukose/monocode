use super::*;
use std::path::PathBuf;

fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn git(dir: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .args([
            "-c",
            "user.name=MonoCode",
            "-c",
            "user.email=monocode@test",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "MonoCode")
        .env("GIT_AUTHOR_EMAIL", "monocode@test")
        .env("GIT_COMMITTER_NAME", "MonoCode")
        .env("GIT_COMMITTER_EMAIL", "monocode@test")
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn init_git(dir: &Path, branch: &str) -> bool {
    git(dir, &["init"])
        && git(
            dir,
            &["symbolic-ref", "HEAD", &format!("refs/heads/{branch}")],
        )
        && git(dir, &["config", "user.name", "MonoCode"])
        && git(dir, &["config", "user.email", "monocode@test"])
        && git(dir, &["config", "commit.gpgsign", "false"])
        && git(dir, &["config", "core.autocrlf", "false"])
}

fn init_git_commit(dir: &Path, files: &[(&str, &str)]) -> bool {
    if !init_git(dir, "main") {
        return false;
    }
    for (name, contents) in files {
        if std::fs::write(dir.join(name), contents).is_err() {
            return false;
        }
    }
    git(dir, &["add", "."]) && git(dir, &["commit", "-m", "init"])
}

fn init_bare(dir: &Path) -> bool {
    git(dir, &["init", "--bare"])
}

fn index(dir: &Path) -> DiffIndex {
    diff_index(dir).unwrap()
}

fn file(dir: &Path, relative: &str) -> ChangedFile {
    index(dir)
        .files
        .into_iter()
        .find(|file| file.relative == relative)
        .unwrap()
}

#[test]
fn review_versions_invalidate_only_changed_file_sides() {
    let dir = tmp();
    let root = dir.path();
    assert!(init_git_commit(
        root,
        &[("a.txt", "base\n"), ("b.txt", "base\n")]
    ));
    std::fs::write(root.join("a.txt"), "stage\n").unwrap();
    assert!(git(root, &["add", "a.txt"]));
    std::fs::write(root.join("a.txt"), "work1\n").unwrap();
    std::fs::write(root.join("b.txt"), "other\n").unwrap();
    let first = diff_files(root).unwrap();
    let versions = file_diff_versions(root, &first).unwrap();
    assert_eq!(versions, file_diff_versions(root, &first).unwrap());

    std::fs::write(root.join("a.txt"), "work2\n").unwrap();
    let second = diff_files(root).unwrap();
    assert_eq!(first, second, "line counts cannot detect this edit");
    let edited = file_diff_versions(root, &second).unwrap();
    assert_ne!(versions["unstaged:a.txt"], edited["unstaged:a.txt"]);
    assert_eq!(versions["staged:a.txt"], edited["staged:a.txt"]);
    assert_eq!(versions["unstaged:b.txt"], edited["unstaged:b.txt"]);

    assert!(git(root, &["add", "a.txt"]));
    let staged = file_diff_versions(root, &second).unwrap();
    assert_ne!(edited["staged:a.txt"], staged["staged:a.txt"]);
    assert_ne!(edited["unstaged:a.txt"], staged["unstaged:a.txt"]);
    assert_eq!(edited["unstaged:b.txt"], staged["unstaged:b.txt"]);
}

#[test]
fn review_index_revalidates_closed_views_and_subdirectories() {
    let dir = tmp();
    let root = dir.path();
    std::fs::create_dir(root.join("src")).unwrap();
    assert!(init_git_commit(root, &[("src/a.txt", "base\n")]));
    std::fs::write(root.join("src/a.txt"), "edit\n").unwrap();
    let before = diff_files(root).unwrap();
    let added_name = if cfg!(windows) {
        " new file.txt"
    } else {
        " new\tfile.txt"
    };
    std::fs::write(root.join("src").join(added_name), "new\n").unwrap();
    let next = review_index(root, Some(&before)).unwrap();
    assert_eq!(next.files.len(), 2);
    assert_eq!(next.files[1].additions, before.files[0].additions);
    let sub = review_index(&root.join("src"), None).unwrap();
    assert_eq!(sub.files[0].relative, added_name);
    assert_eq!(sub.files[1].relative, "a.txt");
    let versions = file_diff_versions(&root.join("src"), &sub).unwrap();
    assert!(versions.contains_key(&format!("unstaged:{added_name}")));
    assert!(versions.contains_key("unstaged:a.txt"));
    std::fs::remove_file(root.join("src/a.txt")).unwrap();
    let deleted = review_index(root, None).unwrap();
    assert_eq!(deleted.files[1].status, "deleted");
    assert!(file_diff_versions(root, &deleted).is_ok());
}

#[test]
fn review_versions_support_repositories_with_only_untracked_files() {
    let dir = tmp();
    assert!(init_git(dir.path(), "main"));
    std::fs::write(dir.path().join("new.txt"), "new\n").unwrap();
    let snapshot = review_index(dir.path(), None).unwrap();
    assert!(file_diff_versions(dir.path(), &snapshot)
        .unwrap()
        .contains_key("unstaged:new.txt"));
}

#[test]
fn diff_stats_are_zero_outside_a_repo() {
    let dir = tmp();
    std::fs::write(dir.path().join("notes.txt"), "hello\n").unwrap();
    assert_eq!(diff_stats(dir.path()).unwrap(), DiffStats::default());
}

#[test]
fn diff_stats_count_unstaged_and_untracked() {
    let dir = tmp();
    let dir = dir.path();
    assert!(init_git_commit(dir, &[("a.txt", "alpha\nbeta\ngamma\n")]));
    std::fs::write(dir.join("a.txt"), "alpha\ngamma\ndelta\n").unwrap();
    std::fs::write(dir.join("new.txt"), "hello\nworld\n").unwrap();
    std::fs::write(dir.join("ignored.txt"), "nope\n").unwrap();
    std::fs::write(dir.join(".gitignore"), "ignored.txt\n").unwrap();
    // a.txt: -beta +delta; new.txt: +2; .gitignore: +1 untracked
    assert_eq!(
        diff_stats(dir).unwrap(),
        DiffStats {
            files: 3,
            additions: 4,
            deletions: 1
        }
    );
}

#[test]
fn diff_stats_are_zero_when_clean() {
    let dir = tmp();
    assert!(init_git_commit(dir.path(), &[("a.txt", "alpha\n")]));
    assert_eq!(diff_stats(dir.path()).unwrap(), DiffStats::default());
}

#[test]
fn diff_index_lists_modified_and_untracked() {
    let dir = tmp();
    let dir = dir.path();
    assert!(init_git_commit(dir, &[("a.txt", "alpha\nbeta\ngamma\n")]));
    std::fs::write(dir.join("a.txt"), "alpha\ngamma\ndelta\n").unwrap();
    std::fs::write(dir.join("new.txt"), "hello\nworld\n").unwrap();

    let index = index(dir);
    assert_eq!(index.branch.as_deref(), Some("main"));
    assert_eq!(index.files.len(), 2);
    assert_eq!((index.additions, index.deletions), (3, 1));
    let modified = &index.files[0];
    assert_eq!(modified.relative, "a.txt");
    assert_eq!(modified.status, "modified");
    assert_eq!((modified.additions, modified.deletions), (1, 1));
    assert_eq!(modified.path, path_to_js(&dir.join("a.txt")));
    let untracked = &index.files[1];
    assert_eq!(untracked.relative, "new.txt");
    assert_eq!(untracked.status, "untracked");
    assert_eq!((untracked.additions, untracked.deletions), (2, 0));
    assert!(untracked.unstaged && !untracked.staged);
}

#[test]
fn diff_index_leaves_out_nested_repositories() {
    let dir = tmp();
    let dir = dir.path();
    assert!(init_git_commit(dir, &[("a.txt", "alpha\n")]));
    // A linked worktree inside the checkout, as `.claude/worktrees/x` is.
    assert!(git(dir, &["worktree", "add", "-q", ".claude/worktrees/x"]));
    std::fs::write(dir.join("new.txt"), "hello\n").unwrap();

    let index = index(dir);
    let names: Vec<&str> = index.files.iter().map(|f| f.relative.as_str()).collect();
    assert_eq!(names, ["new.txt"]);
    assert_eq!(index.additions, 1);
    assert_eq!(diff_stats(dir).unwrap().files, 1);
}

#[test]
fn diff_index_reports_added_and_deleted() {
    let dir = tmp();
    let dir = dir.path();
    assert!(init_git_commit(
        dir,
        &[("a.txt", "alpha\n"), ("b.txt", "b\n")]
    ));
    std::fs::write(dir.join("added.txt"), "x\n").unwrap();
    stage_file(dir, "added.txt").unwrap();
    std::fs::remove_file(dir.join("b.txt")).unwrap();
    assert_eq!(file(dir, "added.txt").status, "added");
    assert_eq!(file(dir, "b.txt").status, "deleted");
}

#[test]
fn diff_index_keeps_literal_non_ascii_paths() {
    let dir = tmp();
    let dir = dir.path();
    assert!(init_git_commit(
        dir,
        &[("한글.md", "before\n"), ("my file.txt", "one\n")]
    ));
    assert!(git(dir, &["config", "core.quotePath", "true"]));
    std::fs::write(dir.join("한글.md"), "before\nafter\n").unwrap();
    std::fs::write(dir.join("my file.txt"), "one\ntwo\n").unwrap();

    let hangul = file(dir, "한글.md");
    assert_eq!(hangul.status, "modified");
    assert_eq!(hangul.additions, 1);
    assert!(hangul.unstaged);
    let spaced = file(dir, "my file.txt");
    assert_eq!(spaced.status, "modified");
    assert_eq!(spaced.additions, 1);
}

#[test]
fn diff_index_keeps_paths_relative_to_nested_workspace() {
    let dir = tmp();
    let dir = dir.path();
    let workspace = dir.join("sub");
    std::fs::create_dir_all(workspace.join("sub")).unwrap();
    assert!(init_git_commit(
        dir,
        &[("sub/file.txt", "original\n"), ("sub/second.txt", "old\n")],
    ));
    std::fs::write(workspace.join("file.txt"), "staged\n").unwrap();
    assert!(git(dir, &["add", "--", "sub/file.txt"]));
    std::fs::write(workspace.join("second.txt"), "unstaged\n").unwrap();
    // Without --relative this collides with the tracked sub/file.txt key.
    std::fs::write(workspace.join("sub/file.txt"), "untracked\n").unwrap();

    let changes = index(&workspace);
    assert_eq!(changes.files.len(), 3);
    for (relative, staged, unstaged) in [
        ("file.txt", true, false),
        ("second.txt", false, true),
        ("sub/file.txt", false, true),
    ] {
        let file = changes
            .files
            .iter()
            .find(|file| file.relative == relative)
            .unwrap();
        assert_eq!(file.path, path_to_js(&workspace.join(relative)));
        assert_eq!((file.staged, file.unstaged), (staged, unstaged));
    }
}

#[test]
fn diff_index_works_without_a_head() {
    let dir = tmp();
    let dir = dir.path();
    assert!(init_git(dir, "main"));
    std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
    std::fs::write(dir.join("b.txt"), "x\n").unwrap();
    stage_file(dir, "a.txt").unwrap();
    let index = index(dir);
    assert_eq!(index.head, None);
    assert_eq!(index.branch.as_deref(), Some("main"));
    let a = &index.files[0];
    assert_eq!((a.relative.as_str(), a.status.as_str()), ("a.txt", "added"));
    assert_eq!(a.additions, 2);
    assert!(a.staged && !a.unstaged);
    assert_eq!(index.files[1].status, "untracked");
}

#[test]
fn parse_decorations_classifies_head_local_remote_and_tags() {
    let remotes = vec!["origin".to_string()];
    let (head, refs) = parse_decorations(
        "HEAD -> main, origin/main, origin/HEAD, tag: v0.1.30, fix/foo",
        Some("abc"),
        "abc",
        &remotes,
    );
    assert!(head);
    let refs: Vec<_> = refs
        .iter()
        .map(|r| (r.name.as_str(), r.kind.as_str()))
        .collect();
    assert_eq!(
        refs,
        [
            ("main", "local"),
            ("origin/main", "remote"),
            ("v0.1.30", "tag"),
            ("fix/foo", "local"),
        ]
    );
    let (head, refs) = parse_decorations("HEAD", None, "abc", &remotes);
    assert!(head && refs.is_empty());
    assert!(!parse_decorations("", Some("def"), "abc", &remotes).0);
}

#[test]
fn parse_history_log_reads_records() {
    let text = "aaaa\0aa\0bbbb cccc\0Ann\x001700000000\0HEAD -> main\0Merge it\x1e\n\
                bbbb\0\0\0Bob\0bad\0\0\x1e\nshort\0record\x1e";
    let commits = parse_history_log(text, Some("aaaa"), &[]);
    assert_eq!(commits.len(), 2);
    assert_eq!(commits[0].parents, ["bbbb", "cccc"]);
    assert_eq!(commits[0].timestamp, 1_700_000_000);
    assert_eq!(commits[0].subject, "Merge it");
    assert!(commits[0].head);
    assert_eq!(commits[1].short_sha, "bbbb");
    assert_eq!(commits[1].timestamp, 0);
    assert!(commits[1].parents.is_empty() && !commits[1].head);
}

#[test]
fn history_lists_commits_parents_and_head() {
    let dir = tmp();
    let dir = dir.path();
    assert!(init_git_commit(dir, &[("a.txt", "alpha\n")]));
    std::fs::write(dir.join("a.txt"), "beta\n").unwrap();
    assert!(git(dir, &["add", "."]));
    assert!(git(dir, &["commit", "-m", "second"]));

    let history = history(dir, Some(10)).unwrap();
    assert_eq!(history.commits.len(), 2);
    assert_eq!(
        history.head.as_deref(),
        Some(history.commits[0].sha.as_str())
    );
    assert_eq!(history.commits[0].subject, "second");
    assert_eq!(history.commits[1].subject, "init");
    assert_eq!(
        history.commits[0].parents,
        vec![history.commits[1].sha.clone()]
    );
    assert!(history.commits[0].head);
    assert!(!history.commits[1].head);
    assert_eq!(history.commits[0].author, "MonoCode");
    assert!(history.commits[0]
        .refs
        .iter()
        .any(|r| r.kind == "local" && r.name == "main"));
    assert_eq!(super::history(dir, Some(1)).unwrap().commits.len(), 1);
}

#[test]
fn history_empty_outside_a_repo() {
    let dir = tmp();
    assert_eq!(history(dir.path(), None).unwrap(), History::default());
}

#[test]
fn history_records_merge_parents() {
    let dir = tmp();
    let dir = dir.path();
    assert!(init_git_commit(dir, &[("a.txt", "alpha\n")]));
    assert!(git(dir, &["checkout", "-b", "feature"]));
    std::fs::write(dir.join("feat.txt"), "one\n").unwrap();
    assert!(git(dir, &["add", "."]));
    assert!(git(dir, &["commit", "-m", "feature work"]));
    assert!(git(dir, &["checkout", "main"]));
    std::fs::write(dir.join("main.txt"), "two\n").unwrap();
    assert!(git(dir, &["add", "."]));
    assert!(git(dir, &["commit", "-m", "main work"]));
    assert!(git(
        dir,
        &["merge", "feature", "--no-ff", "-m", "Merge feature"]
    ));
    let history = history(dir, Some(20)).unwrap();
    let merge = history
        .commits
        .iter()
        .find(|commit| commit.subject == "Merge feature")
        .unwrap();
    assert_eq!(merge.parents.len(), 2);
}

#[test]
fn history_skips_stash_and_unmerged_branches() {
    let dir = tmp();
    let dir = dir.path();
    assert!(init_git_commit(dir, &[("a.txt", "alpha\n")]));
    assert!(git(dir, &["checkout", "-b", "feature"]));
    std::fs::write(dir.join("feat.txt"), "one\n").unwrap();
    assert!(git(dir, &["add", "."]));
    assert!(git(dir, &["commit", "-m", "feature only"]));
    std::fs::write(dir.join("wip.txt"), "stash me\n").unwrap();
    assert!(git(dir, &["stash", "push", "-u", "-m", "wip stash"]));
    assert!(git(dir, &["checkout", "main"]));
    std::fs::write(dir.join("main.txt"), "two\n").unwrap();
    assert!(git(dir, &["add", "."]));
    assert!(git(dir, &["commit", "-m", "main only"]));

    let history = history(dir, Some(20)).unwrap();
    let subjects: Vec<&str> = history
        .commits
        .iter()
        .map(|commit| commit.subject.as_str())
        .collect();
    assert!(subjects.contains(&"main only"));
    assert!(subjects.contains(&"init"));
    assert!(!subjects.iter().any(|s| s.contains("feature only")));
    assert!(!subjects
        .iter()
        .any(|s| s.contains("wip stash") || s.contains("WIP on")));
}

#[test]
fn stage_and_unstage_file() {
    let dir = tmp();
    let dir = dir.path();
    assert!(init_git_commit(dir, &[("a.txt", "alpha\n")]));
    std::fs::write(dir.join("a.txt"), "beta\n").unwrap();
    stage_file(dir, "a.txt").unwrap();
    let staged = file(dir, "a.txt");
    assert!(staged.staged && !staged.unstaged);
    unstage_file(dir, "a.txt").unwrap();
    let unstaged = file(dir, "a.txt");
    assert!(!unstaged.staged && unstaged.unstaged);
}

#[test]
fn stage_and_unstage_directory() {
    let dir = tmp();
    let dir = dir.path();
    std::fs::create_dir_all(dir.join("src/nested")).unwrap();
    std::fs::create_dir_all(dir.join("src-other")).unwrap();
    assert!(init_git_commit(
        dir,
        &[
            ("src/app.ts", "before\n"),
            ("src/nested/deleted.ts", "delete me\n"),
            ("src-other/app.ts", "before\n"),
            ("ready.txt", "before\n"),
            (".gitignore", "src/ignored.txt\n"),
        ],
    ));
    std::fs::write(dir.join("src/app.ts"), "after\n").unwrap();
    std::fs::remove_dir_all(dir.join("src/nested")).unwrap();
    std::fs::create_dir_all(dir.join("src/added")).unwrap();
    std::fs::write(dir.join("src/added/new.ts"), "new\n").unwrap();
    std::fs::write(dir.join("src/ignored.txt"), "ignored\n").unwrap();
    std::fs::write(dir.join("src-other/app.ts"), "outside\n").unwrap();
    std::fs::write(dir.join("ready.txt"), "ready\n").unwrap();
    stage_file(dir, "ready.txt").unwrap();

    stage_file(dir, "src").unwrap();
    let staged = index(dir);
    assert_eq!(staged.files.len(), 5);
    for file in &staged.files {
        let in_folder = file.relative.starts_with("src/");
        assert_eq!(file.staged, in_folder || file.relative == "ready.txt");
        assert_eq!(file.unstaged, file.relative == "src-other/app.ts");
    }

    unstage_file(dir, "src").unwrap();
    let unstaged = index(dir);
    assert_eq!(unstaged.files.len(), 5);
    for file in &unstaged.files {
        assert_eq!(file.staged, file.relative == "ready.txt");
        assert_eq!(file.unstaged, file.relative != "ready.txt");
    }
    assert_eq!(
        std::fs::read_to_string(dir.join("src/app.ts")).unwrap(),
        "after\n"
    );
}

#[cfg(unix)]
#[test]
fn stage_and_unstage_literal_directory_names() {
    for folder in ["*", "folder?", "[ab]", ":(glob)*"] {
        let dir = tmp();
        let dir = dir.path();
        for directory in [folder, "a", "folderx"] {
            std::fs::create_dir(dir.join(directory)).unwrap();
        }
        let inside = format!("{folder}/inside.txt");
        let tracked = [
            inside.as_str(),
            "a/other.txt",
            "folderx/other.txt",
            "ready.txt",
        ];
        let initial: Vec<_> = tracked.iter().map(|path| (*path, "before\n")).collect();
        assert!(init_git_commit(dir, &initial));
        for path in tracked {
            std::fs::write(dir.join(path), "after\n").unwrap();
        }
        let staged_paths = || {
            git_run(dir, &["diff", "--cached", "--name-only", "-z"])
                .unwrap()
                .split('\0')
                .filter(|path| !path.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        };
        stage_file(dir, "ready.txt").unwrap();
        stage_file(dir, folder).unwrap();
        let mut expected = vec![inside.clone(), "ready.txt".to_string()];
        expected.sort();
        assert_eq!(staged_paths(), expected, "stage folder {folder}");
        unstage_file(dir, folder).unwrap();
        assert_eq!(staged_paths(), vec!["ready.txt"], "unstage folder {folder}");
    }
}

#[test]
fn stage_all_and_unstage_all() {
    let dir = tmp();
    let dir = dir.path();
    assert!(init_git_commit(
        dir,
        &[("a.txt", "alpha\n"), ("b.txt", "b\n")]
    ));
    std::fs::write(dir.join("a.txt"), "beta\n").unwrap();
    std::fs::remove_file(dir.join("b.txt")).unwrap();
    std::fs::write(dir.join("new.txt"), "hello\n").unwrap();
    stage_all(dir).unwrap();
    assert!(index(dir).files.iter().all(|f| f.staged && !f.unstaged));
    assert_eq!(file(dir, "new.txt").status, "added");
    unstage_all(dir).unwrap();
    assert!(index(dir).files.iter().all(|f| !f.staged && f.unstaged));
    assert_eq!(file(dir, "new.txt").status, "untracked");
}

#[test]
fn discard_restores_tracked_and_deletes_untracked() {
    let dir = tmp();
    let dir = dir.path();
    assert!(init_git_commit(dir, &[("a.txt", "alpha\n")]));
    std::fs::write(dir.join("a.txt"), "beta\n").unwrap();
    std::fs::write(dir.join("new.txt"), "hello\n").unwrap();
    std::fs::create_dir(dir.join("newdir")).unwrap();
    std::fs::write(dir.join("newdir/x.txt"), "x\n").unwrap();
    discard_file(dir, "a.txt").unwrap();
    discard_file(dir, "new.txt").unwrap();
    discard_file(dir, "newdir").unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.join("a.txt")).unwrap(),
        "alpha\n"
    );
    assert!(!dir.join("new.txt").exists());
    assert!(!dir.join("newdir/x.txt").exists());
    assert!(index(dir).files.is_empty());
}

#[test]
fn discard_all_restores_unstaged_and_keeps_staged() {
    let dir = tmp();
    let dir = dir.path();
    assert!(init_git_commit(
        dir,
        &[("a.txt", "alpha\n"), ("b.txt", "one\n")]
    ));
    std::fs::write(dir.join("a.txt"), "beta\n").unwrap();
    std::fs::write(dir.join("b.txt"), "two\n").unwrap();
    stage_file(dir, "b.txt").unwrap();
    std::fs::write(dir.join("b.txt"), "three\n").unwrap();
    std::fs::write(dir.join("new.txt"), "hello\n").unwrap();
    discard_all(dir).unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.join("a.txt")).unwrap(),
        "alpha\n"
    );
    assert_eq!(std::fs::read_to_string(dir.join("b.txt")).unwrap(), "two\n");
    assert!(!dir.join("new.txt").exists());
    let b = file(dir, "b.txt");
    assert!(b.staged && !b.unstaged);
    assert!(index(dir).files.iter().all(|file| !file.unstaged));
}

#[test]
fn discard_keeps_staged_hunks() {
    let dir = tmp();
    let dir = dir.path();
    assert!(init_git_commit(dir, &[("a.txt", "alpha\n")]));
    std::fs::write(dir.join("a.txt"), "beta\n").unwrap();
    stage_file(dir, "a.txt").unwrap();
    std::fs::write(dir.join("a.txt"), "gamma\n").unwrap();
    discard_file(dir, "a.txt").unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.join("a.txt")).unwrap(),
        "beta\n"
    );
    let a = file(dir, "a.txt");
    assert!(a.staged && !a.unstaged);
}

#[test]
fn stage_rejects_path_escape() {
    let dir = tmp();
    for path in ["../secret.txt", "/etc/passwd", "", "a//b"] {
        assert_eq!(stage_file(dir.path(), path).unwrap_err(), "Invalid path");
        assert!(unstage_file(dir.path(), path).is_err());
        assert!(discard_file(dir.path(), path).is_err());
    }
}

#[test]
fn commit_clears_staged_files() {
    let dir = tmp();
    let dir = dir.path();
    assert!(init_git_commit(dir, &[("a.txt", "alpha\n")]));
    std::fs::write(dir.join("a.txt"), "beta\n").unwrap();
    stage_file(dir, "a.txt").unwrap();
    commit(dir, "update a", false).unwrap();
    let index = index(dir);
    assert!(index.files.is_empty());
    assert_eq!(index.head, git_stdout(dir, &["rev-parse", "HEAD"]));
    assert_eq!(head_message(dir).unwrap(), "update a");
}

#[test]
fn commit_rejects_empty_message() {
    let dir = tmp();
    assert_eq!(
        commit(dir.path(), "   ", false).unwrap_err(),
        "Commit message cannot be empty"
    );
}

#[test]
fn commit_amend_rewrites_head_with_staged_changes() {
    let dir = tmp();
    let dir = dir.path();
    assert!(init_git_commit(dir, &[("a.txt", "alpha\n")]));
    std::fs::write(dir.join("a.txt"), "beta\n").unwrap();
    stage_file(dir, "a.txt").unwrap();
    commit(dir, "amended", true).unwrap();
    assert!(index(dir).files.is_empty());
    assert_eq!(
        git_stdout(dir, &["rev-list", "--count", "HEAD"]).as_deref(),
        Some("1")
    );
    assert_eq!(head_message(dir).unwrap(), "amended");
    assert_eq!(
        git_stdout(dir, &["show", "HEAD:a.txt"]).as_deref(),
        Some("beta")
    );
}

#[test]
fn head_message_returns_subject_and_body() {
    let dir = tmp();
    let dir = dir.path();
    assert!(init_git_commit(dir, &[("a.txt", "alpha\n")]));
    std::fs::write(dir.join("a.txt"), "beta\n").unwrap();
    stage_file(dir, "a.txt").unwrap();
    commit(dir, "Subject line\n\nBody text", false).unwrap();
    assert_eq!(head_message(dir).unwrap(), "Subject line\n\nBody text");
}

#[test]
fn head_message_fails_without_commits() {
    let dir = tmp();
    assert!(init_git(dir.path(), "main"));
    assert_eq!(head_message(dir.path()).unwrap_err(), "No commits yet");
}

#[test]
fn signing_hint_ignores_other_errors() {
    let error = "nothing to commit, working tree clean".to_string();
    assert_eq!(with_signing_hint(error.clone()), error);
}

#[test]
fn commit_reports_signing_failure_with_hint() {
    let dir = tmp();
    let dir = dir.path();
    assert!(init_git_commit(dir, &[("a.txt", "a\n")]));
    std::fs::write(dir.join("a.txt"), "b\n").unwrap();
    for args in [
        ["config", "commit.gpgsign", "true"],
        ["config", "gpg.format", "openpgp"],
        ["config", "gpg.program", "/nonexistent/monocode-gpg"],
    ] {
        assert!(git(dir, &args));
    }
    assert!(git(dir, &["add", "."]));
    let error = commit(dir, "signed", false).unwrap_err();
    assert!(error.contains("Git couldn't sign this commit"), "{error}");
}

#[test]
fn staged_context_reads_cached_diff() {
    let dir = tmp();
    let dir = dir.path();
    assert!(init_git_commit(dir, &[("a.txt", "alpha\n")]));
    std::fs::write(dir.join("a.txt"), "beta\n").unwrap();
    stage_file(dir, "a.txt").unwrap();
    let context = staged_context(dir).unwrap();
    assert_eq!(context.branch.as_deref(), Some("main"));
    assert!(context.summary.contains("a.txt"));
    assert!(context.patch.contains("beta"));
}

#[test]
fn staged_context_falls_back_to_unstaged_and_untracked() {
    let dir = tmp();
    let dir = dir.path();
    assert!(init_git_commit(dir, &[("a.txt", "alpha\n")]));
    assert_eq!(staged_context(dir).unwrap_err(), "No changes to summarize");
    std::fs::write(dir.join("a.txt"), "gamma\n").unwrap();
    std::fs::write(dir.join("new.txt"), "n\n").unwrap();
    let context = staged_context(dir).unwrap();
    assert!(context.patch.contains("gamma"));
    assert!(context.summary.ends_with("\nUntracked files:\nnew.txt"));
}

fn with_origin(repo: &Path, origin: &Path) {
    assert!(init_bare(origin));
    let url = origin.to_string_lossy().into_owned();
    assert!(git(repo, &["remote", "add", "origin", &url]));
    assert!(git(repo, &["push", "-u", "origin", "main"]));
}

#[test]
fn sync_counts_unpushed_commits() {
    let (repo, origin) = (tmp(), tmp());
    let repo = repo.path();
    assert!(init_git_commit(repo, &[("a.txt", "alpha\n")]));
    with_origin(repo, origin.path());
    std::fs::write(repo.join("a.txt"), "beta\n").unwrap();
    stage_file(repo, "a.txt").unwrap();
    commit(repo, "second", false).unwrap();
    let index = index(repo);
    assert_eq!(index.remote.as_deref(), Some("origin"));
    assert_eq!(index.upstream.as_deref(), Some("origin/main"));
    assert_eq!(index.default_branch.as_deref(), Some("main"));
    assert_eq!(
        (index.ahead, index.behind, index.ahead_of_default),
        (1, 0, 1)
    );
    assert!(!index.head_pushed);
    push(repo).unwrap();
    let index = super::diff_index(repo).unwrap();
    assert_eq!(index.ahead, 0);
    assert!(index.head_pushed);
}

#[test]
fn a_worktree_branch_targets_its_recorded_base() {
    let (repo, origin) = (tmp(), tmp());
    let repo = repo.path();
    assert!(init_git_commit(repo, &[("a.txt", "alpha\n")]));
    with_origin(repo, origin.path());
    // develop is one commit ahead of main on the remote.
    assert!(git(repo, &["checkout", "-b", "develop"]));
    std::fs::write(repo.join("d.txt"), "d\n").unwrap();
    stage_file(repo, "d.txt").unwrap();
    commit(repo, "develop work", false).unwrap();
    assert!(git(repo, &["push", "origin", "develop"]));
    // A worktree branch Monocode started from origin/develop.
    assert!(git(repo, &["checkout", "-b", "feature"]));
    assert!(git(
        repo,
        &["config", "branch.feature.base", "origin/develop"]
    ));
    std::fs::write(repo.join("a.txt"), "beta\n").unwrap();
    stage_file(repo, "a.txt").unwrap();
    commit(repo, "feature work", false).unwrap();
    let index = index(repo);
    assert_eq!(index.default_branch.as_deref(), Some("main"));
    assert_eq!(index.pr_base.as_deref(), Some("develop"));
    assert_eq!(
        (index.ahead, index.ahead_of_default),
        (1, 1),
        "only the feature commit, not develop's"
    );
    let range = range_context(repo).unwrap();
    assert_eq!(range.base, "develop");
    assert!(range.commit_summary.contains("feature work"));
    assert!(!range.commit_summary.contains("develop work"));
    // A base the remote no longer has falls back to the default branch.
    assert!(git(repo, &["config", "branch.feature.base", "origin/gone"]));
    assert_eq!(range_context(repo).unwrap().base, "main");
    assert_eq!(
        super::diff_index(repo).unwrap().pr_base.as_deref(),
        Some("main")
    );
}

#[test]
fn feature_branch_without_upstream_counts_and_ranges() {
    let (repo, origin) = (tmp(), tmp());
    let repo = repo.path();
    assert!(init_git_commit(repo, &[("a.txt", "alpha\n")]));
    with_origin(repo, origin.path());
    assert!(git(repo, &["checkout", "-b", "feature"]));
    assert_eq!(
        range_context(repo).unwrap_err(),
        "No commits to include in a pull request"
    );
    std::fs::write(repo.join("a.txt"), "beta\n").unwrap();
    stage_file(repo, "a.txt").unwrap();
    commit(repo, "feature work", false).unwrap();
    let index = index(repo);
    assert_eq!(index.branch.as_deref(), Some("feature"));
    assert_eq!(index.upstream, None);
    assert_eq!((index.ahead, index.ahead_of_default), (1, 1));
    let range = range_context(repo).unwrap();
    assert_eq!(range.base, "main");
    assert_eq!(range.head, "feature");
    assert!(range.commit_summary.contains("feature work"));
    assert!(range.diff_patch.contains("+beta"));
    // First push sets the upstream.
    push(repo).unwrap();
    assert_eq!(
        super::diff_index(repo).unwrap().upstream.as_deref(),
        Some("origin/feature")
    );
}

#[test]
fn push_without_remote_errors() {
    let dir = tmp();
    assert!(init_git_commit(dir.path(), &[("a.txt", "a\n")]));
    assert_eq!(push(dir.path()).unwrap_err(), "No git remote to push to");
}

#[test]
fn sync_pulls_then_pushes() {
    let (origin, a, b) = (tmp(), tmp(), tmp());
    let (a, b): (PathBuf, PathBuf) = (a.path().into(), b.path().into());
    assert!(init_git_commit(&a, &[("a.txt", "alpha\n")]));
    with_origin(&a, origin.path());
    let url = origin.path().to_string_lossy().into_owned();
    assert!(git(
        origin.path(),
        &["symbolic-ref", "HEAD", "refs/heads/main"]
    ));
    assert!(git(
        &b,
        &[
            "clone",
            "-c",
            "core.autocrlf=false",
            "-b",
            "main",
            &url,
            "."
        ]
    ));
    for args in [
        ["config", "user.name", "MonoCode"],
        ["config", "user.email", "monocode@test"],
        ["config", "commit.gpgsign", "false"],
    ] {
        assert!(git(&b, &args));
    }

    std::fs::write(a.join("a.txt"), "beta\n").unwrap();
    stage_file(&a, "a.txt").unwrap();
    commit(&a, "from-a", false).unwrap();
    sync(&a).unwrap();
    pull(&b).unwrap();
    assert_eq!(std::fs::read_to_string(b.join("a.txt")).unwrap(), "beta\n");
    assert_eq!((index(&b).ahead, index(&b).behind), (0, 0));

    std::fs::write(b.join("b.txt"), "from-b\n").unwrap();
    stage_file(&b, "b.txt").unwrap();
    commit(&b, "from-b", false).unwrap();
    std::fs::write(a.join("c.txt"), "from-a-again\n").unwrap();
    stage_file(&a, "c.txt").unwrap();
    commit(&a, "from-a-again", false).unwrap();
    sync(&a).unwrap();
    sync(&b).unwrap();
    assert!(b.join("c.txt").exists());
    sync(&a).unwrap();
    assert!(a.join("b.txt").exists());
}

#[test]
fn parse_gh_pr_list_prefers_open() {
    let json = r#"[{"number":2,"title":"Old","url":"https://example.com/2","state":"MERGED"},{"number":3,"title":"Now","url":"https://example.com/3","state":"OPEN"}]"#;
    let pr = parse_gh_pr_list(json).unwrap();
    assert_eq!(pr.number, 3);
    assert_eq!(pr.state, "open");
    let pr = parse_gh_pr_list(
        r#"[{"number":2,"title":"Old","url":"u","state":"MERGED"},{"number":1,"title":"Older","url":"u","state":"CLOSED"}]"#,
    )
    .unwrap();
    assert_eq!((pr.number, pr.state.as_str()), (2, "merged"));
    assert_eq!(parse_gh_pr_list("[]"), None);
}

#[test]
fn pr_head_filter_qualifies_branch_with_repo_owner() {
    assert_eq!(
        github_pr_head_filter("acme/widget", "feature/x").as_deref(),
        Some("acme:feature/x")
    );
    assert_eq!(github_pr_head_filter("bad", "feature"), None);
}

#[test]
fn split_github_repo_reads_owner_and_name() {
    assert_eq!(
        split_github_repo(" acme/widget ").unwrap(),
        ("acme".into(), "widget".into())
    );
    assert!(split_github_repo("acme").is_err());
    assert!(split_github_repo("acme/a/b").is_err());
    assert!(split_github_repo("ac me/widget").is_err());
}

#[test]
fn pr_create_rejects_empty_title() {
    let dir = tmp();
    assert_eq!(
        pr_create(dir.path(), "  ", "", "main", "feature").unwrap_err(),
        "Pull request title cannot be empty"
    );
}

fn git_cmd_path(cmd: &Command) -> Vec<PathBuf> {
    let path = cmd
        .get_envs()
        .find(|(key, _)| *key == std::ffi::OsStr::new("PATH"))
        .and_then(|(_, value)| value)
        .expect("git should get an explicit PATH");
    std::env::split_paths(path).collect()
}

#[test]
fn read_only_git_cmd_path_adds_fixed_dirs_to_inherited_path() {
    let path = git_cmd_path(&git_cmd());
    assert!(path.contains(&PathBuf::from("/opt/homebrew/bin")));
    assert!(path.contains(&PathBuf::from("/usr/local/bin")));
    if let Some(inherited) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&inherited) {
            assert!(path.contains(&dir), "missing inherited {dir:?}");
        }
    }
}

#[test]
fn local_git_never_resolves_login_shell_path() {
    for action in ["status", "diff", "add", "restore", "cat-file"] {
        let cmd = git_cmd_for_args_with_path(&[action], || {
            panic!("local git must not resolve the login-shell PATH")
        });
        assert!(git_cmd_path(&cmd).contains(&PathBuf::from("/opt/homebrew/bin")));
    }
}

#[test]
fn git_actions_that_need_helpers_use_login_shell_path() {
    for args in [
        &["commit"][..],
        &["push"],
        &["pull"],
        &["fetch"],
        &["clone"],
        &["--literal-pathspecs", "commit"],
    ] {
        let cmd = git_cmd_for_args_with_path(args, || "gui-git-path".into());
        assert!(cmd.get_envs().any(|(key, value)| {
            key == std::ffi::OsStr::new("PATH")
                && value == Some(std::ffi::OsStr::new("gui-git-path"))
        }));
    }
}

#[test]
fn file_diff_reads_each_side_and_flags_binary() {
    let dir = tmp();
    assert!(init_git_commit(
        dir.path(),
        &[("a.txt", "one\ntwo\n"), ("gone.txt", "x\n")]
    ));
    std::fs::write(dir.path().join("a.txt"), "one\nTWO\n").unwrap();
    std::fs::remove_file(dir.path().join("gone.txt")).unwrap();
    std::fs::write(dir.path().join("new.txt"), "fresh\n").unwrap();
    std::fs::write(dir.path().join("bin.dat"), [0u8, 1, 2]).unwrap();
    let unstaged = file_diff(dir.path(), "a.txt", false).unwrap();
    assert_eq!(
        (unstaged.original.as_str(), unstaged.current.as_str()),
        ("one\ntwo\n", "one\nTWO\n")
    );
    assert_eq!(unstaged.status, "modified");
    assert_eq!(
        file_diff(dir.path(), "gone.txt", false).unwrap().status,
        "deleted"
    );
    let fresh = file_diff(dir.path(), "new.txt", false).unwrap();
    assert_eq!(
        (fresh.status.as_str(), fresh.original.as_str()),
        ("untracked", "")
    );
    assert!(file_diff(dir.path(), "bin.dat", false).unwrap().binary);
    assert!(file_diff(dir.path(), "../escape", false).is_err());

    assert!(git(dir.path(), &["add", "a.txt"]));
    std::fs::write(dir.path().join("a.txt"), "one\nTWO\nthree\n").unwrap();
    let staged = file_diff(dir.path(), "a.txt", true).unwrap();
    assert_eq!(
        (staged.original.as_str(), staged.current.as_str()),
        ("one\ntwo\n", "one\nTWO\n")
    );
    assert_eq!(
        file_diff(dir.path(), "a.txt", false).unwrap().original,
        "one\nTWO\n"
    );
}

#[test]
fn stage_contents_stages_a_partial_file_only() {
    let dir = tmp();
    assert!(init_git_commit(dir.path(), &[("a.txt", "one\ntwo\n")]));
    std::fs::write(dir.path().join("a.txt"), "ONE\nTWO\n").unwrap();
    stage_contents(dir.path(), "a.txt", b"ONE\ntwo\n").unwrap();
    assert_eq!(
        file_diff(dir.path(), "a.txt", true).unwrap().current,
        "ONE\ntwo\n"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
        "ONE\nTWO\n"
    );
    let file = file(dir.path(), "a.txt");
    assert!(file.staged && file.unstaged);
}

#[test]
fn commit_files_and_diffs_include_the_root_commit() {
    let dir = tmp();
    assert!(init_git_commit(dir.path(), &[("a.txt", "one\n")]));
    let root = git_stdout(dir.path(), &["rev-parse", "HEAD"]).unwrap();
    std::fs::write(dir.path().join("a.txt"), "one\ntwo\n").unwrap();
    std::fs::write(dir.path().join("b.txt"), "bee\n").unwrap();
    assert!(git(dir.path(), &["add", "."]) && git(dir.path(), &["commit", "-m", "second"]));
    let head = git_stdout(dir.path(), &["rev-parse", "HEAD"]).unwrap();
    let files = commit_files(dir.path(), &head).unwrap();
    let summary: Vec<_> = files
        .iter()
        .map(|f| {
            (
                f.relative.as_str(),
                f.status.as_str(),
                f.additions,
                f.deletions,
            )
        })
        .collect();
    assert_eq!(
        summary,
        [("a.txt", "modified", 1, 0), ("b.txt", "added", 1, 0)]
    );
    let diff = commit_file_diff(dir.path(), &head, "a.txt").unwrap();
    assert_eq!(
        (diff.original.as_str(), diff.current.as_str()),
        ("one\n", "one\ntwo\n")
    );
    let first = commit_file_diff(dir.path(), &root, "a.txt").unwrap();
    assert_eq!(
        (first.status.as_str(), first.original.as_str()),
        ("added", "")
    );
    assert_eq!(commit_files(dir.path(), &root).unwrap().len(), 1);
    assert!(commit_files(dir.path(), "not-a-sha").is_err());
}

#[test]
fn commit_paths_commits_only_the_chosen_files_and_keeps_other_staged_entries() {
    let dir = tmp();
    assert!(init_git_commit(
        dir.path(),
        &[("a.txt", "a\n"), ("b.txt", "b\n"), ("gone.txt", "g\n")]
    ));
    std::fs::write(dir.path().join("a.txt"), "a2\n").unwrap();
    std::fs::write(dir.path().join("b.txt"), "b2\n").unwrap();
    std::fs::write(dir.path().join("new.txt"), "n\n").unwrap();
    std::fs::write(dir.path().join("other.txt"), "o\n").unwrap();
    std::fs::remove_file(dir.path().join("gone.txt")).unwrap();
    assert!(git(dir.path(), &["add", "b.txt"]));

    commit_paths(
        dir.path(),
        "pick",
        &["a.txt".into(), "new.txt".into(), "gone.txt".into()],
        false,
    )
    .unwrap();

    let names = git_stdout(dir.path(), &["show", "--name-status", "--format=", "HEAD"]).unwrap();
    let mut lines: Vec<_> = names.lines().collect();
    lines.sort();
    assert_eq!(lines, ["A\tnew.txt", "D\tgone.txt", "M\ta.txt"]);
    // b.txt stays staged, other.txt stays untracked.
    let status = git_stdout(dir.path(), &["status", "--porcelain"]).unwrap();
    assert!(status.contains("M  b.txt"), "{status}");
    assert!(status.contains("?? other.txt"), "{status}");
    assert!(commit_paths(dir.path(), " ", &["b.txt".into()], false).is_err());
    assert!(commit_paths(dir.path(), "m", &[], false).is_err());
    assert!(commit_paths(dir.path(), "m", &["../x".into()], false).is_err());
}

#[test]
fn commit_paths_amend_without_files_rewrites_only_the_message() {
    let dir = tmp();
    assert!(init_git_commit(dir.path(), &[("a.txt", "a\n")]));
    std::fs::write(dir.path().join("a.txt"), "a2\n").unwrap();
    commit_paths(dir.path(), "reworded", &[], true).unwrap();
    assert_eq!(head_message(dir.path()).unwrap().trim(), "reworded");
    let status = git_stdout(dir.path(), &["status", "--porcelain"]).unwrap();
    assert!(status.contains("M a.txt"), "{status}");
}

#[test]
fn file_diff_head_compares_the_working_tree_with_head_whatever_is_staged() {
    let dir = tmp();
    assert!(init_git_commit(dir.path(), &[("a.txt", "one\n")]));
    std::fs::write(dir.path().join("a.txt"), "two\n").unwrap();
    assert!(git(dir.path(), &["add", "a.txt"]));
    std::fs::write(dir.path().join("a.txt"), "three\n").unwrap();
    std::fs::write(dir.path().join("new.txt"), "n\n").unwrap();
    let diff = file_diff_head(dir.path(), "a.txt").unwrap();
    assert_eq!(
        (diff.original.as_str(), diff.current.as_str()),
        ("one\n", "three\n")
    );
    assert_eq!(
        file_diff_head(dir.path(), "new.txt").unwrap().status,
        "added"
    );
}

#[test]
fn selected_context_describes_only_the_chosen_files_and_leaves_the_index_alone() {
    let dir = tmp();
    assert!(init_git_commit(
        dir.path(),
        &[("a.txt", "a\n"), ("b.txt", "b\n")]
    ));
    std::fs::write(dir.path().join("a.txt"), "a2\n").unwrap();
    std::fs::write(dir.path().join("b.txt"), "b2\n").unwrap();
    std::fs::write(dir.path().join("new.txt"), "n\n").unwrap();
    let context = selected_context(dir.path(), &["a.txt".into(), "new.txt".into()]).unwrap();
    assert!(context.patch.contains("a2") && context.patch.contains("+n"));
    assert!(!context.patch.contains("b2"));
    assert!(context.summary.contains("new.txt") && !context.summary.contains("b.txt"));
    assert_eq!(
        git_stdout(dir.path(), &["diff", "--cached", "--name-only"]).unwrap_or_default(),
        ""
    );
    assert!(selected_context(dir.path(), &[]).is_err());
}

/// A bare origin with two clones, `a` and `b`, both on main at the same commit.
fn two_clones() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let root = tmp();
    let origin = root.path().join("origin.git");
    let (a, b) = (root.path().join("a"), root.path().join("b"));
    std::fs::create_dir_all(&origin).unwrap();
    assert!(init_bare(&origin));
    assert!(git(&origin, &["symbolic-ref", "HEAD", "refs/heads/main"]));
    std::fs::create_dir_all(&a).unwrap();
    assert!(init_git_commit(
        &a,
        &[("shared.txt", "one\n"), ("keep.txt", "k\n")]
    ));
    assert!(git(
        &a,
        &["remote", "add", "origin", origin.to_str().unwrap()]
    ));
    assert!(git(&a, &["push", "-u", "origin", "main"]));
    assert!(git(root.path(), &["clone", origin.to_str().unwrap(), "b"]));
    assert!(git(&b, &["config", "user.name", "MonoCode"]));
    assert!(git(&b, &["config", "user.email", "monocode@test"]));
    assert!(git(&b, &["config", "commit.gpgsign", "false"]));
    (root, a, b)
}

#[test]
fn update_project_fetches_merges_and_reports_commits_and_files() {
    let (_root, a, b) = two_clones();
    std::fs::write(a.join("shared.txt"), "one\ntwo\n").unwrap();
    std::fs::write(a.join("added.txt"), "new\n").unwrap();
    assert!(git(&a, &["add", "."]) && git(&a, &["commit", "-m", "first"]));
    std::fs::write(a.join("keep.txt"), "k2\n").unwrap();
    assert!(git(&a, &["commit", "-am", "second"]) && git(&a, &["push"]));

    let report = update_project(&b, UpdateMethod::Merge).unwrap();
    assert_eq!(
        report
            .commits
            .iter()
            .map(|c| c.subject.as_str())
            .collect::<Vec<_>>(),
        ["second", "first"]
    );
    assert_ne!(report.old, report.new);
    let files: Vec<_> = report
        .files
        .iter()
        .map(|f| (f.relative.as_str(), f.status.as_str()))
        .collect();
    assert_eq!(
        files,
        [
            ("added.txt", "added"),
            ("keep.txt", "modified"),
            ("shared.txt", "modified")
        ]
    );
    assert!(report.conflicts.is_empty());
    assert_eq!(
        std::fs::read_to_string(b.join("shared.txt")).unwrap(),
        "one\ntwo\n"
    );

    let diff = range_file_diff(&b, &report.old, &report.new, "shared.txt").unwrap();
    assert_eq!(
        (diff.original.as_str(), diff.current.as_str()),
        ("one\n", "one\ntwo\n")
    );

    // Nothing more to bring in.
    let again = update_project(&b, UpdateMethod::Merge).unwrap();
    assert!(again.commits.is_empty() && again.files.is_empty() && again.old == again.new);
}

#[test]
fn update_project_rebases_local_commits_onto_the_upstream() {
    let (_root, a, b) = two_clones();
    std::fs::write(a.join("keep.txt"), "remote\n").unwrap();
    assert!(git(&a, &["commit", "-am", "remote change"]) && git(&a, &["push"]));
    std::fs::write(b.join("local.txt"), "l\n").unwrap();
    assert!(git(&b, &["add", "."]) && git(&b, &["commit", "-m", "local change"]));
    let report = update_project(&b, UpdateMethod::Rebase).unwrap();
    assert!(report.rebase && report.conflicts.is_empty());
    let log = git_stdout(&b, &["log", "--format=%s", "-3"]).unwrap();
    assert_eq!(
        log.lines().collect::<Vec<_>>(),
        ["local change", "remote change", "init"]
    );
    assert_eq!(
        git_stdout(&b, &["rev-list", "--merges", "--count", "HEAD"]).unwrap(),
        "0"
    );
}

#[test]
fn update_project_stops_on_conflicts_and_can_be_aborted() {
    let (_root, a, b) = two_clones();
    std::fs::write(a.join("shared.txt"), "from a\n").unwrap();
    assert!(git(&a, &["commit", "-am", "a edit"]) && git(&a, &["push"]));
    std::fs::write(b.join("shared.txt"), "from b\n").unwrap();
    assert!(git(&b, &["commit", "-am", "b edit"]));
    let before = git_stdout(&b, &["rev-parse", "HEAD"]).unwrap();
    let report = update_project(&b, UpdateMethod::Merge).unwrap();
    assert_eq!(report.conflicts, ["shared.txt"]);
    assert_eq!(report.old, report.new);
    abort_update(&b).unwrap();
    assert_eq!(git_stdout(&b, &["rev-parse", "HEAD"]).unwrap(), before);
    assert!(git_stdout(&b, &["status", "--porcelain"])
        .unwrap_or_default()
        .is_empty());
    assert!(abort_update(&b).is_err());
}

#[test]
fn update_project_needs_an_upstream() {
    let dir = tmp();
    assert!(init_git_commit(dir.path(), &[("a.txt", "a\n")]));
    assert!(update_project(dir.path(), UpdateMethod::Merge)
        .unwrap_err()
        .contains("upstream"));
}

#[test]
fn merge_branch_brings_in_another_branch_and_reports_it() {
    let dir = tmp();
    assert!(init_git_commit(dir.path(), &[("a.txt", "a\n")]));
    assert!(git(dir.path(), &["switch", "-c", "feature"]));
    std::fs::write(dir.path().join("f.txt"), "f\n").unwrap();
    assert!(git(dir.path(), &["add", "."]) && git(dir.path(), &["commit", "-m", "feature work"]));
    assert!(git(dir.path(), &["switch", "main"]));
    let report = merge_branch(dir.path(), "feature").unwrap();
    assert_eq!(report.upstream, "feature");
    assert_eq!(report.commits.len(), 1);
    assert_eq!(report.files.len(), 1);
    assert_eq!(report.files[0].relative, "f.txt");
    assert!(dir.path().join("f.txt").exists());
    // Merging again changes nothing.
    assert!(merge_branch(dir.path(), "feature")
        .unwrap()
        .commits
        .is_empty());
    assert!(merge_branch(dir.path(), "nope").is_err());
    assert!(merge_branch(dir.path(), "--abort").is_err());
}

#[test]
fn fetch_downloads_remote_commits_without_moving_the_branch() {
    let (_root, a, b) = two_clones();
    std::fs::write(a.join("keep.txt"), "remote\n").unwrap();
    assert!(git(&a, &["commit", "-am", "remote change"]) && git(&a, &["push"]));
    let before = git_stdout(&b, &["rev-parse", "HEAD"]).unwrap();
    fetch(&b).unwrap();
    assert_eq!(git_stdout(&b, &["rev-parse", "HEAD"]).unwrap(), before);
    assert_eq!(index(&b).behind, 1);
    assert!(fetch(tmp().path()).is_err());
}

#[test]
fn rebase_branch_replays_the_current_branch_onto_another() {
    let dir = tmp();
    assert!(init_git_commit(dir.path(), &[("a.txt", "a\n")]));
    assert!(git(dir.path(), &["switch", "-c", "base"]));
    std::fs::write(dir.path().join("b.txt"), "b\n").unwrap();
    assert!(git(dir.path(), &["add", "."]) && git(dir.path(), &["commit", "-m", "on base"]));
    assert!(git(dir.path(), &["switch", "-c", "topic", "main"]));
    std::fs::write(dir.path().join("t.txt"), "t\n").unwrap();
    assert!(git(dir.path(), &["add", "."]) && git(dir.path(), &["commit", "-m", "on topic"]));
    let report = rebase_branch(dir.path(), "base").unwrap();
    assert!(report.rebase && report.conflicts.is_empty());
    assert_eq!(report.commits.len(), 1);
    let log = git_stdout(dir.path(), &["log", "--format=%s", "-3"]).unwrap();
    assert_eq!(
        log.lines().collect::<Vec<_>>(),
        ["on topic", "on base", "init"]
    );
    assert!(rebase_branch(dir.path(), "--abort").is_err());
}

#[test]
fn outgoing_lists_the_commits_and_files_a_push_would_send() {
    let (_root, _a, b) = two_clones();
    std::fs::write(b.join("keep.txt"), "k2\n").unwrap();
    assert!(git(&b, &["commit", "-am", "edit keep"]));
    std::fs::write(b.join("new.txt"), "n\n").unwrap();
    assert!(git(&b, &["add", "."]) && git(&b, &["commit", "-m", "add new"]));
    let report = outgoing(&b).unwrap();
    assert_eq!(report.upstream, "origin/main");
    assert!(!report.no_upstream && report.behind == 0);
    assert_eq!(
        report
            .commits
            .iter()
            .map(|c| c.subject.as_str())
            .collect::<Vec<_>>(),
        ["add new", "edit keep"]
    );
    let files: Vec<_> = report
        .files
        .iter()
        .map(|f| (f.relative.as_str(), f.status.as_str()))
        .collect();
    assert_eq!(files, [("keep.txt", "modified"), ("new.txt", "added")]);
    let diff = range_file_diff(&b, &report.old, &report.new, "keep.txt").unwrap();
    assert_eq!(
        (diff.original.as_str(), diff.current.as_str()),
        ("k\n", "k2\n")
    );
    assert!(git(&b, &["push"]));
    assert!(outgoing(&b).unwrap().commits.is_empty());
}

#[test]
fn outgoing_for_a_new_branch_counts_commits_no_remote_has() {
    let (_root, _a, b) = two_clones();
    assert!(git(&b, &["switch", "-c", "topic"]));
    std::fs::write(b.join("t.txt"), "t\n").unwrap();
    assert!(git(&b, &["add", "."]) && git(&b, &["commit", "-m", "topic work"]));
    let report = outgoing(&b).unwrap();
    assert!(report.no_upstream);
    assert_eq!(report.upstream, "origin (new branch)");
    assert_eq!(report.commits.len(), 1);
    assert_eq!(report.files.len(), 1);
    assert_eq!(report.files[0].relative, "t.txt");
}

#[test]
fn outgoing_notes_how_far_the_branch_is_behind() {
    let (_root, a, b) = two_clones();
    std::fs::write(a.join("shared.txt"), "from a\n").unwrap();
    assert!(git(&a, &["commit", "-am", "a edit"]) && git(&a, &["push"]));
    std::fs::write(b.join("keep.txt"), "k2\n").unwrap();
    assert!(git(&b, &["commit", "-am", "b edit"]));
    assert!(git(&b, &["fetch"]));
    let report = outgoing(&b).unwrap();
    assert_eq!((report.commits.len(), report.behind), (1, 1));
}

#[test]
fn rollback_drops_staged_and_unstaged_changes_and_restores_deleted_files() {
    let dir = tmp();
    let dir = dir.path();
    assert!(init_git_commit(
        dir,
        &[
            ("a.txt", "alpha\n"),
            ("b.txt", "bravo\n"),
            ("c.txt", "charlie\n")
        ]
    ));
    // a: staged edit then another unstaged edit; b: deleted; c: untouched.
    std::fs::write(dir.join("a.txt"), "staged\n").unwrap();
    stage_file(dir, "a.txt").unwrap();
    std::fs::write(dir.join("a.txt"), "unstaged\n").unwrap();
    std::fs::remove_file(dir.join("b.txt")).unwrap();
    rollback_files(dir, &["a.txt".into(), "b.txt".into()], false).unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.join("a.txt")).unwrap(),
        "alpha\n"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("b.txt")).unwrap(),
        "bravo\n"
    );
    assert!(index(dir).files.is_empty(), "nothing is left to commit");
}

#[test]
fn rollback_unstages_an_added_file_and_keeps_or_deletes_it_as_asked() {
    let dir = tmp();
    let dir = dir.path();
    assert!(init_git_commit(dir, &[("a.txt", "alpha\n")]));
    std::fs::write(dir.join("new.txt"), "hello\n").unwrap();
    std::fs::write(dir.join("gone.txt"), "bye\n").unwrap();
    stage_file(dir, "new.txt").unwrap();
    stage_file(dir, "gone.txt").unwrap();
    assert_eq!(file(dir, "new.txt").status, "added");
    rollback_files(dir, &["new.txt".into()], false).unwrap();
    assert!(dir.join("new.txt").exists(), "kept on disk");
    assert_eq!(file(dir, "new.txt").status, "untracked");
    rollback_files(dir, &["gone.txt".into()], true).unwrap();
    assert!(!dir.join("gone.txt").exists(), "deleted on request");
    assert!(index(dir).files.iter().all(|f| f.relative != "gone.txt"));
}

#[test]
fn rollback_leaves_unversioned_files_and_odd_names_alone() {
    let dir = tmp();
    let dir = dir.path();
    assert!(init_git_commit(
        dir,
        &[("a[1].txt", "alpha\n"), ("b.txt", "bravo\n")]
    ));
    std::fs::write(dir.join("a[1].txt"), "changed\n").unwrap();
    std::fs::write(dir.join("b.txt"), "changed\n").unwrap();
    std::fs::write(dir.join("loose.txt"), "x\n").unwrap();
    // The brackets are part of the name, not a pattern: only that file goes back.
    rollback_files(dir, &["a[1].txt".into(), "loose.txt".into()], false).unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.join("a[1].txt")).unwrap(),
        "alpha\n"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("b.txt")).unwrap(),
        "changed\n"
    );
    assert!(
        dir.join("loose.txt").exists(),
        "an unversioned file is not rolled back"
    );
    assert!(rollback_files(dir, &["../outside".into()], false).is_err());
}

#[test]
fn stage_files_adds_unversioned_files_in_one_go_and_takes_names_literally() {
    let dir = tmp();
    let dir = dir.path();
    assert!(init_git_commit(dir, &[("a.txt", "alpha\n")]));
    std::fs::create_dir(dir.join("sub")).unwrap();
    std::fs::write(dir.join("sub/new.txt"), "n\n").unwrap();
    std::fs::write(dir.join("b[1].txt"), "b\n").unwrap();
    std::fs::write(dir.join("b1.txt"), "other\n").unwrap();
    stage_files(dir, &["sub/new.txt".into(), "b[1].txt".into()]).unwrap();
    assert_eq!(file(dir, "sub/new.txt").status, "added");
    assert_eq!(file(dir, "b[1].txt").status, "added");
    assert_eq!(
        file(dir, "b1.txt").status,
        "untracked",
        "the brackets are a name, not a pattern"
    );
    assert!(stage_files(dir, &[]).is_ok());
    assert!(stage_files(dir, &["../outside".into()]).is_err());
}

fn repo_with_changes() -> tempfile::TempDir {
    let dir = tmp();
    assert!(init_git_commit(
        dir.path(),
        &[("a.txt", "one\n"), ("b.txt", "two\n")]
    ));
    std::fs::write(dir.path().join("a.txt"), "one\nchanged\n").unwrap();
    dir
}

#[test]
fn stash_push_shelves_changes_and_lists_them_newest_first() {
    let dir = repo_with_changes();
    let root = dir.path();
    stash_push(root, "first idea", false, false).unwrap();
    assert!(index(root).files.is_empty(), "the tree is clean again");
    std::fs::write(root.join("b.txt"), "two\nmore\n").unwrap();
    stash_push(root, "", false, false).unwrap();
    let list = stash_list(root).unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].index, 0);
    assert_eq!(list[0].branch, "main");
    assert!(list[0].message.starts_with("WIP: "), "{:?}", list[0]);
    assert_eq!(list[1].message, "first idea");
    assert!(list[0].timestamp > 0 && list[0].sha != list[1].sha);
    assert_eq!(
        stash_push(root, "nothing", false, false).unwrap_err(),
        "There are no changes to stash"
    );
}

#[test]
fn stash_can_take_untracked_files_and_keep_what_is_staged() {
    let dir = repo_with_changes();
    let root = dir.path();
    std::fs::write(root.join("new.txt"), "fresh\n").unwrap();
    stash_push(root, "with new", true, false).unwrap();
    assert!(
        !root.join("new.txt").exists(),
        "untracked files were shelved"
    );
    let entry = &stash_list(root).unwrap()[0];
    let files = stash_files(root, &entry.sha).unwrap();
    let names: Vec<_> = files.iter().map(|f| f.relative.as_str()).collect();
    assert_eq!(names, ["a.txt", "new.txt"]);
    assert_eq!(files[0].additions, 1);
    let diff = stash_file_diff(root, &entry.sha, "new.txt").unwrap();
    assert_eq!(diff.current, "fresh\n");
    assert_eq!(diff.original, "");
    let diff = stash_file_diff(root, &entry.sha, "a.txt").unwrap();
    assert_eq!(
        (diff.original.as_str(), diff.current.as_str()),
        ("one\n", "one\nchanged\n")
    );
    // Without the flag an untracked file stays where it is.
    std::fs::write(root.join("a.txt"), "one\nagain\n").unwrap();
    std::fs::write(root.join("loose.txt"), "x\n").unwrap();
    stash_push(root, "tracked only", false, false).unwrap();
    assert!(root.join("loose.txt").exists());
    // Keeping the index leaves the staged change in the tree.
    std::fs::write(root.join("a.txt"), "one\nstaged\n").unwrap();
    assert!(git(root, &["add", "a.txt"]));
    stash_push(root, "keep", false, true).unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("a.txt")).unwrap(),
        "one\nstaged\n"
    );
}

#[test]
fn applying_keeps_the_entry_and_popping_drops_it() {
    let dir = repo_with_changes();
    let root = dir.path();
    stash_push(root, "work", false, false).unwrap();
    let sha = stash_list(root).unwrap()[0].sha.clone();
    let outcome = stash_apply(root, &sha, false, false).unwrap();
    assert_eq!(outcome, StashOutcome::default());
    assert_eq!(
        std::fs::read_to_string(root.join("a.txt")).unwrap(),
        "one\nchanged\n"
    );
    assert_eq!(stash_list(root).unwrap().len(), 1, "apply keeps it");
    assert!(git(root, &["checkout", "--", "a.txt"]));
    let outcome = stash_apply(root, &sha, true, false).unwrap();
    assert!(outcome.dropped && outcome.conflicts.is_empty());
    assert!(stash_list(root).unwrap().is_empty(), "pop drops it");
    assert_eq!(
        stash_apply(root, &sha, false, false).unwrap_err(),
        "That stash no longer exists"
    );
}

#[test]
fn a_conflicting_pop_reports_the_files_and_keeps_the_entry() {
    let dir = repo_with_changes();
    let root = dir.path();
    stash_push(root, "mine", false, false).unwrap();
    std::fs::write(root.join("a.txt"), "one\ntheirs\n").unwrap();
    assert!(git(root, &["commit", "-am", "other change"]));
    let sha = stash_list(root).unwrap()[0].sha.clone();
    let outcome = stash_apply(root, &sha, true, false).unwrap();
    assert_eq!(outcome.conflicts, ["a.txt"]);
    assert!(!outcome.dropped);
    assert_eq!(stash_list(root).unwrap().len(), 1, "kept for another try");
    // Local edits in the way are an error, with git's own words.
    assert!(git(root, &["checkout", "-f", "HEAD"]));
    std::fs::write(root.join("a.txt"), "local edit\n").unwrap();
    let error = stash_apply(root, &sha, false, false).unwrap_err();
    assert!(
        error.contains("overwritten") || error.contains("conflict"),
        "{error}"
    );
}

#[test]
fn dropping_clearing_and_branching_from_entries() {
    let dir = repo_with_changes();
    let root = dir.path();
    stash_push(root, "one", false, false).unwrap();
    std::fs::write(root.join("b.txt"), "two\nb\n").unwrap();
    stash_push(root, "two", false, false).unwrap();
    std::fs::write(root.join("a.txt"), "one\nthree\n").unwrap();
    stash_push(root, "three", false, false).unwrap();
    let list = stash_list(root).unwrap();
    // Entries are found by commit, so dropping the middle one is exact.
    stash_drop(root, &list[1].sha).unwrap();
    let left: Vec<_> = stash_list(root)
        .unwrap()
        .into_iter()
        .map(|e| e.message)
        .collect();
    assert_eq!(left, ["three", "one"]);
    let newest = stash_list(root).unwrap()[0].sha.clone();
    assert!(stash_branch(root, &newest, "").is_err());
    assert!(stash_branch(root, &newest, "-bad").is_err());
    stash_branch(root, &newest, "from-stash").unwrap();
    assert_eq!(
        git_stdout(root, &["rev-parse", "--abbrev-ref", "HEAD"]).as_deref(),
        Some("from-stash")
    );
    assert_eq!(
        stash_list(root).unwrap().len(),
        1,
        "branching drops its entry"
    );
    stash_clear(root).unwrap();
    assert!(stash_list(root).unwrap().is_empty());
    assert!(stash_apply(root, "zz", false, false).is_err());
}

#[test]
fn a_files_history_follows_it_through_a_rename_and_starts_with_what_is_uncommitted() {
    let dir = tmp();
    assert!(init_git(dir.path(), "main"));
    let write = |name: &str, text: &str| std::fs::write(dir.path().join(name), text).unwrap();
    let commit = |message: &str| {
        assert!(git(dir.path(), &["add", "-A"]));
        assert!(git(dir.path(), &["commit", "-m", message]));
    };
    write("old.txt", "one\ntwo\nthree\nfour\nfive\n");
    write("other.txt", "x\n");
    commit("add the file");
    write("other.txt", "y\n");
    commit("someone else's change");
    write("old.txt", "one\ntwo\nthree\nfour\nfive\nsix\n");
    commit("add a line");
    assert!(git(dir.path(), &["mv", "old.txt", "new name.txt"]));
    commit("rename it");
    write("new name.txt", "one\ntwo\nthree\nfour\nfive\nsix\nseven\n");

    // Asked for by its absolute path, as a file tab knows it.
    let absolute = dir.path().join("new name.txt");
    let history = file_history(dir.path(), &absolute.to_string_lossy(), 100).unwrap();
    let seen: Vec<_> = history
        .iter()
        .map(|v| (v.subject.as_str(), v.path.as_str(), v.status.as_str()))
        .collect();
    assert_eq!(
        seen,
        [
            ("Uncommitted changes", "new name.txt", "modified"),
            ("rename it", "new name.txt", "renamed"),
            ("add a line", "old.txt", "modified"),
            ("add the file", "old.txt", "added"),
        ],
        "the other file's commit is not part of it"
    );
    assert!(history[0].sha.is_empty());
    assert_eq!(history[1].previous_path.as_deref(), Some("old.txt"));
    assert_eq!(history[2].author, "MonoCode");
    assert!(history[2].time > 0 && history[2].short_sha.len() >= 7);

    let texts = |version: &FileVersion| {
        let diff = file_version_diff(dir.path(), version).unwrap();
        (diff.original, diff.current, diff.status)
    };
    // What is on disk against HEAD.
    let (before, after, _) = texts(&history[0]);
    assert!(!before.contains("seven") && after.ends_with("seven\n"));
    // The rename: the old name's text on the left, unchanged.
    let (before, after, status) = texts(&history[1]);
    assert_eq!(before, after);
    assert_eq!(status, "modified");
    let (before, after, _) = texts(&history[2]);
    assert!(!before.contains("six") && after.ends_with("six\n"));
    let (before, after, status) = texts(&history[3]);
    assert_eq!((before.as_str(), status.as_str()), ("", "added"));
    assert!(after.starts_with("one\n"));

    // Relative to the folder too, and only as many as asked for.
    let limited = file_history(dir.path(), "new name.txt", 1).unwrap();
    assert_eq!(
        limited.len(),
        2,
        "what is uncommitted, and the newest commit"
    );
    // Clean again: no uncommitted version.
    commit("add another line");
    let clean = file_history(dir.path(), "new name.txt", 100).unwrap();
    assert_eq!(clean[0].subject, "add another line");
    // A file git has never seen has only what is on disk.
    write("fresh.txt", "new\n");
    let fresh = file_history(dir.path(), "fresh.txt", 100).unwrap();
    assert_eq!(fresh.len(), 1);
    assert_eq!(
        (fresh[0].path.as_str(), fresh[0].status.as_str()),
        ("fresh.txt", "added")
    );
    assert_eq!(texts(&fresh[0]).1, "new\n");
}

#[cfg(unix)]
#[test]
fn metadata_stdout_drains_output_larger_than_a_pipe() {
    let (status, bytes) = metadata_stdout(
        Command::new("/bin/sh").args([
            "-c",
            "i=0; while [ $i -lt 12000 ]; do printf 'metadata line\\n'; i=$((i+1)); done",
        ]),
        Duration::from_secs(5),
    )
    .expect("metadata output should not block on a full pipe");
    assert!(status.success());
    assert_eq!(bytes.len(), 12000 * "metadata line\n".len());
}

#[cfg(unix)]
#[test]
fn metadata_stdout_kills_and_reaps_a_stalled_child() {
    let started = Instant::now();
    let output = metadata_stdout(
        Command::new("/bin/sh").args(["-c", "printf started; exec sleep 10"]),
        Duration::from_millis(50),
    );
    assert!(output.is_none());
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[cfg(unix)]
#[test]
fn upstream_lookup_keeps_a_timeout_apart_from_a_missing_upstream() {
    let lookup = |script: &str, timeout: Duration| {
        upstream_from_lookup(metadata_stdout(
            Command::new("/bin/sh").args(["-c", script]),
            timeout,
        ))
    };
    let limit = Duration::from_secs(5);
    assert_eq!(lookup("printf 'origin/main\\n'", limit), Ok(true));
    // What git prints nothing and exits 128 for: no upstream configured.
    assert_eq!(lookup("exit 128", limit), Ok(false));
    assert!(lookup("exec sleep 10", Duration::from_millis(50)).is_err());
}

mod pull_requests {
    use super::*;
    use crate::scm::pull_request::*;

    fn remote(host: &str, owner: &str, name: &str) -> Option<RemoteUrl> {
        Some(RemoteUrl {
            host: host.into(),
            owner: owner.into(),
            name: name.into(),
        })
    }

    #[test]
    fn remote_urls_parse_github_and_enterprise_hosts() {
        let gh = remote("github.com", "octo", "app");
        assert_eq!(parse_remote_url("https://github.com/octo/app.git"), gh);
        assert_eq!(parse_remote_url("https://github.com/octo/app/"), gh);
        assert_eq!(parse_remote_url("https://user:tok@github.com/octo/app"), gh);
        assert_eq!(parse_remote_url("git@github.com:octo/app.git"), gh);
        assert_eq!(
            parse_remote_url("ssh://git@ssh.github.com:443/octo/app.git"),
            gh
        );
        assert_eq!(parse_remote_url("git@GitHub.com:octo/app"), gh);
        let ghe = remote("ghe.corp.example", "team", "svc");
        assert_eq!(
            parse_remote_url("https://ghe.corp.example/team/svc.git"),
            ghe
        );
        assert_eq!(parse_remote_url("git@ghe.corp.example:team/svc.git"), ghe);
        assert_eq!(
            parse_remote_url("ssh://git@ghe.corp.example:2222/team/svc"),
            ghe
        );
        assert_eq!(
            parse_remote_url("https://ghe.corp.example:8443/team/svc"),
            ghe
        );
        // Parsed but not GitHub: callers ask gh about the host.
        assert_eq!(
            parse_remote_url("git@gitlab.com:group/project.git"),
            remote("gitlab.com", "group", "project")
        );
        assert_eq!(
            parse_remote_url("https://gitlab.com/group/sub/project.git"),
            None
        );
        assert_eq!(parse_remote_url("/tmp/origin.git"), None);
        assert_eq!(parse_remote_url("C:\\repos\\origin.git"), None);
        assert_eq!(parse_remote_url("file:///tmp/a/b"), None);
        assert_eq!(parse_remote_url("https://github.com/octo"), None);
    }

    #[test]
    fn gh_host_uses_hostname_for_api_and_gh_host_otherwise() {
        let (args, env) = gh_host_args("ghe.corp", &["api", "graphql", "--input", "f"]);
        assert_eq!(
            args,
            ["api", "--hostname", "ghe.corp", "graphql", "--input", "f"]
        );
        assert!(!env);
        let (args, env) = gh_host_args("ghe.corp", &["pr", "list"]);
        assert_eq!(args, ["pr", "list"]);
        assert!(env);
    }

    fn commit_file(dir: &Path, name: &str, contents: &str, message: &str) {
        std::fs::write(dir.join(name), contents).unwrap();
        assert!(git(dir, &["add", name]));
        assert!(git(dir, &["commit", "-m", message]));
    }

    #[test]
    fn range_lists_commits_files_and_merge_base() {
        let dir = tmp();
        let root = dir.path();
        assert!(init_git_commit(root, &[("a.txt", "a\n")]));
        let base = git_stdout(root, &["rev-parse", "HEAD"]).unwrap();
        assert!(git(root, &["checkout", "-q", "-b", "feature"]));
        commit_file(root, "b.txt", "b\n", "add b");
        commit_file(root, "a.txt", "a\nmore\n", "grow a");
        assert!(git(root, &["checkout", "-q", "main"]));
        commit_file(root, "c.txt", "c\n", "main moves on");

        let range = pr_range(root, "main", "feature").unwrap();
        assert_eq!(range.merge_base, base);
        assert_eq!(
            range.base_sha,
            git_stdout(root, &["rev-parse", "main"]).unwrap()
        );
        assert_eq!(
            range.head_sha,
            git_stdout(root, &["rev-parse", "feature"]).unwrap()
        );
        let subjects: Vec<&str> = range.commits.iter().map(|c| c.subject.as_str()).collect();
        assert_eq!(subjects, ["grow a", "add b"]);
        let files: Vec<(&str, &str, i64)> = range
            .files
            .iter()
            .map(|f| (f.relative.as_str(), f.status.as_str(), f.additions))
            .collect();
        assert_eq!(files, [("a.txt", "modified", 1), ("b.txt", "added", 1)]);
        let diff = pr_file_diff(root, &range.merge_base, &range.head_sha, "a.txt").unwrap();
        assert_eq!(diff.relative, "a.txt");

        assert!(pr_range(root, "feature", "feature")
            .unwrap()
            .commits
            .is_empty());
        assert!(pr_range(root, "main", "nope").is_err());
        assert!(pr_range(root, "--all", "main").is_err());
    }

    #[test]
    fn push_state_covers_all_four_states_and_push_creates_the_branch() {
        let (repo, origin) = (tmp(), tmp());
        let repo = repo.path();
        assert!(init_git_commit(repo, &[("a.txt", "a\n")]));
        with_origin(repo, origin.path());
        assert_eq!(
            pr_push_state(repo, "main", "origin", "main").unwrap(),
            PrPush::UpToDate
        );
        assert!(git(repo, &["checkout", "-q", "-b", "topic"]));
        commit_file(repo, "t.txt", "t\n", "topic");
        let target = ("origin".to_string(), "remote-topic".to_string());
        assert_eq!(
            pr_push_state(repo, "topic", "origin", "remote-topic").unwrap(),
            PrPush::Create {
                remote: target.0.clone(),
                branch: target.1.clone()
            }
        );

        pr_push(repo, "topic", "origin", "remote-topic").unwrap();
        assert!(git_ref_exists(origin.path(), "refs/heads/remote-topic"));
        assert_eq!(
            git_stdout(repo, &["config", "--get", "branch.topic.merge"]).as_deref(),
            Some("refs/heads/remote-topic")
        );
        assert_eq!(
            pr_push_state(repo, "topic", "origin", "remote-topic").unwrap(),
            PrPush::UpToDate
        );

        commit_file(repo, "t.txt", "t2\n", "ahead");
        assert_eq!(
            pr_push_state(repo, "topic", "origin", "remote-topic").unwrap(),
            PrPush::Sync {
                remote: target.0.clone(),
                branch: target.1.clone()
            }
        );
        assert!(git(repo, &["reset", "-q", "--hard", "HEAD~2"]));
        commit_file(repo, "other.txt", "o\n", "diverge");
        assert_eq!(
            pr_push_state(repo, "topic", "origin", "remote-topic").unwrap(),
            PrPush::Diverged {
                remote: target.0,
                branch: target.1
            }
        );
        assert!(
            pr_push(repo, "topic", "origin", "remote-topic").is_err(),
            "never forces"
        );
        assert!(pr_push_state(repo, "missing", "origin", "x").is_err());
    }

    #[test]
    fn branch_lists_put_current_and_default_first() {
        let (repo, origin) = (tmp(), tmp());
        let repo = repo.path();
        assert!(init_git_commit(repo, &[("a.txt", "a\n")]));
        with_origin(repo, origin.path());
        for branch in ["zeta", "alpha", "feature/x"] {
            assert!(git(repo, &["branch", branch]));
            assert!(git(repo, &["push", "-q", "origin", branch]));
        }
        assert!(git(repo, &["checkout", "-q", "zeta"]));
        assert_eq!(
            pr_local_branches(repo).unwrap(),
            ["zeta", "alpha", "feature/x", "main"]
        );
        let origin_repo = PrRepo {
            remote: "origin".into(),
            host: "github.com".into(),
            owner: "o".into(),
            name: "n".into(),
            default_branch: "main".into(),
            ..Default::default()
        };
        assert_eq!(
            pr_branches(repo, &origin_repo).unwrap(),
            ["main", "alpha", "feature/x", "zeta"]
        );
        pr_fetch(repo, &["origin".into(), "origin".into()]).unwrap();
        assert!(pr_fetch(repo, &["nope".into()]).is_err());
        assert!(pr_fetch(repo, &[]).is_ok());
    }

    #[test]
    fn graphql_bodies_parse_data_and_errors() {
        let gql = parse_graphql(r#"{"data":{"a":1},"errors":[{"message":"boom"}]}"#).unwrap();
        assert_eq!(gql.data["a"], 1);
        assert_eq!(gql.errors, ["boom"]);
        assert!(parse_graphql("gh: not json").is_none());
        assert!(parse_graphql(r#"{"message":"Bad credentials"}"#).is_none());
    }

    #[test]
    fn repos_query_and_answer_carry_fork_parent_and_templates() {
        let query = repos_query(2, true);
        assert!(query.starts_with("query($o0: String!, $n0: String!, $o1: String!, $n1: String!)"));
        assert!(query.contains("r1: repository(owner: $o1, name: $n1)"));
        assert!(query.contains("pullRequestTemplates"));
        assert!(!repos_query(1, false).contains("pullRequestTemplates"));

        let data = json!({
            "viewer": {"login": "me"},
            "r0": {
                "id": "R_fork", "name": "app", "owner": {"login": "me"},
                "nameWithOwner": "me/app", "defaultBranchRef": {"name": "main"},
                "viewerPermission": "ADMIN", "isFork": true,
                "pullRequestTemplates": [
                    {"filename": "PULL_REQUEST_TEMPLATE/bug.md", "body": "bug"},
                    {"filename": "pull_request_template.md", "body": "default"}
                ],
                "parent": {
                    "id": "R_up", "name": "app", "owner": {"login": "octo"},
                    "nameWithOwner": "octo/app", "defaultBranchRef": {"name": "develop"},
                    "viewerPermission": "READ", "isFork": false
                }
            },
            "r1": null
        });
        let (viewer, repos) = parse_repos(&data, 2);
        assert_eq!(viewer, "me");
        assert!(repos[1].is_none());
        let fork = repos[0].as_ref().unwrap();
        assert!(fork.is_fork);
        let parent = fork.parent.as_ref().unwrap();
        assert_eq!(parent.name_with_owner, "octo/app");
        assert_eq!(parent.default_branch_ref.as_ref().unwrap().name, "develop");
    }

    #[test]
    fn rest_pulls_and_created_pr_parse() {
        let pr = parse_rest_pulls(
            r#"[{"number":7,"title":"T","html_url":"https://ghe/x/y/pull/7","state":"OPEN"}]"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!((pr.number, pr.state.as_str()), (7, "open"));
        assert_eq!(parse_rest_pulls("[]").unwrap(), None);

        let base = PrRepo {
            host: "ghe.corp".into(),
            owner: "o".into(),
            name: "n".into(),
            ..Default::default()
        };
        let data = json!({"createPullRequest": {"pullRequest": {
            "id": "PR_1", "number": 3, "url": "https://ghe.corp/o/n/pull/3"}}});
        let created = parse_created_pr(&data, &base).unwrap();
        assert_eq!(created.node_id, "PR_1");
        assert_eq!(
            (created.repo.as_str(), created.host.as_str()),
            ("o/n", "ghe.corp")
        );
        assert!(parse_created_pr(&json!({"createPullRequest": null}), &base).is_err());
    }

    #[test]
    fn create_input_names_fork_heads_by_repository_or_owner() {
        let repo = |owner: &str| PrRepo {
            host: "github.com".into(),
            owner: owner.into(),
            name: "app".into(),
            ..Default::default()
        };
        let mut submit = PrSubmit {
            base: repo("octo"),
            base_branch: "main".into(),
            head: repo("octo"),
            head_branch: "topic".into(),
            title: " Title ".into(),
            body: "Body".into(),
            draft: true,
        };
        let input = create_pr_input(&submit, "R_base", Some("R_head"));
        assert_eq!(input["headRefName"], "topic");
        assert_eq!(input["title"], "Title");
        assert_eq!(input["draft"], true);
        assert!(input.get("headRepositoryId").is_none());

        submit.head = repo("me");
        let input = create_pr_input(&submit, "R_base", Some("R_head"));
        assert_eq!(input["headRefName"], "topic");
        assert_eq!(input["headRepositoryId"], "R_head");
        let input = create_pr_input(&submit, "R_base", None);
        assert_eq!(input["headRefName"], "me:topic");
        assert!(input.get("headRepositoryId").is_none());
    }

    #[test]
    fn ids_query_resolves_users_teams_and_labels() {
        let users = vec!["alice".to_string()];
        let teams = vec!["org/core".to_string()];
        let labels = vec!["bug".to_string(), "needs review".to_string()];
        let (query, variables) = ids_query("o", "n", &users, &teams, &labels)
            .unwrap()
            .unwrap();
        assert!(query.contains("u0: user(login: $u0)"));
        assert!(query.contains("t0: organization(login: $to0) { team(slug: $ts0)"));
        assert!(query.contains("l1: label(name: $l1)"));
        assert_eq!(variables["ts0"], "core");
        assert_eq!(variables["l1"], "needs review");
        assert!(ids_query("o", "n", &[], &[], &[]).unwrap().is_none());
        assert!(ids_query("o", "n", &[], &["core".into()], &[]).is_err());

        let data = json!({
            "u0": {"id": "U_a"},
            "t0": {"team": {"id": "T_c"}},
            "repository": {"l0": {"id": "L_b"}, "l1": null}
        });
        let ids = parse_ids(&data, &users, &teams, &labels);
        assert_eq!(ids.users["alice"], "U_a");
        assert_eq!(ids.teams["org/core"], "T_c");
        assert_eq!(ids.labels["bug"], "L_b");
        assert!(!ids.labels.contains_key("needs review"));
    }

    #[test]
    fn metadata_mutation_skips_empty_lists() {
        let none = MetadataIds::default();
        assert!(metadata_mutation("PR", &none, &none).is_none());
        let add = MetadataIds {
            teams: vec!["T".into()],
            labels: vec!["L".into()],
            ..Default::default()
        };
        let remove = MetadataIds {
            assignees: vec!["U".into()],
            ..Default::default()
        };
        let (query, variables) = metadata_mutation("PR", &add, &remove).unwrap();
        assert!(query.contains("requestReviews(input: { pullRequestId: $pr, union: true"));
        assert!(query.contains("addLabelsToLabelable"));
        assert!(query.contains("removeAssigneesFromAssignable"));
        assert!(!query.contains("addAssigneesToAssignable"));
        assert!(!query.contains("removeLabelsFromLabelable"));
        assert_eq!(variables["reviewers"], json!([]));
        assert_eq!(variables["teams"], json!(["T"]));
        assert_eq!(variables["pr"], "PR");
    }

    #[test]
    fn metadata_parses_and_diffs_case_insensitively() {
        let data = json!({"viewer": {"login": "me"}, "repository": {"pullRequest": {
            "id": "PR_9",
            "reviewRequests": {"nodes": [
                {"requestedReviewer": {"__typename": "User", "login": "Alice"}},
                {"requestedReviewer": {"__typename": "Team", "slug": "core",
                    "organization": {"login": "org"}}}
            ]},
            "assignees": {"nodes": [{"id": "U_b", "login": "bob"}]},
            "labels": {"nodes": [{"id": "L_bug", "name": "bug"}]}
        }}});
        let current = parse_pr_metadata(&data).unwrap();
        assert_eq!(current.pr_id, "PR_9");
        assert_eq!(current.viewer, "me");
        assert_eq!(
            current.metadata,
            PrMetadata {
                reviewers: vec!["Alice".into()],
                team_reviewers: vec!["org/core".into()],
                assignees: vec!["bob".into()],
                labels: vec!["bug".into()],
            }
        );
        assert_eq!(current.assignee_ids["bob"], "U_b");
        assert!(parse_pr_metadata(&json!({"repository": {"pullRequest": null}})).is_err());

        let wanted = PrMetadata {
            reviewers: vec!["alice".into(), "carol".into()],
            team_reviewers: vec![],
            assignees: vec!["bob".into()],
            labels: vec!["feature".into(), "feature".into()],
        };
        let (add, remove) = metadata_diff(&current.metadata, &wanted);
        assert_eq!(
            add,
            PrMetadata {
                reviewers: vec!["carol".into()],
                labels: vec!["feature".into()],
                ..Default::default()
            }
        );
        assert_eq!(
            remove,
            PrMetadata {
                team_reviewers: vec!["org/core".into()],
                labels: vec!["bug".into()],
                ..Default::default()
            }
        );
        assert_eq!(
            remove_reviewers_body(&remove),
            json!({"reviewers": [], "team_reviewers": ["core"]})
        );
    }

    #[test]
    fn default_first_sorts_and_dedups() {
        let branches = vec!["b".into(), "main".into(), "a".into(), "b".into()];
        assert_eq!(default_first(branches, "main"), ["main", "a", "b"]);
    }
}

#[test]
fn pr_parent_fetch_url_keeps_the_main_remote_transport() {
    use super::pull_request::swap_repo_in_url;
    let parent = super::PrRepo {
        host: "github.com".into(),
        owner: "up".into(),
        name: "proj".into(),
        ..Default::default()
    };
    assert_eq!(
        swap_repo_in_url("git@github.com:me/proj.git", &parent),
        "git@github.com:up/proj.git"
    );
    assert_eq!(
        swap_repo_in_url("https://github.com/me/proj", &parent),
        "https://github.com/up/proj.git"
    );
    assert_eq!(
        swap_repo_in_url("ssh://git@ghe.corp:2222/me/proj.git", &parent),
        "ssh://git@ghe.corp:2222/up/proj.git"
    );
}

#[test]
fn a_commit_says_how_it_reached_the_main_branch_and_a_merge_lists_what_it_brought() {
    let dir = tmp();
    let root = dir.path();
    assert!(init_git_commit(root, &[("a.txt", "one\n")]));
    let head = |root: &Path| git_stdout(root, &["rev-parse", "HEAD"]).unwrap();
    let first = head(root);
    // A branch with two commits, merged with a merge commit.
    assert!(git(root, &["checkout", "-q", "-b", "feature"]));
    std::fs::write(root.join("b.txt"), "bee\n").unwrap();
    assert!(git(root, &["add", "."]) && git(root, &["commit", "-m", "add b", "-m", "Why b."]));
    let on_branch = head(root);
    std::fs::write(root.join("c.txt"), "sea\n").unwrap();
    assert!(git(root, &["add", "."]) && git(root, &["commit", "-m", "add c"]));
    assert!(git(root, &["checkout", "-q", "main"]));
    std::fs::write(root.join("a.txt"), "one\ntwo\n").unwrap();
    assert!(git(root, &["commit", "-am", "main moves on"]));
    let direct = head(root);
    assert!(git(
        root,
        &["merge", "--no-ff", "-m", "Merge feature", "feature"]
    ));
    let merge = head(root);
    std::fs::write(root.join("a.txt"), "one\ntwo\nthree\n").unwrap();
    assert!(git(root, &["commit", "-am", "later"]));
    // And one that main does not have.
    assert!(git(root, &["checkout", "-q", "-b", "open", &first]));
    std::fs::write(root.join("d.txt"), "dee\n").unwrap();
    assert!(git(root, &["add", "."]) && git(root, &["commit", "-m", "not merged"]));
    let open = head(root);
    assert!(git(root, &["checkout", "-q", "main"]));

    let info = commit_info(root, &on_branch).unwrap();
    assert_eq!(
        (
            info.commit.subject.as_str(),
            info.body.as_str(),
            info.parents
        ),
        ("add b", "Why b.", 1)
    );
    assert_eq!(info.commit.author, "MonoCode");
    assert_eq!(info.commit.date.len(), "2026-11-25 14:03".len());
    match info.landing {
        Landing::Merged { branch, merge: by } => {
            assert_eq!(branch, "main");
            assert_eq!(
                (by.sha, by.subject.as_str()),
                (merge.clone(), "Merge feature")
            );
        }
        other => panic!("{other:?}"),
    }
    let main = || Landing::Direct {
        branch: "main".into(),
    };
    assert_eq!(commit_info(root, &direct).unwrap().landing, main());
    assert_eq!(commit_info(root, &first).unwrap().landing, main());
    assert_eq!(commit_info(root, &head(root)).unwrap().landing, main());
    let merged = commit_info(root, &merge).unwrap();
    assert_eq!((merged.parents, merged.landing), (2, main()));
    assert_eq!(
        commit_info(root, &open).unwrap().landing,
        Landing::NotMerged {
            branch: "main".into()
        }
    );
    assert!(commit_info(root, "not-a-sha").is_err());

    // The merge lists the branch's files, not nothing.
    let brought: Vec<_> = (commit_files(root, &merge).unwrap().into_iter())
        .map(|f| (f.relative, f.status, f.additions))
        .collect();
    assert_eq!(
        brought,
        [
            ("b.txt".to_owned(), "added".to_owned(), 1),
            ("c.txt".to_owned(), "added".to_owned(), 1)
        ]
    );
}
