//! MongoDB console text, without a server: splitting a script into
//! statements the way the shell reads them, and telling from the text alone
//! whether a statement writes or sweeps a whole collection.
//!
//! A statement ends at a top-level `;`, or at a line break when it is
//! complete: the brackets are balanced, the line does not end in an operator
//! or a comma, and the next line does not continue a call chain with `.`.

use crate::db::analysis::Impact;
use crate::db::split::Statement;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Class {
    /// Code, kept.
    Code,
    /// String contents and comments: blanked for the word scan.
    Opaque,
}

/// Byte classes of `text` (code or opaque), with string and comment
/// delimiters themselves counted as code so quotes still separate words.
fn classes(text: &str) -> Vec<Class> {
    let bytes = text.as_bytes();
    let mut out = vec![Class::Code; bytes.len()];
    let mut i = 0;
    let mut last_significant: Option<u8> = None;
    let mut last_word = String::new();
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                let end = text[i..].find('\n').map_or(bytes.len(), |n| i + n);
                out[i..end].fill(Class::Opaque);
                i = end;
                continue;
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                let end = text[i + 2..]
                    .find("*/")
                    .map_or(bytes.len(), |n| i + 2 + n + 2);
                out[i..end].fill(Class::Opaque);
                i = end;
                continue;
            }
            b'\'' | b'"' | b'`' => {
                let mut j = i + 1;
                while j < bytes.len() {
                    if bytes[j] == b'\\' {
                        j += 2;
                        continue;
                    }
                    if bytes[j] == b {
                        break;
                    }
                    // A plain string does not cross a line break.
                    if bytes[j] == b'\n' && b != b'`' {
                        break;
                    }
                    j += 1;
                }
                let end = j.min(bytes.len());
                out[i + 1..end].fill(Class::Opaque);
                // An unclosed string stops before the line break, which still counts.
                let closed = end < bytes.len() && bytes[end] == b;
                i = if closed { end + 1 } else { end };
                last_significant = Some(b);
                last_word.clear();
                continue;
            }
            b'/' if regex_may_start(last_significant, &last_word) => {
                let mut j = i + 1;
                let mut class = false;
                while j < bytes.len() && bytes[j] != b'\n' {
                    match bytes[j] {
                        b'\\' => j += 1,
                        b'[' => class = true,
                        b']' => class = false,
                        b'/' if !class => break,
                        _ => {}
                    }
                    j += 1;
                }
                if j < bytes.len() && bytes[j] == b'/' {
                    out[i + 1..j].fill(Class::Opaque);
                    j += 1;
                    while j < bytes.len() && bytes[j].is_ascii_alphabetic() {
                        j += 1;
                    }
                    i = j;
                    last_significant = Some(b'/');
                    last_word.clear();
                    continue;
                }
            }
            _ => {}
        }
        if !b.is_ascii_whitespace() {
            if is_ident(b) {
                if !(i > 0 && is_ident(bytes[i - 1])) {
                    last_word.clear();
                }
                last_word.push(b as char);
            } else {
                last_word.clear();
            }
            last_significant = Some(b);
        }
        i += 1;
    }
    out
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

/// Whether a `/` starts a regular expression rather than dividing.
fn regex_may_start(previous: Option<u8>, word: &str) -> bool {
    match previous {
        None => true,
        Some(p) if p.is_ascii_alphanumeric() || p == b'_' || p == b'$' => {
            matches!(
                word,
                "return" | "typeof" | "case" | "in" | "of" | "new" | "delete" | "void"
            )
        }
        Some(b')' | b']' | b'}' | b'\'' | b'"' | b'`') => false,
        Some(_) => true,
    }
}

/// A line ending in one of these goes on on the next line.
fn continues(last: u8) -> bool {
    matches!(
        last,
        b'.' | b','
            | b'('
            | b'['
            | b'{'
            | b'='
            | b'+'
            | b'-'
            | b'*'
            | b'/'
            | b'%'
            | b'&'
            | b'|'
            | b'?'
            | b':'
            | b'!'
            | b'<'
            | b'>'
            | b'^'
            | b'~'
    )
}

/// Every statement of `text` that holds code (comments alone are not
/// statements). Offsets are bytes.
pub fn split(text: &str) -> Vec<Statement> {
    let bytes = text.as_bytes();
    let class = classes(text);
    let mut out = Vec::new();
    let mut depth: i32 = 0;
    let mut start: Option<usize> = None;
    let mut last_code: Option<(usize, u8)> = None;
    let finish = |out: &mut Vec<Statement>, start: &mut Option<usize>, end: usize| {
        if let Some(from) = start.take() {
            let slice = &text[from..end];
            let trimmed = slice.trim_end();
            if !trimmed.is_empty() {
                out.push(Statement {
                    from,
                    to: from + trimmed.len(),
                    text: trimmed.to_owned(),
                });
            }
        }
    };
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if class[i] == Class::Opaque {
            i += 1;
            continue;
        }
        match b {
            b';' if depth <= 0 => {
                finish(&mut out, &mut start, i);
                depth = 0;
                last_code = None;
                i += 1;
                continue;
            }
            b'\n' if depth <= 0 && start.is_some() => {
                let open = last_code.is_some_and(|(_, c)| continues(c));
                let next = (i + 1..bytes.len())
                    .find(|&j| class[j] == Class::Code && !bytes[j].is_ascii_whitespace());
                let chained = next.is_some_and(|j| {
                    bytes[j] == b'.' && !bytes.get(j + 1).is_some_and(u8::is_ascii_digit)
                        || (bytes[j] == b'?' && bytes.get(j + 1) == Some(&b'.'))
                });
                if !open && !chained {
                    finish(&mut out, &mut start, i);
                    depth = 0;
                    last_code = None;
                }
                i += 1;
                continue;
            }
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            _ => {}
        }
        if !b.is_ascii_whitespace() {
            start.get_or_insert(i);
            last_code = Some((i, b));
        }
        i += 1;
    }
    finish(&mut out, &mut start, bytes.len());
    out
}

/// The statement the cursor is in or right after, for "run at cursor".
pub fn at_cursor(text: &str, cursor: usize) -> Option<Statement> {
    let all = split(text);
    all.iter()
        .find(|s| cursor >= s.from && cursor <= s.to + 1)
        .or_else(|| all.iter().rev().find(|s| s.to < cursor))
        .or_else(|| all.first())
        .cloned()
}

/// `text` with string contents and comments blanked out, the same length.
fn code_only(text: &str) -> String {
    let class = classes(text);
    text.char_indices()
        .map(|(i, c)| if class[i] == Class::Opaque { ' ' } else { c })
        .collect()
}

/// Methods that change data, indexes or collections.
pub const WRITE_METHODS: &[&str] = &[
    "insertOne",
    "insertMany",
    "insert",
    "updateOne",
    "updateMany",
    "update",
    "replaceOne",
    "deleteOne",
    "deleteMany",
    "remove",
    "findOneAndUpdate",
    "findOneAndReplace",
    "findOneAndDelete",
    "findAndModify",
    "bulkWrite",
    "createIndex",
    "createIndexes",
    "ensureIndex",
    "dropIndex",
    "dropIndexes",
    "drop",
    "renameCollection",
    "createCollection",
    "createView",
    "dropDatabase",
    "killOp",
    "setProfilingLevel",
];

/// Database commands that only read; any other command run through
/// `runCommand` counts as a write.
pub const READ_COMMANDS: &[&str] = &[
    "find",
    "aggregate",
    "count",
    "distinct",
    "listCollections",
    "listIndexes",
    "listDatabases",
    "dbStats",
    "collStats",
    "dataSize",
    "serverStatus",
    "buildInfo",
    "buildinfo",
    "hostInfo",
    "ping",
    "isMaster",
    "ismaster",
    "hello",
    "connectionStatus",
    "usersInfo",
    "rolesInfo",
    "getParameter",
    "currentOp",
    "explain",
    "getLog",
    "top",
    "validate",
    "getCmdLineOpts",
    "listCommands",
    "replSetGetStatus",
    "replSetGetConfig",
    "getShardMap",
    "lockInfo",
    "whatsmyuri",
    "features",
    "profile",
];

/// The method calls of `code` (`.name(`), with where their arguments start.
fn calls(code: &str) -> Vec<(&str, usize)> {
    let bytes = code.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'.' {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            let start = j;
            while j < bytes.len()
                && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_' || bytes[j] == b'$')
            {
                j += 1;
            }
            let name = &code[start..j];
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if !name.is_empty() && bytes.get(j) == Some(&b'(') {
                out.push((name, j + 1));
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    out
}

/// Whether the arguments starting at `at` begin with an empty filter:
/// `()`, `({})`, `({ }, …)`.
fn empty_filter(code: &str, at: usize) -> bool {
    let rest = code[at..].trim_start();
    if rest.starts_with(')') {
        return true;
    }
    let Some(inner) = rest.strip_prefix('{') else {
        return false;
    };
    let inner = inner.trim_start();
    inner.starts_with('}') && {
        let after = inner[1..].trim_start();
        after.starts_with(')') || after.starts_with(',')
    }
}

/// The name of the command in `runCommand({ name: … })` / `runCommand("name")`.
fn command_name(text: &str, at: usize) -> Option<String> {
    let rest = text[at..].trim_start();
    let rest = rest.strip_prefix('{').map(str::trim_start).unwrap_or(rest);
    let rest = rest.trim_start_matches(['"', '\'']);
    let name: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}

/// What a statement would do, from its text, and why it needs a second look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Assessment {
    pub impact: Impact,
    /// When `impact.sweeping`, what the confirmation says.
    pub reason: Option<&'static str>,
}

/// Whether a statement writes, and whether it sweeps a whole collection or
/// database: `deleteMany({})`, `updateMany({}, …)`, `remove({})`, `drop()`,
/// `dropDatabase()`, `dropIndexes()`.
pub fn assess(statement: &str) -> Assessment {
    let code = code_only(statement);
    let trimmed = code.trim_start();
    if trimmed.starts_with("use ") || trimmed.starts_with("show ") {
        return Assessment {
            impact: Impact {
                writes: false,
                sweeping: false,
            },
            reason: None,
        };
    }
    let mut writes = false;
    let mut reason = None;
    for (name, args) in calls(&code) {
        if WRITE_METHODS.contains(&name) {
            writes = true;
        }
        let sweep = match name {
            "deleteMany" | "remove" if empty_filter(&code, args) => {
                Some("deleteMany({}) removes every document of the collection.")
            }
            "updateMany" if empty_filter(&code, args) => {
                Some("updateMany({}, …) changes every document of the collection.")
            }
            "drop" => Some("drop() removes the collection and its indexes for good."),
            "dropDatabase" => Some("dropDatabase() removes the database and everything in it."),
            "dropIndexes" if code[args..].trim_start().starts_with(')') => {
                Some("dropIndexes() removes every index of the collection but _id.")
            }
            _ => None,
        };
        if reason.is_none() {
            reason = sweep;
        }
        if name == "aggregate" && (statement.contains("$out") || statement.contains("$merge")) {
            writes = true;
        }
        if matches!(name, "runCommand" | "adminCommand") {
            // The command's name is in the text proper, often quoted.
            let command = command_name(statement, args);
            if command
                .as_deref()
                .is_none_or(|c| !READ_COMMANDS.contains(&c))
            {
                writes = true;
            }
            if command.as_deref() == Some("dropDatabase") && reason.is_none() {
                reason = Some("dropDatabase removes the database and everything in it.");
            }
        }
    }
    Assessment {
        impact: Impact {
            writes,
            sweeping: reason.is_some(),
        },
        reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(script: &str) -> Vec<String> {
        split(script).into_iter().map(|s| s.text).collect()
    }

    #[test]
    fn lines_and_semicolons_end_statements() {
        assert_eq!(
            texts("db.a.find()\ndb.b.find(); db.c.find()\n\n"),
            ["db.a.find()", "db.b.find()", "db.c.find()"]
        );
    }

    #[test]
    fn open_brackets_operators_and_chains_carry_on() {
        let script = "db.users.find({\n  age: { $gt: 30 },\n})\n  .sort({ name: 1 })\n  .limit(5)\ndb.x.insertOne(\n  { a: 1 }\n)\nconst n = 1 +\n  2";
        assert_eq!(
            texts(script),
            [
                "db.users.find({\n  age: { $gt: 30 },\n})\n  .sort({ name: 1 })\n  .limit(5)",
                "db.x.insertOne(\n  { a: 1 }\n)",
                "const n = 1 +\n  2"
            ]
        );
    }

    #[test]
    fn strings_comments_and_regexes_hide_their_delimiters() {
        assert_eq!(
            texts("db.a.find({ s: \"a;b\\n(\" }) // ; ( not code\ndb.b.find({ r: /[;/(]x/i })"),
            [
                "db.a.find({ s: \"a;b\\n(\" }) // ; ( not code",
                "db.b.find({ r: /[;/(]x/i })"
            ]
        );
        assert_eq!(
            texts("/* a; b */\n// only a comment\n"),
            Vec::<String>::new()
        );
        // A leading comment line is not part of the statement.
        let script = "// users\ndb.users.find()";
        let all = split(script);
        assert_eq!(all[0].text, "db.users.find()");
        assert_eq!(&script[all[0].from..all[0].to], "db.users.find()");
        // Division is not a regex.
        assert_eq!(
            texts("const a = 4 / 2; const b = a / 2"),
            ["const a = 4 / 2", "const b = a / 2"]
        );
    }

    #[test]
    fn a_chain_after_a_comment_line_carries_on() {
        assert_eq!(
            texts("db.a.find()\n  // newest first\n  .sort({ at: -1 })"),
            ["db.a.find()\n  // newest first\n  .sort({ at: -1 })"]
        );
    }

    #[test]
    fn the_cursor_picks_its_statement() {
        let script = "db.a.find()\ndb.b.find()\n";
        assert_eq!(at_cursor(script, 3).unwrap().text, "db.a.find()");
        assert_eq!(at_cursor(script, 14).unwrap().text, "db.b.find()");
        assert_eq!(at_cursor(script, script.len()).unwrap().text, "db.b.find()");
        assert!(at_cursor("  // nothing", 1).is_none());
    }

    #[test]
    fn reads_writes_and_sweeps_are_told_apart() {
        let read = |s: &str| !assess(s).impact.writes;
        assert!(read("db.users.find({ name: 'deleteMany(' })"));
        assert!(read("db.users.aggregate([{ $match: {} }])"));
        assert!(read("db.runCommand({ ping: 1 })"));
        assert!(read("db.adminCommand('listDatabases')"));
        assert!(read("show dbs"));
        assert!(read("db.users.countDocuments({})"));
        assert!(!read("db.users.insertOne({ a: 1 })"));
        assert!(!read(
            "db.users.aggregate([{ $match: {} }, { $out: 'copy' }])"
        ));
        assert!(!read("db.runCommand({ dropIndexes: 'users', index: '*' })"));
        assert!(
            !read("db.runCommand({ somethingNew: 1 })"),
            "unknown commands count as writes"
        );

        let sweeping = |s: &str| assess(s).impact.sweeping;
        assert!(sweeping("db.users.deleteMany({})"));
        assert!(sweeping("db.users.deleteMany( { } )"));
        assert!(sweeping("db.users.remove({})"));
        assert!(sweeping("db.users.updateMany({}, { $set: { a: 1 } })"));
        assert!(sweeping("db.users.drop()"));
        assert!(sweeping("db.dropDatabase()"));
        assert!(sweeping("db.users.dropIndexes()"));
        assert!(!sweeping("db.users.deleteMany({ age: { $lt: 18 } })"));
        assert!(!sweeping(
            "db.users.updateMany({ a: 1 }, { $set: { b: 2 } })"
        ));
        assert!(!sweeping("db.users.dropIndex('a_1')"));
        assert!(!sweeping("db.users.dropIndexes('a_1')"));
        assert_eq!(
            assess("db.users.drop()").reason,
            Some("drop() removes the collection and its indexes for good.")
        );
    }
}
