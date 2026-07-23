//! Parse Hurl text into the app's [`HurlEntry`] model using `hurl_core`'s
//! parser, so we don't maintain a hand-written Hurl parser. `HurlEntry` stays
//! the editable/persistable model; this maps the parsed AST onto it. Fields that
//! must preserve their exact source text (URL, headers, body, captures, asserts)
//! are taken via `ToSource`/`Display` or by slicing the original source lines.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hurl_core::ast::{
    Body, Bytes, Capture, Entry, KeyValue, MultipartParam, SectionValue, StatusValue,
};
use hurl_core::parser::parse_hurl_file;
use hurl_core::types::ToSource;

use super::entry::{BASE64_FILE_CT_MARKER, FormField, FormFieldKind, HurlEntry, RunStatus};

/// Parse a Hurl-format string into a list of [`HurlEntry`] values. Invalid input
/// yields an empty list (the UI treats "no entries" as a failed load).
pub fn parse_hurl(content: &str) -> Vec<HurlEntry> {
    let Ok(file) = parse_hurl_file(content) else {
        return Vec::new();
    };
    let lines: Vec<&str> = content.lines().collect();
    file.entries.iter().map(|e| map_entry(e, &lines)).collect()
}

fn map_entry(e: &Entry, lines: &[&str]) -> HurlEntry {
    let req = &e.request;

    let mut basic_auth = None;
    let mut form_fields = Vec::new();
    let mut query_params = Vec::new();
    let mut cookies = Vec::new();
    for section in &req.sections {
        // Rows start on the line after the `[Section]` header.
        let rows_start = section.source_info.start.line + 1;
        match &section.value {
            SectionValue::BasicAuth(Some(kv)) => basic_auth = Some(kv_pair(kv)),
            SectionValue::FormParams(kvs, _) => {
                form_fields = form_fields_from_section(kvs, None, lines, rows_start);
            }
            SectionValue::MultipartFormData(parts, _) => {
                form_fields = form_fields_from_section(&[], Some(parts), lines, rows_start);
            }
            // Headers live inline; Cookies/Query are `[Section]`s. All three are
            // scanned straight from source so a disabled row (kept as a
            // `# key: value` comment, invisible to `hurl_core`) round-trips.
            SectionValue::QueryParams(..) => query_params = scan_kv_rows(lines, rows_start),
            SectionValue::Cookies(_) => cookies = scan_kv_rows(lines, rows_start),
            _ => {}
        }
    }

    let mut expected_status = None;
    let mut captures = Vec::new();
    let mut asserts = Vec::new();
    if let Some(resp) = &e.response {
        if let StatusValue::Specific(n) = resp.status.value {
            expected_status = Some(n as u16);
        }
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
        // Headers occupy the lines between the request line and the body /
        // first section; scanning them (instead of reading the AST) recovers
        // disabled rows kept as `# key: value` comments.
        headers: scan_kv_rows(lines, req.url.source_info.start.line + 1),
        basic_auth,
        form_fields,
        is_multipart,
        queries: query_params,
        cookies,
        body: req.body.as_ref().and_then(body_source),
        expected_status,
        captures,
        asserts,
        user_added: false,
        modified: false,
        last_run: RunStatus::default(),
        last_response: None,
    }
}

fn kv_pair(kv: &KeyValue) -> (String, String) {
    (
        kv.key.to_source().to_string(),
        kv.value.to_source().to_string(),
    )
}

/// Scan a contiguous block of `key: value` request-section rows starting at
/// 1-based line `start`, returning each as a `(key, value, enabled)` triple.
/// A row commented out with a leading `#` comes back as a disabled entry —
/// this is how [`to_hurl`](super::entry::HurlEntry::to_hurl) round-trips
/// disabled Header, Cookies and Query rows, which `hurl_core` drops as
/// comments before they ever reach the AST. Scanning stops at the first line
/// that isn't a request-style row (a blank line, a JSON/text body, a
/// `[Section]` header, the `HTTP` response line, or a prose comment), which
/// also bounds the inline header block against the body that follows it.
fn scan_kv_rows(lines: &[&str], start: usize) -> Vec<(String, String, bool)> {
    let mut rows = Vec::new();
    let mut i = start.saturating_sub(1);
    while let Some(&line) = lines.get(i) {
        match parse_kv_row(line) {
            Some(row) => rows.push(row),
            None => break,
        }
        i += 1;
    }
    rows
}

/// Parse a single Header/Cookies/Query row into `(key, value, enabled)`.
/// `enabled` is `false` when the line is commented out (`# key: value`). The
/// key must start with an alphanumeric and contain only token characters, so a
/// JSON body line, a `[Section]` header, an `HTTP` status line or a prose
/// comment all fail to parse (ending a scan) instead of being mistaken for a
/// row.
fn parse_kv_row(line: &str) -> Option<(String, String, bool)> {
    let (enabled, rest) = uncomment(line);
    let (key, value) = split_kv(rest)?;
    Some((key.to_string(), value.to_string(), enabled))
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
/// to be a plausible header/param token (starts alphanumeric, token characters
/// only). Returns `None` for anything else.
fn split_kv(text: &str) -> Option<(&str, &str)> {
    let colon = text.find(':')?;
    let key = text[..colon].trim();
    let mut chars = key.chars();
    let starts_ok = chars.next().is_some_and(|c| c.is_ascii_alphanumeric());
    let token_ok = key
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !starts_ok || !token_ok {
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
                },
            ));
        }
    }
    rows.extend(scan_disabled_form_rows(lines, rows_start));
    rows.sort_by_key(|(line, _)| *line);
    rows.into_iter().map(|(_, f)| f).collect()
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
/// over (they come from the AST); the walk stops at the section's end (a blank
/// line, a following `[Section]`/`HTTP` line, or a prose comment).
///
/// Disabled rows are parsed as file-capable regardless of the section type:
/// a disabled `File`/`Base64File` field is always serialized with the
/// `file,…` syntax (even inside an otherwise text-only `[Form]`, since the
/// section type is chosen from the enabled fields alone), so parsing it back
/// as a file is what restores its original kind.
fn scan_disabled_form_rows(lines: &[&str], start: usize) -> Vec<(usize, FormField)> {
    let mut out = Vec::new();
    let mut i = start.saturating_sub(1);
    while let Some(&line) = lines.get(i) {
        let (enabled, rest) = uncomment(line);
        match parse_form_field_line(rest, true) {
            Some(mut field) if !enabled => {
                field.enabled = false;
                out.push((i + 1, field));
            }
            // An enabled row (already captured from the AST): step over it.
            Some(_) => {}
            // Blank line, next section, or prose comment: end of the rows.
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
    })
}

/// Parse the `PATH; CONTENT-TYPE` part of a `[Multipart]` `file,…` value into a
/// `File`/`Base64File` field, reversing [`escape_form_file_path`] on the path.
fn parse_file_form_value(key: &str, spec: &str) -> FormField {
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
        };
    }
    FormField {
        key: key.to_string(),
        value: path,
        kind: FormFieldKind::File,
        content_type: (!ct.is_empty()).then(|| ct.to_string()),
        base64_prefix: None,
        enabled: true,
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
            }
        }
    }
}

/// Render a request body back to its Hurl source form.
fn body_source(b: &Body) -> Option<String> {
    let s = match &b.value {
        Bytes::Json(v) => v.to_source().to_string(),
        Bytes::Xml(x) => x.clone(),
        Bytes::OnelineString(t) => t.to_source().to_string(),
        Bytes::MultilineString(m) => m.to_source().to_string(),
        Bytes::Hex(h) => h.to_string(),
        // Base64 / file bodies aren't represented in HurlEntry's string body.
        Bytes::Base64(_) | Bytes::File(_) => return None,
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
    fn body_terminates_at_http_so_later_entries_parse() {
        let content = "# First\nPOST http://x/a\nContent-Type: application/json\n{\n  \"k\": \"v\"\n}\nHTTP 200\n\n# Second\nGET http://x/b\nAccept: application/json\nHTTP 200\n";
        let e = parse_hurl(content);
        assert_eq!(e.len(), 2, "the body must not swallow the second entry");
        assert_eq!(e[0].body.as_deref(), Some("{\n  \"k\": \"v\"\n}"));
        assert_eq!(e[1].method, "GET");
        assert!(e[1].body.is_none());
    }

    #[test]
    fn hurl_round_trips_through_serialize_and_parse() {
        let original = vec![
            HurlEntry::from_fields(
                "Create post",
                "POST",
                "{{ BASE_URL }}/posts",
                vec![("Content-Type".into(), "application/json".into(), true)],
                "{\n  \"title\": \"hi\"\n}",
            ),
            HurlEntry::from_fields(
                "Health",
                "GET",
                "{{ BASE_URL }}/health",
                vec![("Accept".into(), "application/json".into(), true)],
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
            assert_eq!(a.body, b.body);
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
            ("session".to_string(), "abc123".to_string(), true),
            ("theme".to_string(), "dark".to_string(), true),
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
            },
            FormField {
                key: "pass".to_string(),
                value: "secret".to_string(),
                kind: FormFieldKind::Text,
                content_type: None,
                base64_prefix: None,
                enabled: true,
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
            },
            FormField {
                key: "field2".to_string(),
                value: "example.txt".to_string(),
                kind: FormFieldKind::File,
                content_type: None,
                base64_prefix: None,
                enabled: true,
            },
            FormField {
                key: "field3".to_string(),
                value: "example.zip".to_string(),
                kind: FormFieldKind::File,
                content_type: Some("application/zip".to_string()),
                base64_prefix: None,
                enabled: true,
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
        }];

        let reparsed = parse_hurl(&entry.to_hurl());
        assert_eq!(reparsed[0].form_fields, entry.form_fields);
    }

    #[test]
    fn disabled_header_round_trips_as_a_comment() {
        let mut entry = HurlEntry::from_fields("Get", "GET", "http://x/y", vec![], "");
        entry.headers = vec![
            ("Accept".to_string(), "application/json".to_string(), true),
            ("X-Off".to_string(), "no".to_string(), false),
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
            ("session".to_string(), "abc".to_string(), true),
            ("stale".to_string(), "1".to_string(), false),
        ];
        entry.queries = vec![
            ("page".to_string(), "2".to_string(), false),
            ("q".to_string(), "hi".to_string(), true),
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
            ("A".to_string(), "1".to_string(), false),
            ("B".to_string(), "2".to_string(), true),
            ("C".to_string(), "3".to_string(), false),
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
            },
            FormField {
                key: "off".to_string(),
                value: "no".to_string(),
                kind: FormFieldKind::Text,
                content_type: None,
                base64_prefix: None,
                enabled: false,
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
            },
            FormField {
                key: "doc".to_string(),
                value: "/tmp/a b.pdf".to_string(),
                kind: FormFieldKind::File,
                content_type: Some("application/pdf".to_string()),
                base64_prefix: None,
                enabled: false,
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
            },
            FormField {
                key: "avatar".to_string(),
                value: "/tmp/off.png".to_string(),
                kind: FormFieldKind::Base64File,
                content_type: None,
                base64_prefix: Some("data:image/png;base64,".to_string()),
                enabled: false,
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
}
