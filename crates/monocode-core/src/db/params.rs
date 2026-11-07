//! Query parameters of console statements (`$1`, `:name`), asked for before
//! the statement runs. Each value is inserted as SQL text, so a string is
//! typed with its quotes; an empty value is `NULL`. A name used twice shares
//! one value.

use super::lexer::{lex, Kind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    /// What the dialog shows and the key of its value: `$1`, `:name`.
    pub name: String,
    pub from: usize,
    pub to: usize,
}

/// The parameters of `text`, in order of appearance.
pub fn find(text: &str) -> Vec<Param> {
    lex(text)
        .tokens
        .into_iter()
        .filter(|t| t.kind == Kind::Param && t.text.len() > 1)
        .map(|t| Param {
            name: t.text,
            from: t.from,
            to: t.to,
        })
        .collect()
}

/// The distinct names across `statements`, in order.
pub fn names(statements: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for statement in statements {
        for p in find(statement) {
            if !out.contains(&p.name) {
                out.push(p.name);
            }
        }
    }
    out
}

/// `text` with every parameter replaced by its value.
pub fn bind(text: &str, values: &dyn Fn(&str) -> String) -> String {
    let mut out = String::new();
    let mut at = 0;
    for p in find(text) {
        out.push_str(&text[at..p.from]);
        let value = values(&p.name);
        let value = value.trim();
        out.push_str(if value.is_empty() { "NULL" } else { value });
        at = p.to;
    }
    out.push_str(&text[at..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_numbered_and_named_parameters_outside_strings_and_casts() {
        let found =
            find("select * from t where a = $1 and b = :name and c = ':no' and d::int = $2 -- $3");
        assert_eq!(
            found.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            ["$1", ":name", "$2"]
        );
    }

    #[test]
    fn names_are_distinct_and_span_statements() {
        let statements = vec!["select :a, :b".to_owned(), "select :a, $1".to_owned()];
        assert_eq!(names(&statements), [":a", ":b", "$1"]);
    }

    #[test]
    fn bind_inserts_values_as_sql_and_empty_means_null() {
        let sql = "select * from t where id = :id and name = :name and x = :id";
        let out = bind(sql, &|name| match name {
            ":id" => "42".into(),
            ":name" => "  ".into(),
            _ => String::new(),
        });
        assert_eq!(
            out,
            "select * from t where id = 42 and name = NULL and x = 42"
        );
        let out = bind("select $1", &|_| "'it''s'".into());
        assert_eq!(out, "select 'it''s'");
    }

    #[test]
    fn a_lone_dollar_or_colon_is_not_a_parameter() {
        assert!(find("select '$', a[1:2], 1 $ 2").is_empty());
    }
}
