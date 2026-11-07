//! What a statement would do, from its text alone: whether it writes, and
//! whether it is the kind of write that is easy to regret (no `WHERE`).

use super::lexer::{lex, Kind};

/// Statements that only read.
const READ_VERBS: [&str; 9] = [
    "SELECT", "WITH", "VALUES", "TABLE", "SHOW", "DESCRIBE", "DESC", "EXPLAIN", "FETCH",
];
/// Words that make a reading statement write: data-modifying CTEs, `SELECT INTO`,
/// sequences, calls.
const WRITES: [&str; 8] = [
    "INSERT", "UPDATE", "DELETE", "MERGE", "INTO", "NEXTVAL", "SETVAL", "CALL",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Impact {
    /// Changes data or schema (anything but a plain read).
    pub writes: bool,
    /// An `UPDATE` or `DELETE` without `WHERE`, `TRUNCATE`, or `DROP`.
    pub sweeping: bool,
}

pub fn classify(statement: &str) -> Impact {
    let tokens = lex(statement).tokens;
    let words: Vec<&str> = tokens
        .iter()
        .filter(|t| t.kind == Kind::Word)
        .map(|t| t.upper.as_str())
        .collect();
    let Some(first) = tokens.iter().find(|t| t.text != "(") else {
        return Impact {
            writes: false,
            sweeping: false,
        };
    };
    let reads = first.kind == Kind::Word && READ_VERBS.contains(&first.upper.as_str());
    // EXPLAIN ANALYZE runs what follows, so look past it.
    let explains_a_write = first.upper == "EXPLAIN"
        && words
            .iter()
            .any(|w| matches!(*w, "INSERT" | "UPDATE" | "DELETE" | "MERGE"))
        && words.contains(&"ANALYZE");
    let writes = if reads {
        explains_a_write || (first.upper != "EXPLAIN" && words.iter().any(|w| WRITES.contains(w)))
    } else {
        // SET, BEGIN, COMMIT, ROLLBACK, SAVEPOINT change nothing in the data.
        !matches!(
            first.upper.as_str(),
            "SET"
                | "RESET"
                | "BEGIN"
                | "START"
                | "COMMIT"
                | "END"
                | "ROLLBACK"
                | "SAVEPOINT"
                | "RELEASE"
                | "DEALLOCATE"
                | "DISCARD"
                | "LISTEN"
                | "UNLISTEN"
                | "CLOSE"
                | "DECLARE"
        )
    };
    let has_where = words.contains(&"WHERE");
    let sweeping = writes
        && match first.upper.as_str() {
            "UPDATE" | "DELETE" => !has_where,
            "TRUNCATE" | "DROP" => true,
            _ => false,
        };
    Impact { writes, sweeping }
}

/// The table a query reads (`from schema.name`), as written, for naming
/// where exported rows would be inserted.
pub fn table_of(statement: &str) -> Option<String> {
    let tokens = lex(statement).tokens;
    let from = tokens
        .iter()
        .position(|t| t.kind == Kind::Word && t.upper == "FROM")?;
    let mut name = String::new();
    let mut expect_part = true;
    for t in &tokens[from + 1..] {
        if expect_part && matches!(t.kind, Kind::Word | Kind::Quoted) {
            if name.is_empty() && t.kind == Kind::Word && t.upper == "SELECT" {
                return None;
            }
            name.push_str(&t.text);
            expect_part = false;
        } else if !expect_part && t.kind == Kind::Punct && t.text == "." {
            name.push('.');
            expect_part = true;
        } else {
            break;
        }
    }
    (!name.is_empty() && !name.ends_with('.')).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn writes(sql: &str) -> bool {
        classify(sql).writes
    }

    #[test]
    fn plain_reads_do_not_write() {
        for sql in [
            "select 1",
            "  SELECT * from t where a = 1",
            "with x as (select 1) select * from x",
            "(select 1) union (select 2)",
            "show search_path",
            "explain select 1",
            "values (1), (2)",
            "table orders",
            "-- note\nselect 1",
        ] {
            assert!(!writes(sql), "{sql}");
        }
    }

    #[test]
    fn writes_are_recognised_even_when_they_hide_inside_a_query() {
        for sql in [
            "insert into t values (1)",
            "update t set a = 1",
            "delete from t",
            "create table t (a int)",
            "drop table t",
            "truncate t",
            "with d as (delete from t returning *) select * from d",
            "select * into backup from t",
            "select nextval('s')",
            "explain analyze delete from t",
            "call do_it()",
            "grant select on t to bob",
            "vacuum t",
        ] {
            assert!(writes(sql), "{sql}");
        }
    }

    #[test]
    fn session_and_transaction_commands_are_not_writes() {
        for sql in [
            "set search_path = a",
            "begin",
            "commit",
            "rollback",
            "reset all",
            "savepoint x",
        ] {
            assert!(!writes(sql), "{sql}");
        }
    }

    #[test]
    fn sweeping_means_no_where_or_a_drop() {
        assert!(classify("delete from t").sweeping);
        assert!(classify("update t set a = 1").sweeping);
        assert!(classify("truncate t").sweeping);
        assert!(classify("drop table t").sweeping);
        assert!(!classify("delete from t where id = 1").sweeping);
        assert!(!classify("update t set a = 1 where b = 2").sweeping);
        assert!(!classify("insert into t values (1)").sweeping);
        assert!(!classify("select * from t").sweeping);
    }

    #[test]
    fn table_of_finds_the_relation_a_query_reads() {
        assert_eq!(
            table_of("select * from orders where id = 1").as_deref(),
            Some("orders")
        );
        assert_eq!(
            table_of("SELECT a FROM public.orders o JOIN x").as_deref(),
            Some("public.orders")
        );
        assert_eq!(
            table_of("select 1 from \"My Schema\".\"Order Items\"").as_deref(),
            Some("\"My Schema\".\"Order Items\"")
        );
        assert_eq!(table_of("select 1"), None);
        assert_eq!(table_of("select * from (select 1) s"), None);
    }

    #[test]
    fn a_word_in_a_string_or_comment_does_not_count() {
        assert!(!writes(
            "select 'delete from t', \"update\" from x -- insert"
        ));
        assert!(!classify("select 1").sweeping);
        assert!(
            classify("delete from t -- where id = 1").sweeping,
            "a commented WHERE does not protect"
        );
    }
}
