//! Writing a result as CSV, TSV or JSON. One writer serves both the clipboard
//! (rows already in memory) and the streaming "export all rows" to a file.

use std::io::{self, Write};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Csv,
    Tsv,
    Json,
}

impl ExportFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Csv => "csv",
            Self::Tsv => "tsv",
            Self::Json => "json",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Csv => "CSV",
            Self::Tsv => "TSV",
            Self::Json => "JSON",
        }
    }
}

/// Writes rows one at a time, so a result never has to fit in memory.
pub struct RowWriter<W: Write> {
    out: W,
    format: ExportFormat,
    columns: Vec<String>,
    rows: u64,
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

/// A cell as a JSON value: numbers and embedded JSON keep their type,
/// everything else is a string, NULL is null.
pub fn json_value(value: Option<&str>) -> String {
    let Some(text) = value else {
        return "null".into();
    };
    let plain_number = {
        // -?(0|[1-9][0-9]*)(\.[0-9]+)?
        let body = text.strip_prefix('-').unwrap_or(text);
        let (int, frac) = match body.split_once('.') {
            Some((i, f)) => (i, Some(f)),
            None => (body, None),
        };
        let int_ok = int == "0"
            || (!int.is_empty()
                && !int.starts_with('0')
                && int.chars().all(|c| c.is_ascii_digit()));
        let frac_ok = frac.is_none_or(|f| !f.is_empty() && f.chars().all(|c| c.is_ascii_digit()));
        int_ok && frac_ok
    };
    if plain_number {
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

impl<W: Write> RowWriter<W> {
    pub fn new(out: W, format: ExportFormat, columns: Vec<String>) -> io::Result<Self> {
        let mut writer = Self {
            out,
            format,
            columns,
            rows: 0,
        };
        match format {
            ExportFormat::Csv => {
                let line: Vec<String> = writer.columns.iter().map(|c| csv_field(c)).collect();
                writeln!(writer.out, "{}", line.join(","))?;
            }
            ExportFormat::Tsv => {
                let line: Vec<String> = writer.columns.iter().map(|c| tsv_field(c)).collect();
                writeln!(writer.out, "{}", line.join("\t"))?;
            }
            ExportFormat::Json => writer.out.write_all(b"[")?,
        }
        Ok(writer)
    }

    pub fn row(&mut self, cells: &[Option<&str>]) -> io::Result<()> {
        match self.format {
            ExportFormat::Csv => {
                let line: Vec<String> = cells.iter().map(|c| csv_field(c.unwrap_or(""))).collect();
                writeln!(self.out, "{}", line.join(","))?;
            }
            ExportFormat::Tsv => {
                let line: Vec<String> = cells.iter().map(|c| tsv_field(c.unwrap_or(""))).collect();
                writeln!(self.out, "{}", line.join("\t"))?;
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
        }
        self.rows += 1;
        Ok(())
    }

    /// Closes the document and returns how many rows were written.
    pub fn finish(mut self) -> io::Result<u64> {
        if self.format == ExportFormat::Json {
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
) -> String {
    let mut buffer = Vec::new();
    let mut writer =
        RowWriter::new(&mut buffer, format, columns.to_vec()).expect("in-memory write");
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

    #[test]
    fn csv_quotes_only_when_needed_and_nulls_are_empty() {
        assert_eq!(
            format_rows(ExportFormat::Csv, &columns(), &rows()),
            "id,\"note, text\"\n1,\"say \"\"hi\"\"\nnow\"\n2,\n"
        );
    }

    #[test]
    fn tsv_flattens_tabs_and_newlines() {
        assert_eq!(
            format_rows(ExportFormat::Tsv, &columns(), &rows()),
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
        let text = format_rows(ExportFormat::Json, &columns(), &rows);
        let parsed: serde_json::Value = serde_json::from_str(&text).expect(&text);
        assert_eq!(parsed[0]["id"], 1);
        assert_eq!(parsed[0]["note, text"]["a"][0], 1);
        assert_eq!(parsed[1]["id"], "007", "leading zeros stay text");
        assert!(parsed[1]["note, text"].is_null());
        assert_eq!(parsed[2]["id"], -2.5);
        assert_eq!(parsed[2]["note, text"], "plain");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&format_rows(
                ExportFormat::Json,
                &columns(),
                &[]
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
}
