//! Reformat SQL: clauses on their own lines, select / set / values items one
//! per line aligned after their keyword, joins indented under FROM, AND / OR
//! of WHERE right-aligned, subqueries aligned after their parenthesis,
//! CREATE TABLE bodies one definition per line. Keyword case and comments
//! are kept; each statement is replaced on its own, text between statements
//! is left alone. A port of openidea's formatter, for PostgreSQL.

use super::lexer::{lex, Kind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub from: usize,
    pub to: usize,
    pub insert: String,
}

#[derive(Clone)]
struct Item {
    kind: ItemKind,
    text: String,
    upper: String,
    from: usize,
    to: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ItemKind {
    Word,
    Quoted,
    String,
    Number,
    Param,
    Punct,
    Comment,
}

impl Item {
    fn punct(&self, p: &str) -> bool {
        self.kind == ItemKind::Punct && self.text == p
    }
}

const RESERVED: &str = "ALL AND ANY AS ASC BETWEEN BY CASE CAST CHECK COLLATE COLUMN CONSTRAINT CREATE CROSS CURRENT_DATE \
CURRENT_TIME CURRENT_TIMESTAMP DEFAULT DELETE DESC DISTINCT DROP ELSE END EXCEPT EXISTS FALSE FETCH FOR \
FOREIGN FROM FULL GRANT GROUP HAVING IN INNER INSERT INTERSECT INTO IS JOIN LEFT LIKE LIMIT NATURAL NOT \
NULL OFFSET ON OR ORDER OUTER PRIMARY REFERENCES RIGHT SELECT SET TABLE THEN TO TRUE UNION UNIQUE UPDATE \
USING VALUES WHEN WHERE WINDOW WITH";

fn reserved(word: &str) -> bool {
    let upper = word.to_uppercase();
    RESERVED.split(' ').any(|w| w == upper)
}

const NEWLINE_CLAUSES: [&str; 15] = [
    "SELECT",
    "FROM",
    "WHERE",
    "GROUP",
    "HAVING",
    "ORDER",
    "LIMIT",
    "OFFSET",
    "WINDOW",
    "RETURNING",
    "VALUES",
    "SET",
    "UNION",
    "INTERSECT",
    "EXCEPT",
];
const JOIN_START: [&str; 6] = ["JOIN", "LEFT", "RIGHT", "FULL", "INNER", "CROSS"];
const JOIN_MODIFIERS: [&str; 6] = ["LEFT", "RIGHT", "FULL", "INNER", "CROSS", "OUTER"];

/// Where a list item continues: the width of `select `, `from `, `set `, `values `.
fn item_column(clause: &str) -> Option<usize> {
    match clause {
        "SELECT" | "VALUES" => Some(7),
        "FROM" => Some(5),
        "SET" => Some(4),
        _ => None,
    }
}

/// Edits formatting every statement (or those overlapping `range`).
pub fn format_sql(text: &str, range: Option<(usize, usize)>) -> Vec<Edit> {
    let lexed = lex(text);
    let mut items: Vec<Item> = lexed
        .tokens
        .iter()
        .map(|t| Item {
            kind: match t.kind {
                Kind::Word => ItemKind::Word,
                Kind::Quoted => ItemKind::Quoted,
                Kind::String => ItemKind::String,
                Kind::Number => ItemKind::Number,
                Kind::Param => ItemKind::Param,
                Kind::Punct => ItemKind::Punct,
            },
            text: t.text.clone(),
            upper: t.upper.clone(),
            from: t.from,
            to: t.to,
        })
        .chain(lexed.comments.iter().map(|c| Item {
            kind: ItemKind::Comment,
            text: text[c.from..c.to].to_owned(),
            upper: String::new(),
            from: c.from,
            to: c.to,
        }))
        .collect();
    items.sort_by_key(|i| i.from);

    let mut edits = Vec::new();
    let mut start = 0;
    let mut depth = 0usize;
    let flush = |edits: &mut Vec<Edit>, start: usize, end: usize| {
        // Comments before a statement stay where they are.
        let mut a = start;
        while a < end && items[a].kind == ItemKind::Comment {
            a += 1;
        }
        if a >= end {
            return;
        }
        let first = &items[a];
        let last = &items[end - 1];
        if let Some((from, to)) = range {
            if last.to < from || first.from > to {
                return;
            }
        }
        let line = text[..first.from].rfind('\n').map_or(0, |n| n + 1);
        let lead = &text[line..first.from];
        let indent = if lead.chars().all(|c| c == ' ' || c == '\t') {
            lead
        } else {
            ""
        };
        edits.push(Edit {
            from: first.from,
            to: last.to,
            insert: format_statement(&items[a..end], indent),
        });
    };
    for (i, it) in items.iter().enumerate() {
        if it.punct("(") {
            depth += 1;
        } else if it.punct(")") {
            depth = depth.saturating_sub(1);
        } else if it.punct(";") && depth == 0 {
            flush(&mut edits, start, i + 1);
            start = i + 1;
        }
    }
    flush(&mut edits, start, items.len());
    edits
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FrameKind {
    Query,
    Create,
    Plain,
}

struct Frame {
    kind: FrameKind,
    /// Column where the block's clauses start.
    indent: usize,
    clause: String,
    /// Tokens written in this frame so far.
    count: usize,
    /// A BETWEEN waits for its AND.
    between: bool,
}

fn frame(kind: FrameKind, indent: usize) -> Frame {
    Frame {
        kind,
        indent,
        clause: String::new(),
        count: 0,
        between: false,
    }
}

fn format_statement(items: &[Item], base: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    let create = is_create_table(items);
    let mut frames = vec![frame(FrameKind::Query, 0)];
    let mut prev: Option<&Item> = None;
    let mut unary = false;

    macro_rules! newline {
        ($column:expr) => {{
            lines.push(line.trim_end().to_owned());
            line = " ".repeat($column);
        }};
    }
    let at_line_start = |line: &str| line.trim().is_empty();

    for (i, it) in items.iter().enumerate() {
        let fi = frames.len() - 1;
        let word = if it.kind == ItemKind::Word {
            it.upper.as_str()
        } else {
            ""
        };
        let is_query = frames[fi].kind == FrameKind::Query;
        let after_line_comment =
            prev.is_some_and(|p| p.kind == ItemKind::Comment && p.text.starts_with("--"));

        // Line breaks before the token.
        if after_line_comment {
            let column = match (is_query, item_column(&frames[fi].clause)) {
                (true, Some(extra)) => frames[fi].indent + extra,
                _ => frames[fi].indent,
            };
            newline!(column);
        } else if it.punct(")") && frames[fi].kind == FrameKind::Create {
            newline!(frames[frames.len() - 2].indent);
        } else if is_query && !word.is_empty() && frames[fi].count > 0 {
            let f = &mut frames[fi];
            if NEWLINE_CLAUSES.contains(&word) {
                if !at_line_start(&line) {
                    let indent = f.indent;
                    newline!(indent);
                }
            } else if word == "ON" && f.clause == "VALUES" {
                let indent = f.indent;
                newline!(indent);
            } else if JOIN_START.contains(&word)
                && !prev.is_some_and(|p| JOIN_MODIFIERS.contains(&p.upper.as_str()))
                && f.clause == "FROM"
            {
                let indent = f.indent + 9;
                newline!(indent);
            } else if (word == "AND" || word == "OR")
                && (f.clause == "WHERE" || f.clause == "HAVING")
            {
                if word == "AND" && f.between {
                    f.between = false;
                } else {
                    let indent = f.indent + if word == "AND" { 2 } else { 3 };
                    newline!(indent);
                }
            } else if matches!(word, "INSERT" | "UPDATE" | "DELETE")
                && prev.is_some_and(|p| p.punct(")"))
            {
                let indent = f.indent;
                newline!(indent);
            }
        }

        // The token itself; a comma after a line comment moves before it.
        if after_line_comment && it.punct(",") {
            if let (Some(last), Some(p)) = (lines.last_mut(), prev) {
                if let Some(at) = last.rfind(p.text.as_str()) {
                    *last = format!("{}, {}", last[..at].trim_end(), &last[at..]);
                }
            }
        } else {
            if !at_line_start(&line)
                && space_before(prev, it, i.checked_sub(2).and_then(|n| items.get(n)), unary)
            {
                line.push(' ');
            }
            line.push_str(&it.text);
        }
        unary = it.kind == ItemKind::Punct
            && (it.text == "-" || it.text == "+")
            && prev.is_none_or(operator_like);

        // Bookkeeping and breaks after the token.
        let fi = frames.len() - 1;
        frames[fi].count += 1;
        let is_query = frames[fi].kind == FrameKind::Query;
        if is_query && !word.is_empty() {
            if NEWLINE_CLAUSES.contains(&word) {
                frames[fi].clause = word.to_owned();
            } else if word == "ON" && frames[fi].clause == "VALUES" {
                frames[fi].clause = "CONFLICT".into();
            }
            if word == "BETWEEN" {
                frames[fi].between = true;
            }
        }
        if it.punct("(") {
            let next = items.get(i + 1);
            if create && frames.len() == 1 && is_create_body_paren(items, i) {
                // `create table t\n(\n    a int,\n    …\n)`.
                line.pop();
                line = line.trim_end().to_owned();
                let indent = frames[fi].indent;
                newline!(indent);
                line.push('(');
                frames.push(frame(FrameKind::Create, indent + 4));
                newline!(indent + 4);
            } else if next.is_some_and(|n| {
                n.kind == ItemKind::Word && (n.upper == "SELECT" || n.upper == "WITH")
            }) {
                frames.push(frame(FrameKind::Query, line.chars().count()));
            } else {
                let indent = frames[fi].indent;
                frames.push(frame(FrameKind::Plain, indent));
            }
        } else if it.punct(")") && frames.len() > 1 {
            frames.pop();
        } else if it.punct(",") {
            let f = &frames[fi];
            if at_line_start(&line) {
                // Already broken after a line comment.
            } else if f.kind == FrameKind::Create {
                let indent = f.indent;
                newline!(indent);
            } else if f.kind == FrameKind::Query {
                if let Some(extra) = item_column(&f.clause) {
                    let indent = f.indent + extra;
                    newline!(indent);
                }
            }
        }
        prev = Some(it);
    }
    lines.push(line.trim_end().to_owned());
    lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            if i == 0 {
                l.trim_start().to_owned()
            } else if l.is_empty() {
                String::new()
            } else {
                format!("{base}{l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn operator_like(t: &Item) -> bool {
    if t.kind == ItemKind::Punct {
        return t.text != ")";
    }
    t.kind == ItemKind::Word && reserved(&t.text)
}

fn space_before(prev: Option<&Item>, it: &Item, before_prev: Option<&Item>, unary: bool) -> bool {
    let Some(prev) = prev else {
        return false;
    };
    if unary {
        return false;
    }
    if it.kind == ItemKind::Comment || prev.kind == ItemKind::Comment {
        return true;
    }
    if it.kind == ItemKind::Punct && [",", ";", ")", ".", "::"].contains(&it.text.as_str()) {
        return false;
    }
    if prev.kind == ItemKind::Punct && ["(", ".", "::"].contains(&prev.text.as_str()) {
        return false;
    }
    if it.punct("(") {
        if prev.kind == ItemKind::Word && prev.upper == "CAST" {
            return false;
        }
        if prev.kind == ItemKind::Word && !reserved(&prev.text) {
            return is_insert_target(before_prev);
        }
        if prev.kind == ItemKind::Quoted {
            return is_insert_target(before_prev);
        }
        return true;
    }
    if it.punct("*") && prev.punct(".") {
        return false;
    }
    true
}

/// `into users (` — a column list, written with a space.
fn is_insert_target(before_prev: Option<&Item>) -> bool {
    before_prev.is_some_and(|b| {
        b.kind == ItemKind::Word && matches!(b.upper.as_str(), "INTO" | "TABLE" | "EXISTS")
    })
}

fn is_create_table(items: &[Item]) -> bool {
    let words: Vec<&str> = items
        .iter()
        .filter(|t| t.kind == ItemKind::Word)
        .take(3)
        .map(|t| t.upper.as_str())
        .collect();
    words.first() == Some(&"CREATE") && words.contains(&"TABLE")
}

/// The first top-level `(` of `CREATE TABLE name (`, not `AS (select…)`.
fn is_create_body_paren(items: &[Item], i: usize) -> bool {
    if items[..i].iter().any(|t| t.punct("(")) {
        return false;
    }
    i > 0
        && matches!(items[i - 1].kind, ItemKind::Word | ItemKind::Quoted)
        && items[i - 1].upper != "AS"
}

/// `text` with every statement formatted.
pub fn formatted(text: &str) -> String {
    let mut out = text.to_owned();
    for e in format_sql(text, None).into_iter().rev() {
        out.replace_range(e.from..e.to, &e.insert);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(sql: &str) -> String {
        formatted(sql)
    }

    #[test]
    fn clauses_go_on_their_own_lines_and_select_items_align() {
        assert_eq!(
            f("select a, b, count(*) from t where x = 1 and y = 2 group by a order by b limit 5"),
            "select a,\n       b,\n       count(*)\nfrom t\nwhere x = 1\n  and y = 2\ngroup by a\norder by b\nlimit 5"
        );
    }

    #[test]
    fn joins_sit_under_from_and_or_is_right_aligned() {
        assert_eq!(
            f("select * from a inner join b on a.id = b.id left join c on c.id = a.id where p or q"),
            "select *\nfrom a\n         inner join b on a.id = b.id\n         left join c on c.id = a.id\nwhere p\n   or q"
        );
    }

    #[test]
    fn between_keeps_its_and_and_casts_and_calls_stay_tight() {
        assert_eq!(
            f("select cast(x as int), y::text from t where z between 1 and 5 and w"),
            "select cast(x as int),\n       y::text\nfrom t\nwhere z between 1 and 5\n  and w"
        );
    }

    #[test]
    fn subqueries_align_after_their_parenthesis() {
        assert_eq!(
            f("select * from (select a from t where b = 1) s where s.a > 2"),
            "select *\nfrom (select a\n      from t\n      where b = 1) s\nwhere s.a > 2"
        );
    }

    #[test]
    fn create_table_bodies_are_one_definition_per_line() {
        assert_eq!(
            f("create table t (id int primary key, name text not null)"),
            "create table t\n(\n    id int primary key,\n    name text not null\n)"
        );
    }

    #[test]
    fn comments_and_keyword_case_are_kept_and_other_statements_are_separate() {
        let out = f("SELECT a -- first\n, b FROM t;\n\n-- next\nselect 1");
        assert_eq!(
            out,
            "SELECT a, -- first\n       b\nFROM t;\n\n-- next\nselect 1"
        );
    }

    #[test]
    fn formatting_is_stable_and_never_changes_what_the_statement_says() {
        let sql = "select a, b from t where x = 1 and y in (1, 2) order by a";
        let once = f(sql);
        assert_eq!(f(&once), once, "formatting twice changes nothing");
        let squash = |s: &str| {
            s.split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .replace(" (", "(")
                .replace("( ", "(")
        };
        assert_eq!(
            squash(&once).replace(", ", ","),
            squash(sql).replace(", ", ",")
        );
    }

    #[test]
    fn only_statements_in_the_range_change() {
        let sql = "select a,b from t;\nselect c,d from u";
        let edits = format_sql(sql, Some((sql.find("select c").unwrap(), sql.len())));
        assert_eq!(edits.len(), 1);
        assert!(edits[0].from > sql.find(';').unwrap());
    }

    #[test]
    fn unary_minus_stays_attached_and_strings_are_untouched() {
        assert_eq!(
            f("select -1, 'a  b' from t where x = -2"),
            "select -1,\n       'a  b'\nfrom t\nwhere x = -2"
        );
    }
}
