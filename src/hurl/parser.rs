//! Parse Hurl text into the app's [`HurlEntry`] model using `hurl_core`'s
//! parser, so we don't maintain a hand-written Hurl parser. `HurlEntry` stays
//! the editable/persistable model; this maps the parsed AST onto it. Fields that
//! must preserve their exact source text (URL, headers, body, captures, asserts)
//! are taken via `ToSource`/`Display` or by slicing the original source lines.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hurl_core::ast::{
    Body, Bytes, Capture, Entry, KeyValue, MultipartParam, SectionValue, StatusValue, VersionValue,
};
use hurl_core::parser::parse_hurl_file;
use hurl_core::types::ToSource;

use super::entry::{
    BASE64_FILE_CT_MARKER, CommentAnchor, EntryComment, FormField, FormFieldKind, HurlEntry, KvRow,
    RunStatus,
};

/// Parse a Hurl-format string into a list of [`HurlEntry`] values. Invalid input
/// yields an empty list (the UI treats "no entries" as a failed load).
pub fn parse_hurl(content: &str) -> Vec<HurlEntry> {
    let Ok(file) = parse_hurl_file(content) else {
        return Vec::new();
    };
    let lines: Vec<&str> = content.lines().collect();
    // Each entry's `# [Reports]` comment block is recovered by scanning the raw
    // source within that entry's line window; the window ends at the next
    // entry's method line (or end-of-file for the last entry). We deliberately
    // use the next entry's *method* line, not its `source_info.start.line`:
    // `hurl_core` attaches an inter-entry comment block (which includes our
    // `# [Reports]` block) to the *following* entry's start, so a start-based
    // window would cut the block off from the entry it belongs to.
    let method_lines: Vec<usize> = file
        .entries
        .iter()
        .map(|e| first_method_line(&lines, e.request.source_info.start.line))
        .collect();
    file.entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let end = method_lines.get(i + 1).copied().unwrap_or(lines.len() + 1);
            map_entry(e, &lines, method_lines[i], end, i == 0)
        })
        .collect()
}

/// The 1-based line number of an entry's HTTP-method line: the first
/// non-comment, non-blank line at or after `start_line` (which may itself be a
/// leading comment). Used to bound each entry's raw-source scan window.
fn first_method_line(lines: &[&str], start_line: usize) -> usize {
    (start_line.saturating_sub(1)..lines.len())
        .find(|&i| {
            let l = lines[i].trim();
            !l.is_empty() && !l.starts_with('#')
        })
        .map(|i| i + 1)
        .unwrap_or(lines.len() + 1)
}

/// Explain, in one short line, *why* `content` isn't valid Hurl — for the Raw
/// editor's "couldn't save" status, which is otherwise the unhelpfully generic
/// "expected exactly one request". Returns `None` when the text parses cleanly
/// (so any "wrong number of requests" problem is left to the caller).
///
/// The most common trip-up is putting `[Captures]`/`[Asserts]` on a request
/// with no `HTTP` response line: those are *response* sections, so `hurl_core`
/// rejects them as unknown *request* sections. We surface that case with a
/// concrete fix (`HTTP *` matches any status) rather than the raw parser jargon.
pub fn parse_hurl_error(content: &str) -> Option<String> {
    use hurl_core::error::DisplaySourceError;
    use hurl_core::parser::ParseErrorKind;
    let err = parse_hurl_file(content).err()?;
    let line = err.pos.line;
    let reason = match &err.kind {
        ParseErrorKind::RequestSectionName { name }
            if matches!(name.as_str(), "Captures" | "Asserts") =>
        {
            format!(
                "line {line}: [{name}] is a response section — add an 'HTTP' status line \
                 above it (use 'HTTP *' to accept any status)"
            )
        }
        ParseErrorKind::RequestSectionName { name } => {
            format!("line {line}: [{name}] is not a valid request section")
        }
        ParseErrorKind::Method { .. } => {
            format!("line {line}: expected an HTTP method (e.g. GET, POST)")
        }
        ParseErrorKind::Version => {
            format!("line {line}: a response line must be 'HTTP <status>' (e.g. 'HTTP 200')")
        }
        ParseErrorKind::Status => format!("line {line}: invalid status code"),
        ParseErrorKind::UrlInvalidStart | ParseErrorKind::UrlIllegalCharacter(_) => {
            format!("line {line}: invalid URL")
        }
        _ => format!("line {line}: {}", err.description().to_lowercase()),
    };
    Some(reason)
}

fn map_entry(
    e: &Entry,
    lines: &[&str],
    scan_start: usize,
    scan_end: usize,
    is_first: bool,
) -> HurlEntry {
    let req = &e.request;

    // Structural anchors that terminate the inline header block and bound each
    // request `[Section]`'s rows: the request body, any `[Section]` header, and
    // the response's `HTTP` status line. Each is strictly inside this entry and
    // *after* the headers, so a source scan bounded by the first anchor below a
    // block can never spill into the body, a later section, the response, or —
    // crucially — the following request. (We can't use `req.source_info.end`:
    // `hurl_core` excludes trailing comment lines from it, which would drop a
    // block's own trailing/all-disabled `# key: value` rows.)
    let mut anchors: Vec<usize> = req
        .sections
        .iter()
        .map(|s| s.source_info.start.line)
        .collect();
    if let Some(b) = &req.body {
        anchors.push(body_start_line(b));
    }
    if let Some(resp) = &e.response {
        anchors.push(resp.status.source_info.start.line);
    }

    // The same anchors, tagged with the block they open, drive prose-comment
    // recovery: a comment is attributed to the first block that begins below it
    // (see `scan_comments`).
    let mut landmarks: Vec<(usize, CommentAnchor)> = Vec::new();
    for section in &req.sections {
        let anchor = match &section.value {
            SectionValue::BasicAuth(_) => CommentAnchor::BasicAuth,
            SectionValue::Cookies(_) => CommentAnchor::Cookies,
            SectionValue::QueryParams(..) => CommentAnchor::Query,
            SectionValue::FormParams(..) | SectionValue::MultipartFormData(..) => {
                CommentAnchor::Form
            }
            SectionValue::Options(_) => CommentAnchor::Options,
            _ => continue,
        };
        landmarks.push((section.source_info.start.line, anchor));
    }
    if let Some(b) = &req.body {
        landmarks.push((body_start_line(b), CommentAnchor::Body));
    }
    if let Some(resp) = &e.response {
        let status_line = resp.status.source_info.start.line;
        landmarks.push((status_line, CommentAnchor::Response));
        // Response headers sit between the `HTTP <status>` line and the first
        // response block; anchor comments among them to `ResponseHeaders` when
        // there are any (enabled rows, or disabled ones recovered from source).
        if !resp.headers.is_empty()
            || scan_kv_rows(lines, status_line + 1, first_response_anchor(resp))
                .iter()
                .any(|r| !r.enabled)
        {
            landmarks.push((status_line + 1, CommentAnchor::ResponseHeaders));
        }
        for section in &resp.sections {
            let anchor = match &section.value {
                SectionValue::Asserts(_) => CommentAnchor::Asserts,
                SectionValue::Captures(_) => CommentAnchor::Captures,
                _ => continue,
            };
            landmarks.push((section.source_info.start.line, anchor));
        }
        if let Some(b) = &resp.body {
            landmarks.push((body_start_line(b), CommentAnchor::ResponseBody));
        }
    }
    landmarks.sort_by_key(|(line, _)| *line);

    // Each body's source line span keeps its multiline `#` lines out of
    // prose-comment recovery (both the request body and the expected response
    // body).
    let mut body_ranges: Vec<(usize, usize)> = Vec::new();
    if let Some(b) = &req.body {
        body_ranges.push(body_line_span(b));
    }
    if let Some(b) = e.response.as_ref().and_then(|r| r.body.as_ref()) {
        body_ranges.push(body_line_span(b));
    }

    let mut basic_auth = None;
    let mut form_fields = Vec::new();
    let mut query_params = Vec::new();
    let mut cookies = Vec::new();
    let mut options = Vec::new();
    for section in &req.sections {
        // Rows start on the line after the `[Section]` header and run up to the
        // next structural anchor below it (the following section / body /
        // response), or — for a trailing section with nothing after it — to the
        // end of the contiguous rows (see `scan_kv_rows`).
        let rows_start = section.source_info.start.line + 1;
        let rows_end = first_anchor_after(&anchors, section.source_info.start.line);
        match &section.value {
            SectionValue::BasicAuth(Some(kv)) => basic_auth = Some(kv_pair(kv)),
            SectionValue::FormParams(kvs, _) => {
                form_fields = form_fields_from_section(kvs, None, lines, rows_start, rows_end);
            }
            SectionValue::MultipartFormData(parts, _) => {
                form_fields =
                    form_fields_from_section(&[], Some(parts), lines, rows_start, rows_end);
            }
            // Headers live inline; Cookies/Query are `[Section]`s. All three are
            // scanned straight from source so a disabled row (kept as a
            // `# key: value` comment, invisible to `hurl_core`) round-trips.
            SectionValue::QueryParams(..) => {
                query_params = scan_kv_rows(lines, rows_start, rows_end)
            }
            SectionValue::Cookies(_) => cookies = scan_kv_rows(lines, rows_start, rows_end),
            // `[Options]` rows are `name: value` too (retry, insecure, …), so
            // the same scan recovers them — including disabled ones — verbatim.
            SectionValue::Options(_) => options = scan_kv_rows(lines, rows_start, rows_end),
            _ => {}
        }
    }

    let mut expected_status = None;
    let mut captures = Vec::new();
    let mut asserts = Vec::new();
    let mut response_version = None;
    let mut response_headers = Vec::new();
    let mut response_body = None;
    if let Some(resp) = &e.response {
        if let StatusValue::Specific(n) = resp.status.value {
            expected_status = Some(n as u16);
        }
        // The version-agnostic `HTTP` keyword (`VersionAny`) carries no explicit
        // version; anything else (`HTTP/1.1`, `HTTP/2`, …) is preserved verbatim.
        response_version = match resp.version.value {
            VersionValue::VersionAny => None,
            v => Some(v.to_string()),
        };
        // Response headers occupy the lines between the `HTTP <status>` line and
        // the first response block; scanning them (like request headers)
        // recovers disabled `# key: value` rows the AST drops as comments.
        response_headers = scan_kv_rows(
            lines,
            resp.status.source_info.start.line + 1,
            first_response_anchor(resp),
        );
        response_body = resp.body.as_ref().and_then(|b| body_source(b, lines));
        for section in &resp.sections {
            match &section.value {
                SectionValue::Captures(caps) => {
                    captures = caps.iter().filter_map(|c| capture_pair(c, lines)).collect();
                }
                SectionValue::Asserts(asrts) => {
                    asserts = asrts
                        .iter()
                        .filter_map(|a| source_line(a.query.source_info.start.line, lines))
                        .collect();
                }
                _ => {}
            }
        }
    }

    let is_multipart = form_fields
        .iter()
        .any(|f| f.enabled && f.kind.is_multipart());

    HurlEntry {
        title: title_from_span(req.source_info.start.line, lines),
        method: req.method.to_string(),
        url: req.url.to_source().to_string(),
        // Headers occupy the lines between the request line and the first
        // structural anchor (body / section / response). Scanning them (instead
        // of reading the AST) recovers disabled rows kept as `# key: value`
        // comments; the anchor bound keeps the scan inside this request.
        headers: scan_kv_rows(
            lines,
            req.url.source_info.start.line + 1,
            first_anchor_after(&anchors, req.url.source_info.start.line),
        ),
        basic_auth,
        form_fields,
        is_multipart,
        queries: query_params,
        cookies,
        options,
        body_src: req.body.as_ref().and_then(|b| body_source(b, lines)),
        expected_status,
        response_version,
        response_headers,
        response_body,
        captures,
        asserts,
        reports: reports_from_span(lines, scan_start, scan_end),
        comments: scan_comments(
            lines,
            &landmarks,
            &body_ranges,
            scan_start,
            scan_end,
            is_first,
        ),
        user_added: false,
        modified: false,
        last_run: RunStatus::default(),
        last_response: None,
    }
}

/// The first structural anchor inside a response strictly below its `HTTP`
/// status line — the earliest response `[Section]` header or the response
/// body's start — bounding the response-header scan. `None` (open mode) when a
/// response has no sections or body, so the header scan stops at the blank line
/// separating this entry from the next (see [`scan_kv_rows`]).
fn first_response_anchor(resp: &hurl_core::ast::Response) -> Option<usize> {
    resp.sections
        .iter()
        .map(|s| s.source_info.start.line)
        .chain(resp.body.as_ref().map(body_start_line))
        .min()
}

fn kv_pair(kv: &KeyValue) -> (String, String) {
    (
        kv.key.to_source().to_string(),
        kv.value.to_source().to_string(),
    )
}

/// The smallest structural anchor line strictly below `after` — i.e. the
/// exclusive upper bound of the source block that begins just under `after`
/// (the request line, for headers, or a `[Section]` header, for its rows).
/// `None` means "no anchor below here": a request with no body, sections or
/// response, or its final trailing section — in which case the block is scanned
/// in the bounded-open mode described on [`scan_kv_rows`].
fn first_anchor_after(anchors: &[usize], after: usize) -> Option<usize> {
    anchors.iter().copied().filter(|&a| a > after).min()
}

/// The 1-based line a request body starts on (its value, past any leading blank
/// lines) — the header block's lower boundary when a body is present.
fn body_start_line(b: &Body) -> usize {
    b.space0.source_info.start.line
}

/// The half-open 1-based line range `[start, end)` a request body occupies in
/// source. Used to keep body content out of prose-comment recovery: a
/// multiline-string body can contain lines that begin with `#`, which are body
/// text — not comments — and must not be captured (or duplicated) as such.
fn body_line_span(b: &Body) -> (usize, usize) {
    let start = body_start_line(b);
    // `line_terminator0` is the terminator right after the body value, so its
    // newline sits on the body's last source line.
    let end = b.line_terminator0.newline.source_info.start.line.max(start);
    (start, end + 1)
}

/// Recover prose comments from an entry's raw source so they aren't silently
/// dropped on load. `hurl_core` melts every comment into an opaque `Comment`
/// node, so — as with disabled rows and the `# [Reports]` block — we scan the
/// source lines ourselves. A comment line is captured unless it's already
/// represented elsewhere: this entry's own title block, the next entry's title
/// block, a disabled `# key: value` row inside a scanned rows region, the
/// `# [Reports]` block, or body content. Each captured comment is anchored to
/// the block it precedes (the first structural line below it — see
/// [`CommentAnchor`]) so it re-emits near its original place even as
/// surrounding lines change. `Lead` collects file-leading comments above the
/// first entry; `Trailing` collects comments below the last block.
fn scan_comments(
    lines: &[&str],
    landmarks: &[(usize, CommentAnchor)],
    body_ranges: &[(usize, usize)],
    method_line: usize,
    scan_end: usize,
    is_first: bool,
) -> Vec<EntryComment> {
    #[derive(Clone, Copy)]
    enum RowKind {
        Kv,
        Form,
    }
    // Rows regions where a `# key: value` comment is a captured *disabled* row
    // (recovered by `scan_kv_rows`/`scan_disabled_form_rows`), not prose. These
    // mirror the row scans in `map_entry`: the inline header block, then each
    // Cookies/Query/Form/Options section's rows and the response-header block,
    // each bounded by the next landmark.
    let first_landmark = landmarks.first().map(|(l, _)| *l);
    let mut regions: Vec<(usize, usize, RowKind)> = vec![(
        method_line + 1,
        first_landmark.unwrap_or(scan_end),
        RowKind::Kv,
    )];
    for (k, (line, anchor)) in landmarks.iter().enumerate() {
        // Section landmarks point at the `[Section]` header line, so their rows
        // start one line below; the response-header landmark already points at
        // the first header row (there's no `[Header]` line above it).
        let (kind, rows_start) = match anchor {
            CommentAnchor::Cookies | CommentAnchor::Query | CommentAnchor::Options => {
                (RowKind::Kv, line + 1)
            }
            CommentAnchor::Form => (RowKind::Form, line + 1),
            CommentAnchor::ResponseHeaders => (RowKind::Kv, *line),
            _ => continue,
        };
        let end = landmarks.get(k + 1).map(|(l, _)| *l).unwrap_or(scan_end);
        regions.push((rows_start, end, kind));
    }

    let in_body = |line_no: usize| {
        body_ranges
            .iter()
            .any(|&(s, e)| line_no >= s && line_no < e)
    };

    // A comment line that the row scans already recover — either as a disabled
    // row (round-tripping via `headers`/`queries`/etc.) or as a row's `# @desc`
    // description — and so must not *also* be captured as prose, which would
    // duplicate it on the next save. Only meaningful for `#`-comment lines.
    let is_disabled_row = |line_no: usize| {
        let Some(&line) = lines.get(line_no.wrapping_sub(1)) else {
            return false;
        };
        regions.iter().any(|&(s, e, kind)| {
            line_no >= s
                && line_no < e
                && (desc_line(line).is_some()
                    || match kind {
                        RowKind::Kv => parse_kv_row(line).is_some(),
                        RowKind::Form => parse_form_field_line(uncomment(line).1, true).is_some(),
                    })
        })
    };

    // A "structural" line marks a block position for anchoring: body content, an
    // enabled row/section/response line, or a disabled row (which sits in its
    // block's rows). Prose comments, the reports block and title comments are
    // *not* structural — they float and take their anchor from the next
    // structural line below them.
    let is_structural = |line_no: usize| {
        if in_body(line_no) {
            return true;
        }
        let Some(&line) = lines.get(line_no.wrapping_sub(1)) else {
            return false;
        };
        let t = line.trim();
        if t.is_empty() {
            return false;
        }
        if !t.starts_with('#') {
            return true;
        }
        is_disabled_row(line_no)
    };

    // The anchor for a structural line at `l2`: the block it belongs to — the
    // greatest landmark at/above it, or `Headers` when it's in the inline header
    // block (above the first landmark).
    let anchor_of_line = |l2: usize| match first_landmark {
        Some(fl) if l2 >= fl => landmarks
            .iter()
            .rev()
            .find(|(l, _)| *l <= l2)
            .map_or(CommentAnchor::Headers, |(_, a)| *a),
        _ => CommentAnchor::Headers,
    };

    // A prose comment is anchored to the first structural line below it (so it
    // precedes that block), or `Trailing` when nothing structural follows.
    let anchor_for_comment = |line_no: usize| {
        (line_no + 1..scan_end)
            .find(|&l2| is_structural(l2))
            .map_or(CommentAnchor::Trailing, anchor_of_line)
    };

    // Lines already claimed elsewhere: the `# [Reports]` block …
    let reports_block = {
        let to = scan_end.min(lines.len() + 1);
        let marker =
            (method_line..to).find(|&i| lines.get(i - 1).is_some_and(|l| is_reports_marker(l)));
        marker.map_or(0..0, |m| {
            let mut j = m + 1;
            while j < to && lines.get(j - 1).and_then(|l| parse_report_row(l)).is_some() {
                j += 1;
            }
            m..j
        })
    };
    // … and the next entry's title block (the contiguous comment lines directly
    // above the next entry's method line, which `title_from_span` will claim as
    // that entry's title). Only when there *is* a next entry in the window.
    let next_title = if scan_end <= lines.len() {
        let mut top = scan_end;
        let mut idx = scan_end - 1;
        while idx >= method_line
            && lines
                .get(idx - 1)
                .is_some_and(|l| l.trim().starts_with('#'))
        {
            top = idx;
            idx -= 1;
        }
        top..scan_end
    } else {
        0..0
    };

    let mut out = Vec::new();

    // File-leading comments above the very first entry (everything above this
    // entry's own title block), kept as `Lead`.
    if is_first {
        let mut title_top = method_line;
        let mut idx = method_line.wrapping_sub(1);
        while idx >= 1
            && lines
                .get(idx - 1)
                .is_some_and(|l| l.trim().starts_with('#'))
        {
            title_top = idx;
            idx -= 1;
        }
        for ln in 1..title_top {
            if let Some(t) = lines
                .get(ln - 1)
                .map(|l| l.trim())
                .filter(|t| t.starts_with('#'))
            {
                out.push(EntryComment {
                    anchor: CommentAnchor::Lead,
                    text: t.to_string(),
                });
            }
        }
    }

    for line_no in method_line..scan_end.min(lines.len() + 1) {
        let Some(t) = lines.get(line_no - 1).map(|l| l.trim()) else {
            break;
        };
        if !t.starts_with('#')
            || in_body(line_no)
            || is_disabled_row(line_no)
            || reports_block.contains(&line_no)
            || next_title.contains(&line_no)
        {
            continue;
        }
        out.push(EntryComment {
            anchor: anchor_for_comment(line_no),
            text: t.to_string(),
        });
    }
    out
}

/// Scan a block of `key: value` request-section rows starting at 1-based line
/// `start`, returning each as a `(key, value, enabled)` triple. A row commented
/// out with a leading `#` comes back as a disabled entry — this is how
/// [`to_hurl`](super::entry::HurlEntry::to_hurl) round-trips disabled Header,
/// Cookies and Query rows, which `hurl_core` drops as comments before they ever
/// reach the AST.
///
/// A `# @desc …` line immediately above a row becomes that row's
/// [`KvRow::desc`] rather than a row of its own; consecutive marker lines are
/// the successive lines of one note.
///
/// `end` is the block's exclusive upper bound — the next structural anchor
/// below it (body / section / response), from [`first_anchor_after`]. It drives
/// two scan modes that together match `hurl_core` without ever reading rows
/// from the *next* request:
///
/// * **Bounded** (`Some(end)`): scan the half-open window `[start, end)`,
///   collecting every `key: value` row and *skipping* blank lines and prose
///   comments in between (including any leading ones, right after the request
///   or section header). This mirrors `hurl_core`, which tolerates blank and
///   comment lines interspersed among headers/section rows. Because `end` is an
///   anchor strictly inside this entry, the window can't reach the next one.
///
/// * **Open** (`None`): a request with no body, section or response (or its
///   last trailing section) has no anchor below it, so there's nothing bounding
///   the window from the following entry. Here the scan stops at the *first*
///   non-row line — including a leading blank line — so it halts at the blank
///   that separates this entry from the next rather than skipping across it and
///   absorbing that entry's leading comments/title as stray rows.
fn scan_kv_rows(lines: &[&str], start: usize, end: Option<usize>) -> Vec<KvRow> {
    let mut rows: Vec<KvRow> = Vec::new();
    // Description lines accumulated since the last row: a `# @desc …` block
    // belongs to the row *below* it, and several of them are the successive
    // lines of one multi-line note.
    let mut pending_desc: Vec<String> = Vec::new();
    let mut i = start.saturating_sub(1);
    let limit = end.map(|e| e.saturating_sub(1)).unwrap_or(lines.len());
    while i < limit {
        let Some(&line) = lines.get(i) else { break };
        if let Some(text) = desc_line(line) {
            pending_desc.push(text.to_string());
            i += 1;
            continue;
        }
        match parse_kv_row(line) {
            Some(mut row) => {
                row.desc = std::mem::take(&mut pending_desc).join("\n");
                rows.push(row);
            }
            // Bounded: skip a blank/prose line (leading or interior) and keep
            // scanning — the anchor keeps us inside this entry. Open: stop, so
            // we never cross into the next request's leading comments.
            None if end.is_some() => {
                // A note followed by prose rather than a row describes nothing;
                // drop it so it can't leap onto an unrelated later row.
                pending_desc.clear();
            }
            None => break,
        }
        i += 1;
    }
    rows
}

/// The text of a `# @desc …` description line, or `None` for anything else.
/// Whitespace before the `#` is tolerated so an indented note still reads.
pub(crate) fn desc_line(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let rest = trimmed.strip_prefix(crate::hurl::entry::DESC_MARKER.trim_end())?;
    // Require the marker to be followed by a separator (or nothing), so a
    // comment like `# @description of the API` isn't mistaken for one.
    match rest.strip_prefix(' ') {
        Some(text) => Some(text.trim_end()),
        None if rest.is_empty() => Some(""),
        None => None,
    }
}

/// Parse a single Header/Cookies/Query row into a [`KvRow`] (without its
/// description, which the caller attaches from the `# @desc` lines above it).
/// `enabled` is `false` when the line is commented out (`# key: value`). The
/// key must start with an alphanumeric and contain only token characters, so a
/// JSON body line, a `[Section]` header, an `HTTP` status line or a prose
/// comment all fail to parse (ending a scan) instead of being mistaken for a
/// row.
fn parse_kv_row(line: &str) -> Option<KvRow> {
    let (enabled, rest) = uncomment(line);
    let (key, value) = split_kv(rest)?;
    Some(KvRow::toggled(key, value, enabled))
}

/// Strip a leading `#` (marking a disabled/commented request row) and the
/// surrounding whitespace, returning `(enabled, remaining_text)`.
fn uncomment(line: &str) -> (bool, &str) {
    let trimmed = line.trim();
    match trimmed.strip_prefix('#') {
        Some(rest) => (false, rest.trim_start()),
        None => (true, trimmed),
    }
}

/// Split a `key: value` line into its trimmed key and value, requiring the key
/// to be a name Hurl can carry. The test is
/// [`key_problem`](crate::hurl::key_problem) — the *same* one the writer
/// enforces, so a row that can be written can always be read back. Returns
/// `None` for anything else, which is also what keeps an ordinary prose comment
/// (`# see also: the docs`) from being mistaken for a disabled row.
fn split_kv(text: &str) -> Option<(&str, &str)> {
    let colon = text.find(':')?;
    let key = text[..colon].trim();
    if crate::hurl::key_problem(key).is_some() {
        return None;
    }
    Some((key, text[colon + 1..].trim()))
}

/// Build the ordered `form_fields` for a `[Form]` (pass `kvs`) or `[Multipart]`
/// (pass `parts`) section. Enabled rows are taken from the parsed AST, which
/// decodes filename escapes and the Base64File marker robustly; disabled rows
/// (kept as `# …` comments, invisible to `hurl_core`) are recovered by
/// scanning the section's source lines. The two are merged by line number so
/// the user's original row order is preserved.
fn form_fields_from_section(
    kvs: &[KeyValue],
    parts: Option<&[MultipartParam]>,
    lines: &[&str],
    rows_start: usize,
    rows_end: Option<usize>,
) -> Vec<FormField> {
    let mut rows: Vec<(usize, FormField)> = Vec::new();
    if let Some(parts) = parts {
        for p in parts {
            rows.push((multipart_param_line(p), multipart_field(p)));
        }
    } else {
        for kv in kvs {
            rows.push((
                kv.key.source_info.start.line,
                FormField {
                    key: kv.key.to_source().to_string(),
                    value: kv.value.to_source().to_string(),
                    kind: FormFieldKind::Text,
                    content_type: None,
                    base64_prefix: None,
                    enabled: true,
                    desc: String::new(),
                },
            ));
        }
    }
    rows.extend(scan_disabled_form_rows(lines, rows_start, rows_end));
    rows.sort_by_key(|(line, _)| *line);
    // Descriptions are matched to rows by line: unlike the kv sections, form
    // rows come from two sources (the AST for enabled rows, a source scan for
    // disabled ones), so a single scan of the `# @desc` lines and the line each
    // one sits above is what joins them back up.
    let descs = scan_row_descriptions(lines, rows_start, rows_end);
    rows.into_iter()
        .map(|(line, mut f)| {
            if let Some(desc) = descs.get(&line) {
                f.desc = desc.clone();
            }
            f
        })
        .collect()
}

/// Map each row line in `[start, end)` to the description written on the
/// `# @desc …` line(s) directly above it. Lines with no note aren't in the map.
fn scan_row_descriptions(
    lines: &[&str],
    start: usize,
    end: Option<usize>,
) -> std::collections::HashMap<usize, String> {
    let mut out = std::collections::HashMap::new();
    let mut pending: Vec<String> = Vec::new();
    let mut i = start.saturating_sub(1);
    let limit = end.map(|e| e.saturating_sub(1)).unwrap_or(lines.len());
    while i < limit {
        let Some(&line) = lines.get(i) else { break };
        match desc_line(line) {
            Some(text) => pending.push(text.to_string()),
            None if !pending.is_empty() => {
                out.insert(i + 1, std::mem::take(&mut pending).join("\n"));
            }
            None => {}
        }
        i += 1;
    }
    out
}

/// The 1-based source line a `[Multipart]` row starts on.
fn multipart_param_line(p: &MultipartParam) -> usize {
    match p {
        MultipartParam::Param(kv) => kv.key.source_info.start.line,
        MultipartParam::FilenameParam(fp) => fp.key.source_info.start.line,
    }
}

/// Walk a `[Form]`/`[Multipart]` section from `start`, collecting the
/// `(line, field)` for each **disabled** (`# …`) row. Enabled rows are stepped
/// over (they come from the AST). `end` is the section's exclusive upper bound
/// (the next structural anchor, from [`first_anchor_after`]) and drives the same
/// two scan modes as [`scan_kv_rows`]: **bounded** (`Some`) skips interior
/// blanks and prose within `[start, end)`; **open** (`None`, a trailing section
/// with nothing below it) skips only leading blanks and then stops at the first
/// non-row line, so the walk never runs into the following request.
///
/// Disabled rows are parsed as file-capable regardless of the section type:
/// a disabled `File`/`Base64File` field is always serialized with the
/// `file,…` syntax (even inside an otherwise text-only `[Form]`, since the
/// section type is chosen from the enabled fields alone), so parsing it back
/// as a file is what restores its original kind.
fn scan_disabled_form_rows(
    lines: &[&str],
    start: usize,
    end: Option<usize>,
) -> Vec<(usize, FormField)> {
    let mut out = Vec::new();
    let mut i = start.saturating_sub(1);
    let limit = end.map(|e| e.saturating_sub(1)).unwrap_or(lines.len());
    while i < limit {
        let Some(&line) = lines.get(i) else { break };
        // A description line is an annotation on the row below, not a row (and
        // not prose that should stop an open-mode scan).
        if desc_line(line).is_some() {
            i += 1;
            continue;
        }
        let (enabled, rest) = uncomment(line);
        match parse_form_field_line(rest, true) {
            Some(mut field) if !enabled => {
                field.enabled = false;
                out.push((i + 1, field));
            }
            // An enabled row (already captured from the AST): step over it.
            Some(_) => {}
            // Bounded: skip a blank/prose line (leading or interior) and keep
            // scanning. Open: stop, so the walk can't reach the next request.
            None if end.is_some() => {}
            None => break,
        }
        i += 1;
    }
    out
}

/// Parse one `[Form]`/`[Multipart]` field line body (the text after any
/// disabled-row `# `). In a `[Multipart]` section a `file,PATH;CT` value is a
/// file upload (a PaperBoy marker restores a Base64File); every other value,
/// and every `[Form]` value, is plain text. Returns `None` when the line isn't
/// a field row (so a scan can tell where the section ends).
fn parse_form_field_line(body: &str, multipart: bool) -> Option<FormField> {
    let (key, value) = split_kv(body)?;
    if multipart && let Some(spec) = value.strip_prefix("file,") {
        return Some(parse_file_form_value(key, spec));
    }
    Some(FormField {
        key: key.to_string(),
        value: value.to_string(),
        kind: FormFieldKind::Text,
        content_type: None,
        base64_prefix: None,
        enabled: true,
        desc: String::new(),
    })
}

/// Parse the `PATH; CONTENT-TYPE` part of a `[Multipart]` `file,…` value into a
/// `File`/`Base64File` field, reversing [`escape_form_file_path`] on the path.
///
/// Also reachable from a report's `USING(multipart.x = …)` override, which has
/// to read the same spelling for the same reason a `.hurl` file does.
pub(crate) fn parse_file_form_value(key: &str, spec: &str) -> FormField {
    let (escaped_path, ct) = split_unescaped_semicolon(spec);
    let path = unescape_form_file_path(escaped_path);
    let ct = ct.trim();
    if let Some(encoded) = ct.strip_prefix(BASE64_FILE_CT_MARKER) {
        let prefix = URL_SAFE_NO_PAD
            .decode(encoded)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .unwrap_or_default();
        return FormField {
            key: key.to_string(),
            value: path,
            kind: FormFieldKind::Base64File,
            content_type: None,
            base64_prefix: Some(prefix),
            enabled: true,
            desc: String::new(),
        };
    }
    FormField {
        key: key.to_string(),
        value: path,
        kind: FormFieldKind::File,
        content_type: (!ct.is_empty()).then(|| ct.to_string()),
        base64_prefix: None,
        enabled: true,
        desc: String::new(),
    }
}

/// Split at the first non-escaped `;` (the separator between a file path and
/// its content-type), returning `(path, rest)`; the whole string as `path`
/// when there's no separator.
fn split_unescaped_semicolon(spec: &str) -> (&str, &str) {
    let bytes = spec.as_bytes();
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate() {
        if escaped {
            escaped = false;
        } else if b == b'\\' {
            escaped = true;
        } else if b == b';' {
            return (&spec[..i], &spec[i + 1..]);
        }
    }
    (spec, "")
}

/// Reverse [`escape_form_file_path`]: turn a Hurl filename token back into a
/// real filesystem path (`\ ` → space, `\n`/`\r` → newlines, `\x` → `x`).
fn unescape_form_file_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some(other) => out.push(other),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// A `[Multipart]` row: a plain text field, or a file field (`key:
/// file,path;content-type`, content-type optional).
fn multipart_field(p: &MultipartParam) -> FormField {
    match p {
        MultipartParam::Param(kv) => FormField {
            key: kv.key.to_source().to_string(),
            value: kv.value.to_source().to_string(),
            kind: FormFieldKind::Text,
            content_type: None,
            base64_prefix: None,
            enabled: true,
            desc: String::new(),
        },
        MultipartParam::FilenameParam(fp) => {
            let content_type = fp
                .value
                .content_type
                .as_ref()
                .map(|t| t.to_source().to_string());
            // A PaperBoy-marked content-type means this was a Base64File on
            // save (Hurl has no native base64-file concept). Restore the
            // Base64File kind and decode its URL-safe-base64 prefix; a bad
            // encoding degrades gracefully to an empty prefix.
            if let Some(encoded) = content_type
                .as_deref()
                .and_then(|ct| ct.strip_prefix(BASE64_FILE_CT_MARKER))
            {
                let prefix = URL_SAFE_NO_PAD
                    .decode(encoded)
                    .ok()
                    .and_then(|bytes| String::from_utf8(bytes).ok())
                    .unwrap_or_default();
                return FormField {
                    key: fp.key.to_source().to_string(),
                    value: fp.value.filename.to_string(),
                    kind: FormFieldKind::Base64File,
                    content_type: None,
                    base64_prefix: Some(prefix),
                    enabled: true,
                    desc: String::new(),
                };
            }
            FormField {
                key: fp.key.to_source().to_string(),
                // The decoded filename (spaces and other escapes resolved), so it
                // matches a real filesystem path — the same form the file picker
                // stores. Re-escaping happens on the way back out (see
                // `entry.rs`'s `escape_form_file_path`).
                value: fp.value.filename.to_string(),
                kind: FormFieldKind::File,
                content_type,
                base64_prefix: None,
                enabled: true,
                desc: String::new(),
            }
        }
    }
}

/// Render a request or response body back to its Hurl source form.
///
/// `file,…;` and `base64,…;` bodies have no textual value to render, so they
/// are recovered from the source line itself. Returning `None` for them (as
/// this did) is indistinguishable from "this request has no body", and since
/// [`collection_to_hurl`](super::entry::collection_to_hurl) rewrites *every*
/// entry on every save — not just the edited one — a single save anywhere in
/// the collection silently deleted the body line of every such request, with
/// no parse error and no change in entry count to hint at it.
///
/// Both forms are a single line and carry a source span, so `source_line`
/// gives them back verbatim and the round trip is byte-stable.
fn body_source(b: &Body, lines: &[&str]) -> Option<String> {
    let s = match &b.value {
        Bytes::Json(v) => v.to_source().to_string(),
        Bytes::Xml(x) => x.clone(),
        Bytes::OnelineString(t) => t.to_source().to_string(),
        Bytes::MultilineString(m) => m.to_source().to_string(),
        Bytes::Hex(h) => h.to_string(),
        Bytes::Base64(x) => source_line(x.space0.source_info.start.line, lines)?,
        Bytes::File(x) => source_line(x.space0.source_info.start.line, lines)?,
    };
    let s = s.trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// The trimmed source line at 1-based `line`, if non-empty.
fn source_line(line: usize, lines: &[&str]) -> Option<String> {
    let idx = line.checked_sub(1)?;
    lines
        .get(idx)
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
}

/// A `[Captures]` line "name: query …" split into (name, expression).
fn capture_pair(c: &Capture, lines: &[&str]) -> Option<(String, String)> {
    let line = source_line(c.query.source_info.start.line, lines)?;
    let (name, expr) = line.split_once(':')?;
    Some((name.trim().to_string(), expr.trim().to_string()))
}

/// Recover a request's PaperBoy `# [Reports]` block from raw source, within the
/// entry's 1-based line window `[start, end)`. A real `[Reports]` response
/// section is a non-recoverable `hurl_core` parse error, so report-field
/// definitions are round-tripped as comments (see [`HurlEntry::to_hurl`]): the
/// `# [Reports]` marker followed by contiguous `# name: query` rows. Scanning
/// stops at the first line that isn't such a row (a blank line, a real section,
/// or a prose comment), mirroring the disabled-row scan.
fn reports_from_span(lines: &[&str], start: usize, end: usize) -> Vec<(String, String)> {
    let from = start.saturating_sub(1);
    let to = end.saturating_sub(1).min(lines.len());
    let mut reports = Vec::new();
    let mut i = from;
    // Locate the marker.
    while i < to {
        if is_reports_marker(lines[i]) {
            i += 1;
            break;
        }
        i += 1;
    }
    // Collect the contiguous `# name: query` rows that follow it.
    while i < to {
        match parse_report_row(lines[i]) {
            Some(row) => reports.push(row),
            None => break,
        }
        i += 1;
    }
    reports
}

/// `true` when `line` is the `# [Reports]` block marker (leading whitespace and
/// the comment `#` allowed, case-insensitive on the section name).
fn is_reports_marker(line: &str) -> bool {
    line.trim_start()
        .strip_prefix('#')
        .map(str::trim)
        .is_some_and(|rest| rest.eq_ignore_ascii_case("[Reports]"))
}

/// Parse one `# name: query` report row into `(name, query)`. `name` must be a
/// single identifier-like token (alphanumeric / `_` / `-`) so a prose comment
/// (which may contain a colon) doesn't get mistaken for a report field.
fn parse_report_row(line: &str) -> Option<(String, String)> {
    let rest = line.trim_start().strip_prefix('#')?.trim_start();
    if rest.starts_with('[') {
        return None;
    }
    let (name, query) = rest.split_once(':')?;
    let name = name.trim();
    let query = query.trim();
    if name.is_empty()
        || query.is_empty()
        || !name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
    {
        return None;
    }
    Some((name.to_string(), query.to_string()))
}

/// Title = the `#` comment lines immediately above the request's method line
/// (reset by a blank line), with `#` and `-`/`=` decoration stripped. The
/// entry's `source_info.start` is sometimes the leading comment and sometimes
/// the method line (depending on how `hurl_core` attaches inter-entry
/// comments), so we first locate the method line, then scan back for its block.
fn title_from_span(start_line: usize, lines: &[&str]) -> String {
    // The method line: first non-comment, non-blank line at/after the start.
    let method = (start_line.saturating_sub(1)..lines.len())
        .find(|&i| {
            let l = lines[i].trim();
            !l.is_empty() && !l.starts_with('#')
        })
        .unwrap_or(lines.len());
    // The contiguous comment block directly above it (bounded below by the
    // last blank or content line, which ends the block).
    let block_start = lines[..method]
        .iter()
        .rposition(|l| !l.trim().starts_with('#'))
        .map_or(0, |i| i + 1);
    lines[block_start..method]
        .iter()
        .map(|l| {
            l.trim_start_matches('#')
                .chars()
                .filter(|c| !matches!(c, '-' | '='))
                .collect::<String>()
                .trim()
                .to_string()
        })
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::super::entry::collection_to_hurl;
    use super::*;

    #[test]
    fn parse_error_explains_captures_needing_a_response_line() {
        // `[Captures]` on a request with no `HTTP` line is the classic trip-up:
        // it's a *response* section, so hurl_core rejects it as an unknown
        // request section. The message must name the section and the fix.
        let content =
            "# Get token\nPOST http://h/oauth2\n[Captures]\naccess_token: jsonpath \"$.token\"\n";
        assert!(parse_hurl(content).is_empty(), "this really is unparseable");
        let why = parse_hurl_error(content).expect("a reason is produced");
        assert!(
            why.contains("Captures"),
            "names the offending section: {why}"
        );
        assert!(
            why.contains("HTTP"),
            "points at the missing response line: {why}"
        );
        assert!(why.contains("line 3"), "cites the line: {why}");
    }

    #[test]
    fn parse_error_is_none_for_valid_hurl() {
        let content = "GET http://h/x\nHTTP 200\n[Captures]\ntok: jsonpath \"$.t\"\n";
        assert_eq!(parse_hurl(content).len(), 1);
        assert!(parse_hurl_error(content).is_none());
    }

    #[test]
    fn body_terminates_at_http_so_later_entries_parse() {
        let content = "# First\nPOST http://x/a\nContent-Type: application/json\n{\n  \"k\": \"v\"\n}\nHTTP 200\n\n# Second\nGET http://x/b\nAccept: application/json\nHTTP 200\n";
        let e = parse_hurl(content);
        assert_eq!(e.len(), 2, "the body must not swallow the second entry");
        assert_eq!(e[0].body_src.as_deref(), Some("{\n  \"k\": \"v\"\n}"));
        assert_eq!(e[1].method, "GET");
        assert!(e[1].body_src.is_none());
    }

    #[test]
    fn blank_line_before_headers_does_not_drop_them() {
        // Hurl allows a blank line between the request line and the header
        // block; hurl_core parses the headers, but PaperBoy's source-scan used
        // to read that first blank line as "no headers". The scan must skip the
        // leading blank line(s) and still recover every header.
        let content = "# Get token\nPOST {{ URL }}/oauth2\n\nContent-Length: 0\nUser-Agent: crabman/0.1.0\nAccept: */*\nclient_id: {{ CLIENT_ID }}\n\nHTTP 200\n[Captures]\naccess_token: jsonpath \"$.token\"\n";
        let e = parse_hurl(content);
        assert_eq!(e.len(), 1);
        assert_eq!(
            e[0].headers,
            vec![
                ("Content-Length".into(), "0".into(), true),
                ("User-Agent".into(), "crabman/0.1.0".into(), true),
                ("Accept".into(), "*/*".into(), true),
                ("client_id".into(), "{{ CLIENT_ID }}".into(), true),
            ],
            "a blank line after the request line must not drop the headers"
        );
    }

    #[test]
    fn blank_line_before_headers_without_body_leaves_headers_intact() {
        // The no-body variant: skipping the leading blank must not run off into
        // the response line and invent rows either.
        let content = "GET http://h/x\n\nAccept: application/json\nHTTP 200\n";
        let e = parse_hurl(content);
        assert_eq!(e.len(), 1);
        assert_eq!(
            e[0].headers,
            vec![("Accept".into(), "application/json".into(), true)]
        );
        assert!(e[0].body_src.is_none());
    }

    #[test]
    fn blank_line_before_json_body_with_no_headers_stays_empty() {
        // No headers, a blank line, then a JSON body: skipping the leading blank
        // must not misread the body's first line as a header row.
        let content = "POST http://h/x\n\n{\n  \"k\": \"v\"\n}\nHTTP 200\n";
        let e = parse_hurl(content);
        assert_eq!(e.len(), 1);
        assert!(e[0].headers.is_empty(), "the JSON body is not a header");
        assert_eq!(e[0].body_src.as_deref(), Some("{\n  \"k\": \"v\"\n}"));
    }

    #[test]
    fn blank_line_after_section_header_keeps_rows() {
        // The same blank-line tolerance must apply to the `[Cookies]` and
        // `[QueryStringParams]` sections, which share the source-scan helper.
        let content = "GET http://h/x\n[QueryStringParams]\n\npage: 1\nsize: 20\n[Cookies]\n\ntheme: dark\nHTTP 200\n";
        let e = parse_hurl(content);
        assert_eq!(e.len(), 1);
        assert_eq!(
            e[0].queries,
            vec![
                ("page".into(), "1".into(), true),
                ("size".into(), "20".into(), true),
            ]
        );
        assert_eq!(e[0].cookies, vec![("theme".into(), "dark".into(), true)]);
    }

    #[test]
    fn blank_and_comment_lines_between_headers_are_tolerated() {
        // hurl_core keeps headers separated by interior blank lines and prose
        // comments; the bounded scan (up to the HTTP line) must match it.
        let content = "GET http://h/x\nAccept: 1\n\n# a prose note\nContent-Type: 2\nHTTP 200\n";
        let e = parse_hurl(content);
        assert_eq!(e.len(), 1);
        assert_eq!(
            e[0].headers,
            vec![
                ("Accept".into(), "1".into(), true),
                ("Content-Type".into(), "2".into(), true),
            ],
            "an interior blank + prose comment must not truncate the header block"
        );
    }

    #[test]
    fn comment_before_first_header_is_skipped() {
        let content = "GET http://h/x\n# leading note\nAccept: 1\nHTTP 200\n";
        let e = parse_hurl(content);
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].headers, vec![("Accept".into(), "1".into(), true)]);
    }

    #[test]
    fn trailing_and_all_disabled_headers_before_http_are_recovered() {
        // Disabled rows kept as `# key: value` comments sit between the enabled
        // headers and the HTTP line. hurl_core excludes them from the request's
        // source span, so the scan must bound the block by the HTTP line, not by
        // that span, or these rows would be silently dropped.
        let trailing = "GET http://h/x\nAccept: 1\n# X-Debug: on\nHTTP 200\n";
        let e = parse_hurl(trailing);
        assert_eq!(
            e[0].headers,
            vec![
                ("Accept".into(), "1".into(), true),
                ("X-Debug".into(), "on".into(), false),
            ]
        );

        let all_disabled = "GET http://h/x\n# A: 1\n# B: 2\nHTTP 200\n";
        let e = parse_hurl(all_disabled);
        assert_eq!(
            e[0].headers,
            vec![
                ("A".into(), "1".into(), false),
                ("B".into(), "2".into(), false),
            ]
        );
    }

    #[test]
    fn a_blank_line_header_scan_never_bleeds_into_the_next_request() {
        // A request with no body/section/response has no structural anchor below
        // its headers, so the scan runs in "open" mode: it must stop at the
        // blank line separating it from the next entry and must NOT absorb that
        // entry's banner as a disabled header of the first one.
        let content = "GET http://h/a\nAccept: 1\n\n# X-Not-Mine: v\nGET http://h/b\nHTTP 200\n";
        let e = parse_hurl(content);
        assert_eq!(e.len(), 2);
        assert_eq!(
            e[0].headers,
            vec![("Accept".into(), "1".into(), true)],
            "the second entry's banner must not leak into the first entry's headers"
        );
    }

    #[test]
    fn open_mode_zero_header_request_does_not_absorb_next_entrys_comment() {
        // Regression: an entry with NO headers, body, section or response scans
        // in open mode starting at the blank separator line. It must stop at
        // that blank rather than skipping it and reading the following entry's
        // leading `# key: value`-shaped comment as a disabled header of its own.
        let content =
            "GET http://api/health\n\n# TODO: fix auth below\nPOST http://api/login\nHTTP 200\n";
        let e = parse_hurl(content);
        assert_eq!(e.len(), 2);
        assert!(
            e[0].headers.is_empty(),
            "a zero-header request must not absorb the next entry's comment: {:?}",
            e[0].headers
        );
    }

    #[test]
    fn all_disabled_and_trailing_section_rows_before_http_are_recovered() {
        // The same anchor-bounded recovery must hold for `[QueryStringParams]`
        // and `[Cookies]`: rows (including all-disabled ones) between the
        // section header and the HTTP line survive.
        let content = "GET http://h/x\n[QueryStringParams]\n# a: 1\n# b: 2\n[Cookies]\ntheme: dark\n# hidden: y\nHTTP 200\n";
        let e = parse_hurl(content);
        assert_eq!(e.len(), 1);
        assert_eq!(
            e[0].queries,
            vec![
                ("a".into(), "1".into(), false),
                ("b".into(), "2".into(), false),
            ]
        );
        assert_eq!(
            e[0].cookies,
            vec![
                ("theme".into(), "dark".into(), true),
                ("hidden".into(), "y".into(), false),
            ]
        );
    }

    #[test]
    fn blank_line_after_form_header_keeps_disabled_rows() {
        // A `[Form]` with an enabled row (from the AST) and a disabled row
        // recovered by scanning, separated from the header by a blank line.
        let content = "POST http://h/x\n[Form]\n\nname: alice\n# nickname: al\nHTTP 200\n";
        let e = parse_hurl(content);
        assert_eq!(e.len(), 1);
        let fields: Vec<(String, bool)> = e[0]
            .form_fields
            .iter()
            .map(|f| (f.key.clone(), f.enabled))
            .collect();
        assert_eq!(
            fields,
            vec![("name".into(), true), ("nickname".into(), false)]
        );
    }

    #[test]
    fn hurl_round_trips_through_serialize_and_parse() {
        let original = vec![
            HurlEntry::from_fields(
                "Create post",
                "POST",
                "{{ BASE_URL }}/posts",
                vec![KvRow::toggled("Content-Type", "application/json", true)],
                "{\n  \"title\": \"hi\"\n}",
            ),
            HurlEntry::from_fields(
                "Health",
                "GET",
                "{{ BASE_URL }}/health",
                vec![KvRow::toggled("Accept", "application/json", true)],
                "",
            ),
        ];

        let text = collection_to_hurl(&original);
        let reparsed = parse_hurl(&text);

        assert_eq!(reparsed.len(), original.len());
        for (a, b) in original.iter().zip(&reparsed) {
            assert_eq!(a.title, b.title);
            assert_eq!(a.method, b.method);
            assert_eq!(a.url, b.url);
            assert_eq!(a.headers, b.headers);
            assert_eq!(a.body_src, b.body_src);
        }
    }

    #[test]
    fn sections_and_captures_round_trip() {
        let src = "# Auth\nGET {{ BASE_URL }}/users/1\n[BasicAuth]\n{{ USER }}: {{ PASS }}\nHTTP 200\n[Captures]\ntoken: jsonpath \"$.token\"\n";
        let parsed = parse_hurl(src);
        assert_eq!(parsed.len(), 1);
        let reparsed = parse_hurl(&collection_to_hurl(&parsed));
        assert_eq!(reparsed.len(), 1);
        assert_eq!(reparsed[0].basic_auth, parsed[0].basic_auth);
        assert_eq!(reparsed[0].expected_status, parsed[0].expected_status);
        assert_eq!(reparsed[0].captures, parsed[0].captures);
    }

    #[test]
    fn asserts_and_captures_without_explicit_status_still_round_trip() {
        // An entry built by hand (e.g. from the request wizard) with asserts or
        // captures but no expected_status must still emit a response section
        // (`HTTP *`, the Hurl wildcard) so those sections survive a save/reload
        // instead of being silently dropped.
        let mut entry =
            HurlEntry::from_fields("Health", "GET", "{{ BASE_URL }}/health", vec![], "");
        entry.asserts = vec!["jsonpath \"$.status\" == \"ok\"".to_string()];
        entry.captures = vec![("id".to_string(), "jsonpath \"$.id\"".to_string())];
        assert!(entry.expected_status.is_none());

        let text = entry.to_hurl();
        assert!(
            text.contains("HTTP *"),
            "wildcard status line expected:\n{text}"
        );
        let reparsed = parse_hurl(&text);
        assert_eq!(reparsed.len(), 1);
        assert_eq!(reparsed[0].asserts, entry.asserts);
        assert_eq!(reparsed[0].captures, entry.captures);
        assert!(reparsed[0].expected_status.is_none());
    }

    #[test]
    fn asserts_are_parsed_and_round_trip() {
        let src = "# Health\nGET {{ BASE_URL }}/health\nHTTP 200\n[Asserts]\njsonpath \"$.status\" == \"ok\"\njsonpath \"$.count\" >= 1\n[Captures]\nid: jsonpath \"$.id\"\n";
        let parsed = parse_hurl(src);
        assert_eq!(parsed.len(), 1);
        assert_eq!(
            parsed[0].asserts,
            vec![
                "jsonpath \"$.status\" == \"ok\"".to_string(),
                "jsonpath \"$.count\" >= 1".to_string(),
            ],
        );
        let reparsed = parse_hurl(&collection_to_hurl(&parsed));
        assert_eq!(reparsed[0].asserts, parsed[0].asserts);
        assert_eq!(reparsed[0].captures, parsed[0].captures);
    }

    #[test]
    fn cookies_round_trip() {
        let mut entry = HurlEntry::from_fields("Login", "GET", "{{ BASE_URL }}/me", vec![], "");
        entry.cookies = vec![
            KvRow::toggled("session", "abc123", true),
            KvRow::toggled("theme", "dark", true),
        ];

        let text = entry.to_hurl();
        assert!(
            text.contains("[Cookies]"),
            "expected a Cookies section:\n{text}"
        );
        let reparsed = parse_hurl(&text);
        assert_eq!(reparsed.len(), 1);
        assert_eq!(reparsed[0].cookies, entry.cookies);
    }

    #[test]
    fn reports_block_round_trips_through_serialize_and_parse() {
        let mut entry = HurlEntry::from_fields("Process", "POST", "{{ URL }}/process", vec![], "");
        entry.reports = vec![
            ("status".to_string(), "jsonpath \"$.status\"".to_string()),
            (
                "overall".to_string(),
                "jsonpath \"$.overall_result\"".to_string(),
            ),
        ];

        let text = entry.to_hurl();
        assert!(
            text.contains("# [Reports]"),
            "expected a commented Reports marker:\n{text}"
        );
        // The block must survive re-parsing.
        let reparsed = parse_hurl(&text);
        assert_eq!(reparsed.len(), 1);
        assert_eq!(reparsed[0].reports, entry.reports);
    }

    #[test]
    fn reports_block_is_ignored_by_hurl_core_so_the_file_still_parses() {
        // The whole point of comment-encoding: `hurl_core` must still parse a
        // request that carries a `# [Reports]` block (a literal `[Reports]`
        // response section would be a non-recoverable parse error).
        let mut entry = HurlEntry::from_fields("Process", "POST", "http://h/process", vec![], "");
        entry.captures = vec![("token".to_string(), "jsonpath \"$.token\"".to_string())];
        entry.reports = vec![("status".to_string(), "jsonpath \"$.status\"".to_string())];
        let text = entry.to_hurl();
        assert!(
            parse_hurl_file(&text).is_ok(),
            "hurl_core should still parse a file with a # [Reports] block:\n{text}"
        );
        // And the real [Captures] section alongside it is unaffected.
        let reparsed = parse_hurl(&text);
        assert_eq!(reparsed[0].captures, entry.captures);
        assert_eq!(reparsed[0].reports, entry.reports);
    }

    #[test]
    fn reports_block_scan_does_not_bleed_across_entries() {
        // Two entries; only the first has a Reports block. The scan window must
        // stop at the next entry so the block isn't attributed to the wrong one.
        let mut a = HurlEntry::from_fields("First", "GET", "http://h/a", vec![], "");
        a.reports = vec![("s".to_string(), "jsonpath \"$.s\"".to_string())];
        let b = HurlEntry::from_fields("Second", "GET", "http://h/b", vec![], "");
        let doc = collection_to_hurl(&[a.clone(), b.clone()]);
        let parsed = parse_hurl(&doc);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].reports, a.reports);
        assert!(
            parsed[1].reports.is_empty(),
            "second entry must not inherit the first's Reports block"
        );
    }

    #[test]
    fn text_only_form_fields_round_trip_as_form_section() {
        let mut entry = HurlEntry::from_fields("Login", "POST", "{{ BASE_URL }}/login", vec![], "");
        entry.form_fields = vec![
            FormField {
                key: "user".to_string(),
                value: "bob".to_string(),
                kind: FormFieldKind::Text,
                content_type: None,
                base64_prefix: None,
                enabled: true,
                desc: String::new(),
            },
            FormField {
                key: "pass".to_string(),
                value: "secret".to_string(),
                kind: FormFieldKind::Text,
                content_type: None,
                base64_prefix: None,
                enabled: true,
                desc: String::new(),
            },
        ];

        let text = entry.to_hurl();
        assert!(
            text.contains("[Form]"),
            "all-Text fields should serialize as [Form]:\n{text}"
        );
        assert!(!text.contains("[Multipart]"));
        let reparsed = parse_hurl(&text);
        assert_eq!(reparsed.len(), 1);
        assert_eq!(reparsed[0].form_fields, entry.form_fields);
    }

    #[test]
    fn file_form_fields_round_trip_as_multipart_section() {
        // A single File field switches the whole section to [Multipart], even
        // when mixed with plain Text fields; content-type is optional.
        let mut entry =
            HurlEntry::from_fields("Upload", "POST", "{{ BASE_URL }}/upload", vec![], "");
        entry.form_fields = vec![
            FormField {
                key: "field1".to_string(),
                value: "value1".to_string(),
                kind: FormFieldKind::Text,
                content_type: None,
                base64_prefix: None,
                enabled: true,
                desc: String::new(),
            },
            FormField {
                key: "field2".to_string(),
                value: "example.txt".to_string(),
                kind: FormFieldKind::File,
                content_type: None,
                base64_prefix: None,
                enabled: true,
                desc: String::new(),
            },
            FormField {
                key: "field3".to_string(),
                value: "example.zip".to_string(),
                kind: FormFieldKind::File,
                content_type: Some("application/zip".to_string()),
                base64_prefix: None,
                enabled: true,
                desc: String::new(),
            },
        ];

        let text = entry.to_hurl();
        assert!(
            text.contains("[Multipart]"),
            "a File field should switch to [Multipart]:\n{text}"
        );
        assert!(!text.contains("[Form]\n"));
        let reparsed = parse_hurl(&text);
        assert_eq!(reparsed.len(), 1);
        assert_eq!(reparsed[0].form_fields, entry.form_fields);
    }

    #[test]
    fn file_form_field_path_with_spaces_round_trips_as_a_real_path() {
        let mut entry =
            HurlEntry::from_fields("Upload", "POST", "{{ BASE_URL }}/upload", vec![], "");
        entry.form_fields = vec![FormField {
            key: "doc".to_string(),
            // A real filesystem path containing spaces (as the file picker
            // would store it) — no backslash escaping in the model.
            value: "/tmp/my report final.pdf".to_string(),
            kind: FormFieldKind::File,
            content_type: None,
            base64_prefix: None,
            enabled: true,
            desc: String::new(),
        }];

        let text = entry.to_hurl();
        assert!(
            text.contains(r"file,/tmp/my\ report\ final.pdf;"),
            "the emitted Hurl escapes spaces in the path:\n{text}"
        );
        let reparsed = parse_hurl(&text);
        assert_eq!(
            reparsed[0].form_fields, entry.form_fields,
            "and it parses back to the same unescaped real path"
        );
    }

    #[test]
    fn loading_a_multipart_file_with_escaped_spaces_yields_a_real_path() {
        let src = "POST http://x/upload\n[Multipart]\ndoc: file,my\\ report.pdf;\n";
        let parsed = parse_hurl(src);
        assert_eq!(parsed[0].form_fields.len(), 1);
        assert_eq!(
            parsed[0].form_fields[0].value, "my report.pdf",
            "the stored path is the decoded real path, not the escaped source"
        );
    }

    #[test]
    fn base64_file_field_round_trips_with_its_prefix() {
        // A Base64File keeps its file path + prefix across a save/reload cycle
        // via the PaperBoy content-type marker (Hurl has no native concept of
        // it). It serializes as a [Multipart] file line and comes back as a
        // Base64File, not a plain File.
        let mut entry =
            HurlEntry::from_fields("Upload", "POST", "{{ BASE_URL }}/upload", vec![], "");
        entry.form_fields = vec![FormField {
            key: "avatar".to_string(),
            value: "/tmp/pic.png".to_string(),
            kind: FormFieldKind::Base64File,
            content_type: None,
            base64_prefix: Some("data:image/png;base64,".to_string()),
            enabled: true,
            desc: String::new(),
        }];

        let text = entry.to_hurl();
        assert!(
            text.contains("[Multipart]"),
            "a Base64File field serializes under [Multipart]:\n{text}"
        );
        assert!(
            text.contains("x-paperboy-base64;"),
            "the emitted Hurl carries the PaperBoy marker:\n{text}"
        );
        let reparsed = parse_hurl(&text);
        assert_eq!(reparsed.len(), 1);
        assert_eq!(
            reparsed[0].form_fields, entry.form_fields,
            "the Base64File kind and its prefix survive the round trip"
        );
    }

    #[test]
    fn base64_file_field_with_empty_prefix_round_trips() {
        let mut entry =
            HurlEntry::from_fields("Upload", "POST", "{{ BASE_URL }}/upload", vec![], "");
        entry.form_fields = vec![FormField {
            key: "blob".to_string(),
            value: "/tmp/data.bin".to_string(),
            kind: FormFieldKind::Base64File,
            content_type: None,
            base64_prefix: Some(String::new()),
            enabled: true,
            desc: String::new(),
        }];

        let reparsed = parse_hurl(&entry.to_hurl());
        assert_eq!(reparsed[0].form_fields, entry.form_fields);
    }

    #[test]
    fn disabled_header_round_trips_as_a_comment() {
        let mut entry = HurlEntry::from_fields("Get", "GET", "http://x/y", vec![], "");
        entry.headers = vec![
            KvRow::toggled("Accept", "application/json", true),
            KvRow::toggled("X-Off", "no", false),
        ];

        let text = entry.to_hurl();
        assert!(
            text.contains("\n# X-Off: no\n"),
            "the disabled header is written as a comment:\n{text}"
        );
        let reparsed = parse_hurl(&text);
        assert_eq!(reparsed.len(), 1);
        assert_eq!(reparsed[0].headers, entry.headers);
    }

    #[test]
    fn disabled_cookie_and_query_rows_round_trip() {
        let mut entry = HurlEntry::from_fields("Get", "GET", "http://x/y", vec![], "");
        entry.cookies = vec![
            KvRow::toggled("session", "abc", true),
            KvRow::toggled("stale", "1", false),
        ];
        entry.queries = vec![
            KvRow::toggled("page", "2", false),
            KvRow::toggled("q", "hi", true),
        ];

        let text = entry.to_hurl();
        assert!(
            text.contains("# stale: 1"),
            "disabled cookie commented:\n{text}"
        );
        assert!(
            text.contains("# page: 2"),
            "disabled query commented:\n{text}"
        );
        let reparsed = parse_hurl(&text);
        assert_eq!(reparsed[0].cookies, entry.cookies);
        assert_eq!(reparsed[0].queries, entry.queries);
    }

    #[test]
    fn a_hand_written_commented_request_line_parses_as_disabled() {
        // A user commenting out a header/query row by hand (not via the app)
        // should be understood as a disabled entry, while a prose comment is
        // left alone.
        let src = "GET http://x/y\nAccept: text/plain\n# X-Debug: 1\n# just a note\n[Query]\npage: 2\n# limit: 10\n";
        let parsed = parse_hurl(src);
        assert_eq!(
            parsed[0].headers,
            vec![
                ("Accept".to_string(), "text/plain".to_string(), true),
                ("X-Debug".to_string(), "1".to_string(), false),
            ],
            "the commented header line is a disabled entry; the prose note is ignored"
        );
        assert_eq!(
            parsed[0].queries,
            vec![
                ("page".to_string(), "2".to_string(), true),
                ("limit".to_string(), "10".to_string(), false),
            ]
        );
    }

    #[test]
    fn disabled_rows_keep_their_position_relative_to_enabled_ones() {
        let mut entry = HurlEntry::from_fields("Get", "GET", "http://x/y", vec![], "");
        entry.headers = vec![
            KvRow::toggled("A", "1", false),
            KvRow::toggled("B", "2", true),
            KvRow::toggled("C", "3", false),
        ];
        let reparsed = parse_hurl(&entry.to_hurl());
        assert_eq!(reparsed[0].headers, entry.headers, "order is preserved");
    }

    #[test]
    fn disabled_text_form_field_round_trips_as_a_comment() {
        let mut entry = HurlEntry::from_fields("Post", "POST", "http://x/y", vec![], "");
        entry.form_fields = vec![
            FormField {
                key: "on".to_string(),
                value: "yes".to_string(),
                kind: FormFieldKind::Text,
                content_type: None,
                base64_prefix: None,
                enabled: true,
                desc: String::new(),
            },
            FormField {
                key: "off".to_string(),
                value: "no".to_string(),
                kind: FormFieldKind::Text,
                content_type: None,
                base64_prefix: None,
                enabled: false,
                desc: String::new(),
            },
        ];

        let text = entry.to_hurl();
        assert!(text.contains("[Form]"), "text-only stays [Form]:\n{text}");
        assert!(
            text.contains("# off: no"),
            "disabled field commented:\n{text}"
        );
        let reparsed = parse_hurl(&text);
        assert_eq!(reparsed[0].form_fields, entry.form_fields);
    }

    #[test]
    fn a_disabled_file_field_does_not_flip_a_form_section_to_multipart() {
        // The section type is chosen from the *enabled* fields only: a disabled
        // File row is a comment and never runs, so an otherwise text-only
        // request must stay `[Form]` (and the disabled row still round-trips).
        let mut entry = HurlEntry::from_fields("Post", "POST", "http://x/y", vec![], "");
        entry.form_fields = vec![
            FormField {
                key: "name".to_string(),
                value: "bob".to_string(),
                kind: FormFieldKind::Text,
                content_type: None,
                base64_prefix: None,
                enabled: true,
                desc: String::new(),
            },
            FormField {
                key: "doc".to_string(),
                value: "/tmp/a b.pdf".to_string(),
                kind: FormFieldKind::File,
                content_type: Some("application/pdf".to_string()),
                base64_prefix: None,
                enabled: false,
                desc: String::new(),
            },
        ];

        let text = entry.to_hurl();
        assert!(text.contains("[Form]"), "stays [Form]:\n{text}");
        assert!(!text.contains("[Multipart]"));
        let reparsed = parse_hurl(&text);
        assert_eq!(reparsed[0].form_fields, entry.form_fields);
    }

    #[test]
    fn a_disabled_multipart_file_field_round_trips() {
        let mut entry = HurlEntry::from_fields("Post", "POST", "http://x/y", vec![], "");
        entry.form_fields = vec![
            FormField {
                key: "upload".to_string(),
                value: "/tmp/on.bin".to_string(),
                kind: FormFieldKind::File,
                content_type: None,
                base64_prefix: None,
                enabled: true,
                desc: String::new(),
            },
            FormField {
                key: "avatar".to_string(),
                value: "/tmp/off.png".to_string(),
                kind: FormFieldKind::Base64File,
                content_type: None,
                base64_prefix: Some("data:image/png;base64,".to_string()),
                enabled: false,
                desc: String::new(),
            },
        ];

        let text = entry.to_hurl();
        assert!(
            text.contains("[Multipart]"),
            "an enabled File stays [Multipart]:\n{text}"
        );
        let reparsed = parse_hurl(&text);
        assert_eq!(reparsed[0].form_fields, entry.form_fields);
    }

    // A parse → serialize → parse cycle that must reproduce the same comments.
    fn assert_comments_round_trip(src: &str) -> Vec<HurlEntry> {
        let first = parse_hurl(src);
        let text = collection_to_hurl(&first);
        let second = parse_hurl(&text);
        let c1: Vec<_> = first.iter().map(|e| e.comments.clone()).collect();
        let c2: Vec<_> = second.iter().map(|e| e.comments.clone()).collect();
        assert_eq!(
            c1, c2,
            "comments must be stable across a round trip\n--- serialized ---\n{text}"
        );
        second
    }

    #[test]
    fn a_comment_before_asserts_round_trips_before_asserts() {
        let src = "GET http://h/a\nHTTP 200\n# validate the token\n[Asserts]\njsonpath \"$.token\" exists\n";
        let e = parse_hurl(src);
        assert_eq!(
            e[0].comments,
            vec![EntryComment {
                anchor: CommentAnchor::Asserts,
                text: "# validate the token".into(),
            }]
        );
        let text = e[0].to_hurl();
        assert!(
            text.contains("# validate the token\n[Asserts]"),
            "the comment must stay directly before [Asserts]:\n{text}"
        );
        assert_comments_round_trip(src);
    }

    #[test]
    fn a_prose_comment_in_the_header_region_is_kept_and_anchored_to_headers() {
        let src = "POST http://h/a\n# auth headers below\nAuthorization: Bearer x\nHTTP 200\n";
        let e = parse_hurl(src);
        assert_eq!(
            e[0].comments,
            vec![EntryComment {
                anchor: CommentAnchor::Headers,
                text: "# auth headers below".into(),
            }]
        );
        // The enabled header still loads (the comment isn't mistaken for one).
        assert_eq!(
            e[0].headers,
            vec![("Authorization".into(), "Bearer x".into(), true)]
        );
        assert_comments_round_trip(src);
    }

    #[test]
    fn a_prose_comment_and_a_disabled_row_coexist_without_duplication() {
        // `# X-Debug: 1` is a disabled header (kv-shaped); `# just a note` is
        // prose. Neither should swallow or duplicate the other.
        let src = "GET http://h/a\n# X-Debug: 1\n# just a note\nAccept: 1\nHTTP 200\n";
        let e = parse_hurl(src);
        assert_eq!(
            e[0].headers,
            vec![
                ("X-Debug".into(), "1".into(), false),
                ("Accept".into(), "1".into(), true),
            ]
        );
        assert_eq!(
            e[0].comments,
            vec![EntryComment {
                anchor: CommentAnchor::Headers,
                text: "# just a note".into(),
            }]
        );
        let entries = assert_comments_round_trip(src);
        // The disabled row survives exactly once (not also captured as prose).
        let text = collection_to_hurl(&entries);
        assert_eq!(text.matches("# X-Debug: 1").count(), 1, "{text}");
        assert_eq!(text.matches("# just a note").count(), 1, "{text}");
    }

    #[test]
    fn a_reports_block_is_not_re_captured_as_prose() {
        let src = "GET http://h/a\nHTTP 200\n# [Reports]\n# total: jsonpath \"$.total\"\n";
        let e = parse_hurl(src);
        assert_eq!(
            e[0].reports,
            vec![("total".into(), "jsonpath \"$.total\"".into())]
        );
        assert!(
            e[0].comments.is_empty(),
            "the reports block must not leak into prose comments: {:?}",
            e[0].comments
        );
        // And it isn't duplicated on re-emit.
        let text = e[0].to_hurl();
        assert_eq!(text.matches("# [Reports]").count(), 1, "{text}");
        assert_eq!(text.matches("# total:").count(), 1, "{text}");
    }

    #[test]
    fn a_banner_and_extra_leading_prose_round_trip() {
        // The contiguous block above the method line is the title; a separate
        // banner higher up (above the first entry) is kept as a Lead comment.
        let src = "#####\n# File header\n#####\n\n# Get token\nGET http://h/a\nHTTP 200\n";
        let e = parse_hurl(src);
        assert_eq!(e[0].title, "Get token");
        assert_eq!(
            e[0].comments,
            vec![
                EntryComment {
                    anchor: CommentAnchor::Lead,
                    text: "#####".into()
                },
                EntryComment {
                    anchor: CommentAnchor::Lead,
                    text: "# File header".into()
                },
                EntryComment {
                    anchor: CommentAnchor::Lead,
                    text: "#####".into()
                },
            ]
        );
        assert_comments_round_trip(src);
    }

    #[test]
    fn a_trailing_comment_round_trips_at_the_end() {
        let src = "GET http://h/a\nHTTP 200\n[Asserts]\njsonpath \"$.x\" == 1\n# checked above\n";
        let e = parse_hurl(src);
        assert_eq!(
            e[0].comments,
            vec![EntryComment {
                anchor: CommentAnchor::Trailing,
                text: "# checked above".into(),
            }]
        );
        assert_comments_round_trip(src);
    }

    #[test]
    fn a_comment_between_two_entries_is_kept_and_does_not_cross_over() {
        let src = "GET http://h/a\nHTTP 200\n# note about the first request\n\n# Second\nPOST http://h/b\nHTTP 201\n";
        let e = parse_hurl(src);
        assert_eq!(e.len(), 2);
        // The note stays with entry 0 (as a trailing comment); entry 1 keeps its
        // title and gains no stray comments.
        assert_eq!(
            e[0].comments,
            vec![EntryComment {
                anchor: CommentAnchor::Trailing,
                text: "# note about the first request".into(),
            }]
        );
        assert_eq!(e[1].title, "Second");
        assert!(e[1].comments.is_empty(), "{:?}", e[1].comments);
        assert_comments_round_trip(src);
    }

    #[test]
    fn a_hash_line_inside_a_multiline_body_is_not_captured_as_a_comment() {
        let src = "POST http://h/a\n```\n# not a comment, this is body text\n```\nHTTP 200\n";
        let e = parse_hurl(src);
        assert!(
            e[0].comments.is_empty(),
            "multiline body content must not be captured as prose: {:?}",
            e[0].comments
        );
        assert!(
            e[0].body_src
                .as_deref()
                .unwrap_or_default()
                .contains("# not a comment"),
            "the body must still contain the # line: {:?}",
            e[0].body_src
        );
    }

    #[test]
    fn a_comment_only_entry_keeps_its_comment_without_bleeding_into_the_next() {
        // The reviewer's bleed case, now viewed through comment recovery: a
        // comment glued directly above the next method line (blank above it) is
        // that entry's *title* by PaperBoy's convention, so it stays with
        // entry 1 and must never be absorbed into entry 0's headers or prose.
        let src = "GET http://h/a\n\n# a floating note\nPOST http://h/b\nHTTP 200\n";
        let e = parse_hurl(src);
        assert_eq!(e.len(), 2);
        assert!(e[0].headers.is_empty());
        assert!(e[0].comments.is_empty(), "{:?}", e[0].comments);
        assert_eq!(e[1].title, "a floating note");
        assert!(e[1].comments.is_empty(), "{:?}", e[1].comments);
        assert_comments_round_trip(src);
    }

    #[test]
    fn comments_survive_a_full_document_round_trip_unchanged() {
        // A dense mix: Lead banner, title, header-region prose, an inter-header
        // disabled row, a section comment, a response comment and a trailing
        // comment — all in one entry — must be byte-identical after two trips.
        let src = "# top of file\n\n# Login\nPOST http://h/login\n# creds below\nContent-Type: json\n[Cookies]\n# the session cookie\nsid: abc\nHTTP 200\n# then assert\n[Asserts]\njsonpath \"$.ok\" == true\n# done\n";
        let once = collection_to_hurl(&parse_hurl(src));
        let twice = collection_to_hurl(&parse_hurl(&once));
        assert_eq!(once, twice, "round trip must be idempotent:\n{once}");
        assert_comments_round_trip(src);
    }

    // ---- Request [Options] + response headers/body/version round-trip ----

    /// Parse `src`, serialize, reparse and assert the whole model is stable
    /// across the round trip for the Part 5 fields (plus that the serialized
    /// text still parses cleanly through `hurl_core`).
    fn assert_sections_round_trip(src: &str) -> Vec<HurlEntry> {
        let first = parse_hurl(src);
        let text = collection_to_hurl(&first);
        assert!(
            parse_hurl_error(&text).is_none(),
            "serialized text must parse via hurl_core:\n{text}\nerror: {:?}",
            parse_hurl_error(&text)
        );
        let second = parse_hurl(&text);
        assert_eq!(first.len(), second.len());
        for (a, b) in first.iter().zip(&second) {
            assert_eq!(a.options, b.options, "options drift:\n{text}");
            assert_eq!(
                a.response_version, b.response_version,
                "version drift:\n{text}"
            );
            assert_eq!(
                a.response_headers, b.response_headers,
                "resp headers drift:\n{text}"
            );
            assert_eq!(a.response_body, b.response_body, "resp body drift:\n{text}");
        }
        second
    }

    #[test]
    fn request_options_section_round_trips() {
        let src = "POST http://h/a\n[Options]\nretry: 3\ninsecure: true\nvariable: host=example.net\nHTTP 200\n";
        let e = parse_hurl(src);
        assert_eq!(
            e[0].options,
            vec![
                ("retry".into(), "3".into(), true),
                ("insecure".into(), "true".into(), true),
                ("variable".into(), "host=example.net".into(), true),
            ]
        );
        assert_sections_round_trip(src);
    }

    #[test]
    fn a_disabled_option_row_round_trips_as_a_comment() {
        let src = "GET http://h/a\n[Options]\nretry: 3\n# insecure: true\nHTTP 200\n";
        let e = parse_hurl(src);
        assert_eq!(
            e[0].options,
            vec![
                ("retry".into(), "3".into(), true),
                ("insecure".into(), "true".into(), false),
            ]
        );
        // The disabled row is an option, not captured a second time as prose.
        assert!(e[0].comments.is_empty(), "{:?}", e[0].comments);
        assert_sections_round_trip(src);
    }

    #[test]
    fn options_and_a_body_coexist_and_round_trip() {
        // The critical ordering case: `hurl_core` parses a request as
        // headers -> sections -> body and rejects a section after the body, so
        // `to_hurl` must emit `[Options]` before the body. If it didn't, the
        // serialized text wouldn't even parse.
        let src = "POST http://h/a\n[Options]\nretry: 2\n```\n{\"x\":1}\n```\nHTTP 200\n";
        let e = parse_hurl(src);
        assert_eq!(e[0].options, vec![("retry".into(), "2".into(), true)]);
        assert_eq!(e[0].body_src.as_deref(), Some("```\n{\"x\":1}\n```"));
        let text = e[0].to_hurl();
        assert!(
            text.find("[Options]").unwrap() < text.find("```").unwrap(),
            "[Options] must be emitted before the body:\n{text}"
        );
        assert_sections_round_trip(src);
    }

    #[test]
    fn response_headers_round_trip() {
        let src = "GET http://h/a\nHTTP 200\nContent-Type: application/json\nX-Trace: abc\n[Asserts]\njsonpath \"$.ok\" == true\n";
        let e = parse_hurl(src);
        assert_eq!(
            e[0].response_headers,
            vec![
                ("Content-Type".into(), "application/json".into(), true),
                ("X-Trace".into(), "abc".into(), true),
            ]
        );
        assert_eq!(e[0].asserts, vec!["jsonpath \"$.ok\" == true".to_string()]);
        assert_sections_round_trip(src);
    }

    #[test]
    fn a_disabled_response_header_round_trips_as_a_comment() {
        let src = "GET http://h/a\nHTTP 200\nContent-Type: application/json\n# X-Trace: abc\n";
        let e = parse_hurl(src);
        assert_eq!(
            e[0].response_headers,
            vec![
                ("Content-Type".into(), "application/json".into(), true),
                ("X-Trace".into(), "abc".into(), false),
            ]
        );
        assert!(e[0].comments.is_empty(), "{:?}", e[0].comments);
        assert_sections_round_trip(src);
    }

    #[test]
    fn response_body_round_trips_after_sections() {
        let src =
            "GET http://h/a\nHTTP 200\n[Asserts]\njsonpath \"$.a\" == 1\n```\n{\"a\":1}\n```\n";
        let e = parse_hurl(src);
        assert_eq!(e[0].response_body.as_deref(), Some("```\n{\"a\":1}\n```"));
        assert_eq!(e[0].asserts, vec!["jsonpath \"$.a\" == 1".to_string()]);
        let text = e[0].to_hurl();
        assert!(
            text.find("[Asserts]").unwrap() < text.rfind("```").unwrap(),
            "the response body must follow the response sections:\n{text}"
        );
        assert_sections_round_trip(src);
    }

    /// A `file,…;` body used to come back as `None` — indistinguishable from
    /// "no body at all" — so the next save dropped the line entirely. Because
    /// `collection_to_hurl` rewrites *every* entry, editing any request in the
    /// collection silently deleted the body of every file-bodied one, with no
    /// parse error to hint at it.
    #[test]
    fn a_file_body_survives_a_save() {
        let src = "POST http://h/a\nContent-Type: application/json\nfile, body.json;\n";
        let e = parse_hurl(src);
        assert_eq!(e[0].body_src.as_deref(), Some("file, body.json;"));
        let text = collection_to_hurl(&e);
        assert!(text.contains("file, body.json;"), "\n{text}");
        assert_eq!(parse_hurl_error(&text), None, "\n{text}");
        // Idempotent: a second save must not rewrite it again.
        assert_eq!(collection_to_hurl(&parse_hurl(&text)), text);
    }

    /// The same for a `base64,…;` body, the other `Bytes` variant with no
    /// textual value to render.
    #[test]
    fn a_base64_body_survives_a_save() {
        let src = "POST http://h/a\nbase64,SGVsbG8=;\n";
        let e = parse_hurl(src);
        assert_eq!(e[0].body_src.as_deref(), Some("base64,SGVsbG8=;"));
        let text = collection_to_hurl(&e);
        assert!(text.contains("base64,SGVsbG8=;"), "\n{text}");
        assert_eq!(parse_hurl_error(&text), None, "\n{text}");
        assert_eq!(collection_to_hurl(&parse_hurl(&text)), text);
    }

    /// Expected *response* bodies read through the same helper, so they were
    /// lost the same way.
    #[test]
    fn a_file_response_body_survives_a_save() {
        let src = "POST http://h/a\n{\"a\":1}\n\nHTTP 200\nfile,expected.json;\n";
        let e = parse_hurl(src);
        assert_eq!(e[0].response_body.as_deref(), Some("file,expected.json;"));
        let text = collection_to_hurl(&e);
        assert!(text.contains("file,expected.json;"), "\n{text}");
        assert_eq!(parse_hurl_error(&text), None, "\n{text}");
        assert_eq!(collection_to_hurl(&parse_hurl(&text)), text);
    }

    /// The blast radius that made this severe: a save triggered by editing one
    /// request must not quietly empty the body of an untouched neighbour.
    #[test]
    fn a_file_body_is_not_lost_when_another_request_is_edited() {
        let src = "POST http://h/a\nfile, body.json;\n\nGET http://h/b\n";
        let mut e = parse_hurl(src);
        e[1].url = "http://h/c".into();
        let text = collection_to_hurl(&e);
        assert!(
            text.contains("file, body.json;"),
            "the untouched request keeps its body:\n{text}"
        );
        assert_eq!(parse_hurl(&text).len(), 2, "\n{text}");
    }

    #[test]
    fn a_hash_line_inside_a_response_body_is_not_captured_as_a_comment() {
        let src = "GET http://h/a\nHTTP 200\n```\n# not a comment\n```\n";
        let e = parse_hurl(src);
        assert!(e[0].comments.is_empty(), "{:?}", e[0].comments);
        assert!(
            e[0].response_body
                .as_deref()
                .unwrap_or_default()
                .contains("# not a comment")
        );
        assert_sections_round_trip(src);
    }

    #[test]
    fn response_http_version_round_trips() {
        let src = "GET http://h/a\nHTTP/1.1 200\n";
        let e = parse_hurl(src);
        assert_eq!(e[0].response_version.as_deref(), Some("HTTP/1.1"));
        assert_eq!(e[0].expected_status, Some(200));
        let text = e[0].to_hurl();
        assert!(
            text.contains("HTTP/1.1 200"),
            "version must round-trip:\n{text}"
        );
        assert_sections_round_trip(src);
    }

    #[test]
    fn version_agnostic_http_keyword_stays_versionless() {
        let src = "GET http://h/a\nHTTP 200\n";
        let e = parse_hurl(src);
        assert_eq!(e[0].response_version, None);
        assert!(e[0].to_hurl().contains("HTTP 200"));
        assert_sections_round_trip(src);
    }

    #[test]
    fn a_version_with_no_explicit_status_uses_the_wildcard() {
        let src = "GET http://h/a\nHTTP/2 *\n[Asserts]\njsonpath \"$.x\" == 1\n";
        let e = parse_hurl(src);
        assert_eq!(e[0].response_version.as_deref(), Some("HTTP/2"));
        assert_eq!(e[0].expected_status, None);
        assert!(e[0].to_hurl().contains("HTTP/2 *"));
        assert_sections_round_trip(src);
    }

    #[test]
    fn a_comment_before_options_anchors_to_options() {
        let src = "GET http://h/a\n# tuning\n[Options]\nretry: 3\nHTTP 200\n";
        let e = parse_hurl(src);
        assert_eq!(
            e[0].comments,
            vec![EntryComment {
                anchor: CommentAnchor::Options,
                text: "# tuning".into(),
            }]
        );
        assert!(e[0].to_hurl().contains("# tuning\n[Options]"));
        assert_comments_round_trip(src);
    }

    #[test]
    fn a_comment_among_response_headers_stays_in_the_response_area() {
        let src = "GET http://h/a\nHTTP 200\n# trace headers\nX-Trace: abc\n[Asserts]\njsonpath \"$.ok\" == true\n";
        let e = parse_hurl(src);
        assert_eq!(
            e[0].comments,
            vec![EntryComment {
                anchor: CommentAnchor::ResponseHeaders,
                text: "# trace headers".into(),
            }]
        );
        let text = e[0].to_hurl();
        assert!(
            text.find("HTTP 200").unwrap() < text.find("# trace headers").unwrap()
                && text.find("# trace headers").unwrap() < text.find("[Asserts]").unwrap(),
            "the comment must stay between the HTTP line and [Asserts]:\n{text}"
        );
        assert_comments_round_trip(src);
    }

    #[test]
    fn options_response_headers_body_and_comments_all_survive_one_document() {
        let src = "# Big one\nPOST http://h/a\nContent-Type: json\n[Options]\nretry: 2\n```\n{\"x\":1}\n```\nHTTP/1.1 201\nX-Trace: t\n[Asserts]\njsonpath \"$.id\" exists\n```\n{\"id\":9}\n```\n# all checked\n";
        let e = parse_hurl(src);
        assert_eq!(e[0].options, vec![("retry".into(), "2".into(), true)]);
        assert_eq!(e[0].response_version.as_deref(), Some("HTTP/1.1"));
        assert_eq!(
            e[0].response_headers,
            vec![("X-Trace".into(), "t".into(), true)]
        );
        assert_eq!(e[0].response_body.as_deref(), Some("```\n{\"id\":9}\n```"));
        assert_eq!(e[0].body_src.as_deref(), Some("```\n{\"x\":1}\n```"));
        assert_sections_round_trip(src);
        assert_comments_round_trip(src);
    }

    #[test]
    fn options_and_response_fields_do_not_bleed_into_the_next_request() {
        // Two full entries, each with a request `[Options]` section, a response
        // version, response headers and a response body. The scans for each of
        // these must be bounded to their own entry — the second request's
        // fields must not be absorbed into the first (and vice versa).
        let src = concat!(
            "GET http://h/a\n",
            "[Options]\nretry: 1\n",
            "HTTP/1.1 200\n",
            "X-A: a\n",
            "[Asserts]\njsonpath \"$.a\" == 1\n",
            "```\n{\"a\":1}\n```\n",
            "\n",
            "GET http://h/b\n",
            "[Options]\nretry: 2\n",
            "HTTP/2 201\n",
            "X-B: b\n",
            "[Asserts]\njsonpath \"$.b\" == 2\n",
            "```\n{\"b\":2}\n```\n",
        );
        let e = parse_hurl(src);
        assert_eq!(e.len(), 2, "two distinct entries");

        assert_eq!(e[0].options, vec![("retry".into(), "1".into(), true)]);
        assert_eq!(e[0].response_version.as_deref(), Some("HTTP/1.1"));
        assert_eq!(
            e[0].response_headers,
            vec![("X-A".into(), "a".into(), true)]
        );
        assert_eq!(e[0].response_body.as_deref(), Some("```\n{\"a\":1}\n```"));

        assert_eq!(e[1].options, vec![("retry".into(), "2".into(), true)]);
        assert_eq!(e[1].response_version.as_deref(), Some("HTTP/2"));
        assert_eq!(
            e[1].response_headers,
            vec![("X-B".into(), "b".into(), true)]
        );
        assert_eq!(e[1].response_body.as_deref(), Some("```\n{\"b\":2}\n```"));

        assert_sections_round_trip(src);
    }

    /// A per-row note has nowhere to live in the Hurl grammar, so it is
    /// smuggled through as a `# @desc ` comment on the line *above* the row
    /// (a trailing comment would be ambiguous — a header value may contain
    /// `#`). Both halves of that convention have to agree.
    #[test]
    fn a_header_description_survives_a_round_trip_through_the_hurl_text() {
        let mut e = HurlEntry::from_fields("Get", "GET", "http://h/x", vec![], "");
        e.headers = vec![KvRow {
            key: "X-Trace".into(),
            value: "on".into(),
            enabled: true,
            desc: "only for staging".into(),
        }];
        let text = collection_to_hurl(&[e]);
        assert!(
            text.contains("# @desc only for staging"),
            "the note should be written above its row: {text}"
        );
        let back = parse_hurl(&text);
        assert_eq!(
            back[0].headers[0].desc, "only for staging",
            "and it should come back attached to the same row"
        );
        assert_eq!(back[0].headers[0].key, "X-Trace");
    }

    #[test]
    fn a_multi_line_description_round_trips_as_several_marker_lines() {
        let mut e = HurlEntry::from_fields("Get", "GET", "http://h/x", vec![], "");
        e.headers = vec![KvRow {
            key: "X-Trace".into(),
            value: "on".into(),
            enabled: true,
            desc: "first line\nsecond line".into(),
        }];
        let text = collection_to_hurl(&[e]);
        assert_eq!(
            text.matches("# @desc ").count(),
            2,
            "one marker per line of the note: {text}"
        );
        let back = parse_hurl(&text);
        assert_eq!(back[0].headers[0].desc, "first line\nsecond line");
    }

    /// The marker lines are also plain comments, so the prose-comment scanner
    /// has to know to leave them alone — otherwise every save would keep a
    /// copy as free text *and* re-emit the row's own marker.
    #[test]
    fn a_description_is_not_also_captured_as_a_prose_comment() {
        let mut e = HurlEntry::from_fields("Get", "GET", "http://h/x", vec![], "");
        e.headers = vec![KvRow {
            key: "X-Trace".into(),
            value: "on".into(),
            enabled: true,
            desc: "only for staging".into(),
        }];
        let once = collection_to_hurl(&[e]);
        let twice = collection_to_hurl(&parse_hurl(&once));
        assert_eq!(once, twice, "a save/load cycle must be a fixed point");
        assert_eq!(
            twice.matches("only for staging").count(),
            1,
            "the note must not be duplicated as prose: {twice}"
        );
    }

    #[test]
    fn a_disabled_row_keeps_both_its_note_and_its_disabled_state() {
        let mut e = HurlEntry::from_fields("Get", "GET", "http://h/x", vec![], "");
        e.headers = vec![KvRow {
            key: "X-Trace".into(),
            value: "on".into(),
            enabled: false,
            desc: "off until the rollout".into(),
        }];
        let back = parse_hurl(&collection_to_hurl(&[e]));
        let row = &back[0].headers[0];
        assert!(!row.enabled, "the row should still be off");
        assert_eq!(row.desc, "off until the rollout");
    }

    #[test]
    fn a_form_field_description_survives_a_round_trip() {
        let mut e = HurlEntry::from_fields("Post", "POST", "http://h/x", vec![], "");
        e.form_fields = vec![crate::hurl::FormField {
            key: "region".into(),
            value: "eu-west-1".into(),
            enabled: true,
            desc: "which cluster to hit".into(),
            ..Default::default()
        }];
        let back = parse_hurl(&collection_to_hurl(&[e]));
        assert_eq!(back[0].form_fields[0].desc, "which cluster to hit");
    }

    /// `# @description` is ordinary prose that happens to start with the same
    /// letters; only the exact marker followed by a space (or end of line)
    /// counts, or a user's comment would silently become a row note.
    #[test]
    fn prose_that_merely_starts_like_the_marker_is_left_as_a_comment() {
        let text = "POST http://h/x\n# @description of the endpoint\nX-Trace: on\nHTTP 200\n";
        let back = parse_hurl(text);
        assert_eq!(
            back[0].headers[0].desc, "",
            "the row should not have adopted the comment as its note"
        );
        assert!(
            back[0]
                .comments
                .iter()
                .any(|c| c.text.contains("@description")),
            "and the line should survive as a comment: {:?}",
            back[0].comments
        );
    }
}
