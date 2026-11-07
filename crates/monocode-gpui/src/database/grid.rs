//! Pure helpers for the result grid: what a cell shows, how wide a column is,
//! and how a value previews or copies. The grid never hands long text to the
//! text shaper: a cell shows a short single-line excerpt and the full value
//! stays in the result.

use monocode_core::db::QueryResult;

pub(super) const ROW_HEIGHT: f32 = 22.;
pub(super) const HEADER_HEIGHT: f32 = 26.;
const CELL_CHARS: usize = 200;
const MIN_WIDTH: f32 = 60.;
const MAX_WIDTH: f32 = 400.;
const CHAR_WIDTH: f32 = 7.2;
const PADDING: f32 = 20.;
const SAMPLE_ROWS: usize = 100;

/// The one-line excerpt a cell shows.
pub(super) fn display_cell(value: &str) -> String {
    let mut out = String::new();
    for (count, ch) in value.chars().enumerate() {
        if count >= CELL_CHARS {
            out.push('…');
            break;
        }
        match ch {
            '\n' | '\r' => out.push('⏎'),
            '\t' => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// A column's pixel width from its header and the first rows.
pub(super) fn column_widths(result: &QueryResult) -> Vec<f32> {
    result
        .columns
        .iter()
        .enumerate()
        .map(|(i, column)| {
            let mut chars = column.name.chars().count() + 2;
            for row in result.rows.iter().take(SAMPLE_ROWS) {
                let text = row.get(i).and_then(|v| v.as_deref()).unwrap_or("NULL");
                chars = chars.max(text.chars().take(CELL_CHARS).count());
            }
            (chars as f32 * CHAR_WIDTH + PADDING).clamp(MIN_WIDTH, MAX_WIDTH)
        })
        .collect()
}

/// Numbers align right; the type is not known from the simple protocol, so
/// a column is numeric when its sampled non-null values all parse.
pub(super) fn numeric_columns(result: &QueryResult) -> Vec<bool> {
    (0..result.columns.len())
        .map(|i| {
            let mut seen = false;
            for row in result.rows.iter().take(SAMPLE_ROWS) {
                if let Some(Some(value)) = row.get(i) {
                    if value.parse::<f64>().is_err() {
                        return false;
                    }
                    seen = true;
                }
            }
            seen
        })
        .collect()
}

/// `value` pretty-printed when it is a JSON object or array.
pub(super) fn pretty_json(value: &str) -> Option<String> {
    let trimmed = value.trim_start();
    if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
        return None;
    }
    let parsed: serde_json::Value = serde_json::from_str(value).ok()?;
    serde_json::to_string_pretty(&parsed).ok()
}

/// A row as tab-separated text, NULL as an empty field.
pub(super) fn row_tsv(row: &[Option<String>]) -> String {
    row.iter()
        .map(|v| v.as_deref().unwrap_or("").replace(['\t', '\n', '\r'], " "))
        .collect::<Vec<_>>()
        .join("\t")
}

#[cfg(test)]
mod tests {
    use super::*;
    use monocode_core::db::ResultColumn;

    fn result(columns: &[&str], rows: &[&[Option<&str>]]) -> QueryResult {
        QueryResult {
            statement: String::new(),
            columns: columns
                .iter()
                .map(|c| ResultColumn { name: (*c).into() })
                .collect(),
            rows: rows
                .iter()
                .map(|r| r.iter().map(|v| v.map(str::to_owned)).collect())
                .collect(),
            affected: None,
            has_more: false,
            execution_ms: 0.,
            elapsed_ms: 0.,
        }
    }

    #[test]
    fn cells_are_one_short_line() {
        assert_eq!(display_cell("a\nb\tc"), "a⏎b c");
        let long = "x".repeat(5000);
        let shown = display_cell(&long);
        assert_eq!(shown.chars().count(), CELL_CHARS + 1);
        assert!(shown.ends_with('…'));
    }

    #[test]
    fn widths_follow_content_within_bounds() {
        let r = result(
            &["id", "payload"],
            &[&[Some("1"), Some(&"y".repeat(3000))], &[None, Some("{}")]],
        );
        let widths = column_widths(&r);
        assert_eq!(widths[0], MIN_WIDTH);
        assert_eq!(widths[1], MAX_WIDTH);
    }

    #[test]
    fn numeric_columns_need_every_sampled_value_to_parse() {
        let r = result(
            &["n", "mixed", "empty"],
            &[
                &[Some("1"), Some("1"), None],
                &[Some("2.5"), Some("a"), None],
                &[None, None, None],
            ],
        );
        assert_eq!(numeric_columns(&r), [true, false, false]);
    }

    #[test]
    fn json_previews_and_plain_text_does_not() {
        assert_eq!(
            pretty_json("{\"a\":[1,2]}").unwrap(),
            "{\n  \"a\": [\n    1,\n    2\n  ]\n}"
        );
        assert!(pretty_json("hello").is_none());
        assert!(pretty_json("{not json").is_none());
    }

    #[test]
    fn rows_copy_as_tsv() {
        assert_eq!(
            row_tsv(&[Some("a\tb".into()), None, Some("c".into())]),
            "a b\t\tc"
        );
    }
}
