//! Provider patches use the same virtualized native review as local changes.
use crate::diff_view::DiffFile;
use gpui::SharedString;
use monocode_core::{
    diff::{Block, Line, LineKind},
    inbox::Diff,
};
use std::{collections::HashSet, sync::Arc};

fn patch_path(section: &str) -> Option<String> {
    section
        .lines()
        .find_map(|line| line.strip_prefix("+++ "))
        .filter(|path| *path != "/dev/null")
        .map(|path| {
            let path = serde_json::from_str::<String>(path).unwrap_or_else(|_| path.to_string());
            path.strip_prefix("b/").unwrap_or(&path).to_string()
        })
}
fn hunk_start(value: &str) -> u32 {
    value
        .trim_start_matches(['+', '-'])
        .split(',')
        .next()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1)
}
fn blocks(section: &str) -> Vec<Block> {
    let (mut old, mut new) = (0, 0);
    let mut lines = vec![];
    let mut in_hunk = false;
    for raw in section.lines() {
        if raw.starts_with("@@ ") {
            let mut parts = raw.split_whitespace().skip(1);
            old = hunk_start(parts.next().unwrap_or("-1"));
            new = hunk_start(parts.next().unwrap_or("+1"));
            in_hunk = true;
            lines.push(Line {
                kind: LineKind::Hunk,
                text: raw.into(),
                old_number: None,
                new_number: None,
                chunk: None,
            });
            continue;
        }
        if !in_hunk {
            continue;
        }
        let (kind, old_number, new_number) = match raw.as_bytes().first() {
            Some(b'+') => {
                let n = new;
                new += 1;
                (LineKind::Add, None, Some(n))
            }
            Some(b'-') => {
                let n = old;
                old += 1;
                (LineKind::Del, Some(n), None)
            }
            Some(b' ') => {
                let (o, n) = (old, new);
                old += 1;
                new += 1;
                (LineKind::Context, Some(o), Some(n))
            }
            _ => continue,
        };
        lines.push(Line {
            kind,
            text: raw[1..].into(),
            old_number,
            new_number,
            chunk: None,
        });
    }
    if lines.is_empty() {
        vec![]
    } else {
        vec![Block::Hunk { lines, chunk: None }]
    }
}
pub(super) fn models(diff: &Diff) -> Vec<DiffFile> {
    let sections = diff.patch.split("diff --git ").skip(1).collect::<Vec<_>>();
    diff.files
        .iter()
        .enumerate()
        .map(|(ix, file)| {
            let section = sections
                .iter()
                .find(|section| patch_path(section).as_deref() == Some(file.path.as_str()))
                .copied()
                .or_else(|| sections.get(ix).copied())
                .unwrap_or("");
            let binary = section.contains("Binary files ") || section.contains("GIT binary patch");
            DiffFile {
                id: file_id(&file.path),
                path: file.path.clone(),
                label: file.path.clone(),
                binary,
                too_large: diff.truncated && section.is_empty(),
                empty_message: if section.is_empty() && !diff.truncated {
                    Some("No textual diff".into())
                } else {
                    None
                },
                additions: file.additions.max(0).min(u32::MAX as i64) as u32,
                deletions: file.deletions.max(0).min(u32::MAX as i64) as u32,
                blocks: Arc::new(blocks(section)),
                can_stage: false,
                can_discard: false,
                can_stage_hunk: false,
            }
        })
        .collect()
}

pub(super) fn file_id(path: &str) -> SharedString {
    SharedString::from(format!("inbox-diff-{path}"))
}

/// One visible line of the changed-files tree.
pub(super) struct TreeRow {
    pub depth: usize,
    pub label: String,
    /// The full path: a directory's key for folding, or the file itself.
    pub path: String,
    pub dir: bool,
    pub additions: i64,
    pub deletions: i64,
}
#[derive(Default)]
struct Node {
    dirs: Vec<(String, Node)>,
    files: Vec<(String, String, i64, i64)>,
}
impl Node {
    fn insert(&mut self, parts: &[&str], full: &str, additions: i64, deletions: i64) {
        match parts {
            [] => {}
            [name] => self
                .files
                .push((name.to_string(), full.to_string(), additions, deletions)),
            [dir, rest @ ..] => {
                let at = match self.dirs.iter().position(|(n, _)| n == dir) {
                    Some(at) => at,
                    None => {
                        self.dirs.push((dir.to_string(), Node::default()));
                        self.dirs.len() - 1
                    }
                };
                self.dirs[at].1.insert(rest, full, additions, deletions);
            }
        }
    }
    fn total(&self) -> (i64, i64) {
        let mut sum = (0, 0);
        for (_, _, a, d) in &self.files {
            sum = (sum.0 + a, sum.1 + d);
        }
        for (_, dir) in &self.dirs {
            let (a, d) = dir.total();
            sum = (sum.0 + a, sum.1 + d);
        }
        sum
    }
    fn flatten(
        &mut self,
        prefix: &str,
        depth: usize,
        collapsed: &HashSet<String>,
        out: &mut Vec<TreeRow>,
    ) {
        self.dirs.sort_by_key(|d| d.0.to_lowercase());
        self.files.sort_by_key(|f| f.0.to_lowercase());
        for (name, dir) in &mut self.dirs {
            // A folder holding only one folder reads as one `a/b/c` line.
            let (mut label, mut node) = (name.clone(), dir);
            while node.files.is_empty() && node.dirs.len() == 1 {
                let (next, child) = &mut node.dirs[0];
                label = format!("{label}/{next}");
                node = child;
            }
            let path = if prefix.is_empty() {
                label.clone()
            } else {
                format!("{prefix}/{label}")
            };
            let (additions, deletions) = node.total();
            out.push(TreeRow {
                depth,
                label,
                path: path.clone(),
                dir: true,
                additions,
                deletions,
            });
            if !collapsed.contains(&path) {
                node.flatten(&path, depth + 1, collapsed, out);
            }
        }
        for (name, full, additions, deletions) in &self.files {
            out.push(TreeRow {
                depth,
                label: name.clone(),
                path: full.clone(),
                dir: false,
                additions: *additions,
                deletions: *deletions,
            });
        }
    }
}
/// Folders first, names compared case-insensitively, single-child folders merged.
pub(super) fn tree_rows(diff: &Diff, collapsed: &HashSet<String>) -> Vec<TreeRow> {
    let mut root = Node::default();
    for file in &diff.files {
        let parts: Vec<&str> = file.path.split('/').collect();
        root.insert(&parts, &file.path, file.additions, file.deletions);
    }
    let mut rows = vec![];
    root.flatten("", 0, collapsed, &mut rows);
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use monocode_core::inbox::DiffFile as Changed;

    fn changed(path: &str, additions: i64, deletions: i64) -> Changed {
        Changed {
            path: path.into(),
            additions,
            deletions,
        }
    }

    #[test]
    fn tree_merges_single_child_folders_and_folds() {
        let diff = Diff {
            files: vec![
                changed("src/app/model/a.ts", 2, 1),
                changed("src/app/model/b.ts", 1, 0),
                changed("README.md", 3, 0),
            ],
            ..Default::default()
        };
        let rows = tree_rows(&diff, &HashSet::new());
        let labels = rows
            .iter()
            .map(|r| (r.depth, r.label.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(
            labels,
            [
                (0, "src/app/model"),
                (1, "a.ts"),
                (1, "b.ts"),
                (0, "README.md")
            ]
        );
        assert_eq!((rows[0].additions, rows[0].deletions), (3, 1));
        let folded = tree_rows(&diff, &HashSet::from(["src/app/model".to_string()]));
        assert_eq!(folded.len(), 2);
    }
}
