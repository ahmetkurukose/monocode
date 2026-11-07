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

pub(super) const BAR: f32 = 10.;

/// Furthest scroll offsets for a grid of `total_w` by `rows` in a
/// `vw` x `vh` viewport (the header and the bar excluded from the body).
pub(super) fn max_scroll(total_w: f32, rows: usize, vw: f32, vh: f32) -> (f32, f32) {
    let body = (vh - HEADER_HEIGHT - BAR).max(0.);
    (
        (total_w - (vw - BAR).max(0.)).max(0.),
        (rows as f32 * ROW_HEIGHT - body).max(0.),
    )
}

pub(super) fn clamp(value: f32, max: f32) -> f32 {
    value.clamp(0., max.max(0.))
}

/// The rows to build for a scroll offset: those in view plus one either side.
pub(super) fn visible_rows(scroll_y: f32, body_h: f32, rows: usize) -> std::ops::Range<usize> {
    let first = ((scroll_y / ROW_HEIGHT).floor().max(0.) as usize).saturating_sub(1);
    let last = (((scroll_y + body_h) / ROW_HEIGHT).ceil().max(0.) as usize + 1).min(rows);
    first.min(rows)..last
}

/// A scrollbar thumb's (offset, length) in a track of `track` for content of
/// `content` scrolled by `scroll`, or `None` when everything fits.
pub(super) fn thumb(track: f32, content: f32, scroll: f32) -> Option<(f32, f32)> {
    if content <= track || track <= 0. {
        return None;
    }
    let len = (track * track / content).clamp(28., track);
    let max = content - track;
    Some((scroll / max * (track - len), len))
}

/// The scroll offset that brings `[pos, pos + size)` into a `viewport`.
pub(super) fn reveal(scroll: f32, pos: f32, size: f32, viewport: f32) -> f32 {
    if pos < scroll {
        pos
    } else if pos + size > scroll + viewport {
        pos + size - viewport
    } else {
        scroll
    }
}

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
    fn scrolling_stops_at_the_content_edges() {
        let (mx, my) = max_scroll(2000., 1000, 800., 400.);
        assert_eq!(mx, 2000. - (800. - BAR));
        assert_eq!(my, 1000. * ROW_HEIGHT - (400. - HEADER_HEIGHT - BAR));
        assert_eq!(max_scroll(100., 3, 800., 400.), (0., 0.));
        assert_eq!(clamp(-5., 10.), 0.);
        assert_eq!(clamp(50., 10.), 10.);
    }

    #[test]
    fn only_the_rows_in_view_are_built() {
        assert_eq!(visible_rows(0., 220., 1000), 0..11);
        let r = visible_rows(22. * 500., 220., 1000);
        assert!(r.start <= 500 && r.end >= 510 && r.len() < 15, "{r:?}");
        assert_eq!(visible_rows(0., 220., 3), 0..3);
        assert_eq!(visible_rows(22. * 999., 220., 1000).end, 1000);
    }

    #[test]
    fn thumbs_track_the_scroll_and_hide_when_everything_fits() {
        assert_eq!(thumb(100., 100., 0.), None);
        let (offset, len) = thumb(100., 400., 0.).unwrap();
        assert_eq!((offset, len), (0., 28.));
        let (offset, len) = thumb(100., 400., 300.).unwrap();
        assert!(
            (offset + len - 100.).abs() < 0.01,
            "at the end the thumb touches the end"
        );
    }

    #[test]
    fn reveal_moves_the_minimum() {
        assert_eq!(reveal(100., 120., 20., 200.), 100.);
        assert_eq!(reveal(100., 50., 20., 200.), 50.);
        assert_eq!(reveal(100., 290., 20., 200.), 110.);
    }

    #[test]
    fn rows_copy_as_tsv() {
        assert_eq!(
            row_tsv(&[Some("a\tb".into()), None, Some("c".into())]),
            "a b\t\tc"
        );
    }
}
