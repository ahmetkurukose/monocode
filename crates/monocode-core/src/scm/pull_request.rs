//! New Pull Request: everything the native "Create Pull Request" dialog needs from
//! git and GitHub (github.com and GitHub Enterprise), via `git` and the `gh` CLI.
//!
//! Synchronous like the rest of `scm`: callers run these on a background executor.
//!
//! Shape mirrors JetBrains' GHPRCreateViewModel: the dialog loads a [`PrSetup`] once,
//! recomputes a [`PrRange`] + [`PrBranchCheck`] whenever base or head changes, loads
//! [`PrCandidates`] for the metadata pickers, and submits a [`PrSubmit`]. Creating and
//! applying metadata are separate calls so a metadata failure never re-creates the PR.
use super::*;
use crate::inbox::Label;
use serde_json::Value;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

/// One GitHub repository reachable through a git remote of the working copy.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PrRepo {
    /// Git remote name (`origin`, `upstream`, …). Empty for a fork parent that has no
    /// local remote: it can be a base but never a head.
    pub remote: String,
    /// `github.com` or the GitHub Enterprise host name.
    pub host: String,
    pub owner: String,
    pub name: String,
    /// Default branch as GitHub reports it.
    pub default_branch: String,
    /// `owner/name` of the parent when this repository is a fork.
    pub parent: Option<String>,
    /// The signed-in user may push here (WRITE/MAINTAIN/ADMIN).
    pub can_push: bool,
}

impl PrRepo {
    /// `owner/name`.
    pub fn slug(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
}

/// A pull request template from the base repository.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PrTemplate {
    /// File name, e.g. `pull_request_template.md` or `PULL_REQUEST_TEMPLATE/bug.md`.
    pub name: String,
    pub body: String,
}

/// What the dialog loads once when it opens.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PrSetup {
    /// Every GitHub repository known through the remotes, plus fork parents
    /// (with an empty `remote`) — candidates for base and (when `remote` is set) head.
    pub repos: Vec<PrRepo>,
    /// Index into `repos`: the fork parent when the main remote is a fork, else the
    /// repository of the main remote (origin).
    pub base_repo: usize,
    /// Index into `repos`: where the current branch is (or would be) pushed.
    pub head_repo: usize,
    /// Suggested base branch: the branch's recorded base, else the base repo's default.
    pub base_branch: String,
    /// The current local branch.
    pub head_branch: String,
    /// Signed-in GitHub login on the head repo's host.
    pub viewer: String,
    /// Templates of the base repo, the default one (`pull_request_template.md`) first.
    pub templates: Vec<PrTemplate>,
}

/// Commits and files a pull request from `head` into `base` would contain, computed
/// locally (three-dot: against the merge base).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PrRange {
    pub base_sha: String,
    pub head_sha: String,
    pub merge_base: String,
    /// Newest first.
    pub commits: Vec<IncomingCommit>,
    /// Files changed between `merge_base` and `head_sha`.
    pub files: Vec<ChangedFile>,
}

/// What must happen to the head branch before GitHub can see it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum PrPush {
    /// The remote branch exists and matches the local one.
    UpToDate,
    /// No remote branch yet: it will be created as `remote/branch`.
    Create { remote: String, branch: String },
    /// The local branch is ahead of `remote/branch`: a fast-forward push.
    Sync { remote: String, branch: String },
    /// The remote branch has commits the local one lacks: pushing would need force.
    Diverged { remote: String, branch: String },
}

/// Whether the selected base/head can become a pull request (gates the button).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum PrBranchCheck {
    Ok,
    /// Head has no commits that base lacks.
    NoChanges,
    /// An open pull request already exists for this head into this base.
    AlreadyExists { pr: Pr },
}

/// One user who can be a reviewer or assignee.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PrUser {
    pub login: String,
    pub name: String,
    pub avatar_url: String,
}

/// A team reviewer (organization repositories only).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PrTeam {
    /// `org/slug`, as GitHub's reviewer APIs take it.
    pub slug: String,
    pub name: String,
}

/// Candidates for the reviewer/assignee/label pickers of a base repository.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PrCandidates {
    /// Assignable users (minus the viewer for reviewers; the UI filters).
    pub users: Vec<PrUser>,
    pub teams: Vec<PrTeam>,
    pub labels: Vec<Label>,
}

/// Reviewers, assignees and labels to set on a pull request.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PrMetadata {
    pub reviewers: Vec<String>,
    /// `org/slug` team reviewers.
    pub team_reviewers: Vec<String>,
    pub assignees: Vec<String>,
    /// Label names.
    pub labels: Vec<String>,
}

impl PrMetadata {
    pub fn is_empty(&self) -> bool {
        self.reviewers.is_empty()
            && self.team_reviewers.is_empty()
            && self.assignees.is_empty()
            && self.labels.is_empty()
    }
}

/// Everything needed to open a pull request.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PrSubmit {
    pub base: PrRepo,
    pub base_branch: String,
    pub head: PrRepo,
    /// Branch name on the head repository (may differ from the local name).
    pub head_branch: String,
    pub title: String,
    pub body: String,
    pub draft: bool,
}

/// A pull request GitHub just created.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct CreatedPr {
    pub number: i64,
    pub url: String,
    /// GraphQL node id, for the metadata mutations.
    pub node_id: String,
    /// `owner/name` of the base repo.
    pub repo: String,
    pub host: String,
}

const REMOTE_UNSUPPORTED: &str = "Pull requests are not available on remote workspaces yet";
/// Most pages any one picker list fetches (100 items each).
const MAX_PAGES: usize = 10;
/// How long "this host is not a GitHub host gh knows" is believed before asking again,
/// so a `gh auth login` in a terminal is picked up without restarting.
const NOT_GITHUB_KEEP: Duration = Duration::from_secs(60);

fn local_root(cwd: &Path) -> Result<PathBuf, String> {
    if remote_workspace::is_remote(cwd) {
        return Err(REMOTE_UNSUPPORTED.into());
    }
    Ok(expand_home(cwd))
}

/// A branch, ref or remote name handed to git as an argument: never an option.
fn arg_name<'a>(what: &str, value: &'a str) -> Result<&'a str, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(format!("Choose a {what}"));
    }
    if value.starts_with('-') || value.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(format!("Invalid {what}: {value}"));
    }
    Ok(value)
}

fn valid_host(host: &str) -> Result<&str, String> {
    let host = host.trim();
    if host.is_empty()
        || host.starts_with('-')
        || host.contains('/')
        || host.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(format!("Invalid GitHub host: {host}"));
    }
    Ok(host)
}

// ---------------------------------------------------------------------------
// Remotes and hosts

/// The GitHub coordinates of a git remote URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RemoteUrl {
    pub host: String,
    pub owner: String,
    pub name: String,
}

/// Host, owner and name of a remote URL: `https://[user@]host[:port]/owner/name(.git)`,
/// `ssh://[user@]host[:port]/owner/name`, `git://…` and scp-like `user@host:owner/name`.
/// Hosts are lowercased; `ssh.github.com` (SSH over 443) is `github.com`. Local paths
/// and paths that are not exactly `owner/name` (GitLab subgroups) are `None`.
pub(super) fn parse_remote_url(url: &str) -> Option<RemoteUrl> {
    let url = url.trim().trim_end_matches('/');
    let (host, path) = if let Some((scheme, rest)) = url.split_once("://") {
        let scheme = scheme.to_ascii_lowercase();
        if !matches!(
            scheme.as_str(),
            "https" | "http" | "ssh" | "git" | "git+ssh" | "ssh+git"
        ) {
            return None;
        }
        let (authority, path) = rest.split_once('/')?;
        let host = authority.rsplit('@').next()?;
        (host.split(':').next()?, path)
    } else {
        let (user_host, path) = url.split_once(':')?;
        if user_host.contains('/') || user_host.contains('\\') {
            return None;
        }
        (user_host.rsplit('@').next()?, path)
    };
    let host = host.trim().to_ascii_lowercase();
    // A single letter is a Windows drive (`C:\repo`), not a host.
    if host.len() < 2 || host.chars().any(|c| c.is_whitespace() || c == '\\') {
        return None;
    }
    let host = match host.as_str() {
        "ssh.github.com" | "www.github.com" => "github.com".to_string(),
        _ => host,
    };
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let mut parts = path.split('/');
    let owner = parts.next()?.trim();
    let name = parts.next()?.trim();
    if parts.next().is_some() || owner.is_empty() || name.is_empty() {
        return None;
    }
    Some(RemoteUrl {
        host,
        owner: owner.to_string(),
        name: name.to_string(),
    })
}

/// Remote names with their (insteadOf-resolved) URLs, the main remote first.
fn remote_urls(root: &Path) -> Vec<(String, String)> {
    let mut names = git_remote_names(root);
    if let Some(main) = git_remote_name(root) {
        names.sort_by_key(|name| *name != main);
    }
    names
        .into_iter()
        .filter_map(|name| {
            let url = git_stdout(root, &["remote", "get-url", &name])?;
            Some((name, url))
        })
        .collect()
}

/// Whether `host` is github.com or a GitHub Enterprise host `gh` is signed in to.
fn is_github_host(root: &Path, host: &str) -> bool {
    if host == "github.com" {
        return true;
    }
    type Known = HashMap<String, (bool, Instant)>;
    static KNOWN: OnceLock<Mutex<Known>> = OnceLock::new();
    let known = KNOWN.get_or_init(Default::default);
    if let Some(&(github, at)) = known.lock().unwrap_or_else(|e| e.into_inner()).get(host) {
        if github || at.elapsed() < NOT_GITHUB_KEEP {
            return github;
        }
    }
    let github = valid_host(host).is_ok()
        && gh_run_with(
            root,
            &["auth", "status", "--active", "--hostname", host],
            &[],
            true,
        )
        .is_ok();
    known
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(host.to_string(), (github, Instant::now()));
    github
}

/// Whether a remote of `root` is on a GitHub Enterprise host `gh` is signed in to
/// (the Inbox counts GitHub as connected then, too).
pub(crate) fn github_enterprise_authenticated(root: &Path) -> bool {
    if remote_workspace::is_remote(root) {
        return false;
    }
    let root = expand_home(root);
    let mut hosts: Vec<String> = remote_urls(&root)
        .into_iter()
        .filter_map(|(_, url)| parse_remote_url(&url))
        .map(|remote| remote.host)
        .filter(|host| host != "github.com")
        .collect();
    hosts.sort();
    hosts.dedup();
    hosts.iter().any(|host| is_github_host(&root, host))
}

/// The remotes of `root` that are GitHub repositories, main remote first.
fn github_remotes(root: &Path) -> Result<Vec<(String, RemoteUrl)>, String> {
    let mut other_hosts = Vec::new();
    let mut remotes = Vec::new();
    for (name, url) in remote_urls(root) {
        let Some(parsed) = parse_remote_url(&url) else {
            continue;
        };
        if is_github_host(root, &parsed.host) {
            remotes.push((name, parsed));
        } else if !other_hosts.contains(&parsed.host) {
            other_hosts.push(parsed.host);
        }
    }
    if remotes.is_empty() {
        return Err(match other_hosts.first() {
            Some(host) => format!(
                "No GitHub remote found. If {host} is GitHub Enterprise, sign in with \
                 `gh auth login --hostname {host}`."
            ),
            None => "No GitHub remote found".into(),
        });
    }
    Ok(remotes)
}

// ---------------------------------------------------------------------------
// gh and GraphQL

/// `args` for `host`: `gh api` takes `--hostname`, every other subcommand `GH_HOST`.
pub(super) fn gh_host_args<'a>(host: &'a str, args: &[&'a str]) -> (Vec<&'a str>, bool) {
    if args.first() == Some(&"api") {
        let mut out = vec!["api", "--hostname", host];
        out.extend_from_slice(&args[1..]);
        (out, false)
    } else {
        (args.to_vec(), true)
    }
}

fn gh_host_output(
    root: &Path,
    host: &str,
    args: &[&str],
) -> Result<(std::process::Output, Vec<String>), String> {
    let host = valid_host(host)?;
    let (args, set_host) = gh_host_args(host, args);
    let envs: &[(&str, &str)] = if set_host { &[("GH_HOST", host)] } else { &[] };
    let output = gh_output(root, &args, envs)?;
    Ok((output, args.iter().map(|s| s.to_string()).collect()))
}

fn gh_host(root: &Path, host: &str, args: &[&str]) -> Result<String, String> {
    let (output, args) = gh_host_output(root, host, args)?;
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    if output.status.success() {
        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if text.is_empty() {
            return Err("gh returned no output".into());
        }
        return Ok(text);
    }
    Err(gh_failure(&output, &args))
}

/// Run `gh` with a temporary JSON file passed as `--input` (request bodies carry
/// user text and arrays, which `-f`/`-F` fields cannot express safely).
fn with_json_input<T>(
    body: &Value,
    run: impl FnOnce(&str) -> Result<T, String>,
) -> Result<T, String> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!(
        "monocode-gh-{}-{stamp}-{}.json",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&path, body.to_string()).map_err(|e| e.to_string())?;
    let result = run(&path.to_string_lossy());
    let _ = std::fs::remove_file(&path);
    result
}

/// A GraphQL answer: `data` (possibly partial) and error messages.
#[derive(Debug, Default)]
pub(super) struct Gql {
    pub data: Value,
    pub errors: Vec<String>,
}

impl Gql {
    /// `data`, or the first error when there is any.
    fn strict(self) -> Result<Value, String> {
        if !self.errors.is_empty() {
            return Err(self.errors.join("\n"));
        }
        if self.data.is_null() {
            return Err("GitHub returned no data".into());
        }
        Ok(self.data)
    }
}

/// Parse a GraphQL response body.
pub(super) fn parse_graphql(text: &str) -> Option<Gql> {
    let value: Value = serde_json::from_str(text.trim()).ok()?;
    let object = value.as_object()?;
    if !object.contains_key("data") && !object.contains_key("errors") {
        return None;
    }
    let errors = object
        .get("errors")
        .and_then(Value::as_array)
        .map(|errors| {
            errors
                .iter()
                .filter_map(|error| error.get("message").and_then(Value::as_str))
                .map(|message| message.trim().to_string())
                .filter(|message| !message.is_empty())
                .collect()
        })
        .unwrap_or_default();
    Some(Gql {
        data: object.get("data").cloned().unwrap_or(Value::Null),
        errors,
    })
}

/// One GraphQL request on `host`. Errors in the body are returned, not raised, so a
/// caller can use partial data; a failure without a GraphQL body is an `Err`.
fn graphql(root: &Path, host: &str, query: &str, variables: Value) -> Result<Gql, String> {
    let body = json!({ "query": query, "variables": variables });
    with_json_input(&body, |path| {
        let args = ["api", "graphql", "--input", path];
        let (output, _) = gh_host_output(root, host, &args)?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        if let Some(gql) = parse_graphql(&stdout) {
            return Ok(gql);
        }
        Err(gh_failure(&output, &args))
    })
}

fn graphql_strict(root: &Path, host: &str, query: &str, variables: Value) -> Result<Value, String> {
    graphql(root, host, query, variables)?.strict()
}

/// All nodes of the connection at `pointer` (JSON pointer into `data`), following
/// `pageInfo` with the `$after` variable, at most [`MAX_PAGES`] pages. Also returns
/// the first page's data for fields outside the connection.
fn paged(
    root: &Path,
    host: &str,
    query: &str,
    mut variables: Value,
    pointer: &str,
) -> Result<(Vec<Value>, Value), String> {
    let mut nodes = Vec::new();
    let mut first = None;
    for _ in 0..MAX_PAGES {
        let data = graphql_strict(root, host, query, variables.clone())?;
        let connection = data
            .pointer(pointer)
            .cloned()
            .ok_or_else(|| "GitHub repository not found".to_string())?;
        if let Some(page) = connection.get("nodes").and_then(Value::as_array) {
            nodes.extend(page.iter().filter(|node| !node.is_null()).cloned());
        }
        first.get_or_insert(data);
        let info = connection.get("pageInfo");
        let next = info
            .and_then(|info| info.get("hasNextPage"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let cursor = info
            .and_then(|info| info.get("endCursor"))
            .and_then(Value::as_str);
        match cursor {
            Some(cursor) if next => variables["after"] = json!(cursor),
            _ => break,
        }
    }
    Ok((nodes, first.unwrap_or(Value::Null)))
}

fn nodes_as<T: serde::de::DeserializeOwned>(nodes: Vec<Value>) -> Vec<T> {
    nodes
        .into_iter()
        .filter_map(|node| serde_json::from_value(node).ok())
        .collect()
}

// ---------------------------------------------------------------------------
// Setup

const REPO_FIELDS: &str =
    "id name owner { login } nameWithOwner defaultBranchRef { name } viewerPermission isFork";
const TEMPLATE_FIELDS: &str = "pullRequestTemplates { filename body }";

/// One query for `count` repositories (variables `$o{i}`/`$n{i}`), their parents and
/// the viewer.
pub(super) fn repos_query(count: usize, templates: bool) -> String {
    let fields = if templates {
        format!("{REPO_FIELDS} {TEMPLATE_FIELDS}")
    } else {
        REPO_FIELDS.to_string()
    };
    let mut decls = Vec::new();
    let mut body = String::from("viewer { login } ");
    for i in 0..count {
        decls.push(format!("$o{i}: String!, $n{i}: String!"));
        body.push_str(&format!(
            "r{i}: repository(owner: $o{i}, name: $n{i}) {{ {fields} parent {{ {fields} }} }} "
        ));
    }
    format!("query({}) {{ {body}}}", decls.join(", "))
}

#[derive(Deserialize, Default, Clone, Debug)]
#[serde(default)]
pub(super) struct GqlLogin {
    pub login: String,
}

#[derive(Deserialize, Default, Clone, Debug)]
#[serde(default)]
pub(super) struct GqlName {
    pub name: String,
}

#[derive(Deserialize, Default, Clone, Debug)]
#[serde(default)]
pub(super) struct GqlTemplate {
    pub filename: String,
    pub body: Option<String>,
}

#[derive(Deserialize, Default, Clone, Debug)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct GqlRepo {
    pub id: String,
    pub name: String,
    pub owner: GqlLogin,
    pub name_with_owner: String,
    pub default_branch_ref: Option<GqlName>,
    pub viewer_permission: Option<String>,
    pub is_fork: bool,
    pub pull_request_templates: Option<Vec<GqlTemplate>>,
    pub parent: Option<Box<GqlRepo>>,
}

impl GqlRepo {
    fn default_branch(&self) -> String {
        self.default_branch_ref
            .as_ref()
            .map(|branch| branch.name.clone())
            .unwrap_or_default()
    }

    fn can_push(&self) -> bool {
        matches!(
            self.viewer_permission.as_deref(),
            Some("WRITE" | "MAINTAIN" | "ADMIN")
        )
    }

    fn templates(&self) -> Vec<PrTemplate> {
        let mut templates: Vec<PrTemplate> = self
            .pull_request_templates
            .iter()
            .flatten()
            .map(|template| PrTemplate {
                name: template.filename.clone(),
                body: template.body.clone().unwrap_or_default(),
            })
            .collect();
        templates.sort_by_key(|template| !is_default_template(&template.name));
        templates
    }
}

fn is_default_template(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let file = name.rsplit('/').next().unwrap_or(&name);
    !name.contains("pull_request_template/")
        && (file == "pull_request_template.md" || file == "pull_request_template")
}

/// The viewer login and each `r{i}` repository of a [`repos_query`] answer.
pub(super) fn parse_repos(data: &Value, count: usize) -> (String, Vec<Option<GqlRepo>>) {
    let viewer = data
        .pointer("/viewer/login")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let repos = (0..count)
        .map(|i| {
            data.get(format!("r{i}"))
                .filter(|repo| !repo.is_null())
                .and_then(|repo| serde_json::from_value(repo.clone()).ok())
        })
        .collect();
    (viewer, repos)
}

/// Ask `host` about `repos` in one round-trip; retried without templates for
/// GitHub Enterprise versions that lack `pullRequestTemplates`.
fn repo_infos(
    root: &Path,
    host: &str,
    repos: &[&RemoteUrl],
) -> Result<(String, Vec<Option<GqlRepo>>), String> {
    let mut variables = serde_json::Map::new();
    for (i, repo) in repos.iter().enumerate() {
        variables.insert(format!("o{i}"), json!(repo.owner));
        variables.insert(format!("n{i}"), json!(repo.name));
    }
    let variables = Value::Object(variables);
    let mut gql = graphql(
        root,
        host,
        &repos_query(repos.len(), true),
        variables.clone(),
    )?;
    if gql
        .errors
        .iter()
        .any(|error| error.contains("pullRequestTemplates"))
    {
        gql = graphql(root, host, &repos_query(repos.len(), false), variables)?;
    }
    if gql.data.is_null() {
        return Err(gql
            .errors
            .first()
            .cloned()
            .unwrap_or_else(|| "GitHub returned no data".into()));
    }
    Ok(parse_repos(&gql.data, repos.len()))
}

fn repo_key(host: &str, owner: &str, name: &str) -> String {
    format!("{host}/{owner}/{name}").to_lowercase()
}

/// Load remotes, fork parent, default/suggested branches, viewer and templates.
pub fn pr_setup(cwd: &Path) -> Result<PrSetup, String> {
    let root = local_root(cwd)?;
    let remotes = github_remotes(&root)?;
    let mut infos: Vec<Option<GqlRepo>> = vec![None; remotes.len()];
    let mut viewers: HashMap<String, String> = HashMap::new();
    let mut hosts: Vec<&str> = Vec::new();
    for (_, remote) in &remotes {
        if !hosts.contains(&remote.host.as_str()) {
            hosts.push(&remote.host);
        }
    }
    for host in hosts {
        let indices: Vec<usize> = (0..remotes.len())
            .filter(|&i| remotes[i].1.host == host)
            .collect();
        let asked: Vec<&RemoteUrl> = indices.iter().map(|&i| &remotes[i].1).collect();
        let (viewer, answers) = repo_infos(&root, host, &asked)?;
        viewers.insert(host.to_string(), viewer);
        for (i, answer) in indices.into_iter().zip(answers) {
            infos[i] = answer;
        }
    }

    let mut repos: Vec<PrRepo> = Vec::new();
    let mut repo_infos_by_index: Vec<Option<GqlRepo>> = Vec::new();
    let mut by_key: HashMap<String, usize> = HashMap::new();
    let mut by_remote: HashMap<String, usize> = HashMap::new();
    for ((remote, url), info) in remotes.iter().zip(infos) {
        let (owner, name) = match &info {
            Some(info) if !info.owner.login.is_empty() && !info.name.is_empty() => {
                (info.owner.login.clone(), info.name.clone())
            }
            _ => (url.owner.clone(), url.name.clone()),
        };
        let key = repo_key(&url.host, &owner, &name);
        if let Some(&index) = by_key.get(&key) {
            by_remote.insert(remote.clone(), index);
            continue;
        }
        let default_branch = info
            .as_ref()
            .map(GqlRepo::default_branch)
            .filter(|branch| !branch.is_empty())
            .or_else(|| git_default_branch(&root, Some(remote)))
            .unwrap_or_default();
        let repo = PrRepo {
            remote: remote.clone(),
            host: url.host.clone(),
            owner,
            name,
            default_branch,
            parent: info
                .as_ref()
                .and_then(|info| info.parent.as_ref())
                .map(|parent| parent.name_with_owner.clone())
                .filter(|slug| !slug.is_empty()),
            can_push: info.as_ref().is_some_and(GqlRepo::can_push),
        };
        by_key.insert(key, repos.len());
        by_remote.insert(remote.clone(), repos.len());
        repos.push(repo);
        repo_infos_by_index.push(info);
    }

    let main = 0;
    let mut base_repo = main;
    let parent = repo_infos_by_index[main]
        .as_ref()
        .filter(|info| info.is_fork)
        .and_then(|info| info.parent.as_deref())
        .cloned();
    if let Some(parent) = parent.filter(|parent| !parent.name.is_empty()) {
        let host = repos[main].host.clone();
        let key = repo_key(&host, &parent.owner.login, &parent.name);
        base_repo = match by_key.get(&key) {
            Some(&index) => index,
            None => {
                repos.push(PrRepo {
                    remote: String::new(),
                    host,
                    owner: parent.owner.login.clone(),
                    name: parent.name.clone(),
                    default_branch: parent.default_branch(),
                    parent: None,
                    can_push: parent.can_push(),
                });
                repo_infos_by_index.push(Some(parent));
                repos.len() - 1
            }
        };
    }

    let head_branch = git_head_branch(&root).unwrap_or_default();
    let head_repo = push_remote(&root, &head_branch)
        .and_then(|remote| by_remote.get(&remote).copied())
        .unwrap_or(main);

    let base = &repos[base_repo];
    let recorded = !head_branch.is_empty()
        && !base.remote.is_empty()
        && git_stdout(
            &root,
            &["config", "--get", &format!("branch.{head_branch}.base")],
        )
        .is_some();
    let base_branch = recorded
        .then(|| git_pr_base(&root, Some(&head_branch), Some(&base.remote)))
        .flatten()
        .map(|(name, _)| name)
        .filter(|name| !name.is_empty())
        .or_else(|| Some(base.default_branch.clone()).filter(|name| !name.is_empty()))
        .or_else(|| git_default_branch(&root, Some(base.remote.as_str()).filter(|r| !r.is_empty())))
        .unwrap_or_default();

    let templates = repo_infos_by_index[base_repo]
        .as_ref()
        .map(GqlRepo::templates)
        .unwrap_or_default();
    let viewer = viewers
        .get(&repos[head_repo].host)
        .cloned()
        .unwrap_or_default();
    Ok(PrSetup {
        repos,
        base_repo,
        head_repo,
        base_branch,
        head_branch,
        viewer,
        templates,
    })
}

/// The remote `branch` pushes to: `branch.<b>.pushRemote`, `remote.pushDefault`, then
/// `branch.<b>.remote`.
fn push_remote(root: &Path, branch: &str) -> Option<String> {
    if branch.is_empty() {
        return None;
    }
    [
        format!("branch.{branch}.pushRemote"),
        "remote.pushDefault".to_string(),
        format!("branch.{branch}.remote"),
    ]
    .iter()
    .find_map(|key| git_stdout(root, &["config", "--get", key]).filter(|remote| remote != "."))
}

// ---------------------------------------------------------------------------
// Branches

/// Default branch first, the rest in the given (alphabetical) order.
pub(super) fn default_first(mut branches: Vec<String>, default_branch: &str) -> Vec<String> {
    branches.sort();
    branches.dedup();
    branches.sort_by_key(|branch| branch != default_branch);
    branches
}

/// Branch names of `repo` as the local remote-tracking refs know them (for the base
/// picker), default branch first. For a repo without a local remote, ask GitHub.
pub fn pr_branches(cwd: &Path, repo: &PrRepo) -> Result<Vec<String>, String> {
    let root = local_root(cwd)?;
    if !repo.remote.is_empty() {
        let remote = arg_name("remote", &repo.remote)?;
        let prefix = format!("refs/remotes/{remote}/");
        let branches: Vec<String> =
            git_run(&root, &["for-each-ref", "--format=%(refname)", &prefix])
                .unwrap_or_default()
                .lines()
                .filter_map(|line| line.trim().strip_prefix(&prefix))
                .filter(|name| !name.is_empty() && *name != "HEAD")
                .map(str::to_string)
                .collect();
        if !branches.is_empty() {
            return Ok(default_first(branches, &repo.default_branch));
        }
    }
    const QUERY: &str = "query($owner: String!, $name: String!, $after: String) { \
        repository(owner: $owner, name: $name) { \
        refs(refPrefix: \"refs/heads/\", first: 100, after: $after, \
        orderBy: { field: ALPHABETICAL, direction: ASC }) { \
        nodes { name } pageInfo { hasNextPage endCursor } } } }";
    let (nodes, _) = paged(
        &root,
        &repo.host,
        QUERY,
        json!({ "owner": repo.owner, "name": repo.name }),
        "/repository/refs",
    )?;
    let branches = nodes_as::<GqlName>(nodes)
        .into_iter()
        .map(|branch| branch.name)
        .filter(|name| !name.is_empty())
        .collect();
    Ok(default_first(branches, &repo.default_branch))
}

/// Local branches (for the head picker), current first.
pub fn pr_local_branches(cwd: &Path) -> Result<Vec<String>, String> {
    let root = local_root(cwd)?;
    let text = git_run(
        &root,
        &["for-each-ref", "--format=%(refname)", "refs/heads/"],
    )
    .ok_or_else(|| "Could not list local branches".to_string())?;
    let branches = text
        .lines()
        .filter_map(|line| line.trim().strip_prefix("refs/heads/"))
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect();
    let current = git_head_branch(&root).unwrap_or_default();
    Ok(default_first(branches, &current))
}

/// `git fetch` the remotes of base and head quietly so the range is not stale.
pub fn pr_fetch(cwd: &Path, remotes: &[String]) -> Result<(), String> {
    let root = local_root(cwd)?;
    let mut unique: Vec<&str> = Vec::new();
    for remote in remotes {
        let remote = remote.trim();
        if !remote.is_empty() && !unique.contains(&remote) {
            unique.push(remote);
        }
    }
    if unique.is_empty() {
        return Ok(());
    }
    let results: Vec<Result<(), String>> = std::thread::scope(|scope| {
        let handles: Vec<_> = unique
            .iter()
            .map(|remote| {
                let root = &root;
                scope.spawn(move || {
                    let remote = arg_name("remote", remote)?;
                    git_checked(root, &["fetch", "--quiet", remote])
                        .map_err(|error| format!("{remote}: {error}"))
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap_or_else(|_| Err("fetch failed".into())))
            .collect()
    });
    if results.iter().any(Result::is_ok) {
        return Ok(());
    }
    Err(results
        .into_iter()
        .filter_map(Result::err)
        .collect::<Vec<_>>()
        .join("\n"))
}

// ---------------------------------------------------------------------------
// Range and push

fn commit_sha(root: &Path, spec: &str) -> Result<String, String> {
    let spec = arg_name("branch", spec)?;
    git_stdout(
        root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{spec}^{{commit}}"),
        ],
    )
    .ok_or_else(|| format!("Unknown branch: {spec}"))
}

/// Commits and files of `base_ref...head_ref` (refs like `origin/main`, `feature/x`).
pub fn pr_range(cwd: &Path, base_ref: &str, head_ref: &str) -> Result<PrRange, String> {
    let root = local_root(cwd)?;
    let base_sha = commit_sha(&root, base_ref)?;
    let head_sha = commit_sha(&root, head_ref)?;
    let merge_base = git_stdout_within(
        &root,
        &["merge-base", &base_sha, &head_sha],
        HISTORY_WALK_TIMEOUT,
    )
    .ok_or_else(|| {
        format!(
            "{} and {} share no history",
            base_ref.trim(),
            head_ref.trim()
        )
    })?;
    let commits = super::update::commits_in(&root, &[&format!("{base_sha}..{head_sha}")]);
    let files = super::update::range_files_for(&root, &merge_base, &head_sha);
    Ok(PrRange {
        base_sha,
        head_sha,
        merge_base,
        commits,
        files,
    })
}

/// Diff of one file between two SHAs of a [`PrRange`] (merge base → head, or a
/// single commit's parent → commit).
pub fn pr_file_diff(cwd: &Path, old: &str, new: &str, path: &str) -> Result<FileDiff, String> {
    local_root(cwd)?;
    super::update::range_file_diff(cwd, old, new, path)
}

/// What pushing the local `branch` to `remote/remote_branch` would take.
pub fn pr_push_state(
    cwd: &Path,
    branch: &str,
    remote: &str,
    remote_branch: &str,
) -> Result<PrPush, String> {
    let root = local_root(cwd)?;
    let branch = arg_name("branch", branch)?;
    let remote = arg_name("remote", remote)?;
    let remote_branch = arg_name("branch", remote_branch)?;
    let local = git_stdout(
        &root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .ok_or_else(|| format!("Unknown branch: {branch}"))?;
    let target = || (remote.to_string(), remote_branch.to_string());
    let Some(pushed) = git_stdout(
        &root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/remotes/{remote}/{remote_branch}"),
        ],
    ) else {
        let (remote, branch) = target();
        return Ok(PrPush::Create { remote, branch });
    };
    if pushed == local {
        return Ok(PrPush::UpToDate);
    }
    let (remote, branch) = target();
    if git_output(&root, &["merge-base", "--is-ancestor", &pushed, &local]).is_some() {
        Ok(PrPush::Sync { remote, branch })
    } else {
        Ok(PrPush::Diverged { remote, branch })
    }
}

/// Push local `branch` to `remote/remote_branch` and set it as upstream. Never forces.
pub fn pr_push(cwd: &Path, branch: &str, remote: &str, remote_branch: &str) -> Result<(), String> {
    let root = local_root(cwd)?;
    let branch = arg_name("branch", branch)?;
    let remote = arg_name("remote", remote)?;
    let remote_branch = arg_name("branch", remote_branch)?;
    let refspec = format!("refs/heads/{branch}:refs/heads/{remote_branch}");
    git_checked(&root, &["push", "-u", remote, &refspec]).map_err(with_signing_hint)
}

// ---------------------------------------------------------------------------
// Branch check

/// The first pull request of a REST `pulls` list.
pub(super) fn parse_rest_pulls(json: &str) -> Result<Option<Pr>, String> {
    #[derive(Deserialize)]
    struct Row {
        number: i64,
        #[serde(default)]
        title: String,
        #[serde(default)]
        html_url: String,
        #[serde(default)]
        state: String,
    }
    let rows: Vec<Row> = serde_json::from_str(json).map_err(|e| e.to_string())?;
    Ok(rows.into_iter().next().map(|row| Pr {
        number: row.number,
        title: row.title,
        url: row.html_url,
        state: row.state.to_lowercase(),
    }))
}

/// NoChanges / AlreadyExists / Ok for `head:head_branch` into `base:base_branch`.
/// Looks for an open PR by `head_owner:branch` even when the branch is not pushed yet.
pub fn pr_branch_check(
    cwd: &Path,
    base: &PrRepo,
    base_branch: &str,
    head: &PrRepo,
    head_branch: &str,
    range: &PrRange,
) -> Result<PrBranchCheck, String> {
    let root = local_root(cwd)?;
    if range.commits.is_empty() {
        return Ok(PrBranchCheck::NoChanges);
    }
    let base_branch = arg_name("base branch", base_branch)?;
    let head_branch = arg_name("head branch", head_branch)?;
    let path = format!("repos/{}/{}/pulls", base.owner, base.name);
    let head_filter = format!("head={}:{head_branch}", head.owner);
    let base_filter = format!("base={base_branch}");
    let json = gh_host(
        &root,
        &base.host,
        &[
            "api",
            "--method",
            "GET",
            &path,
            "-f",
            "state=open",
            "-f",
            &head_filter,
            "-f",
            &base_filter,
            "-f",
            "per_page=1",
        ],
    )?;
    Ok(match parse_rest_pulls(&json)? {
        Some(pr) => PrBranchCheck::AlreadyExists { pr },
        None => PrBranchCheck::Ok,
    })
}

// ---------------------------------------------------------------------------
// Candidates

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct GqlUser {
    login: String,
    name: Option<String>,
    avatar_url: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GqlTeam {
    slug: String,
    name: String,
}

/// Assignable users, org teams and labels of `repo` (all pages).
pub fn pr_candidates(cwd: &Path, repo: &PrRepo) -> Result<PrCandidates, String> {
    let root = local_root(cwd)?;
    const USERS: &str = "query($owner: String!, $name: String!, $after: String) { \
        repository(owner: $owner, name: $name) { \
        assignableUsers(first: 100, after: $after) { \
        nodes { login name avatarUrl } pageInfo { hasNextPage endCursor } } } }";
    const LABELS: &str = "query($owner: String!, $name: String!, $after: String) { \
        repository(owner: $owner, name: $name) { owner { __typename } \
        labels(first: 100, after: $after) { \
        nodes { name color } pageInfo { hasNextPage endCursor } } } }";
    const TEAMS: &str = "query($owner: String!, $after: String) { \
        organization(login: $owner) { teams(first: 100, after: $after) { \
        nodes { slug name } pageInfo { hasNextPage endCursor } } } }";
    let variables = json!({ "owner": repo.owner, "name": repo.name });
    let (users, labels_and_teams) = std::thread::scope(|scope| {
        let users = scope.spawn(|| {
            paged(
                &root,
                &repo.host,
                USERS,
                variables.clone(),
                "/repository/assignableUsers",
            )
        });
        let labels = scope.spawn(|| {
            let (labels, first) = paged(
                &root,
                &repo.host,
                LABELS,
                variables.clone(),
                "/repository/labels",
            )?;
            let organization = first
                .pointer("/repository/owner/__typename")
                .and_then(Value::as_str)
                == Some("Organization");
            // Listing teams needs org membership; anyone else just gets none.
            let teams = if organization {
                paged(
                    &root,
                    &repo.host,
                    TEAMS,
                    json!({ "owner": repo.owner }),
                    "/organization/teams",
                )
                .map(|(teams, _)| teams)
                .unwrap_or_default()
            } else {
                Vec::new()
            };
            Ok::<_, String>((labels, teams))
        });
        (
            users.join().unwrap_or_else(|_| Err("failed".into())),
            labels.join().unwrap_or_else(|_| Err("failed".into())),
        )
    });
    let (users, _) = users?;
    let (labels, teams) = labels_and_teams?;
    Ok(PrCandidates {
        users: nodes_as::<GqlUser>(users)
            .into_iter()
            .filter(|user| !user.login.is_empty())
            .map(|user| PrUser {
                login: user.login,
                name: user.name.unwrap_or_default(),
                avatar_url: user.avatar_url,
            })
            .collect(),
        teams: nodes_as::<GqlTeam>(teams)
            .into_iter()
            .filter(|team| !team.slug.is_empty())
            .map(|team| PrTeam {
                slug: format!("{}/{}", repo.owner, team.slug),
                name: team.name,
            })
            .collect(),
        labels: nodes_as::<Label>(labels)
            .into_iter()
            .filter(|label| !label.name.is_empty())
            .collect(),
    })
}

// ---------------------------------------------------------------------------
// Create

fn same_repo(a: &PrRepo, b: &PrRepo) -> bool {
    a.host.eq_ignore_ascii_case(&b.host) && a.slug().eq_ignore_ascii_case(&b.slug())
}

/// The `CreatePullRequestInput` for `submit`. A head in another repository is named
/// by `headRepositoryId` when known, else as `owner:branch`.
pub(super) fn create_pr_input(submit: &PrSubmit, base_id: &str, head_id: Option<&str>) -> Value {
    let mut input = json!({
        "repositoryId": base_id,
        "baseRefName": submit.base_branch.trim(),
        "headRefName": submit.head_branch.trim(),
        "title": submit.title.trim(),
        "body": submit.body.trim(),
        "draft": submit.draft,
    });
    if !same_repo(&submit.base, &submit.head) {
        match head_id {
            Some(id) => input["headRepositoryId"] = json!(id),
            None => {
                input["headRefName"] = json!(format!(
                    "{}:{}",
                    submit.head.owner,
                    submit.head_branch.trim()
                ))
            }
        }
    }
    input
}

pub(super) fn parse_created_pr(data: &Value, base: &PrRepo) -> Result<CreatedPr, String> {
    let pr = data
        .pointer("/createPullRequest/pullRequest")
        .filter(|pr| !pr.is_null())
        .ok_or_else(|| "GitHub did not return the pull request".to_string())?;
    let created = CreatedPr {
        number: pr.get("number").and_then(Value::as_i64).unwrap_or(0),
        url: pr
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        node_id: pr
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        repo: base.slug(),
        host: base.host.clone(),
    };
    if created.number <= 0 || created.node_id.is_empty() {
        return Err("GitHub did not return the pull request".into());
    }
    Ok(created)
}

/// Create the pull request (GraphQL `createPullRequest`). Fork heads use
/// `owner:branch`. Does not set metadata.
pub fn pr_open(cwd: &Path, submit: &PrSubmit) -> Result<CreatedPr, String> {
    let root = local_root(cwd)?;
    if submit.title.trim().is_empty() {
        return Err("Pull request title cannot be empty".into());
    }
    arg_name("base branch", &submit.base_branch)?;
    arg_name("head branch", &submit.head_branch)?;
    let host = &submit.base.host;
    if !submit.head.host.eq_ignore_ascii_case(host) {
        return Err("Base and head must be on the same GitHub host".into());
    }
    let cross = !same_repo(&submit.base, &submit.head);
    let mut query = String::from("query($bo: String!, $bn: String!");
    if cross {
        query.push_str(", $ho: String!, $hn: String!");
    }
    query.push_str(") { base: repository(owner: $bo, name: $bn) { id }");
    if cross {
        query.push_str(" head: repository(owner: $ho, name: $hn) { id }");
    }
    query.push_str(" }");
    let ids = graphql_strict(
        &root,
        host,
        &query,
        json!({
            "bo": submit.base.owner, "bn": submit.base.name,
            "ho": submit.head.owner, "hn": submit.head.name,
        }),
    )?;
    let base_id = ids
        .pointer("/base/id")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("GitHub repository {} not found", submit.base.slug()))?;
    let head_id = ids.pointer("/head/id").and_then(Value::as_str);
    const CREATE: &str = "mutation($input: CreatePullRequestInput!) { \
        createPullRequest(input: $input) { pullRequest { id number url } } }";
    let create = |head_id: Option<&str>| {
        graphql(
            &root,
            host,
            CREATE,
            json!({ "input": create_pr_input(submit, base_id, head_id) }),
        )
    };
    let mut gql = create(head_id)?;
    if cross
        && head_id.is_some()
        && gql
            .errors
            .iter()
            .any(|error| error.contains("headRepositoryId"))
    {
        // GitHub Enterprise versions before headRepositoryId take `owner:branch`.
        gql = create(None)?;
    }
    parse_created_pr(&gql.strict()?, &submit.base)
}

// ---------------------------------------------------------------------------
// Metadata

/// Node ids for metadata mutations.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct MetadataIds {
    pub reviewers: Vec<String>,
    pub teams: Vec<String>,
    pub assignees: Vec<String>,
    pub labels: Vec<String>,
}

impl MetadataIds {
    fn is_empty(&self) -> bool {
        self.reviewers.is_empty()
            && self.teams.is_empty()
            && self.assignees.is_empty()
            && self.labels.is_empty()
    }
}

fn unique_ci(items: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = HashSet::new();
    items
        .into_iter()
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty() && seen.insert(item.to_lowercase()))
        .collect()
}

/// One query resolving user logins, `org/slug` teams and label names of
/// `owner/name` to node ids (aliases `u{i}`, `t{i}`, `l{i}`).
pub(super) fn ids_query(
    owner: &str,
    name: &str,
    users: &[String],
    teams: &[String],
    labels: &[String],
) -> Result<Option<(String, Value)>, String> {
    if users.is_empty() && teams.is_empty() && labels.is_empty() {
        return Ok(None);
    }
    let mut decls = Vec::new();
    let mut body = String::new();
    let mut variables = serde_json::Map::new();
    for (i, login) in users.iter().enumerate() {
        decls.push(format!("$u{i}: String!"));
        body.push_str(&format!("u{i}: user(login: $u{i}) {{ id }} "));
        variables.insert(format!("u{i}"), json!(login));
    }
    for (i, team) in teams.iter().enumerate() {
        let (org, slug) = team
            .split_once('/')
            .filter(|(org, slug)| !org.is_empty() && !slug.is_empty())
            .ok_or_else(|| format!("Invalid team: {team}"))?;
        decls.push(format!("$to{i}: String!, $ts{i}: String!"));
        body.push_str(&format!(
            "t{i}: organization(login: $to{i}) {{ team(slug: $ts{i}) {{ id }} }} "
        ));
        variables.insert(format!("to{i}"), json!(org));
        variables.insert(format!("ts{i}"), json!(slug));
    }
    if !labels.is_empty() {
        decls.push("$owner: String!, $name: String!".into());
        variables.insert("owner".into(), json!(owner));
        variables.insert("name".into(), json!(name));
        body.push_str("repository(owner: $owner, name: $name) { ");
        for (i, label) in labels.iter().enumerate() {
            decls.push(format!("$l{i}: String!"));
            body.push_str(&format!("l{i}: label(name: $l{i}) {{ id }} "));
            variables.insert(format!("l{i}"), json!(label));
        }
        body.push_str("} ");
    }
    Ok(Some((
        format!("query({}) {{ {body}}}", decls.join(", ")),
        Value::Object(variables),
    )))
}

/// Node ids by lowercased login / team / label of an [`ids_query`] answer.
#[derive(Debug, Default)]
pub(super) struct ResolvedIds {
    pub users: HashMap<String, String>,
    pub teams: HashMap<String, String>,
    pub labels: HashMap<String, String>,
}

impl ResolvedIds {
    fn pick(
        map: &HashMap<String, String>,
        what: &str,
        names: &[String],
    ) -> Result<Vec<String>, String> {
        unique_ci(names.iter().cloned())
            .iter()
            .map(|name| {
                map.get(&name.to_lowercase())
                    .cloned()
                    .ok_or_else(|| format!("Unknown {what}: {name}"))
            })
            .collect()
    }
}

pub(super) fn parse_ids(
    data: &Value,
    users: &[String],
    teams: &[String],
    labels: &[String],
) -> ResolvedIds {
    let id = |pointer: String| {
        data.pointer(&pointer)
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_string)
    };
    let mut ids = ResolvedIds::default();
    for (i, login) in users.iter().enumerate() {
        if let Some(id) = id(format!("/u{i}/id")) {
            ids.users.insert(login.to_lowercase(), id);
        }
    }
    for (i, team) in teams.iter().enumerate() {
        if let Some(id) = id(format!("/t{i}/team/id")) {
            ids.teams.insert(team.to_lowercase(), id);
        }
    }
    for (i, label) in labels.iter().enumerate() {
        if let Some(id) = id(format!("/repository/l{i}/id")) {
            ids.labels.insert(label.to_lowercase(), id);
        }
    }
    ids
}

fn resolve_ids(
    root: &Path,
    host: &str,
    repo: &str,
    users: &[String],
    teams: &[String],
    labels: &[String],
) -> Result<ResolvedIds, String> {
    let (owner, name) = split_github_repo(repo)?;
    let users = unique_ci(users.iter().cloned());
    let teams = unique_ci(teams.iter().cloned());
    let labels = unique_ci(labels.iter().cloned());
    let Some((query, variables)) = ids_query(&owner, &name, &users, &teams, &labels)? else {
        return Ok(ResolvedIds::default());
    };
    let data = graphql_strict(root, host, &query, variables)?;
    Ok(parse_ids(&data, &users, &teams, &labels))
}

/// One mutation that adds `add` and removes `remove` (assignees and labels; review
/// requests are removed through REST) on the pull request `pr_id`.
pub(super) fn metadata_mutation(
    pr_id: &str,
    add: &MetadataIds,
    remove: &MetadataIds,
) -> Option<(String, Value)> {
    let mut decls = vec!["$pr: ID!".to_string()];
    let mut body = String::new();
    let mut variables = serde_json::Map::new();
    variables.insert("pr".into(), json!(pr_id));
    let mut var = |name: &str, ids: &[String]| {
        decls.push(format!("${name}: [ID!]!"));
        variables.insert(name.into(), json!(ids));
    };
    if !add.reviewers.is_empty() || !add.teams.is_empty() {
        var("reviewers", &add.reviewers);
        var("teams", &add.teams);
        body.push_str(
            "reviews: requestReviews(input: { pullRequestId: $pr, union: true, \
             userIds: $reviewers, teamIds: $teams }) { clientMutationId } ",
        );
    }
    if !add.assignees.is_empty() {
        var("assignees", &add.assignees);
        body.push_str(
            "assign: addAssigneesToAssignable(input: { assignableId: $pr, \
             assigneeIds: $assignees }) { clientMutationId } ",
        );
    }
    if !add.labels.is_empty() {
        var("labels", &add.labels);
        body.push_str(
            "label: addLabelsToLabelable(input: { labelableId: $pr, labelIds: $labels }) \
             { clientMutationId } ",
        );
    }
    if !remove.assignees.is_empty() {
        var("unassignees", &remove.assignees);
        body.push_str(
            "unassign: removeAssigneesFromAssignable(input: { assignableId: $pr, \
             assigneeIds: $unassignees }) { clientMutationId } ",
        );
    }
    if !remove.labels.is_empty() {
        var("unlabels", &remove.labels);
        body.push_str(
            "unlabel: removeLabelsFromLabelable(input: { labelableId: $pr, \
             labelIds: $unlabels }) { clientMutationId } ",
        );
    }
    if body.is_empty() {
        return None;
    }
    Some((
        format!("mutation({}) {{ {body}}}", decls.join(", ")),
        Value::Object(variables),
    ))
}

/// Set reviewers, assignees and labels in one GraphQL request; skips empty lists.
/// Safe to retry after a partial failure.
pub fn pr_apply_metadata(cwd: &Path, pr: &CreatedPr, metadata: &PrMetadata) -> Result<(), String> {
    let root = local_root(cwd)?;
    if metadata.is_empty() {
        return Ok(());
    }
    if pr.node_id.trim().is_empty() {
        return Err("Unknown pull request".into());
    }
    let users: Vec<String> = metadata
        .reviewers
        .iter()
        .chain(&metadata.assignees)
        .cloned()
        .collect();
    let ids = resolve_ids(
        &root,
        &pr.host,
        &pr.repo,
        &users,
        &metadata.team_reviewers,
        &metadata.labels,
    )?;
    let add = MetadataIds {
        reviewers: ResolvedIds::pick(&ids.users, "reviewer", &metadata.reviewers)?,
        teams: ResolvedIds::pick(&ids.teams, "team", &metadata.team_reviewers)?,
        assignees: ResolvedIds::pick(&ids.users, "assignee", &metadata.assignees)?,
        labels: ResolvedIds::pick(&ids.labels, "label", &metadata.labels)?,
    };
    if let Some((query, variables)) = metadata_mutation(&pr.node_id, &add, &MetadataIds::default())
    {
        graphql_strict(&root, &pr.host, &query, variables)?;
    }
    Ok(())
}

/// An existing pull request's metadata, with the node ids needed to remove some.
#[derive(Debug, Default)]
pub(super) struct CurrentMetadata {
    pub pr_id: String,
    pub metadata: PrMetadata,
    pub assignee_ids: HashMap<String, String>,
    pub label_ids: HashMap<String, String>,
}

const PR_METADATA_QUERY: &str = "query($owner: String!, $name: String!, $number: Int!) { \
    repository(owner: $owner, name: $name) { pullRequest(number: $number) { id \
    reviewRequests(first: 100) { nodes { requestedReviewer { __typename \
    ... on User { login } ... on Team { slug organization { login } } } } } \
    assignees(first: 100) { nodes { id login } } \
    labels(first: 100) { nodes { id name } } } } }";

pub(super) fn parse_pr_metadata(data: &Value) -> Result<CurrentMetadata, String> {
    let pr = data
        .pointer("/repository/pullRequest")
        .filter(|pr| !pr.is_null())
        .ok_or_else(|| "GitHub pull request not found".to_string())?;
    let nodes = |field: &str| {
        pr.pointer(&format!("/{field}/nodes"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let text = |value: &Value, pointer: &str| {
        value
            .pointer(pointer)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let mut current = CurrentMetadata {
        pr_id: text(pr, "/id"),
        ..Default::default()
    };
    for request in nodes("reviewRequests") {
        let reviewer = &request["requestedReviewer"];
        match reviewer.get("__typename").and_then(Value::as_str) {
            Some("Team") => {
                let (org, slug) = (
                    text(reviewer, "/organization/login"),
                    text(reviewer, "/slug"),
                );
                if !org.is_empty() && !slug.is_empty() {
                    current
                        .metadata
                        .team_reviewers
                        .push(format!("{org}/{slug}"));
                }
            }
            _ => {
                let login = text(reviewer, "/login");
                if !login.is_empty() {
                    current.metadata.reviewers.push(login);
                }
            }
        }
    }
    for assignee in nodes("assignees") {
        let login = text(&assignee, "/login");
        if !login.is_empty() {
            current
                .assignee_ids
                .insert(login.to_lowercase(), text(&assignee, "/id"));
            current.metadata.assignees.push(login);
        }
    }
    for label in nodes("labels") {
        let name = text(&label, "/name");
        if !name.is_empty() {
            current
                .label_ids
                .insert(name.to_lowercase(), text(&label, "/id"));
            current.metadata.labels.push(name);
        }
    }
    if current.pr_id.is_empty() {
        return Err("GitHub pull request not found".into());
    }
    Ok(current)
}

fn current_metadata(
    root: &Path,
    host: &str,
    repo: &str,
    number: i64,
) -> Result<CurrentMetadata, String> {
    if number <= 0 {
        return Err("Invalid pull request number".into());
    }
    let (owner, name) = split_github_repo(repo)?;
    let data = graphql_strict(
        root,
        host,
        PR_METADATA_QUERY,
        json!({ "owner": owner, "name": name, "number": number }),
    )?;
    parse_pr_metadata(&data)
}

/// Current reviewers/assignees/labels of an existing pull request (Inbox editing).
pub fn pr_metadata(cwd: &Path, host: &str, repo: &str, number: i64) -> Result<PrMetadata, String> {
    let root = local_root(cwd)?;
    Ok(current_metadata(&root, host, repo, number)?.metadata)
}

fn minus(left: &[String], right: &[String]) -> Vec<String> {
    let right: HashSet<String> = right
        .iter()
        .map(|item| item.trim().to_lowercase())
        .collect();
    unique_ci(
        left.iter()
            .filter(|item| !right.contains(&item.trim().to_lowercase()))
            .cloned(),
    )
}

/// What to add and what to remove to turn `current` into `wanted` (case-insensitive).
pub(super) fn metadata_diff(current: &PrMetadata, wanted: &PrMetadata) -> (PrMetadata, PrMetadata) {
    let diff = |pick: fn(&PrMetadata) -> &Vec<String>, a: &PrMetadata, b: &PrMetadata| {
        minus(pick(a), pick(b))
    };
    let side = |a: &PrMetadata, b: &PrMetadata| PrMetadata {
        reviewers: diff(|m| &m.reviewers, a, b),
        team_reviewers: diff(|m| &m.team_reviewers, a, b),
        assignees: diff(|m| &m.assignees, a, b),
        labels: diff(|m| &m.labels, a, b),
    };
    (side(wanted, current), side(current, wanted))
}

/// The REST body removing review requests (`team_reviewers` take bare slugs).
pub(super) fn remove_reviewers_body(remove: &PrMetadata) -> Value {
    let teams: Vec<&str> = remove
        .team_reviewers
        .iter()
        .map(|team| team.split_once('/').map_or(team.as_str(), |(_, slug)| slug))
        .collect();
    json!({ "reviewers": remove.reviewers, "team_reviewers": teams })
}

/// Replace reviewers/assignees/labels of an existing pull request with `metadata`
/// (adds and removes as needed; Inbox editing).
pub fn pr_set_metadata(
    cwd: &Path,
    host: &str,
    repo: &str,
    number: i64,
    metadata: &PrMetadata,
) -> Result<(), String> {
    let root = local_root(cwd)?;
    let current = current_metadata(&root, host, repo, number)?;
    let (add, remove) = metadata_diff(&current.metadata, metadata);
    let users: Vec<String> = add
        .reviewers
        .iter()
        .chain(&add.assignees)
        .cloned()
        .collect();
    let ids = resolve_ids(&root, host, repo, &users, &add.team_reviewers, &add.labels)?;
    let adds = MetadataIds {
        reviewers: ResolvedIds::pick(&ids.users, "reviewer", &add.reviewers)?,
        teams: ResolvedIds::pick(&ids.teams, "team", &add.team_reviewers)?,
        assignees: ResolvedIds::pick(&ids.users, "assignee", &add.assignees)?,
        labels: ResolvedIds::pick(&ids.labels, "label", &add.labels)?,
    };
    let removes = MetadataIds {
        assignees: ResolvedIds::pick(&current.assignee_ids, "assignee", &remove.assignees)?,
        labels: ResolvedIds::pick(&current.label_ids, "label", &remove.labels)?,
        ..Default::default()
    };
    if !adds.is_empty() || !removes.is_empty() {
        if let Some((query, variables)) = metadata_mutation(&current.pr_id, &adds, &removes) {
            graphql_strict(&root, host, &query, variables)?;
        }
    }
    if !remove.reviewers.is_empty() || !remove.team_reviewers.is_empty() {
        let (owner, name) = split_github_repo(repo)?;
        let path = format!("repos/{owner}/{name}/pulls/{number}/requested_reviewers");
        with_json_input(&remove_reviewers_body(&remove), |input| {
            gh_host(
                &root,
                host,
                &["api", "--method", "DELETE", &path, "--input", input],
            )
        })?;
    }
    Ok(())
}
