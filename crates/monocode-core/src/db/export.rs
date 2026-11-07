//! Writing a result as CSV, TSV, JSON, SQL inserts or a Markdown table. One
//! writer serves both the clipboard (rows already in memory) and the
//! streaming "export all rows" to a file.

use std::io::{self, Write};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Csv,
    Tsv,
    Json,
    SqlInserts,
    Markdown,
}

impl ExportFormat {
    pub const ALL: [Self; 5] = [
        Self::Csv,
        Self::Tsv,
        Self::Json,
        Self::SqlInserts,
        Self::Markdown,
    ];

    pub fn extension(self) -> &'static str {
        match self {
            Self::Csv => "csv",
            Self::Tsv => "tsv",
            Self::Json => "json",
            Self::SqlInserts => "sql",
            Self::Markdown => "md",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Csv => "CSV",
            Self::Tsv => "TSV",
            Self::Json => "JSON",
            Self::SqlInserts => "SQL inserts",
            Self::Markdown => "Markdown",
        }
    }

    /// Whether the header-row and transpose options mean anything for it.
    pub fn is_tabular(self) -> bool {
        matches!(self, Self::Csv | Self::Tsv | Self::Markdown)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportOptions {
    /// A first line naming the columns (CSV, TSV, Markdown).
    pub header: bool,
    /// A line per column instead of a line per row. Needs every row in
    /// memory, so a streamed export holds the result until the end.
    pub transpose: bool,
    /// What `INSERT INTO` names.
    pub table: String,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            header: true,
            transpose: false,
            table: "table_name".into(),
        }
    }
}

fn csv_field(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

fn tsv_field(value: &str) -> String {
    value.replace(['\t', '\n', '\r'], " ")
}

fn markdown_field(value: &str) -> String {
    value
        .replace('|', "\\|")
        .replace("\r\n", "<br>")
        .replace(['\n', '\r'], "<br>")
}

/// Whether `text` is a plain number JSON and SQL can carry unquoted:
/// `-?(0|[1-9][0-9]*)(\.[0-9]+)?`.
fn plain_number(text: &str) -> bool {
    let body = text.strip_prefix('-').unwrap_or(text);
    let (int, frac) = match body.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (body, None),
    };
    let int_ok = int == "0"
        || (!int.is_empty() && !int.starts_with('0') && int.chars().all(|c| c.is_ascii_digit()));
    let frac_ok = frac.is_none_or(|f| !f.is_empty() && f.chars().all(|c| c.is_ascii_digit()));
    int_ok && frac_ok
}

/// A cell as a JSON value: numbers and embedded JSON keep their type,
/// everything else is a string, NULL is null.
pub fn json_value(value: Option<&str>) -> String {
    let Some(text) = value else {
        return "null".into();
    };
    if plain_number(text) {
        return text.to_owned();
    }
    let trimmed = text.trim_start();
    if (trimmed.starts_with('{') || trimmed.starts_with('['))
        && serde_json::from_str::<serde_json::Value>(text).is_ok()
    {
        return text.trim().to_owned();
    }
    serde_json::to_string(text).unwrap_or_else(|_| "null".into())
}

/// A cell as a SQL literal: NULL, a plain number, or a quoted string.
pub fn sql_value(value: Option<&str>) -> String {
    match value {
        None => "NULL".into(),
        Some(text) if plain_number(text) => text.to_owned(),
        Some(text) => format!("'{}'", text.replace('\'', "''")),
    }
}

/// Writes rows one at a time, so a result never has to fit in memory
/// (unless the output is transposed).
pub struct RowWriter<W: Write> {
    out: W,
    format: ExportFormat,
    options: ExportOptions,
    columns: Vec<String>,
    rows: u64,
    /// Held back when transposing.
    held: Vec<Vec<Option<String>>>,
}

impl<W: Write> RowWriter<W> {
    pub fn new(
        out: W,
        format: ExportFormat,
        columns: Vec<String>,
        options: ExportOptions,
    ) -> io::Result<Self> {
        let mut writer = Self {
            out,
            format,
            options,
            columns,
            rows: 0,
            held: Vec::new(),
        };
        if writer.transposed() {
            return Ok(writer);
        }
        match format {
            ExportFormat::Csv | ExportFormat::Tsv | ExportFormat::Markdown => {
                if writer.options.header {
                    let names = writer.columns.clone();
                    let header: Vec<Option<&str>> =
                        names.iter().map(|c| Some(c.as_str())).collect();
                    writer.table_line(&header)?;
                    if format == ExportFormat::Markdown {
                        let rule = vec!["---"; writer.columns.len().max(1)].join(" | ");
                        writeln!(writer.out, "| {rule} |")?;
                    }
                } else if format == ExportFormat::Markdown {
                    // A Markdown table needs a header line; leave it blank.
                    let blank = vec![" "; writer.columns.len().max(1)].join(" | ");
                    writeln!(writer.out, "| {blank} |")?;
                    let rule = vec!["---"; writer.columns.len().max(1)].join(" | ");
                    writeln!(writer.out, "| {rule} |")?;
                }
            }
            ExportFormat::Json => writer.out.write_all(b"[")?,
            ExportFormat::SqlInserts => {}
        }
        Ok(writer)
    }

    fn transposed(&self) -> bool {
        self.options.transpose && self.format.is_tabular()
    }

    fn table_line(&mut self, cells: &[Option<&str>]) -> io::Result<()> {
        match self.format {
            ExportFormat::Csv => {
                let line: Vec<String> = cells.iter().map(|c| csv_field(c.unwrap_or(""))).collect();
                writeln!(self.out, "{}", line.join(","))
            }
            ExportFormat::Tsv => {
                let line: Vec<String> = cells.iter().map(|c| tsv_field(c.unwrap_or(""))).collect();
                writeln!(self.out, "{}", line.join("\t"))
            }
            _ => {
                let line: Vec<String> = cells
                    .iter()
                    .map(|c| markdown_field(c.unwrap_or("")))
                    .collect();
                writeln!(self.out, "| {} |", line.join(" | "))
            }
        }
    }

    pub fn row(&mut self, cells: &[Option<&str>]) -> io::Result<()> {
        if self.transposed() {
            self.held
                .push(cells.iter().map(|c| c.map(str::to_owned)).collect());
            self.rows += 1;
            return Ok(());
        }
        match self.format {
            ExportFormat::Csv | ExportFormat::Tsv | ExportFormat::Markdown => {
                self.table_line(cells)?
            }
            ExportFormat::Json => {
                write!(self.out, "{}\n  {{", if self.rows == 0 { "" } else { "," })?;
                for (i, cell) in cells.iter().enumerate() {
                    let name =
                        serde_json::to_string(self.columns.get(i).map_or("", String::as_str))
                            .unwrap_or_default();
                    write!(
                        self.out,
                        "{}{name}: {}",
                        if i == 0 { "" } else { ", " },
                        json_value(*cell)
                    )?;
                }
                self.out.write_all(b"}")?;
            }
            ExportFormat::SqlInserts => {
                let names: Vec<String> = self
                    .columns
                    .iter()
                    .map(|c| super::introspect::quote(c))
                    .collect();
                let values: Vec<String> = cells.iter().map(|c| sql_value(*c)).collect();
                writeln!(
                    self.out,
                    "INSERT INTO {} ({}) VALUES ({});",
                    self.options.table,
                    names.join(", "),
                    values.join(", ")
                )?;
            }
        }
        self.rows += 1;
        Ok(())
    }

    /// Closes the document and returns how many rows were written.
    pub fn finish(mut self) -> io::Result<u64> {
        if self.transposed() {
            // A line per column: its name, then its value in every row.
            let held = std::mem::take(&mut self.held);
            if self.format == ExportFormat::Markdown {
                let width = held.len() + 1;
                let blank = vec![" "; width].join(" | ");
                writeln!(self.out, "| {blank} |")?;
                writeln!(self.out, "| {} |", vec!["---"; width].join(" | "))?;
            }
            for (i, name) in self.columns.clone().iter().enumerate() {
                let mut cells: Vec<Option<&str>> = vec![Some(name.as_str())];
                cells.extend(held.iter().map(|row| row.get(i).and_then(|c| c.as_deref())));
                self.table_line(&cells)?;
            }
        } else if self.format == ExportFormat::Json {
            self.out
                .write_all(if self.rows == 0 { b"]\n" } else { b"\n]\n" })?;
        }
        self.out.flush()?;
        Ok(self.rows)
    }
}

/// Rows already in memory, as one string (for the clipboard).
pub fn format_rows(
    format: ExportFormat,
    columns: &[String],
    rows: &[Vec<Option<String>>],
    options: &ExportOptions,
) -> String {
    let mut buffer = Vec::new();
    let mut writer = RowWriter::new(&mut buffer, format, columns.to_vec(), options.clone())
        .expect("in-memory write");
    for row in rows {
        let cells: Vec<Option<&str>> = row.iter().map(|c| c.as_deref()).collect();
        writer.row(&cells).expect("in-memory write");
    }
    writer.finish().expect("in-memory write");
    String::from_utf8_lossy(&buffer).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn columns() -> Vec<String> {
        vec!["id".into(), "note, text".into()]
    }

    fn rows() -> Vec<Vec<Option<String>>> {
        vec![
            vec![Some("1".into()), Some("say \"hi\"\nnow".into())],
            vec![Some("2".into()), None],
        ]
    }

    fn fmt(format: ExportFormat) -> String {
        format_rows(format, &columns(), &rows(), &ExportOptions::default())
    }

    #[test]
    fn csv_quotes_only_when_needed_and_nulls_are_empty() {
        assert_eq!(
            fmt(ExportFormat::Csv),
            "id,\"note, text\"\n1,\"say \"\"hi\"\"\nnow\"\n2,\n"
        );
    }

    #[test]
    fn tsv_flattens_tabs_and_newlines() {
        assert_eq!(
            fmt(ExportFormat::Tsv),
            "id\tnote, text\n1\tsay \"hi\" now\n2\t\n"
        );
    }

    #[test]
    fn json_is_a_valid_array_of_objects_keeping_numbers_and_nested_json() {
        let rows = vec![
            vec![Some("1".into()), Some("{\"a\": [1]}".into())],
            vec![Some("007".into()), None],
            vec![Some("-2.5".into()), Some("plain".into())],
        ];
        let text = format_rows(
            ExportFormat::Json,
            &columns(),
            &rows,
            &ExportOptions::default(),
        );
        let parsed: serde_json::Value = serde_json::from_str(&text).expect(&text);
        assert_eq!(parsed[0]["id"], 1);
        assert_eq!(parsed[0]["note, text"]["a"][0], 1);
        assert_eq!(parsed[1]["id"], "007", "leading zeros stay text");
        assert!(parsed[1]["note, text"].is_null());
        assert_eq!(parsed[2]["id"], -2.5);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&format_rows(
                ExportFormat::Json,
                &columns(),
                &[],
                &ExportOptions::default()
            ))
            .unwrap(),
            serde_json::json!([])
        );
    }

    #[test]
    fn numbers_are_recognised_strictly() {
        for n in ["0", "12", "-7", "3.14", "0.5"] {
            assert_eq!(json_value(Some(n)), n, "{n}");
        }
        for s in ["01", "1e5", "NaN", "1.2.3", "", "-", "+1", "1,5"] {
            assert!(json_value(Some(s)).starts_with('"'), "{s}");
        }
    }

    #[test]
    fn sql_inserts_quote_text_keep_numbers_and_name_columns_safely() {
        let mut options = ExportOptions::default();
        options.table = "public.orders".into();
        let text = format_rows(
            ExportFormat::SqlInserts,
            &["id".into(), "Note".into()],
            &[
                vec![Some("1".into()), Some("it's".into())],
                vec![Some("007".into()), None],
            ],
            &options,
        );
        assert_eq!(
            text,
            "INSERT INTO public.orders (id, \"Note\") VALUES (1, 'it''s');\n\
             INSERT INTO public.orders (id, \"Note\") VALUES ('007', NULL);\n"
        );
    }

    #[test]
    fn markdown_is_a_table_with_escaped_pipes_and_breaks() {
        let text = format_rows(
            ExportFormat::Markdown,
            &["a".into(), "b".into()],
            &[
                vec![Some("x|y".into()), Some("l1\nl2".into())],
                vec![None, Some("z".into())],
            ],
            &ExportOptions::default(),
        );
        assert_eq!(
            text,
            "| a | b |\n| --- | --- |\n| x\\|y | l1<br>l2 |\n|  | z |\n"
        );
    }

    #[test]
    fn the_header_can_be_left_out() {
        let mut options = ExportOptions::default();
        options.header = false;
        assert_eq!(
            format_rows(ExportFormat::Csv, &columns(), &rows()[1..], &options),
            "2,\n"
        );
        let md = format_rows(
            ExportFormat::Markdown,
            &["a".into()],
            &[vec![Some("1".into())]],
            &options,
        );
        assert!(
            md.starts_with("|   |\n| --- |\n| 1 |"),
            "a Markdown table keeps a blank header: {md}"
        );
    }

    #[test]
    fn transposing_writes_a_line_per_column() {
        let mut options = ExportOptions::default();
        options.transpose = true;
        assert_eq!(
            format_rows(
                ExportFormat::Csv,
                &["a".into(), "b".into()],
                &[
                    vec![Some("1".into()), Some("2".into())],
                    vec![Some("3".into()), None],
                ],
                &options
            ),
            "a,1,3\nb,2,\n"
        );
        let md = format_rows(
            ExportFormat::Markdown,
            &["a".into()],
            &[vec![Some("1".into())], vec![Some("2".into())]],
            &options,
        );
        assert_eq!(md, "|   |   |   |\n| --- | --- | --- |\n| a | 1 | 2 |\n");
        // JSON and SQL keep their shape.
        let json = format_rows(
            ExportFormat::Json,
            &["a".into()],
            &[vec![Some("1".into())]],
            &options,
        );
        assert!(json.contains("\"a\": 1"));
    }
}
