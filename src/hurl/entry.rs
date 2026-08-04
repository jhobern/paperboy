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

/// Where a preserved prose comment sits relative to an entry's structural
/// blocks. Comments are anchored to the block they precede (rather than to an
/// absolute line) so they round-trip to roughly the same place even when
/// unrelated lines are added or removed above or below them — e.g. a comment
/// written just before `[Asserts]` stays just before `[Asserts]`. The variants
/// are listed in the order [`HurlEntry::to_hurl`] emits their blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommentAnchor {
    /// Above the entry's title / method line (file- or entry-leading comments).
    Lead,
    /// In the header region, between the request line and the first block.
    Headers,
    BasicAuth,
    Cookies,
    Query,
    Form,
    /// Just before the request `[Options]` section.
    Options,
    Body,
    /// Just before the `HTTP <status>` response line.
    Response,
    /// In the response-header region, between the `HTTP <status>` line and the
    /// first response block (`[Asserts]`/`[Captures]`/response body).
    ResponseHeaders,
    Asserts,
    Captures,
    /// Just before the expected response body.
    ResponseBody,
    /// After every other block (trailing comments).
    Trailing,
}

/// A prose comment recovered from a `.hurl` file that isn't otherwise
/// represented in the model (i.e. not the title, a disabled `# key: value`
/// row, or the `# [Reports]` block). Kept so comments aren't silently dropped
/// on load: [`parse_hurl`](super::parse_hurl) captures them and
/// [`HurlEntry::to_hurl`] re-emits them at their `anchor`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryComment {
    pub anchor: CommentAnchor,
    /// The comment line verbatim, including its leading `#` (e.g. `# note` or a
    /// `#####` banner), so decoration round-trips unchanged.
    pub text: String,
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
    /// Request `[Options]` rows: `(name, value, enabled)` — e.g. `retry: 3`,
    /// `insecure: true`, `variable: host=example.net`. Behavioral per-request
    /// options honoured by the runner (and `hurl_core`). A disabled row
    /// round-trips as a `# name: value` comment, exactly like a disabled
    /// header. `#[serde(default)]` keeps older saved states loadable.
    #[serde(default)]
    pub options: Vec<(String, String, bool)>,
    pub body: Option<String>,
    pub expected_status: Option<u16>,
    /// Expected response HTTP version prefix, e.g. `HTTP/1.1`, taken from the
    /// `HTTP/1.1 200` status line. `None` means the version-agnostic `HTTP`
    /// keyword (any version, the common case). Preserved so it round-trips;
    /// not surfaced in the request wizard. `#[serde(default)]` keeps older
    /// saved states loadable.
    #[serde(default)]
    pub response_version: Option<String>,
    /// Expected response headers: `(name, value, enabled)` implicit header
    /// asserts written after the `HTTP <status>` line. Preserved so they
    /// round-trip (and are checked by the runner); not surfaced in the
    /// wizard. `#[serde(default)]` keeps older saved states loadable.
    #[serde(default)]
    pub response_headers: Vec<(String, String, bool)>,
    /// Expected response body (the implicit body assert that follows the
    /// response sections). Preserved so it round-trips; not surfaced in the
    /// wizard. `#[serde(default)]` keeps older saved states loadable.
    #[serde(default)]
    pub response_body: Option<String>,
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
    /// Prose comments recovered from a loaded `.hurl` file that aren't captured
    /// by any other field (banners, notes, comments between blocks). Each is
    /// anchored to the block it precedes (see [`CommentAnchor`]) so it
    /// round-trips near its original place through
    /// [`to_hurl`](HurlEntry::to_hurl). `#[serde(default)]` keeps older saved
    /// requests (which had no comments field) loadable.
    #[serde(default)]
    pub comments: Vec<EntryComment>,
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

    /// The key of the first enabled `[Form]`/`[Multipart]` file field with an
    /// empty path, if any. Such a field serializes to an invalid `file,;` line
    /// that PaperBoy's own parser rejects, so an entry carrying one can't be
    /// written to a reloadable `.hurl`. (Disabled rows round-trip as comments,
    /// so they never break parsing — only enabled ones are checked.)
    pub fn first_empty_file_field(&self) -> Option<&str> {
        self.form_fields
            .iter()
            .find(|f| f.enabled && f.kind.is_multipart() && f.value.trim().is_empty())
            .map(|f| f.key.as_str())
    }

    /// Serialize this entry to Hurl text. Ordered so it round-trips through
    /// [`parse_hurl`](super::parse_hurl): a body (which must be JSON/quoted to
    /// be re-detected) is emitted right after the headers; request sections and
    /// the response line follow. In practice an entry has either a body or
    /// request sections.
    pub fn to_hurl(&self) -> String {
        use CommentAnchor::*;
        let mut out = String::new();
        // Emit every preserved comment anchored to `anchor`, in stored order.
        let push_comments = |out: &mut String, anchor: CommentAnchor| {
            for c in self.comments.iter().filter(|c| c.anchor == anchor) {
                out.push_str(&c.text);
                out.push('\n');
            }
        };
        push_comments(&mut out, Lead);
        // File-leading comments are separated from the title/method by a blank
        // line so they aren't re-absorbed into the title on the next load (the
        // title is the *contiguous* comment block directly above the method).
        if self.comments.iter().any(|c| c.anchor == Lead) {
            out.push('\n');
        }
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
        push_comments(&mut out, Headers);
        for (k, v, enabled) in &self.headers {
            push_kv_line(&mut out, k, v, *enabled);
        }
        push_comments(&mut out, BasicAuth);
        if let Some((user, pass)) = &self.basic_auth {
            out.push_str(&format!("[BasicAuth]\n{user}: {pass}\n"));
        }
        push_comments(&mut out, Cookies);
        if !self.cookies.is_empty() {
            out.push_str("[Cookies]\n");
            for (k, v, enabled) in &self.cookies {
                push_kv_line(&mut out, k, v, *enabled);
            }
        }
        push_comments(&mut out, Query);
        if !self.queries.is_empty() {
            out.push_str("[Query]\n");
            for (k, v, enabled) in &self.queries {
                push_kv_line(&mut out, k, v, *enabled);
            }
        }
        push_comments(&mut out, Form);
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
        push_comments(&mut out, Options);
        if !self.options.is_empty() {
            out.push_str("[Options]\n");
            for (k, v, enabled) in &self.options {
                push_kv_line(&mut out, k, v, *enabled);
            }
        }
        // The request body must follow every request `[Section]`: `hurl_core`
        // parses a request as headers → sections → body and stops at the body,
        // so a section emitted *after* the body would be read as the start of
        // the next entry (a parse error). Keeping the body last here lets a
        // request carry both a body and, say, `[Options]` and still round-trip.
        push_comments(&mut out, Body);
        if let Some(body) = &self.body {
            out.push_str(body);
            if !body.ends_with('\n') {
                out.push('\n');
            }
        }
        push_comments(&mut out, Response);
        // Response-side comments (anywhere from the `HTTP` line down to the
        // expected body) force the `HTTP` line below so they stay in the
        // response area instead of drifting up into the request's sections,
        // where a `# k: v`-shaped comment could be re-read as a disabled row on
        // the next load.
        let has_response_comments = self.comments.iter().any(|c| {
            matches!(
                c.anchor,
                Response | ResponseHeaders | Asserts | Captures | ResponseBody
            )
        });
        // The `HTTP` line also carries the expected version (`HTTP/1.1`, …); an
        // absent version is the version-agnostic `HTTP` keyword. It's emitted
        // whenever there's *any* response-side content so headers, an expected
        // body, an explicit version, asserts, captures, the reports block or a
        // preserved comment all stay in — and are re-parsed from — the response
        // area rather than being dropped or drifting into the request sections.
        let version = self.response_version.as_deref().unwrap_or("HTTP");
        let has_response_area = self.expected_status.is_some()
            || self.response_version.is_some()
            || !self.response_headers.is_empty()
            || self.response_body.is_some()
            || !self.asserts.is_empty()
            || !self.captures.is_empty()
            || !self.reports.is_empty()
            || has_response_comments;
        if let Some(status) = self.expected_status {
            out.push_str(&format!("{version} {status}\n"));
        } else if has_response_area {
            out.push_str(&format!("{version} *\n"));
        }
        push_comments(&mut out, ResponseHeaders);
        for (k, v, enabled) in &self.response_headers {
            push_kv_line(&mut out, k, v, *enabled);
        }
        push_comments(&mut out, Asserts);
        if !self.asserts.is_empty() {
            out.push_str("[Asserts]\n");
            for a in &self.asserts {
                out.push_str(a);
                out.push('\n');
            }
        }
        push_comments(&mut out, Captures);
        if !self.captures.is_empty() {
            out.push_str("[Captures]\n");
            for (name, expr) in &self.captures {
                out.push_str(&format!("{name}: {expr}\n"));
            }
        }
        // The expected response body follows the response sections, mirroring
        // the request body's placement after the request sections (`hurl_core`
        // parses a response as headers → sections → body).
        push_comments(&mut out, ResponseBody);
        if let Some(body) = &self.response_body {
            out.push_str(body);
            if !body.ends_with('\n') {
                out.push('\n');
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
        push_comments(&mut out, Trailing);
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

/// Recognise a `status == <code>` assert expression and return the numeric
/// status code it checks.
///
/// The Hurl `HTTP <code>` response line and a `status == <code>` assert are
/// equivalent — both fail the entry unless the response status matches — and
/// PaperBoy stores that check once, as [`HurlEntry::expected_status`]
/// (serialized back out as the `HTTP <code>` line). This lets the request
/// wizard surface the status expectation as an editable assert row and fold it
/// back into `expected_status` on save.
///
/// Returns `None` for anything that isn't a plain equality on `status`
/// (e.g. `status >= 200`, `status != 404`, or a `jsonpath` assert), which stay
/// as ordinary `[Asserts]` rows. Whitespace around `status` and `==` is
/// tolerated so the row survives light hand-editing.
pub fn status_eq_code(expr: &str) -> Option<u16> {
    let rest = expr.trim().strip_prefix("status")?;
    // Require a boundary after `status` so `status_line`/`statusCode` don't
    // match; the next thing must be the `==` operator (after any whitespace).
    let rest = rest.trim_start().strip_prefix("==")?;
    rest.trim().parse::<u16>().ok()
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
