//! Splitting a SQL script into statements at top-level `;`, aware of quoted
//! strings and identifiers, comments and PostgreSQL dollar quoting.
//! Offsets are byte offsets into the script.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Statement {
    /// Byte range of the statement text (surrounding whitespace trimmed, `;` excluded).
    pub from: usize,
    pub to: usize,
    pub text: String,
}

/// Every statement of `text` that holds more than whitespace and comments.
pub fn split(text: &str) -> Vec<Statement> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut start = 0;
    let mut has_code = false;
    let mut i = 0;
    let finish = |out: &mut Vec<Statement>, from: usize, to: usize, has_code: bool| {
        if !has_code {
            return;
        }
        let slice = &text[from..to];
        let lead = slice.len() - slice.trim_start().len();
        let trimmed = slice.trim();
        if !trimmed.is_empty() {
            out.push(Statement {
                from: from + lead,
                to: from + lead + trimmed.len(),
                text: trimmed.to_owned(),
            });
        }
    };
    while i < bytes.len() {
        match bytes[i] {
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                // Block comments nest in PostgreSQL.
                let mut depth = 1;
                i += 2;
                while i < bytes.len() && depth > 0 {
                    if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
                        depth += 1;
                        i += 2;
                    } else if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
                        depth -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
            }
            quote @ (b'\'' | b'"') => {
                has_code = true;
                let escapes = quote == b'\'' && i > 0 && matches!(bytes[i - 1], b'e' | b'E');
                i += 1;
                while i < bytes.len() {
                    if escapes && bytes[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == quote {
                        if bytes.get(i + 1) == Some(&quote) {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
                i += 1;
            }
            b'$' => {
                has_code = true;
                match dollar_tag(bytes, i) {
                    Some(end) => {
                        let tag = &text[i..end];
                        i = match text[end..].find(tag) {
                            Some(close) => end + close + tag.len(),
                            None => bytes.len(),
                        };
                    }
                    None => i += 1,
                }
            }
            b';' => {
                finish(&mut out, start, i, has_code);
                start = i + 1;
                has_code = false;
                i += 1;
            }
            b if b.is_ascii_whitespace() => i += 1,
            _ => {
                has_code = true;
                i += 1;
            }
        }
    }
    finish(&mut out, start, bytes.len().min(text.len()), has_code);
    out
}

/// End (exclusive) of a dollar-quote opener (`$$` or `$tag$`) at `at`.
fn dollar_tag(bytes: &[u8], at: usize) -> Option<usize> {
    // `$1` parameters and identifiers like `a$b` are not openers.
    if at > 0 && (bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'_') {
        return None;
    }
    let mut j = at + 1;
    while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
        if j == at + 1 && bytes[j].is_ascii_digit() {
            return None;
        }
        j += 1;
    }
    (bytes.get(j) == Some(&b'$')).then_some(j + 1)
}

/// The statement the cursor (a byte offset) is in or right after, for "run at cursor".
pub fn at_cursor(text: &str, cursor: usize) -> Option<Statement> {
    let all = split(text);
    all.iter()
        .find(|s| cursor >= s.from && cursor <= s.to + 1)
        .or_else(|| all.iter().rev().find(|s| s.to < cursor))
        .or_else(|| all.first())
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(sql: &str) -> Vec<String> {
        split(sql).into_iter().map(|s| s.text).collect()
    }

    #[test]
    fn splits_at_top_level_semicolons_only() {
        assert_eq!(
            texts("select 1; select ';' ; select \";\""),
            ["select 1", "select ';'", "select \";\""]
        );
        assert_eq!(
            texts("select 1 -- a;b\n; select 2 /* ; /* nested ; */ ; */"),
            ["select 1 -- a;b", "select 2 /* ; /* nested ; */ ; */"]
        );
    }

    #[test]
    fn dollar_quoting_and_escape_strings() {
        assert_eq!(
            texts("create function f() returns int as $$ begin; return 1; end $$ language plpgsql; select 2"),
            [
                "create function f() returns int as $$ begin; return 1; end $$ language plpgsql",
                "select 2"
            ]
        );
        assert_eq!(
            texts("select $tag$ a; $$ b $tag$; select $1"),
            ["select $tag$ a; $$ b $tag$", "select $1"]
        );
        assert_eq!(
            texts(r"select E'it\'s; ok'; select 2"),
            [r"select E'it\'s; ok'", "select 2"]
        );
    }

    #[test]
    fn comments_alone_are_not_statements_and_offsets_are_bytes() {
        assert!(split("-- nothing\n/* x */ ;;  ").is_empty());
        let sql = "select 'é';\n  select 2;";
        let all = split(sql);
        assert_eq!(&sql[all[1].from..all[1].to], "select 2");
    }

    #[test]
    fn cursor_picks_the_statement_it_is_in_or_after() {
        let sql = "select 1;\nselect 2;\n";
        assert_eq!(at_cursor(sql, 3).unwrap().text, "select 1");
        assert_eq!(at_cursor(sql, 12).unwrap().text, "select 2");
        assert_eq!(at_cursor(sql, sql.len()).unwrap().text, "select 2");
        assert!(at_cursor("  ", 1).is_none());
    }
}
