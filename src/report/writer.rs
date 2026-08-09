//! Serializing a [`ReportResult`] to an output format. CSV, JSON and `.xlsx`
//! are supported; the [`ReportWriter`] trait keeps the interpreter/model
//! independent of the format so more can be added without touching either.
//!
//! Output is driven entirely by the resolved columns (the `columns:` header
//! directive, else the produced columns in first-seen order — see
//! [`ReportResult::resolved_columns`]) and the table-wide no-match marker
//! ([`ReportResult::no_match_marker`]), so what a run writes matches exactly
//! what the TUI grid shows (both read the same columns).

use super::compare::{CORRECT_COLUMN, MATCH, NO_BASELINE, NO_CANDIDATE, RESULT_COLUMN};
use super::flow::Header;
use super::model::{OutputColumn, ReportResult, Verdict};

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

/// The preferred output extension for `report`: its `# output:` header format
/// when that names a supported writer, else `csv`.
///
/// Used by both front-ends to seed their export picker, so a report declaring
/// `# output: xlsx` exports `.xlsx` by default (and the user can still choose
/// another format in the dialog). An unparseable report, or one naming a format
/// PaperBoy can't write, falls back to CSV rather than refusing to export.
pub fn report_output_extension(report: &crate::report::Report) -> String {
    report
        .flow()
        .ok()
        .and_then(|f| f.header.output().map(|o| o.trim().to_ascii_lowercase()))
        .filter(|ext| writer_for_extension(ext).is_some())
        .unwrap_or_else(|| "csv".to_string())
}

/// Where an export with extension `ext` lands: alongside a saved report (same
/// stem), else `<name>.<ext>` in the current directory for a scratch report.
///
/// When the report *name* carries an output token (`{time}`), the expanded name
/// wins — even for a saved report — and lands in the report's own folder (or the
/// current directory for a scratch report), so repeated runs write distinct
/// timestamped files rather than overwriting one export. Shared by both
/// front-ends and by both kinds of export (a results file and a `.baseline`
/// snapshot), so the same report always suggests the same name.
pub fn export_path(report: &crate::report::Report, ext: &str) -> std::path::PathBuf {
    if let Some(p) = tokened_output_path(report, ext) {
        return p;
    }
    if let Some(path) = &report.path {
        return path.with_extension(ext);
    }
    std::path::PathBuf::from(format!("{}.{ext}", sanitize_file_stem(&report.name)))
}

/// The output path when the report name carries an output token (`{time}`): the
/// token-expanded, sanitised name as the file stem with extension `ext`, placed
/// in the saved report's own folder (or the current directory for a scratch
/// report). `None` when the name has no token, so callers fall back to their
/// normal (file-stem-based) derivation.
fn tokened_output_path(report: &crate::report::Report, ext: &str) -> Option<std::path::PathBuf> {
    if !crate::report::name_has_output_token(&report.name) {
        return None;
    }
    let stem = sanitize_file_stem(&crate::report::expand_output_tokens(&report.name));
    let file = format!("{stem}.{ext}");
    match report.path.as_ref().and_then(|p| p.parent()) {
        Some(d) => Some(d.join(file)),
        None => Some(std::path::PathBuf::from(file)),
    }
}

/// Turn a display name into a safe single-segment file stem (replacing path
/// separators and other awkward characters with `_`), so a scratch report's
/// name can't escape the current directory when exported.
fn sanitize_file_stem(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "report".to_string()
    } else {
        trimmed.to_string()
    }
}

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

        // Appended statistics and ground-truth metric rows (empty when the
        // report asked for neither). CSV has one table and no header block, so
        // the metrics can only live in the footer.
        for srow in result.footer_rows(&columns, header) {
            let cells: Vec<String> = (0..columns.len()).map(|c| srow.text_cell(c)).collect();
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
        let mut doc = doc;
        // Appended statistics summary rows, keyed like the data rows (the row's
        // label lands in the first column). Omitted entirely when none exist.
        let summary: Vec<serde_json::Value> = result
            .footer_rows(&columns, header)
            .iter()
            .map(|srow| {
                let mut obj = serde_json::Map::new();
                for (ci, c) in columns.iter().enumerate() {
                    obj.insert(
                        c.header.clone(),
                        serde_json::Value::String(srow.text_cell(ci)),
                    );
                }
                serde_json::Value::Object(obj)
            })
            .collect();
        if !summary.is_empty() {
            doc.as_object_mut()
                .unwrap()
                .insert("summary".to_string(), serde_json::Value::Array(summary));
        }
        // The metrics also go out structured, not just as footer text: JSON is
        // the format a dashboard or a CI gate reads, and re-parsing "95.9%" out
        // of a summary row would be a silly thing to make anyone do.
        if let Some(metrics) = result.metrics(&columns, header) {
            doc.as_object_mut()
                .unwrap()
                .insert("metrics".to_string(), metrics_json(&metrics));
        }
        serde_json::to_vec_pretty(&doc).map_err(|e| e.to_string())
    }
}

/// The `metrics` object of the JSON export: the same figures the footer rows
/// carry, but as numbers a dashboard or a CI gate can read without parsing
/// "95.9%" back out of a string.
fn metrics_json(metrics: &super::metrics::Metrics) -> serde_json::Value {
    let column = |m: &super::metrics::ColumnMetrics| {
        let mut obj = serde_json::json!({
            "column": m.header,
            "total": m.total,
            "compared": m.compared,
            "correct": m.correct,
            "incorrect": m.incorrect,
            "accuracy": m.accuracy(),
        });
        if let Some(matrix) = &m.matrix {
            obj.as_object_mut().unwrap().insert(
                "confusion".to_string(),
                serde_json::json!({
                    "axis": matrix.axis,
                    // Rows are the truth, columns the value the run produced.
                    "counts": matrix.counts,
                }),
            );
        }
        obj
    };
    let mut doc = serde_json::json!({
        "columns": metrics.columns.iter().map(column).collect::<Vec<_>>(),
    });
    if let Some(overall) = &metrics.overall {
        doc.as_object_mut()
            .unwrap()
            .insert("overall".to_string(), column(overall));
    }
    doc
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
             table{border-collapse:collapse;table-layout:fixed;width:max-content;\
             min-width:100%;font-size:14px}\n\
             th,td{border:1px solid #ccc;padding:6px 8px;text-align:left;vertical-align:top;\
             white-space:pre-wrap;overflow-wrap:anywhere}\n\
             thead th{position:sticky;top:0;background:#333;color:#fff;\
             white-space:nowrap;overflow-wrap:normal}\n\
             tbody tr:nth-child(even){background:#f7f7f7}\n\
             tfoot td{font-weight:bold;background:#ececec;border-top:2px solid #999}\n\
             td.pass{background:#c6efce}\n\
             td.fail{background:#ffc7ce}\n\
             td.warn{background:#ffeb9c}\n\
             .metrics{display:flex;flex-wrap:wrap;gap:.75rem;margin:0 0 1rem}\n\
             .card{border:1px solid #ccc;border-radius:6px;padding:.5rem .9rem;background:#fafafa}\n\
             .card .k{display:block;font-size:12px;color:#666;text-transform:uppercase;\
             letter-spacing:.04em}\n\
             .card .v{display:block;font-size:20px;font-weight:600}\n\
             .matrix{margin:0 0 1.25rem}\n\
             .matrix h2{font-size:14px;font-weight:600;margin:0 0 .35rem}\n\
             .matrix table{width:auto;min-width:0;font-size:13px}\n\
             .matrix th,.matrix td{text-align:center;white-space:nowrap}\n\
             .matrix thead th{position:static;background:#fafafa;color:#222;font-weight:600}\n\
             .matrix th.axis{background:#fafafa;color:#222;text-align:right;font-weight:600}\n\
             .matrix td.cell{color:#12305a}\n\
             .matrix td.hot{color:#fff}\n\
             .matrix caption{caption-side:bottom;font-size:12px;color:#666;padding-top:.3rem;\
             text-align:left}\n\
             </style>\n</head>\n<body>\n",
        );
        // Ground-truth metrics go *above* the table, as cards and a matrix:
        // HTML has a header block, and a reader who wants to know whether the
        // run was any good should not have to scroll 500 rows to find out. The
        // flat formats put the same figures in the footer instead, so no
        // document ever states them twice.
        let metrics = result.metrics(&columns, header);
        if let Some(metrics) = &metrics {
            push_metric_cards(&mut out, metrics);
            for m in &metrics.columns {
                if let Some(matrix) = &m.matrix {
                    push_confusion_matrix(&mut out, &m.header, matrix);
                }
            }
        }
        out.push_str("<table>\n");
        // Sized columns, so the browser doesn't squeeze a short column to a few
        // characters and hyphenate its header (see `html_column_widths`). The
        // table is `table-layout:fixed`, which is what makes these binding
        // rather than a hint, and `width:max-content` so the sum is honoured and
        // the page scrolls sideways instead of the columns being squashed back.
        out.push_str("<colgroup>");
        for w in html_column_widths(&columns, result) {
            out.push_str(&format!("<col style=\"width:{w}ch\">"));
        }
        out.push_str("</colgroup>\n<thead>\n<tr>");
        for c in &columns {
            out.push_str("<th>");
            push_escaped(&mut out, &c.header);
            out.push_str("</th>");
        }
        out.push_str("</tr>\n</thead>\n<tbody>\n");
        for (r, row) in result.rows.iter().enumerate() {
            out.push_str("<tr>");
            for c in &columns {
                let value = c.value(row, &result.no_match_marker);
                let class = match run_cell_tint(result, r, &c.header, &value) {
                    Some(Tint::Green) => " class=\"pass\"",
                    Some(Tint::Red) => " class=\"fail\"",
                    Some(Tint::Amber) => " class=\"warn\"",
                    None => "",
                };
                out.push_str("<td");
                out.push_str(class);
                out.push('>');
                // An `IMAGE` column embeds the picture as a `data:` URI so the
                // file stays self-contained (the whole point of the HTML
                // export): a `<img src="http://…">` would break the moment the
                // pre-signed URL it came from expired.
                match result.images.get(&(r, c.header.clone())) {
                    Some(img) => push_html_image(&mut out, img, c.image, &value),
                    None => push_escaped(&mut out, &value),
                }
                out.push_str("</td>");
            }
            out.push_str("</tr>\n");
        }
        out.push_str("</tbody>\n");
        // Appended statistics summary rows in a distinct, bold footer.
        let summary = result.summary_rows(&columns);
        if !summary.is_empty() {
            out.push_str("<tfoot>\n");
            for srow in &summary {
                out.push_str("<tr>");
                for ci in 0..columns.len() {
                    out.push_str("<td>");
                    push_escaped(&mut out, &srow.text_cell(ci));
                    out.push_str("</td>");
                }
                out.push_str("</tr>\n");
            }
            out.push_str("</tfoot>\n");
        }
        out.push_str("</table>\n</body>\n</html>\n");
        Ok(out.into_bytes())
    }
}

/// The metric cards drawn above the table: one group per ground-truthed column
/// (plus the row roll-up), each stating what was compared, how much of it was
/// wrong, and the resulting accuracy.
fn push_metric_cards(out: &mut String, metrics: &super::metrics::Metrics) {
    use super::metrics::{ACCURACY_LABEL, COMPARED_LABEL, INCORRECT_LABEL};
    fn card(out: &mut String, k: &str, v: &str) {
        out.push_str("<div class=\"card\"><span class=\"k\">");
        push_escaped(out, k);
        out.push_str("</span><span class=\"v\">");
        push_escaped(out, v);
        out.push_str("</span></div>");
    }
    // The roll-up first when there is one: with several truth-bearing columns
    // it is the figure that answers "did this run pass?", and the per-column
    // breakdown is the follow-up question.
    let groups = metrics
        .overall
        .iter()
        .chain(metrics.columns.iter())
        .collect::<Vec<_>>();
    for m in groups {
        out.push_str("<div class=\"metrics\">");
        card(
            out,
            &format!("{} — {COMPARED_LABEL}", m.header),
            &format!("{} of {}", m.compared, m.total),
        );
        card(out, INCORRECT_LABEL, &m.incorrect.to_string());
        card(
            out,
            ACCURACY_LABEL,
            m.accuracy_text().as_deref().unwrap_or("\u{2014}"),
        );
        out.push_str("</div>\n");
    }
}

/// A confusion matrix as a heatmap: truth down the side, the value the run
/// produced across the top.
///
/// Shaded in a single hue rather than a green-to-red scale, because the
/// diagonal is not "good" in every matrix — for a rare-event detector the
/// interesting cells are off it — and a colour scheme that pre-judges which
/// cells are the bad ones is a scheme that misleads on exactly those reports.
/// The count is always printed, so the shading only ever ranks what the reader
/// can already read (and the report stays legible in greyscale, to a
/// colour-blind reader, and on a printout).
fn push_confusion_matrix(out: &mut String, column: &str, matrix: &super::metrics::ConfusionMatrix) {
    let max = matrix.max();
    out.push_str("<div class=\"matrix\"><h2>");
    push_escaped(out, column);
    out.push_str("</h2>\n<table><caption>");
    // A perfect matrix says so in words: leaving the reader to verify that
    // every off-diagonal cell is a zero is work a caption can do for them.
    let clean = if matrix.is_diagonal() {
        " Every scored row matched its ground truth."
    } else {
        ""
    };
    push_escaped(
        out,
        &format!(
            "Rows: ground truth. Columns: reported value. {} scored row(s).{clean}",
            matrix.total()
        ),
    );
    out.push_str("</caption>\n<thead><tr><th class=\"axis\"></th>");
    for label in &matrix.axis {
        out.push_str("<th>");
        push_escaped(out, label);
        out.push_str("</th>");
    }
    out.push_str("</tr></thead>\n<tbody>\n");
    for (t, label) in matrix.axis.iter().enumerate() {
        out.push_str("<tr><th class=\"axis\">");
        push_escaped(out, label);
        out.push_str("</th>");
        for p in 0..matrix.axis.len() {
            let n = matrix.counts[t][p];
            let (bg, hot) = heat_shade(n, max);
            out.push_str(&format!(
                "<td class=\"cell{}\" style=\"background:{bg}\">{n}</td>",
                if hot { " hot" } else { "" }
            ));
        }
        out.push_str("</tr>\n");
    }
    out.push_str("</tbody></table></div>\n");
}

/// The background for a heatmap cell holding `n` of a maximum `max`, and
/// whether the text on it needs to flip to white. A blue ramp: colour-blind
/// safe at both ends, and distinct from the green/amber/red the *data* cells
/// use for pass/changed/fail, so nobody reads a busy matrix cell as a failure.
fn heat_shade(n: usize, max: usize) -> (String, bool) {
    if n == 0 || max == 0 {
        return ("#ffffff".to_string(), false);
    }
    let f = n as f64 / max as f64;
    // Interpolate from a very pale blue to a deep one.
    let lerp = |from: f64, to: f64| (from + (to - from) * f).round() as u32;
    let (r, g, b) = (lerp(234.0, 8.0), lerp(242.0, 48.0), lerp(252.0, 107.0));
    (format!("#{r:02x}{g:02x}{b:02x}"), f > 0.55)
}

/// Append an `<img>` for a resolved picture, sized per the column's `IMAGE`
/// clause. The cell's text becomes the `alt`/`title`, so the source it came
/// from is still available on hover and to a screen reader.
fn push_html_image(
    out: &mut String,
    img: &super::model::ImageData,
    spec: Option<crate::report::flow::ImageSpec>,
    value: &str,
) {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&img.bytes);
    let style = match spec.and_then(|s| s.scaled_size(img.natural)) {
        Some((w, h)) => format!("width:{}px;height:{}px", w.round(), h.round()),
        // A `FIT` column has no fixed box, so the picture is capped to the
        // column instead -- the browser's equivalent of fitting to the cell.
        None => "max-width:100%;height:auto".to_string(),
    };
    out.push_str("<img style=\"");
    out.push_str(&style);
    out.push_str("\" alt=\"");
    push_escaped(out, value);
    out.push_str("\" title=\"");
    push_escaped(out, value);
    out.push_str("\" src=\"data:");
    out.push_str(&img.mime);
    out.push_str(";base64,");
    out.push_str(&b64);
    out.push_str("\">");
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
        // Pixel boxes for every picture that is going to be embedded, worked
        // out up front because they drive the row heights and column widths,
        // which have to be set before the cells are written.
        let boxes = xlsx_image_boxes(&columns, result);
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

        // Size the columns to their content. Left at Excel's 8.43-character
        // default, every column comes out equally tiny and the wrapped cells
        // become tall thin ribbons — the same run's HTML export looks right
        // only because the browser sizes the table itself.
        let mut widths = xlsx_column_widths(&columns, result);
        // An image column is sized to its widest picture, not to the URL or
        // path text underneath it -- the text is only a fallback there, and
        // sizing to it would leave every picture clipped.
        for (col, c) in columns.iter().enumerate() {
            if c.image.is_none() {
                continue;
            }
            let widest = (0..result.rows.len())
                .filter_map(|r| boxes.get(&(r, col)))
                .fold(0.0f64, |acc, (w, _)| acc.max(*w));
            if widest > 0.0 {
                widths[col] = clamp_xlsx_width(px_to_char_width(widest));
            }
        }
        for (col, width) in widths.into_iter().enumerate() {
            sheet
                .set_column_width(col as u16, width)
                .map_err(|e| e.to_string())?;
        }

        // Keep the headers on screen while scrolling a long run, and let the
        // reviewer filter it. The autofilter deliberately spans only the data
        // rows: including the appended statistics rows below would let them be
        // filtered away, or sorted into the middle of the data.
        if !columns.is_empty() {
            sheet.set_freeze_panes(1, 0).map_err(|e| e.to_string())?;
            let last_col = (columns.len() - 1) as u16;
            let last_row = result.rows.len() as u32;
            sheet
                .autofilter(0, 0, last_row, last_col)
                .map_err(|e| e.to_string())?;
        }

        // Which columns are numeric (every non-empty cell parses as a number):
        // their cells are written as real numbers so the spreadsheet can run
        // statistics on them, instead of text that Excel flags "stored as text".
        let numeric: Vec<bool> = columns
            .iter()
            .map(|c| column_is_numeric(c, result))
            .collect();

        // Data rows.
        for (r, row) in result.rows.iter().enumerate() {
            let excel_row = (r + 1) as u32;
            // A row carrying pictures has to be tall enough for the tallest of
            // them, or Excel draws the image overflowing into the rows below.
            let tallest = (0..columns.len())
                .filter_map(|col| boxes.get(&(r, col)))
                .fold(0.0f64, |acc, (_, h)| acc.max(*h));
            if tallest > 0.0 {
                sheet
                    .set_row_height_pixels(excel_row, tallest.ceil() as u32)
                    .map_err(|e| e.to_string())?;
            }
            for (col, c) in columns.iter().enumerate() {
                let value = c.value(row, &result.no_match_marker);
                let fmt = match run_cell_tint(result, r, &c.header, &value) {
                    Some(Tint::Green) => &green,
                    Some(Tint::Red) => &red,
                    Some(Tint::Amber) => &amber,
                    None => &body_fmt,
                };
                // An embedded picture replaces the cell's text rather than
                // sitting on top of it: the value is a URL or a path, which
                // would show through around the image and is of no interest to
                // the reader once the picture is there. It stays in the CSV and
                // JSON exports, which is where it is actually useful.
                if let Some(img) = result.images.get(&(r, c.header.clone())) {
                    let mut image = rust_xlsxwriter::Image::new_from_buffer(&img.bytes)
                        .map_err(|e| e.to_string())?
                        // The alt text is the value, so the information isn't
                        // lost -- a screen reader, or anyone who clicks the
                        // picture, still gets the source it came from.
                        .set_alt_text(&value);
                    if c.image.is_some_and(|i| i.fit) {
                        sheet
                            .insert_image_fit_to_cell(excel_row, col as u16, &image, true)
                            .map_err(|e| e.to_string())?;
                    } else {
                        if let Some((w, h)) = boxes.get(&(r, col)) {
                            image = image.set_scale_to_size(*w, *h, false);
                        }
                        sheet
                            .insert_image(excel_row, col as u16, &image)
                            .map_err(|e| e.to_string())?;
                    }
                    continue;
                }
                match parse_report_number(&value) {
                    Some(n) if numeric[col] => sheet
                        .write_number_with_format(excel_row, col as u16, n, fmt)
                        .map_err(|e| e.to_string())?,
                    _ => sheet
                        .write_string_with_format(excel_row, col as u16, &value, fmt)
                        .map_err(|e| e.to_string())?,
                };
            }
        }

        // Appended statistics summary rows. Numeric statistics are written as
        // *live* spreadsheet formulas over the column's data range (so they
        // recompute if a cell is edited); non-numeric ones and labels are
        // written as bold text.
        let nrows = result.rows.len();
        let summary = result.summary_rows(&columns);
        if !summary.is_empty() {
            let summary_fmt = Format::new()
                .set_bold()
                .set_text_wrap()
                .set_align(FormatAlign::Top)
                .set_background_color(Color::RGB(0xEC_ECEC));
            for (si, srow) in summary.iter().enumerate() {
                let excel_row = (nrows + 1 + si) as u32;
                for col in 0..columns.len() {
                    match srow.cells.get(col).and_then(|c| c.as_ref()) {
                        Some(v) => match xlsx_stat_formula(v, col, nrows) {
                            Some(formula) => {
                                sheet
                                    .write_formula_with_format(
                                        excel_row,
                                        col as u16,
                                        formula.as_str(),
                                        &summary_fmt,
                                    )
                                    .map_err(|e| e.to_string())?;
                            }
                            None => {
                                sheet
                                    .write_string_with_format(
                                        excel_row,
                                        col as u16,
                                        &v.text,
                                        &summary_fmt,
                                    )
                                    .map_err(|e| e.to_string())?;
                            }
                        },
                        None => {
                            let text = srow.text_cell(col);
                            if !text.is_empty() {
                                sheet
                                    .write_string_with_format(
                                        excel_row,
                                        col as u16,
                                        &text,
                                        &summary_fmt,
                                    )
                                    .map_err(|e| e.to_string())?;
                            }
                        }
                    }
                }
            }
        }

        // Ground-truth metrics get a sheet of their own rather than more footer
        // rows: a confusion matrix is a second table with its own axes, and
        // pasting it under a filtered data table would put it inside the
        // filter's range — where sorting the report would scramble it.
        if let Some(metrics) = result.metrics(&columns, header) {
            write_metrics_sheet(&mut workbook, &metrics)?;
        }

        workbook.save_to_buffer().map_err(|e| e.to_string())
    }
}

/// The `Metrics` worksheet: the accuracy figures, then one confusion matrix per
/// ground-truthed column that declared a label vocabulary.
fn write_metrics_sheet(
    workbook: &mut rust_xlsxwriter::Workbook,
    metrics: &super::metrics::Metrics,
) -> Result<(), String> {
    use super::metrics::{ACCURACY_LABEL, COMPARED_LABEL, INCORRECT_LABEL};
    use rust_xlsxwriter::{Color, Format, FormatAlign};

    let head = Format::new()
        .set_bold()
        .set_background_color(Color::RGB(0x33_3333))
        .set_font_color(Color::White);
    let label = Format::new().set_bold();
    let axis = Format::new().set_bold().set_align(FormatAlign::Right);
    let sheet = workbook.add_worksheet();
    sheet.set_name("Metrics").map_err(|e| e.to_string())?;
    sheet.set_column_width(0, 28.0).map_err(|e| e.to_string())?;

    let mut r: u32 = 0;
    let put = |sheet: &mut rust_xlsxwriter::Worksheet,
               row: u32,
               col: u16,
               text: &str,
               fmt: &Format|
     -> Result<(), String> {
        sheet
            .write_string_with_format(row, col, text, fmt)
            .map(|_| ())
            .map_err(|e| e.to_string())
    };
    for (c, h) in ["Column", COMPARED_LABEL, INCORRECT_LABEL, ACCURACY_LABEL]
        .iter()
        .enumerate()
    {
        put(sheet, r, c as u16, h, &head)?;
    }
    r += 1;
    for m in metrics.overall.iter().chain(metrics.columns.iter()) {
        put(sheet, r, 0, &m.header, &label)?;
        put(
            sheet,
            r,
            1,
            &format!("{} of {}", m.compared, m.total),
            &Format::new(),
        )?;
        sheet
            .write_number(r, 2, m.incorrect as f64)
            .map_err(|e| e.to_string())?;
        // Written as a real percentage, not the "95.9%" string the flat
        // formats show, so the cell can be charted or thresholded.
        if let Some(a) = m.accuracy() {
            sheet
                .write_number_with_format(r, 3, a, &Format::new().set_num_format("0.0%"))
                .map_err(|e| e.to_string())?;
        }
        r += 1;
    }

    for m in &metrics.columns {
        let Some(matrix) = &m.matrix else { continue };
        r += 2;
        put(
            sheet,
            r,
            0,
            &format!("{} — truth (down) by reported value (across)", m.header),
            &label,
        )?;
        r += 1;
        for (c, a) in matrix.axis.iter().enumerate() {
            put(sheet, r, c as u16 + 1, a, &head)?;
        }
        r += 1;
        for (t, a) in matrix.axis.iter().enumerate() {
            put(sheet, r, 0, a, &axis)?;
            for (p, n) in matrix.counts[t].iter().enumerate() {
                sheet
                    .write_number(r, p as u16 + 1, *n as f64)
                    .map_err(|e| e.to_string())?;
            }
            r += 1;
        }
    }
    Ok(())
}

/// A background tint for a colour-coded cell.
enum Tint {
    Green,
    Red,
    Amber,
}

/// The narrowest a column may be sized, in Excel character widths. Excel's own
/// default is 8.43, and a report column is never usefully narrower than its
/// (bold, filtered) header.
const XLSX_MIN_COL_WIDTH: f64 = 9.0;

/// The widest a column may be sized, in Excel character widths. Report cells
/// can hold an entire JSON response body, so the width has to be capped or one
/// such column pushes every other column off the screen — which is exactly what
/// a sized-to-content-only export looks like. Cells are already wrapped and
/// top-aligned, so anything longer than this stays fully visible by growing the
/// row taller instead of the column wider.
const XLSX_MAX_COL_WIDTH: f64 = 60.0;

/// Padding added to a measured header, in characters: headers are bold (so
/// wider per character than the body font Excel measures against) and carry an
/// autofilter dropdown arrow, which overlaps the text without it.
const XLSX_HEADER_PADDING: usize = 5;

/// Padding added to a measured body cell, so text doesn't touch the gridline.
const XLSX_CELL_PADDING: usize = 2;

/// How wide a cell's text needs to be displayed, in characters.
///
/// Cells are wrapped, so what matters is the longest *line*, not the total
/// length: a 40-line JSON body whose longest line is 30 characters needs 30,
/// not 1200. Measured in `char`s rather than bytes so non-ASCII content isn't
/// over-measured into a needlessly wide column.
fn text_display_width(text: &str) -> usize {
    text.lines().map(|l| l.chars().count()).max().unwrap_or(0)
}

/// Clamp a measured character count to the column-width range Excel is given.
fn clamp_xlsx_width(measured: usize) -> f64 {
    (measured as f64).clamp(XLSX_MIN_COL_WIDTH, XLSX_MAX_COL_WIDTH)
}

/// Excel's column width unit is "characters of the default font", which is
/// about 7 pixels wide, plus ~5px of cell padding. Converting the other way is
/// what lets an image column be sized to its pictures.
fn px_to_char_width(px: f64) -> usize {
    (((px - 5.0).max(0.0)) / 7.0).ceil() as usize
}

/// The pixel `(width, height)` box each embedded picture is drawn in, keyed by
/// `(row index, column index)`.
///
/// Computed before anything is written because the boxes drive both the row
/// heights and the image columns' widths, and Excel wants those set before the
/// cells. `FIT` columns are absent from the map: their sizing is the cell's, so
/// they neither need nor should get a row-height bump.
fn xlsx_image_boxes(
    columns: &[OutputColumn],
    result: &ReportResult,
) -> std::collections::HashMap<(usize, usize), (f64, f64)> {
    let mut out = std::collections::HashMap::new();
    for (col, c) in columns.iter().enumerate() {
        let Some(spec) = c.image else { continue };
        if spec.fit {
            continue;
        }
        for r in 0..result.rows.len() {
            if let Some(img) = result.images.get(&(r, c.header.clone()))
                && let Some(size) = spec.scaled_size(img.natural)
            {
                out.insert((r, col), size);
            }
        }
    }
    out
}

/// Per-column widths for the xlsx export, sized to the widest thing each column
/// actually has to show — header, data cells and the appended statistics rows
/// alike — then clamped to [`XLSX_MIN_COL_WIDTH`]..=[`XLSX_MAX_COL_WIDTH`].
///
/// Without this every column is left at Excel's 8.43-character default, so a
/// report exports as a row of tiny columns full of wrapped ribbons of text,
/// while the same run's HTML export looks fine (the browser sizes the table
/// for us). Kept separate from the writing loop so the sizing can be tested
/// without unzipping a workbook.
fn xlsx_column_widths(columns: &[OutputColumn], result: &ReportResult) -> Vec<f64> {
    measured_column_widths(columns, result)
        .into_iter()
        .map(clamp_xlsx_width)
        .collect()
}

/// How many characters wide each column needs to be to show its content
/// unwrapped — header, data cells and the appended statistics rows alike, each
/// with its own padding. Unclamped: every export wants the same measurement but
/// caps it in its own units.
fn measured_column_widths(columns: &[OutputColumn], result: &ReportResult) -> Vec<usize> {
    let mut widths: Vec<usize> = columns
        .iter()
        .map(|c| text_display_width(&c.header) + XLSX_HEADER_PADDING)
        .collect();
    for row in &result.rows {
        for (col, c) in columns.iter().enumerate() {
            let value = c.value(row, &result.no_match_marker);
            let want = text_display_width(&value) + XLSX_CELL_PADDING;
            if want > widths[col] {
                widths[col] = want;
            }
        }
    }
    // Statistics rows are bold, and their labels ("Mean", "Distribution") can
    // be wider than anything in the column above them.
    for srow in result.summary_rows(columns) {
        for (col, width) in widths.iter_mut().enumerate() {
            let want = text_display_width(&srow.text_cell(col)) + XLSX_HEADER_PADDING;
            if want > *width {
                *width = want;
            }
        }
    }
    widths
}

/// The widest a column is sized in the HTML export, in `ch` units. Higher than
/// the xlsx cap because a browser scrolls a wide table sideways rather than
/// hiding what runs off the page, but still a cap: one column holding a JSON
/// body must not push every other column out of the first screenful.
const HTML_MAX_COL_WIDTH: usize = 70;

/// The narrowest, so a one-character column ("#", a tick) still reads as a
/// column rather than a sliver.
const HTML_MIN_COL_WIDTH: usize = 6;

/// Per-column `<colgroup>` widths for the HTML export, in `ch`.
///
/// Without them the browser's automatic table layout distributes the width it
/// is given, which squeezes a narrow column down to a few characters and — with
/// the wrapping the long cells need — breaks its header mid-word, so an
/// `Environment` column reads "Enviro / ment" with every value wrapped beneath
/// it. That is the same failure the xlsx export had before it was sized to its
/// content, so it is fixed the same way and off the same measurement.
fn html_column_widths(columns: &[OutputColumn], result: &ReportResult) -> Vec<usize> {
    measured_column_widths(columns, result)
        .into_iter()
        .map(|w| w.clamp(HTML_MIN_COL_WIDTH, HTML_MAX_COL_WIDTH))
        .collect()
}

/// The colour tint for a cell of a *run*, which is [`cell_tint`] with ground
/// truth layered over it.
///
/// A ground-truthed cell is tinted by whether it is **right**, not by what it
/// says: an engine that answers `fail` where the truth is `fail` is a green
/// cell, even though `cell_tint`'s word list would call it red. That is the
/// whole point of declaring a truth — without this the colours would go on
/// reporting the sentiment of the word rather than the quality of the answer.
///
/// `Untested` is left plain, deliberately: a row nobody has labelled must not
/// borrow the appearance of one that passed.
fn run_cell_tint(result: &ReportResult, r: usize, header: &str, value: &str) -> Option<Tint> {
    if let Some(v) = result.verdicts.get(&(r, header.to_string())) {
        return match v {
            Verdict::Correct => Some(Tint::Green),
            Verdict::Incorrect => Some(Tint::Red),
            Verdict::Untested => None,
        };
    }
    // The roll-up column holds the verdict as text, so it tints the same way.
    if header == CORRECT_COLUMN {
        return match value.trim() {
            v if v == Verdict::Correct.as_str() => Some(Tint::Green),
            v if v == Verdict::Incorrect.as_str() => Some(Tint::Red),
            _ => None,
        };
    }
    cell_tint(header, value)
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

/// The Excel A1 column letters for a 0-based column index (0 → `A`, 26 → `AA`).
fn col_letter(mut n: usize) -> String {
    let mut s = String::new();
    loop {
        s.insert(0, (b'A' + (n % 26) as u8) as char);
        if n < 26 {
            break;
        }
        n = n / 26 - 1;
    }
    s
}

/// The live xlsx formula for a summary statistic cell, or `None` when it should
/// be written as plain text instead (a non-numeric column's Mode/Count, or an
/// empty data range). Numeric statistics use legacy function names (`STDEVP`,
/// `MODE`) so no `_xlfn.` future-function prefix is needed; `Distribution` uses
/// `COUNTIF` with the value as a quoted criterion.
fn xlsx_stat_formula(
    v: &crate::report::model::StatValue,
    col: usize,
    nrows: usize,
) -> Option<String> {
    use crate::report::model::StatKind;
    if nrows == 0 {
        return None;
    }
    let letter = col_letter(col);
    // Data occupies A1-style rows 2..=nrows+1 (row 1 is the header).
    let range = format!("{letter}2:{letter}{}", nrows + 1);
    if v.stat == Some(StatKind::Distribution) {
        let crit = v.match_value.as_deref().unwrap_or("").replace('"', "\"\"");
        return Some(format!("=COUNTIF({range},\"{crit}\")"));
    }
    if !v.numeric {
        return None;
    }
    let f = match v.stat? {
        StatKind::Mean => format!("=AVERAGE({range})"),
        StatKind::Median => format!("=MEDIAN({range})"),
        StatKind::Sum => format!("=SUM({range})"),
        StatKind::Min => format!("=MIN({range})"),
        StatKind::Max => format!("=MAX({range})"),
        StatKind::StdDev => format!("=STDEVP({range})"),
        StatKind::Mode => format!("=MODE({range})"),
        StatKind::Count => format!("=COUNT({range})"),
        StatKind::Distribution => unreachable!(),
    };
    Some(f)
}

/// Parse a report cell as a finite number for spreadsheet output. Trims
/// surrounding whitespace and rejects empties. A value with a redundant leading
/// zero on its integer part (e.g. an id like `007` or `-08`) is deliberately
/// *not* treated as a number, so writing it as an xlsx number can't silently
/// drop the zero; a lone `0` is fine. This keeps genuine numeric columns
/// (times, counts, amounts) usable for spreadsheet statistics while leaving
/// identifier-like text intact.
pub(crate) fn parse_report_number(value: &str) -> Option<f64> {
    let v = value.trim();
    if v.is_empty() {
        return None;
    }
    let digits = v.strip_prefix(['+', '-']).unwrap_or(v);
    let mut chars = digits.chars();
    if chars.next() == Some('0') && chars.next().is_some_and(|c| c.is_ascii_digit()) {
        return None;
    }
    let n: f64 = v.parse().ok()?;
    n.is_finite().then_some(n)
}

/// Whether output `column` is numeric: it has at least one non-empty cell and
/// every non-empty cell parses as a number (see [`parse_report_number`]). Such
/// a column is written to xlsx as real numbers so the spreadsheet can run
/// statistics on it, rather than as text.
fn column_is_numeric(column: &OutputColumn, result: &ReportResult) -> bool {
    let mut saw_value = false;
    for row in &result.rows {
        let v = column.value(row, &result.no_match_marker);
        let t = v.trim();
        if t.is_empty() || v == result.no_match_marker {
            continue;
        }
        if parse_report_number(&v).is_none() {
            return false;
        }
        saw_value = true;
    }
    saw_value
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

    /// The `# output:` directive picks the export format, whatever its case.
    #[test]
    fn the_output_directive_chooses_the_export_extension() {
        let r = crate::report::Report::from_text("s", "# output: XLSX\n# collection: c.hurl\n");
        assert_eq!(report_output_extension(&r), "xlsx");
    }

    /// Anything PaperBoy can't write — and a report with no directive at all —
    /// falls back to CSV, so the export button always does something.
    #[test]
    fn an_unwritable_or_absent_output_directive_falls_back_to_csv() {
        let r = crate::report::Report::from_text("s", "# output: pdf\n# collection: c.hurl\n");
        assert_eq!(report_output_extension(&r), "csv");
        let r = crate::report::Report::from_text("s", "# collection: c.hurl\n");
        assert_eq!(report_output_extension(&r), "csv");
        let r = crate::report::Report::from_text("s", "this is not a report at all {{{\n");
        assert_eq!(report_output_extension(&r), "csv");
    }

    /// An export lands beside the report it came from, under its own stem.
    #[test]
    fn an_export_lands_beside_a_saved_report() {
        let mut r = crate::report::Report::from_text("sample", "# collection: c.hurl\n");
        r.path = Some(std::path::PathBuf::from("/tmp/reports/sample.trail"));
        assert_eq!(
            export_path(&r, "xlsx"),
            std::path::PathBuf::from("/tmp/reports/sample.xlsx")
        );
        // The same rule names a baseline snapshot, so the two agree.
        assert_eq!(
            export_path(&r, "baseline"),
            std::path::PathBuf::from("/tmp/reports/sample.baseline")
        );
    }

    /// A scratch report has no file to sit beside, so its display name becomes
    /// the stem — sanitised, so it can't escape the current directory.
    #[test]
    fn a_scratch_reports_name_is_sanitised_into_the_stem() {
        let r = crate::report::Report::from_text("s", "# name: ../../etc/passwd\n");
        let p = export_path(&r, "csv");
        assert_eq!(p, std::path::PathBuf::from("______etc_passwd.csv"));
        assert_eq!(p.components().count(), 1, "must stay a single segment");
    }

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

    /// The reported bug: an `Environment` column came out a few characters wide
    /// with its header broken across two lines ("Enviro"/"ment") and every
    /// value wrapped under it, because the browser's automatic layout squeezed
    /// the table to the page. The columns are sized to their content, the same
    /// as the xlsx export.
    #[test]
    fn html_columns_are_sized_to_their_content() {
        let res = ReportResult {
            column_order: vec!["Environment".into(), "Body".into()],
            rows: vec![row(&[
                ("Environment", "staging_au"),
                ("Body", &"x".repeat(400)),
            ])],
            ..Default::default()
        };
        let widths = html_column_widths(&res.resolved_columns(&Header::default()), &res);
        assert!(
            widths[0] >= "Environment".len(),
            "the header fits on one line, got {}ch",
            widths[0]
        );
        assert_eq!(
            widths[1], HTML_MAX_COL_WIDTH,
            "a 400-character body is capped, not allowed to push everything else off the page"
        );

        let html = String::from_utf8(HtmlWriter.write(&res, &Header::default()).unwrap()).unwrap();
        assert!(
            html.contains(&format!("<col style=\"width:{}ch\">", widths[0])),
            "the widths reach the document: {html}"
        );
        // Fixed layout is what makes the widths binding, and `max-content` is
        // what stops the table being squashed back to the page width.
        assert!(html.contains("table-layout:fixed"), "{html}");
        assert!(html.contains("width:max-content"), "{html}");
        // A header must never be broken inside a word — that is the reported
        // symptom — while a long body cell still has to break somewhere.
        assert!(
            html.contains("white-space:nowrap;overflow-wrap:normal"),
            "{html}"
        );
        assert!(html.contains("overflow-wrap:anywhere"), "{html}");
    }

    /// A one-character column is still a column: sized to a readable minimum
    /// rather than to its content.
    #[test]
    fn a_tiny_html_column_keeps_a_readable_minimum() {
        let res = ReportResult {
            column_order: vec!["#".into()],
            rows: vec![row(&[("#", "1")])],
            ..Default::default()
        };
        let widths = html_column_widths(&res.resolved_columns(&Header::default()), &res);
        assert_eq!(widths[0], HTML_MIN_COL_WIDTH);
    }

    #[test]
    fn parse_report_number_accepts_quantities_but_not_identifiers() {
        assert_eq!(parse_report_number(" 123 "), Some(123.0));
        assert_eq!(parse_report_number("-3.5"), Some(-3.5));
        assert_eq!(parse_report_number("0"), Some(0.0));
        assert_eq!(parse_report_number("0.5"), Some(0.5));
        // Redundant leading zeros usually mark an id, not a quantity.
        assert_eq!(parse_report_number("007"), None);
        assert_eq!(parse_report_number("-08"), None);
        // Non-numeric / empty.
        assert_eq!(parse_report_number(""), None);
        assert_eq!(parse_report_number("High Risk"), None);
        assert_eq!(parse_report_number("123 ms"), None);
    }

    #[test]
    fn column_is_numeric_only_when_every_value_is_a_number() {
        let numeric_col = OutputColumn {
            header: "Time".into(),
            sources: vec!["Time".into()],
            stats: Vec::new(),
            image: None,
            truth: None,
        };
        let res = ReportResult {
            no_match_marker: "-".into(),
            column_order: vec!["Time".into()],
            rows: vec![
                row(&[("Time", "12")]),
                row(&[("Time", "34.5")]),
                row(&[]), // missing -> marker, skipped
            ],
            ..Default::default()
        };
        assert!(column_is_numeric(&numeric_col, &res));

        // A single non-numeric value disqualifies the column.
        let mixed = ReportResult {
            column_order: vec!["Time".into()],
            rows: vec![row(&[("Time", "12")]), row(&[("Time", "n/a")])],
            ..Default::default()
        };
        assert!(!column_is_numeric(&numeric_col, &mixed));

        // A column with no values at all is not numeric.
        let empty = ReportResult {
            column_order: vec!["Time".into()],
            rows: vec![row(&[])],
            ..Default::default()
        };
        assert!(!column_is_numeric(&numeric_col, &empty));
    }

    /// Cells are wrapped, so a column only has to be as wide as the longest
    /// *line* in it — otherwise one multi-line JSON body would demand a column
    /// thousands of characters wide.
    #[test]
    fn text_width_measures_the_longest_line_not_the_whole_string() {
        assert_eq!(text_display_width("abc"), 3);
        assert_eq!(text_display_width("abc\nlonger line\nx"), 11);
        assert_eq!(text_display_width(""), 0);
        // Counted in chars, not bytes, so accented text isn't over-measured.
        assert_eq!(text_display_width("héllo"), 5);
    }

    #[test]
    fn measured_widths_are_clamped_to_the_readable_range() {
        assert_eq!(clamp_xlsx_width(0), XLSX_MIN_COL_WIDTH);
        assert_eq!(clamp_xlsx_width(1), XLSX_MIN_COL_WIDTH);
        assert_eq!(clamp_xlsx_width(20), 20.0);
        assert_eq!(clamp_xlsx_width(10_000), XLSX_MAX_COL_WIDTH);
    }

    /// The bug this fixes: with no widths written at all, every column came out
    /// at Excel's 8.43-character default regardless of content.
    #[test]
    fn xlsx_columns_are_sized_to_their_widest_content() {
        let res = ReportResult {
            column_order: vec!["id".into(), "url".into()],
            rows: vec![
                row(&[
                    ("id", "1"),
                    ("url", "https://example.com/a/fairly/long/path"),
                ]),
                row(&[("id", "2"), ("url", "short")]),
            ],
            ..Default::default()
        };
        let columns = res.resolved_columns(&Header::default());
        let widths = xlsx_column_widths(&columns, &res);

        // "id" holds only single characters, so it falls back to the minimum
        // rather than being sized down to nothing.
        assert_eq!(widths[0], XLSX_MIN_COL_WIDTH);
        // "url" is sized to its longest value plus padding.
        assert_eq!(
            widths[1],
            ("https://example.com/a/fairly/long/path".len() + XLSX_CELL_PADDING) as f64
        );
        assert!(widths[1] > widths[0], "the wide column is genuinely wider");
    }

    #[test]
    fn a_very_long_cell_is_capped_so_it_cannot_squeeze_out_every_other_column() {
        let res = ReportResult {
            column_order: vec!["body".into(), "id".into()],
            rows: vec![row(&[("body", &"x".repeat(5_000)), ("id", "1")])],
            ..Default::default()
        };
        let columns = res.resolved_columns(&Header::default());
        let widths = xlsx_column_widths(&columns, &res);
        assert_eq!(widths[0], XLSX_MAX_COL_WIDTH);
        // The cap must not drag the other columns along with it.
        assert_eq!(widths[1], XLSX_MIN_COL_WIDTH);
    }

    #[test]
    fn a_long_header_widens_its_column_even_when_every_value_is_short() {
        let res = ReportResult {
            column_order: vec!["a_rather_long_column_header".into()],
            rows: vec![row(&[("a_rather_long_column_header", "1")])],
            ..Default::default()
        };
        let columns = res.resolved_columns(&Header::default());
        let widths = xlsx_column_widths(&columns, &res);
        assert_eq!(
            widths[0],
            ("a_rather_long_column_header".len() + XLSX_HEADER_PADDING) as f64
        );
    }

    #[test]
    fn statistics_labels_widen_the_column_they_sit_in() {
        // A Distribution row is labelled "<header> = <value>", which is longer
        // than the "Name" header or any of the one-character values above it,
        // and lands in the first column.
        let res = stats_result();
        let header = stats_header("Name, Time STATISTICS(DISTRIBUTION)");
        let columns = res.resolved_columns(&header);
        let widths = xlsx_column_widths(&columns, &res);
        assert_eq!(
            widths[0],
            ("Time = 100".len() + XLSX_HEADER_PADDING) as f64,
            "label column must fit its widest statistics label"
        );
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

    /// A `columns:` directive carrying `STATISTICS(…)` appends summary rows
    /// after the data rows in every text writer. Numeric stats (Mean/Sum) fill
    /// only the requesting column; the stat label sits in the first column when
    /// that column has no value of its own.
    fn stats_header(spec: &str) -> Header {
        Header {
            lines: vec![super::super::flow::HeaderLine::Directive {
                key: "columns".into(),
                value: spec.into(),
            }],
        }
    }

    /// A ground-truthed result: three scored rows (one wrong) and one row
    /// nobody labelled, with a declared vocabulary so a matrix is produced.
    fn truth_result() -> (ReportResult, Header) {
        use crate::report::model::Verdict;
        let mut res = ReportResult {
            column_order: vec!["Correct".into(), "Name".into(), "Verdict".into()],
            rows: vec![
                row(&[
                    ("Name", "a"),
                    ("Verdict", "Low Risk"),
                    ("Correct", "correct"),
                ]),
                row(&[
                    ("Name", "b"),
                    ("Verdict", "High Risk"),
                    ("Correct", "correct"),
                ]),
                row(&[
                    ("Name", "c"),
                    ("Verdict", "Low Risk"),
                    ("Correct", "incorrect"),
                ]),
                row(&[("Name", "d"), ("Verdict", "Low Risk")]),
            ],
            ..Default::default()
        };
        res.column_truths
            .insert("Verdict".into(), "{{ expected }}".into());
        for (r, (v, t)) in [
            (Verdict::Correct, "real"),
            (Verdict::Correct, "fake"),
            (Verdict::Incorrect, "fake"),
        ]
        .into_iter()
        .enumerate()
        {
            res.verdicts.insert((r, "Verdict".into()), v);
            res.truths.insert((r, "Verdict".into()), t.into());
        }
        let header = Header {
            lines: vec![
                super::super::flow::HeaderLine::Directive {
                    key: "labels".into(),
                    value: "Pass = pass, real, low risk".into(),
                },
                super::super::flow::HeaderLine::Directive {
                    key: "labels".into(),
                    value: "Fail = fail, fake, high risk".into(),
                },
            ],
        };
        (res, header)
    }

    /// CSV has one table and no header block, so the metrics ride in the
    /// footer — and a report with no `TRUTH` gains nothing at all.
    #[test]
    fn csv_appends_the_ground_truth_metrics_to_the_footer() {
        let (res, header) = truth_result();
        let out = String::from_utf8(CsvWriter.write(&res, &header).unwrap()).unwrap();
        // Roll-up under `Correct`, per-column figure under `Verdict`, and the
        // row's label in the first column that had nothing of its own.
        assert!(
            out.contains("3 of 4,Compared,3 of 4"),
            "compared row: {out}"
        );
        assert!(out.contains("66.7%,Accuracy,66.7%"), "accuracy row: {out}");
        // Nothing is appended to a report that never declared a truth.
        let plain = String::from_utf8(
            CsvWriter
                .write(&stats_result(), &Header::default())
                .unwrap(),
        )
        .unwrap();
        assert!(!plain.contains("Accuracy"), "{plain}");
    }

    /// HTML states the figures once, above the table, and draws the matrix in
    /// the declared axis order. Nothing it emits reaches out to the network.
    #[test]
    fn html_draws_metric_cards_and_a_confusion_matrix() {
        let (res, header) = truth_result();
        let out = String::from_utf8(HtmlWriter.write(&res, &header).unwrap()).unwrap();
        assert!(out.contains("class=\"metrics\""), "cards: {out}");
        assert!(out.contains("66.7%"), "accuracy card: {out}");
        assert!(out.contains("class=\"matrix\""), "matrix: {out}");
        let pass = out.find("Pass").expect("Pass axis label");
        let fail = out.find("Fail").expect("Fail axis label");
        assert!(pass < fail, "the axis keeps its declared order");
        assert!(
            !out.contains("<tfoot"),
            "the metrics are not also repeated in the footer: {out}"
        );
        assert!(
            !out.contains("http://") && !out.contains("https://"),
            "the export stays self-contained"
        );
    }

    /// JSON carries the metrics structured, so a CI gate doesn't have to parse
    /// "66.7%" back out of a string.
    #[test]
    fn json_exports_the_metrics_as_numbers() {
        let (res, header) = truth_result();
        let out = JsonWriter.write(&res, &header).unwrap();
        let doc: serde_json::Value = serde_json::from_slice(&out).unwrap();
        let col = &doc["metrics"]["columns"][0];
        assert_eq!(col["column"], "Verdict");
        assert_eq!(col["compared"], 3);
        assert_eq!(col["incorrect"], 1);
        assert!((col["accuracy"].as_f64().unwrap() - 2.0 / 3.0).abs() < 1e-9);
        assert_eq!(col["confusion"]["axis"][0], "Pass");
        // Truth down, prediction across: the one wrong row is a Fail read as a Pass.
        assert_eq!(col["confusion"]["counts"][1][0], 1);
        assert_eq!(doc["metrics"]["overall"]["correct"], 2);
    }

    /// A matrix only exists where a vocabulary was declared: without one there
    /// is no meaningful axis order, and a matrix of whatever turned up is noise.
    #[test]
    fn no_labels_directive_means_no_matrix_but_still_metrics() {
        let (res, _) = truth_result();
        let out = String::from_utf8(HtmlWriter.write(&res, &Header::default()).unwrap()).unwrap();
        assert!(out.contains("class=\"metrics\""), "figures still shown");
        assert!(!out.contains("class=\"matrix\""), "but no matrix: {out}");
    }

    fn stats_result() -> ReportResult {
        ReportResult {
            column_order: vec!["Name".into(), "Time".into()],
            rows: vec![
                row(&[("Name", "a"), ("Time", "100")]),
                row(&[("Name", "b"), ("Time", "200")]),
                row(&[("Name", "c"), ("Time", "300")]),
            ],
            ..Default::default()
        }
    }

    #[test]
    fn csv_appends_statistics_summary_rows() {
        let res = stats_result();
        let header = stats_header("Name, Time STATISTICS(SUM, MEAN)");
        let text = String::from_utf8(CsvWriter.write(&res, &header).unwrap()).unwrap();
        // Data first, then a Sum row (600) and a Mean row (200), the label in
        // the first (value-less) column.
        assert!(
            text.contains("Sum,600"),
            "CSV should carry the Sum row: {text}"
        );
        assert!(
            text.contains("Mean,200"),
            "CSV should carry the Mean row: {text}"
        );
    }

    #[test]
    fn csv_distribution_counts_each_value() {
        let res = ReportResult {
            column_order: vec!["Overall".into()],
            rows: vec![
                row(&[("Overall", "Low")]),
                row(&[("Overall", "High")]),
                row(&[("Overall", "Low")]),
            ],
            ..Default::default()
        };
        let header = stats_header("Overall STATISTICS(DISTRIBUTION)");
        let text = String::from_utf8(CsvWriter.write(&res, &header).unwrap()).unwrap();
        // Single column: the count sits in the (only) column, and the distinct
        // values are each counted (Low=2, High=1).
        assert!(text.contains("2"), "Low should be counted twice: {text}");
        assert!(text.contains("1"), "High should be counted once: {text}");
    }

    #[test]
    fn json_includes_a_summary_array() {
        let res = stats_result();
        let header = stats_header("Name, Time STATISTICS(MEAN)");
        let bytes = JsonWriter.write(&res, &header).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let summary = v
            .get("summary")
            .and_then(|s| s.as_array())
            .expect("summary array present");
        assert!(!summary.is_empty(), "summary should hold the Mean row");
        let mean = &summary[0];
        assert_eq!(mean.get("Time").and_then(|t| t.as_str()), Some("200"));
        assert_eq!(mean.get("Name").and_then(|t| t.as_str()), Some("Mean"));
    }

    #[test]
    fn html_includes_a_tfoot_summary() {
        let res = stats_result();
        let header = stats_header("Name, Time STATISTICS(MEAN)");
        let bytes = HtmlWriter.write(&res, &header).unwrap();
        let html = String::from_utf8(bytes).unwrap();
        assert!(
            html.contains("<tfoot>"),
            "HTML should carry a tfoot: {html}"
        );
        assert!(html.contains("Mean"), "tfoot should show the Mean label");
        assert!(html.contains("200"), "tfoot should show the computed mean");
    }

    #[test]
    fn xlsx_with_statistics_is_a_valid_zip() {
        let res = stats_result();
        let header = stats_header("Name, Time STATISTICS(MEAN, SUM)");
        let bytes = XlsxWriter.write(&res, &header).unwrap();
        assert!(!bytes.is_empty(), "xlsx with stats produced bytes");
        assert_eq!(&bytes[..2], b"PK", "still a valid ZIP container");
    }

    /// A result whose `Frame` column is an IMAGE column holding one resolved
    /// 1x1 PNG in row 0.
    fn image_result() -> (ReportResult, Header) {
        use crate::report::flow::ImageSpec;
        let png = crate::report::image::tests::png_1x1();
        let mut res = ReportResult {
            column_order: vec!["Name".into(), "Frame".into()],
            rows: vec![row(&[("Name", "a"), ("Frame", "shots/a.png")])],
            ..Default::default()
        };
        res.column_images.insert(
            "Frame".to_string(),
            ImageSpec {
                height: Some(60),
                ..Default::default()
            },
        );
        res.images.insert(
            (0, "Frame".to_string()),
            crate::report::model::ImageData {
                bytes: png,
                mime: "image/png".to_string(),
                natural: (1, 1),
            },
        );
        (res, Header::default())
    }

    /// The picture reaches the workbook as a real media part, and the cell's
    /// source text is not also written beside it.
    #[test]
    fn xlsx_embeds_a_resolved_picture_as_a_media_part() {
        let (res, header) = image_result();
        let bytes = XlsxWriter.write(&res, &header).unwrap();
        assert_eq!(&bytes[..2], b"PK");
        // A ZIP stores each member's name uncompressed in its local header, so
        // the media part is findable without unzipping.
        let hay = String::from_utf8_lossy(&bytes);
        assert!(
            hay.contains("xl/media/image"),
            "the workbook should carry an embedded picture"
        );
        assert!(
            hay.contains("xl/drawings/drawing1.xml"),
            "and the drawing that anchors it"
        );
    }

    /// HTML inlines the picture as a `data:` URI so the export is a single
    /// self-contained file, keeping the source value as alt/title text.
    #[test]
    fn html_inlines_a_resolved_picture_as_a_data_uri() {
        let (res, header) = image_result();
        let html = String::from_utf8(HtmlWriter.write(&res, &header).unwrap()).unwrap();
        assert!(
            html.contains("<img style=\"width:60px;height:60px\""),
            "sized from the IMAGE clause and the 1x1 aspect ratio: {html}"
        );
        assert!(
            html.contains("data:image/png;base64,"),
            "inlined rather than linked: {html}"
        );
        assert!(
            html.contains("alt=\"shots/a.png\""),
            "the source value survives as alt text: {html}"
        );
    }

    /// `IMAGE` is a render hint, so formats that cannot show a picture keep
    /// writing the value exactly as before — CSV and JSON stay lossless.
    #[test]
    fn text_formats_ignore_the_image_clause_entirely() {
        let (res, header) = image_result();
        let text = String::from_utf8(CsvWriter.write(&res, &header).unwrap()).unwrap();
        assert_eq!(text, "Name,Frame\r\na,shots/a.png\r\n");
        let bytes = JsonWriter.write(&res, &header).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            v["rows"][0]["Frame"].as_str(),
            Some("shots/a.png"),
            "JSON carries the value, not the picture"
        );
    }

    /// A row whose value never resolved (a bad path, an offline fetch) has no
    /// entry in `images`, and must fall back to its text rather than a blank.
    #[test]
    fn an_unresolved_image_cell_falls_back_to_its_text() {
        let (mut res, header) = image_result();
        res.rows
            .push(row(&[("Name", "b"), ("Frame", "missing.png")]));
        let html = String::from_utf8(HtmlWriter.write(&res, &header).unwrap()).unwrap();
        assert!(
            html.contains("missing.png</td>") || html.contains(">missing.png<"),
            "the unresolved row keeps its text: {html}"
        );
        // And the workbook still writes, with one picture rather than two.
        let bytes = XlsxWriter.write(&res, &header).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("xl/media/image2"));
    }

    /// A ground-truthed cell is coloured by whether it is right, overriding the
    /// word-sentiment heuristic: an engine that correctly answers `fail` is a
    /// green cell, and one that wrongly answers `pass` is a red one.
    #[test]
    fn a_verdict_tints_a_cell_by_correctness_not_by_its_wording() {
        let mut res = ReportResult::default();
        res.rows = vec![
            row(&[("Verdict", "fail"), ("Correct", "correct")]),
            row(&[("Verdict", "pass"), ("Correct", "incorrect")]),
            row(&[("Verdict", "pass"), ("Correct", "untested")]),
        ];
        res.column_order = vec!["Correct".to_string(), "Verdict".to_string()];
        res.verdicts.insert((0, "Verdict".into()), Verdict::Correct);
        res.verdicts
            .insert((1, "Verdict".into()), Verdict::Incorrect);
        res.verdicts
            .insert((2, "Verdict".into()), Verdict::Untested);

        assert!(matches!(
            run_cell_tint(&res, 0, "Verdict", "fail"),
            Some(Tint::Green)
        ));
        assert!(matches!(
            run_cell_tint(&res, 1, "Verdict", "pass"),
            Some(Tint::Red)
        ));
        assert!(
            run_cell_tint(&res, 2, "Verdict", "pass").is_none(),
            "an untested row never borrows the look of a passing one"
        );
        // The roll-up column tints from its own text.
        assert!(matches!(
            run_cell_tint(&res, 0, "Correct", "correct"),
            Some(Tint::Green)
        ));
        assert!(matches!(
            run_cell_tint(&res, 1, "Correct", "incorrect"),
            Some(Tint::Red)
        ));
        // A report with no ground truth is tinted exactly as before.
        let plain = ReportResult::default();
        assert!(matches!(
            run_cell_tint(&plain, 0, "Verdict", "fail"),
            Some(Tint::Red)
        ));
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
