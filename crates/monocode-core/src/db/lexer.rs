//! A PostgreSQL lexer for the console: words, quoted identifiers, strings
//! (including `$tag$…$tag$` and `E'…'`), numbers, parameters and punctuation,
//! with comments kept apart. Offsets are byte offsets.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Word,
    Quoted,
    String,
    Number,
    /// `$1` or `:name`.
    Param,
    Punct,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: Kind,
    pub text: String,
    pub from: usize,
    pub to: usize,
    /// Upper-cased text of a word, for keyword checks (empty otherwise).
    pub upper: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    pub from: usize,
    pub to: usize,
}

#[derive(Debug, Clone, Default)]
pub struct Lexed {
    pub tokens: Vec<Token>,
    pub comments: Vec<Comment>,
}

const TWO_CHAR: [&str; 9] = ["::", "<=", ">=", "<>", "!=", "||", "->", "=>", ":="];

fn word_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

fn word_part(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

pub fn lex(text: &str) -> Lexed {
    let bytes = text.as_bytes();
    let mut out = Lexed::default();
    let mut i = 0;
    let push = |out: &mut Lexed, kind: Kind, from: usize, to: usize| {
        let t = &text[from..to];
        out.tokens.push(Token {
            kind,
            text: t.to_owned(),
            from,
            to,
            upper: if kind == Kind::Word {
                t.to_uppercase()
            } else {
                String::new()
            },
        });
    };
    // End of a quoted run starting at `start` (a doubled closing quote is an escape).
    let quoted = |start: usize, close: u8, backslash: bool| -> usize {
        let mut j = start + 1;
        while j < bytes.len() {
            if backslash && bytes[j] == b'\\' {
                j += 2;
            } else if bytes[j] == close {
                if bytes.get(j + 1) == Some(&close) {
                    j += 2;
                } else {
                    return j + 1;
                }
            } else {
                j += 1;
            }
        }
        bytes.len()
    };
    while i < bytes.len() {
        let c = text[i..].chars().next().unwrap_or(' ');
        let next = bytes.get(i + 1).copied();
        if c.is_whitespace() {
            i += c.len_utf8();
        } else if c == '-' && next == Some(b'-') {
            let end = text[i..].find('\n').map_or(text.len(), |n| i + n);
            out.comments.push(Comment { from: i, to: end });
            i = end;
        } else if c == '/' && next == Some(b'*') {
            let mut depth = 0;
            let mut j = i;
            while j < bytes.len() {
                if text[j..].starts_with("/*") {
                    depth += 1;
                    j += 2;
                } else if text[j..].starts_with("*/") {
                    depth -= 1;
                    j += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    j += text[j..].chars().next().map_or(1, char::len_utf8);
                }
            }
            let end = j.min(text.len());
            out.comments.push(Comment { from: i, to: end });
            i = end;
        } else if c == '\'' {
            let escapes = i > 0
                && matches!(bytes[i - 1], b'e' | b'E')
                && out
                    .tokens
                    .last()
                    .is_some_and(|t| t.kind == Kind::Word && t.to == i);
            if escapes {
                // `E'…'` is one string token including its prefix.
                out.tokens.pop();
                let from = i - 1;
                let to = quoted(i, b'\'', true);
                push(&mut out, Kind::String, from, to);
                i = to;
            } else {
                let to = quoted(i, b'\'', false);
                push(&mut out, Kind::String, i, to);
                i = to;
            }
        } else if c == '"' {
            let to = quoted(i, b'"', false);
            push(&mut out, Kind::Quoted, i, to);
            i = to;
        } else if c == '$' {
            let tag_end = {
                let mut j = i + 1;
                while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                    if j == i + 1 && bytes[j].is_ascii_digit() {
                        break;
                    }
                    j += 1;
                }
                (bytes.get(j) == Some(&b'$')).then_some(j + 1)
            };
            if let Some(open) = tag_end {
                let tag = &text[i..open];
                let end = text[open..]
                    .find(tag)
                    .map_or(text.len(), |n| open + n + tag.len());
                push(&mut out, Kind::String, i, end);
                i = end;
            } else {
                let mut j = i + 1;
                while j < bytes.len() && bytes[j].is_ascii_digit() {
                    j += 1;
                }
                push(&mut out, Kind::Param, i, j);
                i = j;
            }
        } else if c.is_ascii_digit() || (c == '.' && next.is_some_and(|n| n.is_ascii_digit())) {
            let mut j = i + 1;
            while j < bytes.len()
                && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'.' || bytes[j] == b'_')
            {
                j += 1;
            }
            push(&mut out, Kind::Number, i, j);
            i = j;
        } else if word_start(c) {
            let mut j = i + c.len_utf8();
            while j < text.len() {
                let ch = text[j..].chars().next().unwrap_or(' ');
                if !word_part(ch) {
                    break;
                }
                j += ch.len_utf8();
            }
            push(&mut out, Kind::Word, i, j);
            i = j;
        } else if c == ':'
            && text[i + 1..].chars().next().is_some_and(word_start)
            && !(i > 0 && bytes[i - 1] == b':')
            && !text[i + 1..].starts_with(':')
        {
            let mut j = i + 1;
            while j < text.len() {
                let ch = text[j..].chars().next().unwrap_or(' ');
                if !(ch.is_alphanumeric() || ch == '_') {
                    break;
                }
                j += ch.len_utf8();
            }
            push(&mut out, Kind::Param, i, j);
            i = j;
        } else {
            let two = text.get(i..i + 2).unwrap_or("");
            let len = if TWO_CHAR.contains(&two) {
                2
            } else {
                c.len_utf8()
            };
            push(&mut out, Kind::Punct, i, i + len);
            i += len;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(sql: &str) -> Vec<(Kind, String)> {
        lex(sql)
            .tokens
            .into_iter()
            .map(|t| (t.kind, t.text))
            .collect()
    }

    #[test]
    fn lexes_words_strings_numbers_and_params() {
        let t = kinds("select a, 'x;y', 12.5 from t where id = $1 and n = :name");
        assert!(t.contains(&(Kind::String, "'x;y'".into())));
        assert!(t.contains(&(Kind::Number, "12.5".into())));
        assert!(t.contains(&(Kind::Param, "$1".into())));
        assert!(t.contains(&(Kind::Param, ":name".into())));
    }

    #[test]
    fn casts_are_not_parameters_and_comments_are_apart() {
        let lexed = lex("select x::int, y::text -- :nope\n, /* $1 */ z");
        assert!(lexed.tokens.iter().all(|t| t.kind != Kind::Param));
        assert_eq!(lexed.comments.len(), 2);
        assert!(lexed.tokens.iter().any(|t| t.text == "::"));
    }

    #[test]
    fn dollar_quotes_escape_strings_and_quoted_identifiers_are_single_tokens() {
        let t = kinds("select $q$ it's $1 $q$, E'a\\'b', \"My \"\"Col\"\"\"");
        assert_eq!(t[1], (Kind::String, "$q$ it's $1 $q$".into()));
        assert_eq!(t[3], (Kind::String, "E'a\\'b'".into()));
        assert_eq!(t[5].0, Kind::Quoted);
        assert_eq!(t.iter().filter(|(k, _)| *k == Kind::Param).count(), 0);
    }

    #[test]
    fn offsets_are_bytes_and_cover_the_token() {
        let sql = "select 'é' as x";
        for t in lex(sql).tokens {
            assert_eq!(&sql[t.from..t.to], t.text);
        }
    }

    #[test]
    fn unterminated_input_does_not_run_past_the_text() {
        for sql in [
            "select 'open",
            "/* open",
            "select $$ open",
            "select \"open",
            "select E'open",
        ] {
            for t in lex(sql).tokens {
                assert!(t.to <= sql.len(), "{sql}");
            }
        }
    }
}
