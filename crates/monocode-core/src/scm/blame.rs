//! Git from inside one file: who last changed each of its lines (blame),
//! the commits that touched some of its lines (`git log -L`), and the
//! file's page on the repository's web host.
use super::review::git_blob;
use super::*;
use similar::{capture_diff_slices_deadline, Algorithm, DiffOp};
use std::path::PathBuf;

/// One commit that last changed some lines of a blamed file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlameCommit {
    pub sha: String,
    pub short_sha: String,
    pub author: String,
    /// Seconds since the epoch.
    pub time: u64,
    pub subject: String,
}

/// A line of the blamed version and the commit (an index into
/// [`Blame::commits`]) that last changed it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlameLine {
    pub commit: usize,
    pub text: String,
}

/// A file as HEAD has it, line by line, with who last changed each line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Blame {
    /// The commit that was blamed.
    pub head: String,
    /// The file's path from the repository's root.
    pub path: String,
    pub commits: Vec<BlameCommit>,
    pub lines: Vec<BlameLine>,
}

/// How long lining a buffer up with its committed version may take before
/// the rest counts as changed.
const ALIGN_BUDGET: Duration = Duration::from_millis(300);

/// The repository and repository path of a file. Run from the file's own
/// folder, so a nested checkout or one outside the project still answers.
fn locate(path: &Path) -> Result<(PathBuf, String), String> {
    if remote_workspace::is_remote(path) {
        return Err("Git is not available here for remote projects.".into());
    }
    let path = expand_home(path);
    let dir = path.parent().ok_or("Not a git repository")?;
    let name = path
        .file_name()
        .ok_or("Not a git repository")?
        .to_string_lossy()
        .into_owned();
    let root = git_stdout(dir, &["rev-parse", "--show-toplevel"])
        .map(PathBuf::from)
        .ok_or("Not a git repository")?;
    let relative = git_run(
        dir,
        &[
            "-c",
            "core.quotePath=false",
            "ls-files",
            "--full-name",
            "--",
            &name,
        ],
    )
    .and_then(|out| out.lines().next().map(str::to_owned))
    .filter(|line| !line.is_empty())
    .ok_or("Git does not track this file.")?;
    Ok((root, relative))
}

fn head_of(root: &Path) -> Result<String, String> {
    git_stdout(root, &["rev-parse", "--verify", "HEAD"])
        .ok_or_else(|| "The repository has no commits yet.".to_string())
}

/// The commit HEAD points at in the repository holding `path`.
pub fn head_revision(path: &Path) -> Option<String> {
    let (root, _) = locate(path).ok()?;
    head_of(&root).ok()
}

/// Runs git, keeping what it said on failure.
fn git_checked_output(root: &Path, args: &[&str]) -> Result<String, String> {
    let output = crate::process::output(
        git_cmd()
            .arg("--no-pager")
            .arg("-C")
            .arg(root)
            .args(args)
            .env("GIT_OPTIONAL_LOCKS", "0"),
    )
    .map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let said = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        Err(said
            .strip_prefix("fatal: ")
            .map(str::to_owned)
            .unwrap_or(said))
    }
}

/// Who last changed each line of `path` as HEAD has it (`git blame
/// --porcelain`). `path` is absolute.
pub fn blame_file(path: &Path) -> Result<Blame, String> {
    let (root, relative) = locate(path)?;
    let head = head_of(&root)?;
    if git_blob(&root, &format!("{head}:{relative}")).is_none() {
        return Err("This file is not committed yet.".into());
    }
    let out = git_checked_output(&root, &["blame", "--porcelain", &head, "--", &relative])?;
    let (commits, lines) = parse_blame_porcelain(&out);
    Ok(Blame {
        head,
        path: relative,
        commits,
        lines,
    })
}

fn is_sha(token: &str) -> bool {
    matches!(token.len(), 40 | 64) && token.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Reads `git blame --porcelain`: each line's header names its commit, the
/// commit's details follow its first header only, and the line's text comes
/// last, after a tab.
pub fn parse_blame_porcelain(text: &str) -> (Vec<BlameCommit>, Vec<BlameLine>) {
    let mut commits: Vec<BlameCommit> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut lines = Vec::new();
    let mut current = None;
    for line in text.split('\n') {
        if let Some(content) = line.strip_prefix('\t') {
            if let Some(commit) = current {
                lines.push(BlameLine {
                    commit,
                    text: content.strip_suffix('\r').unwrap_or(content).to_owned(),
                });
            }
            continue;
        }
        let mut words = line.splitn(2, ' ');
        let key = words.next().unwrap_or_default();
        let value = words.next().unwrap_or_default();
        if is_sha(key) {
            let at = *index.entry(key.to_owned()).or_insert_with(|| {
                commits.push(BlameCommit {
                    sha: key.to_owned(),
                    short_sha: key[..7].to_owned(),
                    ..Default::default()
                });
                commits.len() - 1
            });
            current = Some(at);
            continue;
        }
        let Some(commit) = current.and_then(|at| commits.get_mut(at)) else {
            continue;
        };
        match key {
            "author" => commit.author = value.to_owned(),
            "author-time" => commit.time = value.trim().parse().unwrap_or(0),
            "summary" => commit.subject = value.to_owned(),
            _ => {}
        }
    }
    (commits, lines)
}

fn split_lines(text: &str) -> Vec<&str> {
    text.lines().collect()
}

/// For each line of `current`, the line of `base` it still is (zero-based),
/// or `None` where it was added or changed since.
pub fn align_lines<S: AsRef<str>>(base: &[S], current: &str) -> Vec<Option<usize>> {
    let base: Vec<&str> = base
        .iter()
        .map(|line| {
            let line = line.as_ref();
            line.strip_suffix('\r').unwrap_or(line)
        })
        .collect();
    let now = split_lines(current);
    let mut rows = vec![None; now.len()];
    let deadline = Instant::now() + ALIGN_BUDGET;
    for op in capture_diff_slices_deadline(Algorithm::Myers, &base, &now, Some(deadline)) {
        if let DiffOp::Equal {
            old_index,
            new_index,
            len,
        } = op
        {
            for n in 0..len {
                rows[new_index + n] = Some(old_index + n);
            }
        }
    }
    rows
}

/// The commits that changed lines `first..=last` (one-based, of `current`
/// when given, else of HEAD's version), newest first, as the file history
/// lists versions. Lines changed in `current` and not committed come first
/// as the uncommitted version.
pub fn line_history(
    path: &Path,
    first: usize,
    last: usize,
    current: Option<&str>,
    limit: usize,
) -> Result<Vec<FileVersion>, String> {
    let (root, relative) = locate(path)?;
    head_of(&root)?;
    let committed = git_blob(&root, &format!("HEAD:{relative}"))
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .ok_or("This file is not committed yet.")?;
    let head_lines = split_lines(&committed);
    let (first, last) = (first.max(1), last.max(first.max(1)));
    let (from, to, dirty) = match current {
        Some(current) => {
            let rows = align_lines(&head_lines, current);
            let mapped: Vec<usize> = rows
                .get(first - 1..last.min(rows.len()))
                .unwrap_or_default()
                .iter()
                .flatten()
                .copied()
                .collect();
            let (Some(from), Some(to)) = (mapped.iter().min(), mapped.iter().max()) else {
                return Err("The selected lines are not committed yet.".into());
            };
            let now = split_lines(current);
            let selected = now.get(first - 1..last.min(now.len())).unwrap_or_default();
            let dirty = selected != &head_lines[*from..=*to];
            (from + 1, to + 1, dirty)
        }
        None => (first, last.min(head_lines.len().max(1)), false),
    };
    let out = git_checked_output(
        &root,
        &[
            "-c",
            "core.quotePath=false",
            "log",
            &format!("-n{limit}"),
            &format!("-L{from},{to}:{relative}"),
            "--format=%x1e%H%x1f%h%x1f%an%x1f%at%x1f%s",
        ],
    )?;
    let mut versions = parse_line_log(&out);
    if dirty {
        versions.insert(
            0,
            FileVersion {
                sha: String::new(),
                short_sha: String::new(),
                subject: "Uncommitted changes".into(),
                author: String::new(),
                time: 0,
                path: relative,
                previous_path: None,
                status: "modified".into(),
            },
        );
    }
    Ok(versions)
}

/// Reads `git log -L` with the format `line_history` asks for: a record per
/// commit, its fields, then the patch of the lines, whose `---`/`+++`
/// headers name the file before and after.
pub fn parse_line_log(text: &str) -> Vec<FileVersion> {
    let mut versions = Vec::new();
    for record in text.split('\x1e').filter(|r| !r.trim().is_empty()) {
        let mut lines = record.lines();
        let mut fields = lines.next().unwrap_or_default().split('\x1f');
        let mut field = || fields.next().unwrap_or_default().to_owned();
        let (sha, short_sha, author, time, subject) = (field(), field(), field(), field(), field());
        let (mut before, mut after) = (None, None);
        for line in lines {
            if before.is_none() {
                if let Some(path) = line.strip_prefix("--- ") {
                    before = Some(path.to_owned());
                    continue;
                }
            }
            if let Some(path) = line.strip_prefix("+++ ") {
                after = Some(path.to_owned());
                break;
            }
        }
        let side = |path: Option<String>, prefix: &str| {
            path.filter(|p| p != "/dev/null")
                .map(|p| p.strip_prefix(prefix).map(str::to_owned).unwrap_or(p))
        };
        let (before, after) = (side(before, "a/"), side(after, "b/"));
        let Some(path) = after.clone().or_else(|| before.clone()) else {
            continue;
        };
        let (previous_path, status) = match (&before, &after) {
            (None, _) => (None, "added"),
            (Some(old), Some(new)) if old != new => (Some(old.clone()), "renamed"),
            _ => (None, "modified"),
        };
        versions.push(FileVersion {
            sha,
            short_sha,
            subject,
            author,
            time: time.parse().unwrap_or(0),
            path,
            previous_path,
            status: status.into(),
        });
    }
    versions
}

/// Which kind of web host a remote is, for its links' shapes and names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteKind {
    GitHub,
    GitLab,
    Bitbucket,
    /// Gitea, Forgejo and Codeberg.
    Gitea,
    /// Anything else, linked to as GitHub Enterprise is.
    Other,
}

/// A remote's repository page, such as `https://github.com/owner/name`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteWeb {
    pub kind: RemoteKind,
    pub host: String,
    pub base: String,
}
impl RemoteWeb {
    /// What menus call the host: "GitHub", or its address when unknown.
    pub fn name(&self) -> &str {
        match self.kind {
            RemoteKind::GitHub => "GitHub",
            RemoteKind::GitLab => "GitLab",
            RemoteKind::Bitbucket => "Bitbucket",
            RemoteKind::Gitea if self.host == "codeberg.org" => "Codeberg",
            RemoteKind::Gitea | RemoteKind::Other => &self.host,
        }
    }
    /// The page of `path` (from the repository's root) at `commit`, with
    /// one-based lines `first..=last` marked.
    pub fn file_url(&self, commit: &str, path: &str, lines: Option<(usize, usize)>) -> String {
        let path = encode_path(path);
        let base = &self.base;
        let (page, anchor) = match self.kind {
            RemoteKind::GitLab => (
                format!("{base}/-/blob/{commit}/{path}"),
                lines.map(|(a, b)| match a == b {
                    true => format!("#L{a}"),
                    false => format!("#L{a}-{b}"),
                }),
            ),
            RemoteKind::Bitbucket => (
                format!("{base}/src/{commit}/{path}"),
                lines.map(|(a, b)| match a == b {
                    true => format!("#lines-{a}"),
                    false => format!("#lines-{a}:{b}"),
                }),
            ),
            RemoteKind::Gitea => (
                format!("{base}/src/commit/{commit}/{path}"),
                lines.map(|(a, b)| github_lines(a, b)),
            ),
            RemoteKind::GitHub | RemoteKind::Other => (
                format!("{base}/blob/{commit}/{path}"),
                lines.map(|(a, b)| github_lines(a, b)),
            ),
        };
        page + &anchor.unwrap_or_default()
    }
}
fn github_lines(a: usize, b: usize) -> String {
    match a == b {
        true => format!("#L{a}"),
        false => format!("#L{a}-L{b}"),
    }
}
/// Percent-encodes a repository path for a URL, keeping its slashes.
fn encode_path(path: &str) -> String {
    let mut out = String::new();
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// The web page behind a remote's URL: HTTPS, SSH (`ssh://` or
/// `git@host:path`) and `git://` remotes. Local paths have none.
pub fn remote_web(url: &str) -> Option<RemoteWeb> {
    let url = url.trim();
    let (scheme, rest) = match url.split_once("://") {
        Some((scheme, rest)) => (scheme.to_ascii_lowercase(), rest),
        None => {
            // scp-like `[user@]host:path`; a colon after a slash is a path.
            let colon = url.find(':')?;
            if url[..colon].contains('/') || colon < 2 {
                return None;
            }
            ("scp".to_owned(), url)
        }
    };
    if !matches!(
        scheme.as_str(),
        "https" | "http" | "ssh" | "git" | "git+ssh" | "ssh+git" | "scp"
    ) {
        return None;
    }
    let (authority, path) = match scheme.as_str() {
        "scp" => rest.split_once(':')?,
        _ => rest.split_once('/')?,
    };
    let authority = authority.rsplit('@').next()?;
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if port.bytes().all(|b| b.is_ascii_digit()) => (host, Some(port)),
        _ => (authority, None),
    };
    let host = host.to_ascii_lowercase();
    let host = match host.as_str() {
        "ssh.github.com" => "github.com".to_owned(),
        "altssh.gitlab.com" => "gitlab.com".to_owned(),
        "altssh.bitbucket.org" => "bitbucket.org".to_owned(),
        _ => host,
    };
    let path = path.trim_matches('/');
    let path = path
        .strip_suffix(".git")
        .unwrap_or(path)
        .trim_end_matches('/');
    if host.is_empty() || !path.contains('/') {
        return None;
    }
    let kind = if host == "github.com" || host.starts_with("github.") {
        RemoteKind::GitHub
    } else if host.contains("gitlab") {
        RemoteKind::GitLab
    } else if host.contains("bitbucket") {
        RemoteKind::Bitbucket
    } else if host == "codeberg.org" || host.contains("gitea") || host.contains("forgejo") {
        RemoteKind::Gitea
    } else {
        RemoteKind::Other
    };
    // An HTTPS remote's port serves its pages too; an SSH port does not.
    let port = match (scheme.as_str(), port) {
        ("https" | "http", Some(port)) => format!(":{port}"),
        _ => String::new(),
    };
    let web = if scheme == "http" { "http" } else { "https" };
    Some(RemoteWeb {
        base: format!("{web}://{host}{port}/{path}"),
        kind,
        host,
    })
}

/// The Git folders of the checkout holding `path`: its own (a worktree's,
/// for a worktree) and the shared one with the config. Read from disk only,
/// so menus can ask while they are built.
fn git_dirs_of(path: &Path) -> Option<(PathBuf, PathBuf)> {
    let mut dir = Some(path);
    while let Some(at) = dir {
        let dot = at.join(".git");
        let own = if dot.is_dir() {
            Some(dot)
        } else if dot.is_file() {
            let text = std::fs::read_to_string(&dot).ok()?;
            let target = text.trim().strip_prefix("gitdir:")?.trim().to_owned();
            Some(at.join(target))
        } else {
            None
        };
        if let Some(own) = own {
            let common = std::fs::read_to_string(own.join("commondir"))
                .ok()
                .map(|common| own.join(common.trim()))
                .unwrap_or_else(|| own.clone());
            return Some((own, common));
        }
        dir = at.parent();
    }
    None
}

/// A checkout as the code menu sees it before asking git anything.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalRepo {
    /// The web page of the remote the current branch pushes to, else of
    /// `origin`, else of the first remote.
    pub remote: Option<RemoteWeb>,
}

/// Whether `path` is inside a Git checkout, and its remote, from the files
/// in its Git folder.
pub fn local_repo(path: &Path) -> Option<LocalRepo> {
    if remote_workspace::is_remote(path) {
        return None;
    }
    let (own, common) = git_dirs_of(&expand_home(path))?;
    let config = std::fs::read_to_string(common.join("config")).unwrap_or_default();
    let branch = std::fs::read_to_string(own.join("HEAD"))
        .ok()
        .and_then(|head| {
            head.trim()
                .strip_prefix("ref: refs/heads/")
                .map(str::to_owned)
        });
    let url = remote_url_in_config(&config, branch.as_deref());
    Some(LocalRepo {
        remote: url.as_deref().and_then(remote_web),
    })
}

/// The URL of the remote `branch` pushes to in a Git config, else of
/// `origin`, else of the first remote listed.
pub fn remote_url_in_config(config: &str, branch: Option<&str>) -> Option<String> {
    let mut section = String::new();
    let mut urls: Vec<(String, String)> = Vec::new();
    let mut tracked = None;
    for line in config.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            section = line.trim_matches(|c| c == '[' || c == ']').to_owned();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim().to_ascii_lowercase(), value.trim());
        let value = value.trim_matches('"').to_owned();
        let (kind, name) = match section.split_once(' ') {
            Some((kind, name)) => (kind.to_ascii_lowercase(), name.trim_matches('"')),
            None => (section.to_ascii_lowercase(), ""),
        };
        match (kind.as_str(), key.as_str()) {
            ("remote", "url") if !urls.iter().any(|(n, _)| n == name) => {
                urls.push((name.to_owned(), value))
            }
            ("branch", "remote") if Some(name) == branch => tracked = Some(value),
            _ => {}
        }
    }
    let find = |wanted: &str| {
        urls.iter()
            .find(|(n, _)| n == wanted)
            .map(|(_, u)| u.clone())
    };
    tracked
        .as_deref()
        .and_then(find)
        .or_else(|| find("origin"))
        .or_else(|| urls.first().map(|(_, url)| url.clone()))
}

/// The page of `path` at HEAD on its remote, with lines `first..=last` (of
/// `current` when given, carried over to HEAD's version) marked.
pub fn remote_file_link(
    path: &Path,
    lines: Option<(usize, usize)>,
    current: Option<&str>,
) -> Result<String, String> {
    let web = local_repo(path)
        .ok_or("Not a git repository")?
        .remote
        .ok_or("The repository has no remote on a web host.")?;
    let (root, relative) = locate(path)?;
    let head = head_of(&root)?;
    let committed = git_blob(&root, &format!("{head}:{relative}"))
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .ok_or("This file is not committed yet.")?;
    let lines = lines.map(|(first, last)| match current {
        Some(current) => {
            let rows = align_lines(&split_lines(&committed), current);
            let mapped: Vec<usize> = rows
                .get(first.saturating_sub(1)..last.min(rows.len()))
                .unwrap_or_default()
                .iter()
                .flatten()
                .copied()
                .collect();
            match (mapped.iter().min(), mapped.iter().max()) {
                (Some(a), Some(b)) => (a + 1, b + 1),
                _ => (first, last),
            }
        }
        None => (first, last),
    });
    Ok(web.file_url(&head, &relative, lines))
}

#[cfg(test)]
mod blame_tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let done = Command::new("git")
            .args(["-c", "user.name=Ann", "-c", "user.email=ann@example.com"])
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            done.status.success(),
            "{}",
            String::from_utf8_lossy(&done.stderr)
        );
    }

    #[test]
    fn porcelain_names_each_lines_commit_once_and_reuses_it() {
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        let text = format!(
            "{a} 1 1 2\nauthor Ann\nauthor-mail <ann@x>\nauthor-time 1700000000\n\
             summary First\nfilename f.txt\n\tone\n{a} 2 2\n\ttwo\r\n\
             {b} 3 3 1\nauthor Bob\nauthor-time 1700000100\nsummary Second\n\
             previous {a} f.txt\nfilename f.txt\n\tthree\n"
        );
        let (commits, lines) = parse_blame_porcelain(&text);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].author, "Ann");
        assert_eq!(commits[0].short_sha, "aaaaaaa");
        assert_eq!(commits[1].subject, "Second");
        assert_eq!(commits[1].time, 1_700_000_100);
        let rows: Vec<_> = lines.iter().map(|l| (l.commit, l.text.as_str())).collect();
        assert_eq!(rows, [(0, "one"), (0, "two"), (1, "three")]);
    }

    #[test]
    fn edited_lines_lose_their_place_and_the_rest_keep_it() {
        let base = ["a", "b", "c", "d"];
        assert_eq!(
            align_lines(&base, "a\nb\nc\nd\n"),
            [Some(0), Some(1), Some(2), Some(3)]
        );
        // A line typed above `b` and `c` changed: the rest follow.
        assert_eq!(
            align_lines(&base, "a\nnew\nb\nC\nd"),
            [Some(0), None, Some(1), None, Some(3)]
        );
        // Deleting lines pulls the rest up; CRLF matches LF.
        assert_eq!(align_lines(&base, "c\r\nd\r\n"), [Some(2), Some(3)]);
        assert!(align_lines(&base, "").is_empty());
    }

    #[test]
    fn remotes_link_to_their_hosts_pages() {
        let github = remote_web("git@github.com:acme/app.git").unwrap();
        assert_eq!(github.kind, RemoteKind::GitHub);
        assert_eq!(github.name(), "GitHub");
        assert_eq!(github.base, "https://github.com/acme/app");
        assert_eq!(
            github.file_url("abc", "src/a b.rs", Some((10, 20))),
            "https://github.com/acme/app/blob/abc/src/a%20b.rs#L10-L20"
        );
        assert_eq!(
            github.file_url("abc", "x.rs", Some((3, 3))),
            "https://github.com/acme/app/blob/abc/x.rs#L3"
        );
        for url in [
            "https://github.com/acme/app",
            "https://user:token@github.com/acme/app.git/",
            "ssh://git@ssh.github.com:443/acme/app.git",
            "git://github.com/acme/app.git",
        ] {
            assert_eq!(remote_web(url).unwrap().base, github.base, "{url}");
        }
        let gitlab = remote_web("ssh://git@gitlab.example.com:2222/group/sub/app.git").unwrap();
        assert_eq!((gitlab.kind, gitlab.name()), (RemoteKind::GitLab, "GitLab"));
        assert_eq!(
            gitlab.file_url("abc", "a.rs", Some((1, 4))),
            "https://gitlab.example.com/group/sub/app/-/blob/abc/a.rs#L1-4"
        );
        let https = remote_web("https://gitlab.com:8443/a/b.git").unwrap();
        assert_eq!(https.base, "https://gitlab.com:8443/a/b");
        let bitbucket = remote_web("git@bitbucket.org:team/repo.git").unwrap();
        assert_eq!(
            bitbucket.file_url("abc", "a.rs", Some((2, 5))),
            "https://bitbucket.org/team/repo/src/abc/a.rs#lines-2:5"
        );
        let codeberg = remote_web("https://codeberg.org/me/thing").unwrap();
        assert_eq!(codeberg.name(), "Codeberg");
        assert_eq!(
            codeberg.file_url("abc", "a.rs", None),
            "https://codeberg.org/me/thing/src/commit/abc/a.rs"
        );
        let other = remote_web("git@git.example.org:me/thing.git").unwrap();
        assert_eq!(other.name(), "git.example.org");
        for url in [
            "/srv/repos/app.git",
            "../app",
            "file:///srv/app.git",
            "C:\\repo",
            "host:",
        ] {
            assert!(remote_web(url).is_none(), "{url}");
        }
    }

    #[test]
    fn the_branchs_remote_wins_then_origin_then_the_first() {
        let config = "[core]\n\tbare = false\n[remote \"upstream\"]\n\turl = git@github.com:up/app.git\n\
                      [remote \"origin\"]\n\turl = https://github.com/me/app.git\n\
                      [branch \"main\"]\n\tremote = origin\n[branch \"fix\"]\n\tremote = upstream\n";
        assert_eq!(
            remote_url_in_config(config, Some("fix")).as_deref(),
            Some("git@github.com:up/app.git")
        );
        assert_eq!(
            remote_url_in_config(config, None).as_deref(),
            Some("https://github.com/me/app.git")
        );
        let lone = "[remote \"fork\"]\n\turl = git@gitlab.com:me/app.git\n";
        assert_eq!(
            remote_url_in_config(lone, Some("main")).as_deref(),
            Some("git@gitlab.com:me/app.git")
        );
        assert_eq!(remote_url_in_config("[core]\n", None), None);
    }

    #[test]
    fn a_checkouts_blame_line_history_and_link_come_from_git() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path();
        git(dir, &["init"]);
        let file = dir.join("a.txt");
        std::fs::write(&file, "one\ntwo\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-m", "first"]);
        std::fs::write(&file, "one\ntwo\nthree\n").unwrap();
        git(dir, &["commit", "-am", "second"]);
        std::fs::write(dir.join("new.txt"), "x\n").unwrap();

        let blame = blame_file(&file).unwrap();
        assert_eq!(blame.path, "a.txt");
        let by: Vec<_> = blame
            .lines
            .iter()
            .map(|l| (blame.commits[l.commit].subject.as_str(), l.text.as_str()))
            .collect();
        assert_eq!(
            by,
            [("first", "one"), ("first", "two"), ("second", "three")]
        );
        assert_eq!(blame.commits[0].author, "Ann");
        assert_eq!(head_revision(&file).as_deref(), Some(blame.head.as_str()));
        assert!(blame_file(&dir.join("new.txt")).is_err());
        assert!(local_repo(&file).is_some_and(|repo| repo.remote.is_none()));

        // Line 3 only came with the second commit; line 1 with the first.
        let subjects = |versions: Vec<FileVersion>| -> Vec<String> {
            versions.into_iter().map(|v| v.subject).collect()
        };
        assert_eq!(
            subjects(line_history(&file, 3, 3, None, 50).unwrap()),
            ["second"]
        );
        assert_eq!(
            subjects(line_history(&file, 1, 1, None, 50).unwrap()),
            ["first"]
        );
        // A buffer with a line typed above carries the selection to HEAD's
        // lines; an edited selection lists the uncommitted version first.
        let edited = "zero\none\ntwo\nTHREE\n";
        assert_eq!(
            subjects(line_history(&file, 2, 2, Some(edited), 50).unwrap()),
            ["first"]
        );
        assert_eq!(
            subjects(line_history(&file, 3, 4, Some(edited), 50).unwrap()),
            ["Uncommitted changes", "first"]
        );
        assert!(line_history(&file, 1, 1, Some(edited), 50).is_err());

        git(
            dir,
            &["remote", "add", "origin", "git@github.com:acme/app.git"],
        );
        let link = remote_file_link(&file, Some((2, 3)), Some(edited)).unwrap();
        assert_eq!(
            link,
            format!(
                "https://github.com/acme/app/blob/{}/a.txt#L1-L2",
                blame.head
            )
        );
        assert_eq!(
            local_repo(&file).and_then(|r| r.remote).map(|r| r.kind),
            Some(RemoteKind::GitHub)
        );
    }

    #[test]
    fn a_line_log_lists_renames_and_the_commit_that_added_the_lines() {
        let text = "\x1eaaa\x1fa1\x1fAnn\x1f100\x1fMove\n\ndiff --git a/old.rs b/new.rs\n\
                    --- a/old.rs\n+++ b/new.rs\n@@ -1,1 +1,1 @@\n-x\n+y\n\
                    \x1ebbb\x1fb1\x1fBob\x1f50\x1fStart\n\ndiff --git a/old.rs b/old.rs\n\
                    --- /dev/null\n+++ b/old.rs\n@@ -0,0 +1,1 @@\n+x\n";
        let versions = parse_line_log(text);
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[0].path, "new.rs");
        assert_eq!(versions[0].previous_path.as_deref(), Some("old.rs"));
        assert_eq!(versions[0].status, "renamed");
        assert_eq!(
            (versions[1].status.as_str(), versions[1].time),
            ("added", 50)
        );
        assert_eq!(versions[1].author, "Bob");
    }
}
