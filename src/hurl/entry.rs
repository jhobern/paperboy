//! The `HurlEntry` request model and its Hurl-text serializer.

use serde::{Deserialize, Serialize};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

/// Whether a `[Form]`/`[Multipart]` field is a plain text value, a file
/// upload, or a file whose base64 encoding is sent inline as text. A
/// `Text`-only set of fields serializes as `[Form]`; the presence of any
/// `File` field switches the whole section to `[Multipart]`, matching Hurl
/// semantics (see https://hurl.dev/docs/request.html). `Base64File` is a
/// PaperBoy-specific kind: the user picks a file (like `File`), but at send
/// time it is transmitted as a plain text field whose value is
/// `base64_prefix` followed by the file's base64 encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum FormFieldKind {
    #[default]
    Text,
    File,
    Base64File,
}

impl FormFieldKind {
    pub fn is_multipart(&self) -> bool {
        matches!(self, FormFieldKind::Base64File | FormFieldKind::File)
    }
}

/// Marker stored in a `Base64File` field's Hurl content-type slot so a saved
/// `.hurl` round-trips back into a `Base64File` (Hurl has no native concept
/// of "encode this file as base64 text"). The base64_prefix follows the
/// marker, URL-safe-base64 encoded so it is a valid Hurl content-type token.
pub(crate) const BASE64_FILE_CT_MARKER: &str = "x-paperboy-base64;";

/// One row of a `[Form]`/`[Multipart]` section. `content_type` is only
/// meaningful for `File` fields: when `None`, Hurl infers the content type
/// from the file extension (defaulting to `application/octet-stream`).
/// `base64_prefix` is only meaningful for `Base64File` fields: it is
/// prepended to the file's base64 encoding at send time.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct FormField {
    pub key: String,
    pub value: String,
    pub kind: FormFieldKind,
    pub content_type: Option<String>,
    #[serde(default)]
    pub base64_prefix: Option<String>,
    pub enabled: bool,
}

/// Escape a `[Multipart]` File field's path for Hurl source. `value` is stored
/// as a real filesystem path (spaces and other characters unescaped, as the
/// file picker produces), but Hurl's filename grammar requires a backslash
/// before spaces and a few other characters. `{`/`}` are deliberately left
/// alone so `{{var}}` placeholders in a path still substitute at run time.
fn escape_form_file_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for c in path.chars() {
        match c {
            ' ' | '#' | ';' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(c),
        }
    }
    out
}

/// Append `line` to `out` as its own line, commenting it out with a leading
/// `# ` when the row is disabled. Disabled request rows are round-tripped as
/// comments (Hurl ignores them at run time) so the enabled flag survives a
/// save/reload; [`parse_hurl`](super::parse_hurl) restores a commented row
/// that still looks like a real request line as a disabled entry.
fn push_line(out: &mut String, line: &str, enabled: bool) {
    if !enabled {
        out.push_str("# ");
    }
    out.push_str(line);
    out.push('\n');
}

/// Append one `key: value` request-section row to `out` (see [`push_line`] for
/// the disabled-row handling). Shared by the Header, Cookies and Query
/// sections, which are otherwise identical.
fn push_kv_line(out: &mut String, k: &str, v: &str, enabled: bool) {
    push_line(out, &format!("{k}: {v}"), enabled);
}

/// The Hurl source for one `[Form]`/`[Multipart]` field line (without the
/// trailing newline or any disabled-row `# ` prefix). Split out so an enabled
/// row and the commented form of a disabled row share one code path.
fn form_field_line(f: &FormField) -> String {
    match f.kind {
        FormFieldKind::Text => format!("{}: {}", f.key, f.value),
        FormFieldKind::File => {
            let path = escape_form_file_path(&f.value);
            match f.content_type.as_deref().map(str::trim) {
                Some(ct) if !ct.is_empty() => format!("{}: file,{}; {}", f.key, path, ct),
                _ => format!("{}: file,{};", f.key, path),
            }
        }
        // A Base64File is transformed into a plain Text field before an actual
        // request runs (see `expand_base64_form_fields`); this branch only runs
        // when serializing for *saving* to disk. Encode it as a file line whose
        // content-type carries a PaperBoy marker plus the URL-safe-base64
        // encoded prefix, so parsing restores the Base64File kind and its prefix.
        FormFieldKind::Base64File => {
            let path = escape_form_file_path(&f.value);
            let encoded_prefix =
                URL_SAFE_NO_PAD.encode(f.base64_prefix.as_deref().unwrap_or("").as_bytes());
            format!(
                "{}: file,{}; {}{}",
                f.key, path, BASE64_FILE_CT_MARKER, encoded_prefix
            )
        }
    }
}

/// Outcome of the most recent "Run All" (Alt+F5) pass over this entry's
/// collection. Purely a runtime display marker for the Requests list — never
/// persisted, and reset to `Running` for every entry the instant a new batch
/// run is kicked off (so the marker reflects "in progress" rather than
/// silently keeping a stale result while the new run is still executing).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunStatus {
    /// Hasn't run yet this session, or the last "Run All" never reached it
    /// (e.g. the batch stopped partway through).
    #[default]
    NotRun,
    /// A "Run All" is currently executing on a background thread.
    Running,
    Passed,
    Failed,
}

/// A single request entry from a Hurl file, or a user-created request.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HurlEntry {
    /// Leading comment block describing this entry (its display name).
    pub title: String,
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String, bool)>,
    pub basic_auth: Option<(String, String)>,
    /// `[Form]` (all `Text`) or `[Multipart]` (any `File`) fields, chosen
    /// automatically by [`to_hurl`](HurlEntry::to_hurl). `#[serde(default)]`
    /// keeps older saved requests (which had a plain `form_params` shape)
    /// loadable — they simply start with no form fields.
    #[serde(default)]
    pub form_fields: Vec<FormField>,
    #[serde(default)]
    pub is_multipart: bool,
    pub queries: Vec<(String, String, bool)>,
    /// `[Cookies]` `(name, value)` pairs — syntactic sugar over a `Cookie:`
    /// header. `#[serde(default)]` keeps older saved states loadable.
    #[serde(default)]
    pub cookies: Vec<(String, String, bool)>,
    pub body: Option<String>,
    pub expected_status: Option<u16>,
    /// (variable_name, query_expression) pairs, e.g. `("token", "jsonpath \"$.token\"")`.
    pub captures: Vec<(String, String)>,
    /// Raw `[Asserts]` expressions (e.g. `jsonpath "$.x" == "y"`), kept for
    /// display and round-tripping. `#[serde(default)]` keeps older saved
    /// requests (which had no asserts field) loadable.
    #[serde(default)]
    pub asserts: Vec<String>,
    /// PaperBoy-specific per-request report-field definitions: `(name, query)`
    /// pairs, each a Hurl query (same grammar as `[Captures]`) evaluated against
    /// the response to populate a report column. Stored inside valid Hurl as a
    /// `# [Reports]` comment block (a literal `[Reports]` response section is a
    /// non-recoverable `hurl_core` parse error), recovered by the line-scanning
    /// parser — the same comment-encoding used for titles and disabled rows.
    /// `#[serde(default)]` keeps older saved requests loadable.
    #[serde(default)]
    pub reports: Vec<(String, String)>,
    /// `true` when the user created this request by hand in a collection other
    /// than the Scratch Space. UI-only and never written to `.hurl` files (which
    /// use the manual [`to_hurl`](HurlEntry::to_hurl) serializer); persisted in
    /// session state so the marker survives restarts. `#[serde(default)]` keeps
    /// older saved states loadable and defaults it to `false` on parse.
    #[serde(default)]
    pub user_added: bool,
    /// `true` once the user has edited this request (via the request-JSON editor)
    /// away from its loaded state. Shown with a pencil marker in the list and
    /// counted when saving; cleared when the collection is saved to disk.
    /// UI-only; `#[serde(default)]` keeps older saved states loadable.
    #[serde(default)]
    pub modified: bool,
    /// See [`RunStatus`]. `#[serde(skip)]`: transient UI state, not persisted.
    #[serde(skip)]
    pub last_run: RunStatus,
    /// Snapshot of the most recent response actually received for this
    /// specific entry (from a single `F5` run or a "Run All" pass), so
    /// switching the Requests list selection always shows that request's
    /// own last response rather than whichever entry most recently
    /// finished elsewhere. Transient UI/session state; `#[serde(skip)]`
    /// since it's never persisted.
    #[serde(skip)]
    pub last_response: Option<crate::http::ApiResponse>,
}

impl HurlEntry {
    /// Build an entry from user-entered form fields. `headers` is a list of
    /// `(key, value, enabled)` triples; triples with an false enabled are skipped. An empty
    /// `body` becomes `None`.
    pub fn from_fields(
        name: &str,
        method: &str,
        url: &str,
        headers: Vec<(String, String, bool)>,
        body: &str,
    ) -> Self {
        let headers = headers
            .into_iter()
            .filter(|(k, _, _)| !k.trim().is_empty())
            .map(|(k, v, e)| (k.trim().to_string(), v.trim().to_string(), e))
            .collect();
        let body = if body.trim().is_empty() {
            None
        } else {
            Some(body.to_string())
        };
        Self {
            title: name.trim().to_string(),
            method: method.to_string(),
            url: url.trim().to_string(),
            headers,
            body,
            ..Default::default()
        }
    }

    /// Add an explicit `Content-Length: 0` header when this is a bodyless
    /// request whose method normally carries a body (`POST`/`PUT`/`PATCH`/
    /// `DELETE`). Browsers and Postman always send `Content-Length: 0` in this
    /// case, but libcurl (which the Hurl runner uses) omits it for a bodyless
    /// request over HTTP/2, and some servers reject such a request with a
    /// `400 Bad Request`. Matching the Postman/browser behaviour keeps those
    /// requests working.
    ///
    /// A no-op when a body or form fields are present (libcurl computes the
    /// length itself), when the method doesn't carry a body, or when the user
    /// already set a `Content-Length` header. Applied only to the transient
    /// copy that's run — never to what's saved to disk — so saved `.hurl`
    /// files stay free of synthesized headers.
    pub fn ensure_run_content_length(&mut self) {
        let carries_body = matches!(
            self.method.to_ascii_uppercase().as_str(),
            "POST" | "PUT" | "PATCH" | "DELETE"
        );
        let has_forms = !self.form_fields.is_empty();
        let has_body = self.body.as_deref().is_some_and(|b| !b.trim().is_empty())
            || !self.form_fields.is_empty();
        let has_content_length = self
            .headers
            .iter()
            .any(|(k, _, _)| k.eq_ignore_ascii_case("content-length"));
        if carries_body && !has_body && !has_content_length && !has_forms {
            self.headers
                .push(("Content-Length".to_string(), "0".to_string(), true));
        }
    }

    /// Serialize this entry to Hurl text. Ordered so it round-trips through
    /// [`parse_hurl`](super::parse_hurl): a body (which must be JSON/quoted to
    /// be re-detected) is emitted right after the headers; request sections and
    /// the response line follow. In practice an entry has either a body or
    /// request sections.
    pub fn to_hurl(&self) -> String {
        let mut out = String::new();
        if !self.title.trim().is_empty() {
            out.push_str("# ");
            out.push_str(self.title.trim());
            out.push('\n');
        }
        let method = if self.method.is_empty() {
            "GET"
        } else {
            self.method.as_str()
        };
        out.push_str(&format!("{method} {}\n", self.url));
        for (k, v, enabled) in &self.headers {
            push_kv_line(&mut out, k, v, *enabled);
        }
        if let Some(body) = &self.body {
            out.push_str(body);
            if !body.ends_with('\n') {
                out.push('\n');
            }
        }
        if let Some((user, pass)) = &self.basic_auth {
            out.push_str(&format!("[BasicAuth]\n{user}: {pass}\n"));
        }
        if !self.cookies.is_empty() {
            out.push_str("[Cookies]\n");
            for (k, v, enabled) in &self.cookies {
                push_kv_line(&mut out, k, v, *enabled);
            }
        }
        if !self.queries.is_empty() {
            out.push_str("[Query]\n");
            for (k, v, enabled) in &self.queries {
                push_kv_line(&mut out, k, v, *enabled);
            }
        }
        if !self.form_fields.is_empty() {
            // Any enabled File field switches the whole section to `[Multipart]`
            // (Hurl's `[Form]` section is text-only); a Base64File also
            // serializes as a `file,...` line (carrying its marker), so it
            // forces `[Multipart]` too. Plain Text-only fields stay `[Form]`.
            // Only *enabled* fields drive this choice: a disabled row is a
            // comment and never reaches the wire, so it must not flip an
            // otherwise-`[Form]` request onto the multipart code path.
            let multipart = self
                .form_fields
                .iter()
                .any(|f| f.enabled && f.kind.is_multipart())
                || self.is_multipart;
            out.push_str(if multipart {
                "[Multipart]\n"
            } else {
                "[Form]\n"
            });
            for f in &self.form_fields {
                push_line(&mut out, &form_field_line(f), f.enabled);
            }
        }
        // The response section (delimited by the `HTTP <status>` line) is only
        // needed to carry asserts/captures; use the wildcard `HTTP *` (any
        // status, per the Hurl spec) when no explicit status was set so those
        // sections still round-trip through parsing instead of being dropped.
        // A `# [Reports]` block also lives in the response area, so emit the
        // wildcard when reports are the only response-side metadata too.
        if let Some(status) = self.expected_status {
            out.push_str(&format!("HTTP {status}\n"));
        } else if !self.asserts.is_empty() || !self.captures.is_empty() || !self.reports.is_empty()
        {
            out.push_str("HTTP *\n");
        }
        if !self.asserts.is_empty() {
            out.push_str("[Asserts]\n");
            for a in &self.asserts {
                out.push_str(a);
                out.push('\n');
            }
        }
        if !self.captures.is_empty() {
            out.push_str("[Captures]\n");
            for (name, expr) in &self.captures {
                out.push_str(&format!("{name}: {expr}\n"));
            }
        }
        // Report fields: a comment-encoded pseudo-section. `hurl_core` treats
        // every line here as a comment and ignores it; the PaperBoy parser
        // recognises the `# [Reports]` marker and scans the `# name: query`
        // rows back into `reports`. Reads like a real `[Reports]` section.
        if !self.reports.is_empty() {
            out.push_str("# [Reports]\n");
            for (name, query) in &self.reports {
                out.push_str(&format!("# {name}: {query}\n"));
            }
        }
        out
    }
}

/// Serialize a whole collection to a Hurl document (entries separated by a
/// blank line).
pub fn collection_to_hurl(entries: &[HurlEntry]) -> String {
    entries
        .iter()
        .map(HurlEntry::to_hurl)
        .collect::<Vec<_>>()
        .join("\n")
}

/// HTTP methods offered when creating a request.
pub const METHODS: &[&str] = &["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD"];

/// The RGB colour used to badge each HTTP method in the TUI. `None` for
/// unknown methods, for which callers fall back to their own neutral (grey)
/// colour.
pub fn method_rgb(method: &str) -> Option<(u8, u8, u8)> {
    Some(match method {
        "GET" => (97, 175, 239),
        "POST" => (73, 204, 144),
        "PUT" => (252, 161, 48),
        "DELETE" => (248, 81, 73),
        "PATCH" => (80, 227, 194),
        "ANY" => (252, 161, 48),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(method: &str) -> HurlEntry {
        HurlEntry {
            method: method.to_string(),
            url: "http://x/y".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn bodyless_post_gets_an_explicit_content_length_zero() {
        let mut e = entry("POST");
        e.ensure_run_content_length();
        assert!(
            e.headers
                .iter()
                .any(|(k, v, _)| k == "Content-Length" && v == "0")
        );
    }

    #[test]
    fn content_length_added_for_all_body_carrying_methods() {
        for m in ["POST", "PUT", "PATCH", "DELETE", "post", "Put"] {
            let mut e = entry(m);
            e.ensure_run_content_length();
            assert!(
                e.headers.iter().any(|(k, _, _)| k == "Content-Length"),
                "expected Content-Length for {m}"
            );
        }
    }

    #[test]
    fn get_and_head_never_get_a_content_length() {
        for m in ["GET", "HEAD"] {
            let mut e = entry(m);
            e.ensure_run_content_length();
            assert!(
                !e.headers
                    .iter()
                    .any(|(k, _, _)| k.eq_ignore_ascii_case("content-length")),
                "did not expect Content-Length for {m}"
            );
        }
    }

    #[test]
    fn content_length_skipped_when_a_body_is_present() {
        let mut e = entry("POST");
        e.body = Some("{\"a\":1}".to_string());
        e.ensure_run_content_length();
        assert!(
            !e.headers
                .iter()
                .any(|(k, _, _)| k.eq_ignore_ascii_case("content-length"))
        );
    }

    #[test]
    fn content_length_skipped_when_form_fields_are_present() {
        let mut e = entry("POST");
        e.form_fields = vec![FormField {
            key: "a".to_string(),
            value: "b".to_string(),
            kind: FormFieldKind::Text,
            content_type: None,
            base64_prefix: None,
            enabled: true,
        }];
        e.ensure_run_content_length();
        assert!(
            !e.headers
                .iter()
                .any(|(k, _, _)| k.eq_ignore_ascii_case("content-length"))
        );
    }

    #[test]
    fn a_user_set_content_length_is_not_duplicated() {
        let mut e = entry("POST");
        e.headers
            .push(("content-length".to_string(), "5".to_string(), true));
        e.ensure_run_content_length();
        let count = e
            .headers
            .iter()
            .filter(|(k, _, _)| k.eq_ignore_ascii_case("content-length"))
            .count();
        assert_eq!(count, 1);
    }
}
