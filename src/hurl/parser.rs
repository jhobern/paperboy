//! Parse Hurl text into the app's [`HurlEntry`] model using `hurl_core`'s
//! parser, so we don't maintain a hand-written Hurl parser. `HurlEntry` stays
//! the editable/persistable model; this maps the parsed AST onto it. Fields that
//! must preserve their exact source text (URL, headers, body, captures, asserts)
//! are taken via `ToSource`/`Display` or by slicing the original source lines.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hurl_core::ast::{
    Body, Bytes, Capture, Cookie, Entry, KeyValue, MultipartParam, SectionValue, StatusValue,
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
        match &section.value {
            SectionValue::BasicAuth(Some(kv)) => basic_auth = Some(kv_pair(kv)),
            SectionValue::FormParams(kvs, _) => {
                form_fields = kvs
                    .iter()
                    .map(|kv| {
                        let (key, value) = kv_pair(kv);
                        FormField {
                            key,
                            value,
                            kind: FormFieldKind::Text,
                            content_type: None,
                            base64_prefix: None,
                        }
                    })
                    .collect();
            }
            SectionValue::MultipartFormData(parts, _) => {
                form_fields = parts.iter().map(multipart_field).collect();
            }
            SectionValue::QueryParams(kvs, _) => query_params = kvs.iter().map(kv_pair).collect(),
            SectionValue::Cookies(cs) => cookies = cs.iter().map(cookie_pair).collect(),
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

    HurlEntry {
        title: title_from_span(req.source_info.start.line, lines),
        method: req.method.to_string(),
        url: req.url.to_source().to_string(),
        headers: req.headers.iter().map(kv_pair).collect(),
        basic_auth,
        form_fields,
        query_params,
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

/// A `[Cookies]` row's `(name, value)` pair.
fn cookie_pair(c: &Cookie) -> (String, String) {
    (
        c.name.to_source().to_string(),
        c.value.to_source().to_string(),
    )
}

/// A `[Multipart]` row: a plain text field, or a file field (`key:
/// file,path;content-type`, content-type optional).
fn multipart_field(p: &MultipartParam) -> FormField {
    match p {
        MultipartParam::Param(kv) => {
            let (key, value) = kv_pair(kv);
            FormField {
                key,
                value,
                kind: FormFieldKind::Text,
                content_type: None,
                base64_prefix: None,
            }
        }
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
                vec![("Content-Type".into(), "application/json".into())],
                "{\n  \"title\": \"hi\"\n}",
            ),
            HurlEntry::from_fields(
                "Health",
                "GET",
                "{{ BASE_URL }}/health",
                vec![("Accept".into(), "application/json".into())],
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
            ("session".to_string(), "abc123".to_string()),
            ("theme".to_string(), "dark".to_string()),
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
            },
            FormField {
                key: "pass".to_string(),
                value: "secret".to_string(),
                kind: FormFieldKind::Text,
                content_type: None,
                base64_prefix: None,
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
            },
            FormField {
                key: "field2".to_string(),
                value: "example.txt".to_string(),
                kind: FormFieldKind::File,
                content_type: None,
                base64_prefix: None,
            },
            FormField {
                key: "field3".to_string(),
                value: "example.zip".to_string(),
                kind: FormFieldKind::File,
                content_type: Some("application/zip".to_string()),
                base64_prefix: None,
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
        }];

        let reparsed = parse_hurl(&entry.to_hurl());
        assert_eq!(reparsed[0].form_fields, entry.form_fields);
    }
}
