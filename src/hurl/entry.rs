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
    pub headers: Vec<(String, String)>,
    pub basic_auth: Option<(String, String)>,
    /// `[Form]` (all `Text`) or `[Multipart]` (any `File`) fields, chosen
    /// automatically by [`to_hurl`](HurlEntry::to_hurl). `#[serde(default)]`
    /// keeps older saved requests (which had a plain `form_params` shape)
    /// loadable — they simply start with no form fields.
    #[serde(default)]
    pub form_fields: Vec<FormField>,
    pub query_params: Vec<(String, String)>,
    /// `[Cookies]` `(name, value)` pairs — syntactic sugar over a `Cookie:`
    /// header. `#[serde(default)]` keeps older saved states loadable.
    #[serde(default)]
    pub cookies: Vec<(String, String)>,
    pub body: Option<String>,
    pub expected_status: Option<u16>,
    /// (variable_name, query_expression) pairs, e.g. `("token", "jsonpath \"$.token\"")`.
    pub captures: Vec<(String, String)>,
    /// Raw `[Asserts]` expressions (e.g. `jsonpath "$.x" == "y"`), kept for
    /// display and round-tripping. `#[serde(default)]` keeps older saved
    /// requests (which had no asserts field) loadable.
    #[serde(default)]
    pub asserts: Vec<String>,
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
    /// `(key, value)` pairs; pairs with an empty key are skipped. An empty
    /// `body` becomes `None`.
    pub fn from_fields(
        name: &str,
        method: &str,
        url: &str,
        headers: Vec<(String, String)>,
        body: &str,
    ) -> Self {
        let headers = headers
            .into_iter()
            .filter(|(k, _)| !k.trim().is_empty())
            .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
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
        for (k, v) in &self.headers {
            out.push_str(&format!("{k}: {v}\n"));
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
            for (k, v) in &self.cookies {
                out.push_str(&format!("{k}: {v}\n"));
            }
        }
        if !self.query_params.is_empty() {
            out.push_str("[Query]\n");
            for (k, v) in &self.query_params {
                out.push_str(&format!("{k}: {v}\n"));
            }
        }
        if !self.form_fields.is_empty() {
            // Any File field switches the whole section to `[Multipart]`
            // (Hurl's `[Form]` section is text-only); a Base64File also
            // serializes as a `file,...` line (carrying its marker), so it
            // forces `[Multipart]` too. Plain Text-only fields stay `[Form]`.
            let multipart = self
                .form_fields
                .iter()
                .any(|f| matches!(f.kind, FormFieldKind::File | FormFieldKind::Base64File));
            out.push_str(if multipart {
                "[Multipart]\n"
            } else {
                "[Form]\n"
            });
            for f in &self.form_fields {
                match f.kind {
                    FormFieldKind::Text => out.push_str(&format!("{}: {}\n", f.key, f.value)),
                    FormFieldKind::File => {
                        let path = escape_form_file_path(&f.value);
                        match f.content_type.as_deref().map(str::trim) {
                            Some(ct) if !ct.is_empty() => {
                                out.push_str(&format!("{}: file,{}; {}\n", f.key, path, ct));
                            }
                            _ => out.push_str(&format!("{}: file,{};\n", f.key, path)),
                        }
                    }
                    // A Base64File is transformed into a plain Text field
                    // before an actual request runs (see
                    // `expand_base64_form_fields`); this branch only runs when
                    // serializing for *saving* to disk. Encode it as a file
                    // line whose content-type carries a PaperBoy marker plus
                    // the URL-safe-base64 encoded prefix, so parsing restores
                    // the Base64File kind and its prefix.
                    FormFieldKind::Base64File => {
                        let path = escape_form_file_path(&f.value);
                        let encoded_prefix = URL_SAFE_NO_PAD
                            .encode(f.base64_prefix.as_deref().unwrap_or("").as_bytes());
                        out.push_str(&format!(
                            "{}: file,{}; {}{}\n",
                            f.key, path, BASE64_FILE_CT_MARKER, encoded_prefix
                        ));
                    }
                }
            }
        }
        // The response section (delimited by the `HTTP <status>` line) is only
        // needed to carry asserts/captures; use the wildcard `HTTP *` (any
        // status, per the Hurl spec) when no explicit status was set so those
        // sections still round-trip through parsing instead of being dropped.
        if let Some(status) = self.expected_status {
            out.push_str(&format!("HTTP {status}\n"));
        } else if !self.asserts.is_empty() || !self.captures.is_empty() {
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
