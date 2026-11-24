//! Toggling comments in the code editor: the lines of the selection, or the
//! caret's line, gain the language's comment marker, or lose it when every
//! one of them already has it. The language is told from the file's name.
use super::*;

/// How a language writes a comment that this editor can put on one line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Comment {
    /// From the marker to the end of the line.
    Line(&'static str),
    /// Between an opening and a closing marker.
    Block(&'static str, &'static str),
}

/// The comment syntax of the file at `path`, by its name or extension.
/// `None` for a format without comments (JSON) or one not known here.
pub(super) fn comment_for(path: &str) -> Option<Comment> {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let lower = name.to_ascii_lowercase();
    let extension = lower
        .rsplit_once('.')
        .map_or("", |(_, extension)| extension);
    let hash = Comment::Line("#");
    let slashes = Comment::Line("//");
    if matches!(
        lower.as_str(),
        "dockerfile" | "makefile" | "gnumakefile" | "rakefile" | "gemfile" | "justfile"
    ) || lower.starts_with("dockerfile.")
        || lower.starts_with(".env")
        || lower == ".gitignore"
        || lower == ".dockerignore"
    {
        return Some(hash);
    }
    Some(match extension {
        "py" | "pyi" | "pyw" | "sh" | "bash" | "zsh" | "fish" | "yaml" | "yml" | "toml" | "rb"
        | "pl" | "pm" | "r" | "cfg" | "ini" | "conf" | "properties" | "env" | "mk" | "tf"
        | "tfvars" | "ex" | "exs" | "nix" | "ps1" | "graphql" | "gql" => hash,
        "js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" | "mts" | "cts" | "rs" | "go" | "c" | "h"
        | "cc" | "cpp" | "cxx" | "hpp" | "hh" | "java" | "kt" | "kts" | "swift" | "cs"
        | "scala" | "dart" | "php" | "zig" | "proto" | "groovy" | "gradle" | "jsonc" | "json5"
        | "scss" | "sass" | "less" | "m" | "mm" | "v" | "sol" => slashes,
        "sql" | "lua" | "hs" | "elm" | "ada" => Comment::Line("--"),
        "clj" | "cljs" | "lisp" | "el" | "scm" | "asm" | "s" => Comment::Line(";"),
        "tex" | "erl" | "m4" => Comment::Line("%"),
        "vim" => Comment::Line("\""),
        "bat" | "cmd" => Comment::Line("REM"),
        "css" => Comment::Block("/*", "*/"),
        "html" | "htm" | "xml" | "svg" | "vue" | "md" | "markdown" | "xhtml" | "plist" => {
            Comment::Block("<!--", "-->")
        }
        _ => return None,
    })
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start_matches([' ', '\t']).len()
}

fn commented(line: &str, comment: Comment) -> bool {
    let body = line.trim_start_matches([' ', '\t']);
    match comment {
        Comment::Line(marker) => body.starts_with(marker),
        Comment::Block(open, close) => {
            body.starts_with(open)
                && body.trim_end().ends_with(close)
                && body.len() >= open.len() + close.len()
        }
    }
}

/// The lines with their comments toggled: taken off when every line that is
/// not blank has one, put on otherwise, at the shallowest indent among them
/// so the block stays aligned. Blank lines are left alone.
pub(super) fn toggle(lines: &[&str], comment: Comment) -> Vec<String> {
    let full: Vec<&&str> = lines
        .iter()
        .filter(|line| !line.trim().is_empty())
        .collect();
    if full.is_empty() {
        return lines.iter().map(|line| (*line).to_owned()).collect();
    }
    let uncomment = full.iter().all(|line| commented(line, comment));
    let column = full.iter().map(|line| indent_of(line)).min().unwrap_or(0);
    lines
        .iter()
        .map(|line| {
            if line.trim().is_empty() {
                return (*line).to_owned();
            }
            if uncomment {
                let at = indent_of(line);
                let (indent, body) = line.split_at(at);
                match comment {
                    Comment::Line(marker) => {
                        let rest = &body[marker.len()..];
                        format!("{indent}{}", rest.strip_prefix(' ').unwrap_or(rest))
                    }
                    Comment::Block(open, close) => {
                        let trailing = body.len() - body.trim_end().len();
                        let inner = &body[open.len()..body.len() - trailing - close.len()];
                        let inner = inner.strip_prefix(' ').unwrap_or(inner);
                        let inner = inner.strip_suffix(' ').unwrap_or(inner);
                        format!("{indent}{inner}{}", &body[body.len() - trailing..])
                    }
                }
            } else {
                let (indent, body) = line.split_at(column);
                match comment {
                    Comment::Line(marker) => format!("{indent}{marker} {body}"),
                    Comment::Block(open, close) => format!("{indent}{open} {body} {close}"),
                }
            }
        })
        .collect()
}

impl Input {
    /// The comment syntax of this editor's file, when it has one and may be
    /// edited.
    pub(super) fn comment_syntax(&self) -> Option<Comment> {
        if self.read_only {
            return None;
        }
        comment_for(self.code.as_ref()?.path())
    }

    pub(super) fn toggle_comment(
        &mut self,
        _: &ToggleComment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(comment) = self.comment_syntax() else {
            cx.propagate();
            return;
        };
        if self.marked.is_some() {
            return;
        }
        let selection = self.selection.clone();
        let start = self.content[..selection.start]
            .rfind('\n')
            .map_or(0, |at| at + 1);
        // A selection that ends at the start of a line does not take it.
        let last = if selection.end > selection.start
            && self.content.as_bytes().get(selection.end - 1) == Some(&b'\n')
        {
            selection.end - 1
        } else {
            selection.end
        };
        let end = self.content[last..]
            .find('\n')
            .map_or(self.content.len(), |n| last + n);
        let before = self.content[start..end].to_owned();
        let lines: Vec<&str> = before.split('\n').collect();
        let replacement = toggle(&lines, comment).join("\n");
        if replacement == before {
            return;
        }
        let inserted = replacement.len();
        self.replace_text_in_range(Some(self.range16(start..end)), &replacement, window, cx);
        if selection.is_empty() {
            // The caret keeps its place in the text it was in.
            let grown = inserted as isize - before.len() as isize;
            let caret = (selection.start as isize + grown).max(start as isize) as usize;
            self.move_to(caret.min(start + inserted), cx);
        } else {
            self.select_range(start..start + inserted, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toggled(text: &str, comment: Comment) -> String {
        toggle(&text.split('\n').collect::<Vec<_>>(), comment).join("\n")
    }

    #[test]
    fn a_block_gains_its_markers_at_the_shallowest_indent_and_loses_them_again() {
        let py = Comment::Line("#");
        let code = "    if ok:\n        run()\n\n    done()";
        let off = "    # if ok:\n    #     run()\n\n    # done()";
        assert_eq!(toggled(code, py), off);
        assert_eq!(toggled(off, py), code);
        // One line without a marker: the whole block is commented once more.
        assert_eq!(toggled("# a\nb", py), "# # a\n# b");
        // A marker with no space after it comes off too.
        assert_eq!(toggled("#a\n  #b", py), "a\n  b");
        assert_eq!(toggled("\n  \n", py), "\n  \n");
    }

    #[test]
    fn block_markers_wrap_each_line() {
        let css = Comment::Block("/*", "*/");
        assert_eq!(toggled("  color: red;", css), "  /* color: red; */");
        assert_eq!(toggled("  /* color: red; */", css), "  color: red;");
        let html = Comment::Block("<!--", "-->");
        assert_eq!(
            toggled("<p>a</p>\n<p>b</p>", html),
            "<!-- <p>a</p> -->\n<!-- <p>b</p> -->"
        );
    }

    #[test]
    fn the_file_name_tells_the_syntax() {
        assert_eq!(comment_for("/a/b/app.py"), Some(Comment::Line("#")));
        assert_eq!(comment_for("main.RS"), Some(Comment::Line("//")));
        assert_eq!(comment_for("query.sql"), Some(Comment::Line("--")));
        assert_eq!(comment_for("Dockerfile"), Some(Comment::Line("#")));
        assert_eq!(comment_for("style.css"), Some(Comment::Block("/*", "*/")));
        assert_eq!(comment_for("data.json"), None);
        assert_eq!(comment_for("README"), None);
    }
}
