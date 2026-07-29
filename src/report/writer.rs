//! Serializing a [`ReportResult`] to an output format. CSV, JSON and `.xlsx`
//! are supported; the [`ReportWriter`] trait keeps the interpreter/model
//! independent of the format so more can be added without touching either.
//!
//! Output is driven entirely by the resolved columns (the `columns:` header
//! directive, else the produced columns in first-seen order — see
//! [`ReportResult::resolved_columns`]) and the table-wide no-match marker
//! ([`ReportResult::no_match_marker`]), so what a run writes matches exactly
//! what the TUI grid shows (both read the same columns).

use super::compare::{MATCH, NO_BASELINE, NO_CANDIDATE, RESULT_COLUMN};
use super::flow::Header;
use super::model::ReportResult;

/// Serializes a run result to a concrete output format (bytes, so a binary
/// format like `.xlsx` fits the same interface). Fallible because a binary
/// serializer (xlsx) can fail (e.g. exceeding the format's row limit).
pub trait ReportWriter {
    /// Render `result` to bytes, using `header` for the `columns:` directive.
    fn write(&self, result: &ReportResult, header: &Header) -> Result<Vec<u8>, String>;
}

/// The set of output formats PaperTrail can write, keyed by lower-case file
/// extension (`csv`/`json`/`html`/`xlsx`). Returns `None` for anything else so
/// callers can report an unsupported-format error naming the extension.
pub fn writer_for_extension(ext: &str) -> Option<Box<dyn ReportWriter>> {
    match ext.to_ascii_lowercase().as_str() {
        "csv" => Some(Box::new(CsvWriter)),
        "json" => Some(Box::new(JsonWriter)),
        "html" | "htm" => Some(Box::new(HtmlWriter)),
        "xlsx" => Some(Box::new(XlsxWriter)),
        _ => None,
    }
}

/// The list of supported output extensions, for help/error text.
pub const OUTPUT_EXTENSIONS: [&str; 4] = ["csv", "json", "html", "xlsx"];

/// Writes a report as RFC 4180 CSV (comma-separated, `\r\n` line endings,
/// minimal quoting). The header row is the resolved column headers; each data
/// row coalesces its column sources, substituting the no-match marker for an
/// empty result.
pub struct CsvWriter;

impl ReportWriter for CsvWriter {
    fn write(&self, result: &ReportResult, header: &Header) -> Result<Vec<u8>, String> {
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

        Ok(out.into_bytes())
    }
}

/// Writes a report as JSON: an object with the resolved `columns` (in output
/// order) and `rows` (one object per row keyed by column header). Cell values
/// are the same coalesced strings the CSV/grid show, so the JSON never loses or
/// reorders information (object key order is preserved via serde_json's
/// `preserve_order`).
pub struct JsonWriter;

impl ReportWriter for JsonWriter {
    fn write(&self, result: &ReportResult, header: &Header) -> Result<Vec<u8>, String> {
        let columns = result.resolved_columns(header);
        let headers: Vec<&str> = columns.iter().map(|c| c.header.as_str()).collect();
        let rows: Vec<serde_json::Value> = result
            .rows
            .iter()
            .map(|row| {
                let mut obj = serde_json::Map::new();
                for c in &columns {
                    obj.insert(
                        c.header.clone(),
                        serde_json::Value::String(c.value(row, &result.no_match_marker)),
                    );
                }
                serde_json::Value::Object(obj)
            })
            .collect();
        let doc = serde_json::json!({ "columns": headers, "rows": rows });
        serde_json::to_vec_pretty(&doc).map_err(|e| e.to_string())
    }
}

/// Writes a report as a self-contained `.html` file: a single styled `<table>`
/// (all CSS inline in a `<style>` block, no external assets) that opens in any
/// browser with a double-click. The header row and colour-coded status/verdict
/// cells mirror the xlsx output (green = pass/`OK`, red = error, amber =
/// changed), so a non-technical reviewer can read a run without a spreadsheet
/// program. Cell text is HTML-escaped and newlines preserved, so a multi-line
/// response body is shown faithfully.
pub struct HtmlWriter;

impl ReportWriter for HtmlWriter {
    fn write(&self, result: &ReportResult, header: &Header) -> Result<Vec<u8>, String> {
        let columns = result.resolved_columns(header);
        let mut out = String::new();
        out.push_str(
            "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
             <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
             <title>PaperTrail report</title>\n<style>\n\
             body{font-family:system-ui,-apple-system,Segoe UI,Roboto,sans-serif;margin:1rem;color:#222}\n\
             table{border-collapse:collapse;width:100%;font-size:14px}\n\
             th,td{border:1px solid #ccc;padding:6px 8px;text-align:left;vertical-align:top;white-space:pre-wrap;word-break:break-word}\n\
             thead th{position:sticky;top:0;background:#333;color:#fff}\n\
             tbody tr:nth-child(even){background:#f7f7f7}\n\
             td.pass{background:#c6efce}\n\
             td.fail{background:#ffc7ce}\n\
             td.warn{background:#ffeb9c}\n\
             </style>\n</head>\n<body>\n<table>\n<thead>\n<tr>",
        );
        for c in &columns {
            out.push_str("<th>");
            push_escaped(&mut out, &c.header);
            out.push_str("</th>");
        }
        out.push_str("</tr>\n</thead>\n<tbody>\n");
        for row in &result.rows {
            out.push_str("<tr>");
            for c in &columns {
                let value = c.value(row, &result.no_match_marker);
                let class = match cell_tint(&c.header, &value) {
                    Some(Tint::Green) => " class=\"pass\"",
                    Some(Tint::Red) => " class=\"fail\"",
                    Some(Tint::Amber) => " class=\"warn\"",
                    None => "",
                };
                out.push_str("<td");
                out.push_str(class);
                out.push('>');
                push_escaped(&mut out, &value);
                out.push_str("</td>");
            }
            out.push_str("</tr>\n");
        }
        out.push_str("</tbody>\n</table>\n</body>\n</html>\n");
        Ok(out.into_bytes())
    }
}

/// Append `text` to `out` with the five XML/HTML special characters escaped, so
/// a cell value can never break out of its `<td>` or inject markup.
fn push_escaped(out: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
}

/// Writes a report as a styled `.xlsx` workbook: one worksheet, a bold header
/// row, and one row per data row. Recognisable status/verdict cells are
/// colour-coded (green = pass/`OK`, red = error/failure, amber = changed) so a
/// reviewer can scan a large run the way the sample production reports do —
/// without any product-specific knowledge (colouring is purely value-driven).
pub struct XlsxWriter;

impl ReportWriter for XlsxWriter {
    fn write(&self, result: &ReportResult, header: &Header) -> Result<Vec<u8>, String> {
        use rust_xlsxwriter::{Color, Format, FormatAlign, Workbook};

        let columns = result.resolved_columns(header);
        let mut workbook = Workbook::new();
        let sheet = workbook.add_worksheet();

        let header_fmt = Format::new()
            .set_bold()
            .set_background_color(Color::RGB(0x33_3333))
            .set_font_color(Color::White)
            .set_align(FormatAlign::Left);
        // Data cells wrap and top-align so a tall JSON response body stays
        // readable (mirrors the sample's tall rows) instead of being clipped.
        let body_fmt = Format::new().set_text_wrap().set_align(FormatAlign::Top);
        let make_status = |rgb: u32| {
            Format::new()
                .set_text_wrap()
                .set_align(FormatAlign::Top)
                .set_background_color(Color::RGB(rgb))
        };
        let green = make_status(0xC6_EFCE);
        let red = make_status(0xFF_C7CE);
        let amber = make_status(0xFF_EB9C);

        // Header row.
        for (col, c) in columns.iter().enumerate() {
            sheet
                .write_string_with_format(0, col as u16, &c.header, &header_fmt)
                .map_err(|e| e.to_string())?;
        }

        // Data rows.
        for (r, row) in result.rows.iter().enumerate() {
            let excel_row = (r + 1) as u32;
            for (col, c) in columns.iter().enumerate() {
                let value = c.value(row, &result.no_match_marker);
                let fmt = match cell_tint(&c.header, &value) {
                    Some(Tint::Green) => &green,
                    Some(Tint::Red) => &red,
                    Some(Tint::Amber) => &amber,
                    None => &body_fmt,
                };
                sheet
                    .write_string_with_format(excel_row, col as u16, &value, fmt)
                    .map_err(|e| e.to_string())?;
            }
        }

        workbook.save_to_buffer().map_err(|e| e.to_string())
    }
}

/// A background tint for a colour-coded cell.
enum Tint {
    Green,
    Red,
    Amber,
}

/// The colour tint for a cell, or `None` to leave it plain. Recognises the
/// comparison [`RESULT_COLUMN`] verdicts, common textual status tokens
/// (`ok`/`success`/`pass` → green, `error`/`fail` → red, `changed`/`warning` →
/// amber), and 3-digit HTTP status codes (2xx green, 3xx amber, 4xx/5xx red).
/// Value-driven only, so it carries no product-specific assumptions.
fn cell_tint(header: &str, value: &str) -> Option<Tint> {
    let v = value.trim();
    if v.is_empty() {
        return None;
    }
    // The comparison Result column has known verdicts; anything else non-empty
    // there is a diff listing (a real difference) → amber.
    if header == RESULT_COLUMN {
        return Some(match v {
            MATCH => Tint::Green,
            NO_BASELINE => Tint::Amber,
            NO_CANDIDATE => Tint::Red,
            _ => Tint::Amber,
        });
    }
    let lower = v.to_ascii_lowercase();
    match lower.as_str() {
        "ok" | "success" | "succeeded" | "pass" | "passed" | "match" | "matched" | "true"
        | "done" | "complete" | "completed" => return Some(Tint::Green),
        "error" | "fail" | "failed" | "failure" | "false" | "no candidate" => {
            return Some(Tint::Red);
        }
        "changed" | "change" | "warning" | "warn" | "diff" | "different" | "mismatch" => {
            return Some(Tint::Amber);
        }
        _ => {}
    }
    // A bare 3-digit HTTP status code.
    if v.len() == 3
        && let Ok(code) = v.parse::<u16>()
        && (100..600).contains(&code)
    {
        return Some(match code / 100 {
            2 => Tint::Green,
            3 => Tint::Amber,
            _ => Tint::Red,
        });
    }
    None
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
///
/// Cells come from arbitrary HTTP responses, so a value beginning with a
/// spreadsheet formula trigger (`=`, `+`, `@`, tab or CR) is prefixed with a `'`
/// first — the standard mitigation against CSV/formula injection when the file
/// is opened in Excel/Sheets. The apostrophe is Excel's "treat as text" marker.
/// A leading `-` is deliberately *not* treated as a trigger: it almost always
/// denotes a negative number or a placeholder (the no-match marker is `-`), so
/// neutralising it would corrupt far more legitimate data than it protects.
fn escape_field(field: &str) -> String {
    let neutralised;
    let field = if field.starts_with(['=', '+', '@', '\t', '\r']) {
        neutralised = format!("'{field}");
        neutralised.as_str()
    } else {
        field
    };
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
            path: Vec::new(),
            target: None,
        }
    }

    fn csv(result: &ReportResult) -> String {
        String::from_utf8(CsvWriter.write(result, &Header::default()).unwrap()).unwrap()
    }

    #[test]
    fn default_columns_follow_first_seen_order() {
        let res = ReportResult {
            column_order: vec!["p.HttpStatus".into(), "p.status".into()],
            rows: vec![row(&[("p.HttpStatus", "200"), ("p.status", "ok")])],
            ..Default::default()
        };
        assert_eq!(csv(&res), "p.HttpStatus,p.status\r\n200,ok\r\n");
    }

    #[test]
    fn columns_directive_renames_reorders_and_marks_missing() {
        let res = ReportResult {
            no_match_marker: "-".into(),
            column_order: vec!["FILE".into(), "p.status".into()],
            rows: vec![row(&[("FILE", "a.jpg")])], // p.status missing -> marker
            ..Default::default()
        };
        let header = Header {
            lines: vec![super::super::flow::HeaderLine::Directive {
                key: "columns".into(),
                value: "FILE as Name, p.status as Status".into(),
            }],
        };
        let text = String::from_utf8(CsvWriter.write(&res, &header).unwrap()).unwrap();
        assert_eq!(text, "Name,Status\r\na.jpg,-\r\n");
    }

    #[test]
    fn fields_with_commas_quotes_and_newlines_are_escaped() {
        let res = ReportResult {
            column_order: vec!["resp".into()],
            rows: vec![row(&[("resp", "a,\"b\"\nc")])],
            ..Default::default()
        };
        assert_eq!(csv(&res), "resp\r\n\"a,\"\"b\"\"\nc\"\r\n");
    }

    #[test]
    fn formula_leading_fields_are_neutralised_against_injection() {
        let mut res = ReportResult {
            column_order: vec!["body".into()],
            rows: vec![row(&[("body", "=1+SUM(A1)")])],
            ..Default::default()
        };
        // The `=` trigger is prefixed with `'` so a spreadsheet treats it as
        // text; the resulting field has no special chars so it stays unquoted.
        assert_eq!(csv(&res), "body\r\n'=1+SUM(A1)\r\n");

        // A trigger combined with a special char is still fully quoted.
        res.rows = vec![row(&[("body", "@cmd,tail")])];
        assert_eq!(csv(&res), "body\r\n\"'@cmd,tail\"\r\n");

        // A leading `-` is left alone (negatives / the no-match marker `-`).
        res.rows = vec![row(&[("body", "-42")])];
        assert_eq!(csv(&res), "body\r\n-42\r\n");
    }

    #[test]
    fn json_output_is_columns_plus_row_objects_in_order() {
        let res = ReportResult {
            column_order: vec!["FILE".into(), "status".into()],
            rows: vec![
                row(&[("FILE", "a.jpg"), ("status", "ok")]),
                row(&[("FILE", "b.jpg"), ("status", "error")]),
            ],
            ..Default::default()
        };
        let bytes = JsonWriter.write(&res, &Header::default()).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["columns"], serde_json::json!(["FILE", "status"]));
        assert_eq!(v["rows"][0]["FILE"], "a.jpg");
        assert_eq!(v["rows"][1]["status"], "error");
        // Object key order follows the column order (preserve_order).
        let keys: Vec<&str> = v["rows"][0]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, vec!["FILE", "status"]);
    }

    #[test]
    fn json_missing_source_uses_the_no_match_marker() {
        let res = ReportResult {
            no_match_marker: "∅".into(),
            column_order: vec!["FILE".into(), "missing".into()],
            rows: vec![row(&[("FILE", "a.jpg")])],
            ..Default::default()
        };
        let bytes = JsonWriter.write(&res, &Header::default()).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["rows"][0]["missing"], "∅");
    }

    #[test]
    fn html_output_is_a_self_contained_table_with_escaping_and_tints() {
        let res = ReportResult {
            column_order: vec!["FILE".into(), "Status".into()],
            rows: vec![
                row(&[("FILE", "a & <b>.jpg"), ("Status", "success")]),
                row(&[("FILE", "c.jpg"), ("Status", "error")]),
            ],
            ..Default::default()
        };
        let bytes = HtmlWriter.write(&res, &Header::default()).unwrap();
        let html = String::from_utf8(bytes).unwrap();
        assert!(html.starts_with("<!DOCTYPE html>"), "is an HTML document");
        assert!(html.contains("<table>"), "has a table");
        assert!(html.contains("<th>FILE</th>"), "header cell: {html}");
        // The special characters in the first FILE cell are escaped.
        assert!(
            html.contains("a &amp; &lt;b&gt;.jpg"),
            "cell text is HTML-escaped: {html}"
        );
        // Status cells are colour-coded by class.
        assert!(
            html.contains("<td class=\"pass\">success</td>"),
            "success is tinted pass: {html}"
        );
        assert!(
            html.contains("<td class=\"fail\">error</td>"),
            "error is tinted fail: {html}"
        );
        // Self-contained: no external stylesheet/script references.
        assert!(!html.contains("http://") && !html.contains("https://"));
    }

    #[test]
    fn xlsx_output_is_a_valid_nonempty_zip() {
        let res = ReportResult {
            column_order: vec!["FILE".into(), "status".into()],
            rows: vec![row(&[("FILE", "a.jpg"), ("status", "success")])],
            ..Default::default()
        };
        let bytes = XlsxWriter.write(&res, &Header::default()).unwrap();
        assert!(!bytes.is_empty(), "xlsx produced bytes");
        // Every .xlsx is a ZIP container, so it starts with the ZIP magic `PK`.
        assert_eq!(&bytes[..2], b"PK", "starts with the ZIP local-file magic");
    }

    #[test]
    fn cell_tint_recognises_status_and_result_verdicts() {
        assert!(matches!(cell_tint("Status", "success"), Some(Tint::Green)));
        assert!(matches!(cell_tint("Status", "ERROR"), Some(Tint::Red)));
        assert!(matches!(cell_tint("Status", "changed"), Some(Tint::Amber)));
        assert!(matches!(cell_tint("HttpStatus", "200"), Some(Tint::Green)));
        assert!(matches!(cell_tint("HttpStatus", "503"), Some(Tint::Red)));
        assert!(matches!(cell_tint("Result", MATCH), Some(Tint::Green)));
        assert!(matches!(cell_tint("Result", NO_CANDIDATE), Some(Tint::Red)));
        assert!(matches!(
            cell_tint("Result", "status: a≠b"),
            Some(Tint::Amber)
        ));
        assert!(cell_tint("Name", "anything.jpg").is_none());
        assert!(cell_tint("Status", "  ").is_none());
    }
}
