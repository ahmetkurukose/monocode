//! The changed-files tree shared by the Commit and Update windows: folders
//! first, names compared case-insensitively, and a folder holding only one
//! folder shown as a single `a/b/c` line.
use monocode_core::scm::ChangedFile;
use std::collections::HashSet;

#[derive(Clone)]
pub(crate) enum TreeRow {
    Dir {
        key: String,
        name: String,
        depth: usize,
        files: Vec<usize>,
        open: bool,
    },
    File {
        ix: usize,
        depth: usize,
    },
}

#[derive(Default)]
pub(crate) struct Node {
    dirs: Vec<(String, Node)>,
    files: Vec<usize>,
}
impl Node {
    pub(crate) fn insert(&mut self, parts: &[&str], ix: usize) {
        match parts {
            [] => {}
            [_] => self.files.push(ix),
            [dir, rest @ ..] => {
                let at = match self.dirs.iter().position(|(name, _)| name == dir) {
                    Some(at) => at,
                    None => {
                        self.dirs.push((dir.to_string(), Node::default()));
                        self.dirs.len() - 1
                    }
                };
                self.dirs[at].1.insert(rest, ix);
            }
        }
    }
    pub(crate) fn all(&self, out: &mut Vec<usize>) {
        out.extend(&self.files);
        for (_, dir) in &self.dirs {
            dir.all(out);
        }
    }
    pub(crate) fn flatten(
        &mut self,
        files: &[ChangedFile],
        group: &str,
        prefix: &str,
        depth: usize,
        collapsed: &HashSet<String>,
        out: &mut Vec<TreeRow>,
    ) {
        self.dirs.sort_by_key(|d| d.0.to_lowercase());
        self.files
            .sort_by_key(|&ix| basename(&files[ix].relative).to_lowercase());
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
            let key = format!("d:{group}:{path}");
            let open = !collapsed.contains(&key);
            let mut members = Vec::new();
            node.all(&mut members);
            out.push(TreeRow::Dir {
                key,
                name: label,
                depth,
                files: members,
                open,
            });
            if open {
                node.flatten(files, group, &path, depth + 1, collapsed, out);
            }
        }
        for &ix in &self.files {
            out.push(TreeRow::File { ix, depth });
        }
    }
}
pub(crate) fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}
