//! The commits of a GitHub pull request and what each one changed, so a
//! review can go commit by commit as well as over the whole diff.
use super::{run_gh, Diff, DiffFile, Item, Service};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Per-file patches over this are left out, like the whole-PR diff.
const MAX_COMMIT_DIFF_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PrCommit {
    pub sha: String,
    pub short_sha: String,
    pub headline: String,
    pub author: String,
    /// RFC 3339, as GitHub reports it.
    pub date: String,
}

fn repo_parts(repo: &str) -> Result<(String, String), String> {
    let (owner, name) = repo
        .trim()
        .split_once('/')
        .ok_or("Not a GitHub repository")?;
    let valid = |part: &str| {
        !part.is_empty()
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    if !valid(owner) || !valid(name) {
        return Err("Not a GitHub repository".into());
    }
    Ok((owner.to_string(), name.to_string()))
}

pub(super) fn parse_commits(json: &str) -> Result<Vec<PrCommit>, String> {
    #[derive(Deserialize)]
    struct Author {
        #[serde(default)]
        login: String,
        #[serde(default)]
        name: String,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Row {
        #[serde(default)]
        oid: String,
        #[serde(default)]
        message_headline: String,
        #[serde(default)]
        committed_date: String,
        #[serde(default)]
        authors: Vec<Author>,
    }
    #[derive(Deserialize)]
    struct Root {
        #[serde(default)]
        commits: Vec<Row>,
    }
    let root: Root = serde_json::from_str(json).map_err(|e| e.to_string())?;
    Ok(root
        .commits
        .into_iter()
        .filter(|row| !row.oid.is_empty())
        .map(|row| {
            let author = row
                .authors
                .into_iter()
                .map(|a| if a.login.is_empty() { a.name } else { a.login })
                .find(|name| !name.is_empty())
                .unwrap_or_default();
            PrCommit {
                short_sha: row.oid.chars().take(7).collect(),
                sha: row.oid,
                headline: row.message_headline,
                author,
                date: row.committed_date,
            }
        })
        .collect())
}

/// `gh api repos/o/r/commits/<sha>` as a [`Diff`], with a unified patch the
/// review's parser reads like the whole-PR one.
pub(super) fn parse_commit_diff(json: &str) -> Result<Diff, String> {
    #[derive(Deserialize)]
    struct File {
        filename: String,
        #[serde(default)]
        additions: i64,
        #[serde(default)]
        deletions: i64,
        #[serde(default)]
        patch: Option<String>,
    }
    #[derive(Deserialize)]
    struct Root {
        #[serde(default)]
        files: Vec<File>,
    }
    let root: Root = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let mut diff = Diff::default();
    let mut patch = String::new();
    for file in &root.files {
        diff.additions += file.additions;
        diff.deletions += file.deletions;
        diff.files.push(DiffFile {
            path: file.filename.clone(),
            additions: file.additions,
            deletions: file.deletions,
        });
        patch.push_str(&format!(
            "diff --git a/{0} b/{0}\n--- a/{0}\n+++ b/{0}\n",
            file.filename
        ));
        match file.patch.as_deref() {
            Some(text) if !text.is_empty() => {
                patch.push_str(text);
                if !text.ends_with('\n') {
                    patch.push('\n');
                }
            }
            // GitHub sends no patch for binary or oversized files.
            _ => patch.push_str(&format!(
                "Binary files a/{0} and b/{0} differ\n",
                file.filename
            )),
        }
    }
    if patch.len() > MAX_COMMIT_DIFF_BYTES {
        diff.truncated = true;
    } else {
        diff.patch = patch;
    }
    Ok(diff)
}

/// One path as GitHub's contents API wants it: each segment percent-encoded.
fn encode_path(path: &str) -> String {
    let mut out = String::new();
    for (ix, segment) in path.split('/').enumerate() {
        if ix > 0 {
            out.push('/');
        }
        for byte in segment.bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    out.push(byte as char)
                }
                _ => out.push_str(&format!("%{byte:02X}")),
            }
        }
    }
    out
}

fn valid_sha(sha: &str) -> bool {
    (7..=40).contains(&sha.len()) && sha.bytes().all(|b| b.is_ascii_hexdigit())
}

pub(super) fn parse_pr_oids(json: &str) -> Result<(String, String), String> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Row {
        #[serde(default)]
        base_ref_oid: String,
        #[serde(default)]
        head_ref_oid: String,
    }
    let row: Row = serde_json::from_str(json).map_err(|e| e.to_string())?;
    if !valid_sha(&row.base_ref_oid) || !valid_sha(&row.head_ref_oid) {
        return Err("Pull request is missing its base or head commit".into());
    }
    Ok((row.base_ref_oid, row.head_ref_oid))
}

fn gh(root: &Path, args: &[&str]) -> Result<String, String> {
    run_gh(root, args, false)
}

impl Service {
    /// The pull request's commits, oldest first.
    pub fn pr_commits(&self, item: &Item) -> Result<Vec<PrCommit>, String> {
        if item.provider != "github" || item.number <= 0 {
            return Ok(vec![]);
        }
        let (owner, name) = repo_parts(&item.repo)?;
        let number = item.number.to_string();
        let json = gh(
            self.project(item),
            &[
                "pr",
                "view",
                &number,
                "--repo",
                &format!("{owner}/{name}"),
                "--json",
                "commits",
            ],
        )?;
        parse_commits(&json)
    }
    /// The two commits a review scope compares: where the pull request
    /// branched from the base and its head, or one commit's parent and the
    /// commit. An empty `old` means the commit has no parent.
    pub fn pr_scope_shas(
        &self,
        item: &Item,
        commit: Option<&str>,
    ) -> Result<(String, String), String> {
        if item.provider != "github" || item.number <= 0 {
            return Err("Whole files are only available for GitHub pull requests".into());
        }
        let (owner, name) = repo_parts(&item.repo)?;
        let root = self.project(item);
        if let Some(sha) = commit {
            if !valid_sha(sha) {
                return Err("Unknown commit".into());
            }
            let parent = gh(
                root,
                &[
                    "api",
                    &format!("repos/{owner}/{name}/commits/{sha}"),
                    "--jq",
                    ".parents[0].sha // \"\"",
                ],
            )?;
            return Ok((parent.trim().to_string(), sha.to_string()));
        }
        let number = item.number.to_string();
        let json = gh(
            root,
            &[
                "pr",
                "view",
                &number,
                "--repo",
                &format!("{owner}/{name}"),
                "--json",
                "baseRefOid,headRefOid",
            ],
        )?;
        let (base, head) = parse_pr_oids(&json)?;
        let merge_base = gh(
            root,
            &[
                "api",
                &format!("repos/{owner}/{name}/compare/{base}...{head}"),
                "--jq",
                ".merge_base_commit.sha",
            ],
        )?;
        let merge_base = merge_base.trim();
        Ok((
            if valid_sha(merge_base) {
                merge_base
            } else {
                &base
            }
            .to_string(),
            head,
        ))
    }

    /// A file before and after, whole, as `(original, current)`. A side that
    /// does not exist at its commit (added or deleted file, no parent) is empty.
    pub fn pr_file_pair(
        &self,
        item: &Item,
        old: &str,
        new: &str,
        path: &str,
    ) -> Result<(String, String), String> {
        if path.is_empty() || path.split('/').any(|part| part == ".." || part.is_empty()) {
            return Err("Invalid path".into());
        }
        let (owner, name) = repo_parts(&item.repo)?;
        let root = self.project(item);
        let side = |sha: &str| -> Result<String, String> {
            if sha.is_empty() {
                return Ok(String::new());
            }
            if !valid_sha(sha) {
                return Err("Unknown commit".into());
            }
            let endpoint = format!(
                "repos/{owner}/{name}/contents/{}?ref={sha}",
                encode_path(path)
            );
            match gh(
                root,
                &["api", "-H", "Accept: application/vnd.github.raw", &endpoint],
            ) {
                Ok(text) if text.len() > MAX_COMMIT_DIFF_BYTES => {
                    Err("This file is too large to show whole".into())
                }
                Ok(text) if text.contains('\0') => Err("This is a binary file".into()),
                Ok(text) => Ok(text),
                Err(error) if error.contains("404") || error.contains("Not Found") => {
                    Ok(String::new())
                }
                Err(error) => Err(error),
            }
        };
        Ok((side(old)?, side(new)?))
    }

    /// What one commit of the pull request changed.
    pub fn pr_commit_diff(&self, item: &Item, sha: &str) -> Result<Diff, String> {
        let sha = sha.trim();
        if item.provider != "github"
            || !(7..=40).contains(&sha.len())
            || !sha.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err("Unknown commit".into());
        }
        let (owner, name) = repo_parts(&item.repo)?;
        let json = gh(
            self.project(item),
            &["api", &format!("repos/{owner}/{name}/commits/{sha}")],
        )?;
        parse_commit_diff(&json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commits_keep_order_and_prefer_the_login() {
        let json = r#"{"commits":[
            {"oid":"1111111aaaa","messageHeadline":"first","committedDate":"2026-11-01T10:00:00Z",
             "authors":[{"login":"ahmet","name":"Ahmet K"}]},
            {"oid":"2222222bbbb","messageHeadline":"second","committedDate":"2026-11-02T10:00:00Z",
             "authors":[{"login":"","name":"Only Name"}]},
            {"oid":"","messageHeadline":"dropped"}]}"#;
        let commits = parse_commits(json).unwrap();
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].short_sha, "1111111");
        assert_eq!(commits[0].author, "ahmet");
        assert_eq!(commits[1].author, "Only Name");
        assert_eq!(commits[1].headline, "second");
    }

    #[test]
    fn a_commit_becomes_a_patch_the_review_can_split_per_file() {
        let json = r#"{"files":[
            {"filename":"src/a.rs","additions":1,"deletions":1,"patch":"@@ -1 +1 @@\n-old\n+new"},
            {"filename":"logo.png","additions":0,"deletions":0}]}"#;
        let diff = parse_commit_diff(json).unwrap();
        assert_eq!((diff.additions, diff.deletions), (1, 1));
        assert_eq!(diff.files.len(), 2);
        let sections: Vec<&str> = diff.patch.split("diff --git ").skip(1).collect();
        assert_eq!(sections.len(), 2);
        assert!(sections[0].contains("+++ b/src/a.rs") && sections[0].contains("+new"));
        assert!(sections[1].contains("Binary files"));
        assert!(!diff.truncated);
    }

    #[test]
    fn paths_are_encoded_per_segment_for_the_contents_api() {
        assert_eq!(encode_path("src/app/a b#c.rs"), "src/app/a%20b%23c.rs");
        assert_eq!(encode_path("ünï/x.py"), "%C3%BCn%C3%AF/x.py");
        assert_eq!(encode_path("plain-name_1.rs"), "plain-name_1.rs");
    }

    #[test]
    fn a_pull_requests_base_and_head_must_be_commit_ids() {
        let json = r#"{"baseRefOid":"1111111aaaa","headRefOid":"2222222bbbb"}"#;
        assert_eq!(
            parse_pr_oids(json).unwrap(),
            ("1111111aaaa".to_string(), "2222222bbbb".to_string())
        );
        assert!(parse_pr_oids(r#"{"baseRefOid":"","headRefOid":"2222222bbbb"}"#).is_err());
        assert!(parse_pr_oids(r#"{"baseRefOid":"zz","headRefOid":"2222222bbbb"}"#).is_err());
    }

    #[test]
    fn repository_and_commit_names_are_checked() {
        assert!(repo_parts("socradar-io/socradar").is_ok());
        assert!(repo_parts("a/b/c").is_err());
        assert!(repo_parts("owner/na me").is_err());
        assert!(repo_parts("noslash").is_err());
    }
}
