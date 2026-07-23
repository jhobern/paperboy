//! Serializing a [`ReportResult`] to an output format. CSV is the v1 format;
//! the [`ReportWriter`] trait keeps the interpreter/model independent of the
//! format so xlsx/json can be added later without touching either.
//!
//! Output is driven entirely by the resolved columns (the `columns:` header
//! directive, else the produced columns in first-seen order — see
//! [`ReportResult::resolved_columns`]) and the table-wide no-match marker
//! ([`ReportResult::no_match_marker`]), so what a run writes matches exactly
//! what the TUI grid shows (both read the same columns).

use super::flow::Header;
use super::model::ReportResult;

/// Serializes a run result to a concrete output format (bytes, so a future
/// binary format like xlsx fits the same interface).
pub trait ReportWriter {
    /// The file extension for this format (no dot), e.g. `"csv"`.
    fn extension(&self) -> &'static str;
    /// Render `result` to bytes, using `header` for the `columns:` directive.
    fn write(&self, result: &ReportResult, header: &Header) -> Vec<u8>;
}

/// Writes a report as RFC 4180 CSV (comma-separated, `\r\n` line endings,
/// minimal quoting). The header row is the resolved column headers; each data
/// row coalesces its column sources, substituting the no-match marker for an
/// empty result.
pub struct CsvWriter;

impl ReportWriter for CsvWriter {
    fn extension(&self) -> &'static str {
        "csv"
    }

    fn write(&self, result: &ReportResult, header: &Header) -> Vec<u8> {
        let columns = result.resolved_columns(header);
        let mut out = String::new();

        // Header row.
        push_record(&mut out, columns.iter().map(|c| c.header.as_str()));

        // Data rows.
        for row in &result.rows {
            let cells: Vec<String> = columns
                .iter()
                .map(|c| c.value(row, &result.no_match_marker))
                .collect();
            push_record(&mut out, cells.iter().map(String::as_str));
        }

        out.into_bytes()
    }
}

/// Append one CSV record (a `\r\n`-terminated line of escaped, comma-joined
/// fields) to `out`.
fn push_record<'a>(out: &mut String, fields: impl Iterator<Item = &'a str>) {
    let mut first = true;
    for field in fields {
        if !first {
            out.push(',');
        }
        first = false;
        out.push_str(&escape_field(field));
    }
    out.push_str("\r\n");
}

/// Escape one CSV field per RFC 4180: wrap in double quotes (doubling any
/// interior quote) when it contains a comma, quote, CR or LF; otherwise emit it
/// verbatim. Reports must never lose information, so multi-line response bodies
/// are quoted and preserved rather than flattened.
fn escape_field(field: &str) -> String {
    if field.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::model::{ReportResult, ReportRow};
    use std::collections::HashMap;

    fn row(cells: &[(&str, &str)]) -> ReportRow {
        ReportRow {
            cells: cells
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            vars: HashMap::new(),
            key: vec![],
            target: None,
        }
    }

    fn csv(result: &ReportResult) -> String {
        String::from_utf8(CsvWriter.write(result, &Header::default())).unwrap()
    }

    #[test]
    fn default_columns_follow_first_seen_order() {
        let mut res = ReportResult::default();
        res.column_order = vec!["p.HttpStatus".into(), "p.status".into()];
        res.rows = vec![row(&[("p.HttpStatus", "200"), ("p.status", "ok")])];
        assert_eq!(csv(&res), "p.HttpStatus,p.status\r\n200,ok\r\n");
    }

    #[test]
    fn columns_directive_renames_reorders_and_marks_missing() {
        let mut res = ReportResult::default();
        res.no_match_marker = "-".into();
        res.column_order = vec!["FILE".into(), "p.status".into()];
        res.rows = vec![row(&[("FILE", "a.jpg")])]; // p.status missing -> marker
        let header = Header {
            lines: vec![super::super::flow::HeaderLine::Directive {
                key: "columns".into(),
                value: "FILE as Name, p.status as Status".into(),
            }],
        };
        let text = String::from_utf8(CsvWriter.write(&res, &header)).unwrap();
        assert_eq!(text, "Name,Status\r\na.jpg,-\r\n");
    }

    #[test]
    fn fields_with_commas_quotes_and_newlines_are_escaped() {
        let mut res = ReportResult::default();
        res.column_order = vec!["resp".into()];
        res.rows = vec![row(&[("resp", "a,\"b\"\nc")])];
        assert_eq!(csv(&res), "resp\r\n\"a,\"\"b\"\"\nc\"\r\n");
    }
}
