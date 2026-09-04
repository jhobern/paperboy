//! The `HurlEntry` request model and its Hurl-text serializer.

use std::borrow::Cow;

use serde::{Deserialize, Serialize};

use super::json_comments;

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
    /// Read leniently: a kind invented by a newer build must cost this one
    /// field, not the whole saved session. See [`crate::persistence::lenient`].
    #[serde(default, deserialize_with = "crate::persistence::lenient")]
    pub kind: FormFieldKind,
    pub content_type: Option<String>,
    #[serde(default)]
    pub base64_prefix: Option<String>,
    pub enabled: bool,
    /// A note about this field, round-tripped as a `# @desc …` line above it
    /// exactly as for [`KvRow`]. `#[serde(default)]` keeps older saved states
    /// loadable.
    #[serde(default)]
    pub desc: String,
}

/// One row of a `[Header]`/`[Cookies]`/`[Query]`/`[Options]` section:
/// `key: value`, whether it is sent, and a free-text note about it.
///
/// The note is PaperBoy's own; Hurl has no concept of a per-row description.
/// It round-trips as a `# @desc …` comment line directly above the row (see
/// [`DESC_MARKER`]) — the same comment-encoding trick already used for entry
/// titles, disabled rows and the `# [Reports]` block, so a file with notes is
/// still a valid Hurl file that any other tool will run unchanged.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct KvRow {
    pub key: String,
    pub value: String,
    pub enabled: bool,
    /// A note about this row. Empty means "no note", which is by far the
    /// common case and emits nothing.
    pub desc: String,
}

impl KvRow {
    /// An enabled, undescribed row — the shape almost every caller wants.
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
            enabled: true,
            desc: String::new(),
        }
    }

    /// A row with an explicit enabled flag and no note.
    pub fn toggled(key: impl Into<String>, value: impl Into<String>, enabled: bool) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
            enabled,
            desc: String::new(),
        }
    }
}

impl From<(String, String, bool)> for KvRow {
    fn from((key, value, enabled): (String, String, bool)) -> Self {
        Self {
            key,
            value,
            enabled,
            desc: String::new(),
        }
    }
}

/// Compare a row against a bare `(key, value, enabled)` triple, as the many
/// round-trip tests written before rows had descriptions do.
///
/// A described row is deliberately *not* equal to a triple: were the note
/// ignored here, a test asserting the parsed rows would silently pass while a
/// description leaked onto the wrong row. Tests that expect a note assert on
/// [`KvRow::desc`] directly.
#[cfg(test)]
impl PartialEq<(String, String, bool)> for KvRow {
    fn eq(&self, (key, value, enabled): &(String, String, bool)) -> bool {
        self.key == *key && self.value == *value && self.enabled == *enabled && self.desc.is_empty()
    }
}

/// Saved-state shape for a [`KvRow`].
///
/// Sessions written before rows had descriptions stored them as a plain
/// `["key", "value", true]` array, so the untagged `Legacy` arm keeps every
/// existing `state.json` (and any hand-written one) loadable — it simply comes
/// back with no note. New state is always written in the `Full` form.
#[derive(Deserialize)]
#[serde(untagged)]
enum KvRowRepr {
    Full {
        key: String,
        value: String,
        #[serde(default = "enabled_by_default")]
        enabled: bool,
        #[serde(default)]
        desc: String,
    },
    Legacy(String, String, bool),
}

fn enabled_by_default() -> bool {
    true
}

impl<'de> Deserialize<'de> for KvRow {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(match KvRowRepr::deserialize(d)? {
            KvRowRepr::Full {
                key,
                value,
                enabled,
                desc,
            } => KvRow {
                key,
                value,
                enabled,
                desc,
            },
            KvRowRepr::Legacy(key, value, enabled) => KvRow {
                key,
                value,
                enabled,
                desc: String::new(),
            },
        })
    }
}

/// The comment marker that carries a row's description in Hurl source. Chosen
/// to look like an annotation rather than prose so the parser can tell the two
/// apart and the comment scanner doesn't also capture it as a stray comment.
pub(crate) const DESC_MARKER: &str = "# @desc ";

/// The characters Hurl can carry *inside* a request row's name, outside of a
/// `{{…}}` template.
///
/// Deliberately the same set [`split_kv`](super::parser::split_kv) accepts when
/// reading a row back, because the two must agree: a name the writer emits but
/// the reader won't take is a row that vanishes on the next load, and a name
/// the reader would take but Hurl won't parse is a collection that fails to
/// load at all. Anything outside the set — `:` `#` `"` `\` `;` `,`, whitespace
/// — either ends the name early, starts a comment, or breaks the line in two.
///
/// Non-ASCII letters and digits are allowed (Hurl accepts `X-Ké`), as are the
/// square brackets real APIs use for nested parameters: `filter[name]`. Braces
/// are *not* in this set — Hurl only accepts them as a matched `{{…}}` pair,
/// which [`key_problem`] handles separately.
pub(crate) fn key_char_allowed(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | '[' | ']' | '$')
}

/// The narrower set a name may *start* with: a leading `[` opens a Hurl
/// section, so `[Body]: v` is read as a malformed section header rather than a
/// row — and a file that doesn't parse yields no requests at all.
pub(crate) fn key_start_allowed(c: char) -> bool {
    key_char_allowed(c) && c != '['
}

/// Why a Header/Cookie/Query/Options/Form row's name can't be written to a
/// Hurl file, if it can't. `None` means the name is fine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyProblem {
    /// No name at all — the line would begin with its own `:`.
    Empty,
    /// A leading `[`, which Hurl reads as the start of a section.
    LeadingBracket,
    /// A character Hurl's row grammar can't carry in a name.
    Char(char),
}

/// The first reason `key` can't be a row name, if there is one.
///
/// A `{{…}}` template is skipped whole: a name may be, contain, or be built
/// from variable references (`{{VAR}}`, `X-{{Tenant}}-Id`), and their contents
/// are the substitution's business, not the row grammar's. A *lone* brace is
/// refused, because Hurl fails the whole file on one (`A}B: v` doesn't parse).
pub fn key_problem(key: &str) -> Option<KeyProblem> {
    let key = key.trim();
    let Some(first) = key.chars().next() else {
        return Some(KeyProblem::Empty);
    };
    if first == '[' {
        return Some(KeyProblem::LeadingBracket);
    }
    let mut rest = key;
    let mut at_start = true;
    while let Some(c) = rest.chars().next() {
        if let Some(after) = rest.strip_prefix("{{") {
            // Skip the template whole, including an unterminated one: Hurl
            // rejects `{{` without a closing `}}` too, and reporting the `{`
            // is the clearest thing to say about it either way.
            let Some(end) = after.find("}}") else {
                return Some(KeyProblem::Char('{'));
            };
            rest = &after[end + 2..];
            at_start = false;
            continue;
        }
        let ok = if at_start {
            key_start_allowed(c)
        } else {
            key_char_allowed(c)
        };
        if !ok {
            return Some(KeyProblem::Char(c));
        }
        rest = &rest[c.len_utf8()..];
        at_start = false;
    }
    None
}

/// The first character a row's *value* can't carry, if any.
///
/// Values are far more permissive than names — `#`, `:`, `[` and quotes are all
/// fine — but three characters still break the file: a newline ends the row
/// mid-way and the remainder is parsed as Hurl, a tab isn't accepted in the
/// value grammar at all, and a lone backslash starts an escape sequence.
///
/// Escaping these on the way out would be the nicer fix, but the reader stores
/// a value as its raw source text rather than unescaping it, so a writer-only
/// escape would double the backslashes on every save/load cycle. Until the two
/// are changed together, such a value is refused.
pub fn value_problem(value: &str) -> Option<char> {
    value
        .chars()
        .find(|c| matches!(c, '\n' | '\r' | '\t' | '\\'))
}

/// What Hurl will make of a `{{…}}` placeholder — which is not always what
/// PaperBoy makes of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaceholderProblem {
    /// Hurl reads a name off the front and silently drops the rest. Carries the
    /// placeholder as written and the name Hurl actually resolves.
    Truncated { written: String, read: String },
    /// Hurl can't read a name at all, so the whole file fails to parse and the
    /// collection loads as nothing.
    Unparsable { written: String },
}

/// The first character Hurl will carry in a `{{name}}`, per `hurl_core`'s
/// `parser::expr::variable_name`: alphanumeric (Unicode — `{{Ké}}` is fine),
/// `_` or `-`. Notably *not* `.`, and not `$`.
fn hurl_name_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-'
}

/// Characters that don't truncate a placeholder but end the template early, so
/// the file fails to parse rather than sending the wrong thing.
///
/// `hurl_core`'s `parser::string::any_char` refuses these outright, and `#`
/// additionally closes an unquoted template (it opens a comment). Unquoted is
/// the only kind that matters here: [`for_each_wire_text`] covers URLs and row
/// values, which PaperBoy always writes unquoted. Inside a *quoted* template —
/// an assert, say — `#` is ordinary text, but asserts are stored as raw source
/// and never scanned.
///
/// [`value_problem`] already refuses `\n`, `\r`, `\t` and `\` in a row value,
/// so for values this is a second line of defence; a URL has no such check.
fn hurl_template_stopper(c: char) -> bool {
    matches!(c, '\\' | '\u{8}' | '\n' | '\u{c}' | '\r' | '\t' | '#')
}

/// How Hurl reads the placeholder whose contents (between the braces) are
/// `inner`, when that differs from how PaperBoy reads it.
///
/// The two really do differ, and dangerously. PaperBoy's own
/// [`PLACEHOLDER`](crate::environment) pattern takes any brace-free text
/// between the braces, so `{{ api.key }}` resolves perfectly well in the
/// request preview. Hurl's grammar is much narrower — a name is a run of
/// alphanumerics, `_` and `-` — and `hurl_core`'s `templatize` parses the
/// expression out of the placeholder *without checking that it consumed all of
/// it*, discarding the remainder without a word. `{{ api.key }}` therefore goes
/// on the wire as the value of `api`, and `{{ api.key }}` in the preview beside
/// it says everything is fine.
///
/// So this is checked at the point of use rather than left to Hurl: a truncated
/// placeholder makes a request *succeed* having sent the wrong thing, which is
/// the same reason [`HurlEntry::body_form_conflict`] blocks a run instead of
/// warning about it. A name Hurl can't read at all is caught here too, because
/// the failure it causes is "the collection is empty" several steps away from
/// the line responsible.
///
/// Hurl's own two placeholder functions, `{{ newUuid }}` and `{{ newDate }}`,
/// are ordinary names to this test and pass it unchanged.
pub fn placeholder_problem(inner: &str) -> Option<PlaceholderProblem> {
    let written = format!("{{{{{inner}}}}}");
    // Hurl skips spaces on both sides of the name. Not tabs: `zero_or_more_spaces`
    // would take one, but a tab never survives long enough to reach it — it ends
    // the template first (see `hurl_template_stopper`).
    let body = inner.trim_matches(' ');
    if body.chars().any(hurl_template_stopper) {
        return Some(PlaceholderProblem::Unparsable { written });
    }
    let name: String = body.chars().take_while(|c| hurl_name_char(*c)).collect();
    if name.is_empty() {
        return Some(PlaceholderProblem::Unparsable { written });
    }
    if name.len() == body.len() {
        return None;
    }
    Some(PlaceholderProblem::Truncated {
        written,
        read: name,
    })
}

/// Every placeholder in `text` that Hurl would read differently from PaperBoy,
/// in the order written.
///
/// The scan mirrors `hurl_core`'s `templatize`: a placeholder opens at `{{` and
/// closes at the first following `}}`, and anything in between is its contents.
pub fn placeholder_problems(text: &str) -> Vec<PlaceholderProblem> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find("{{") {
        let after = &rest[open + 2..];
        let Some(close) = after.find("}}") else {
            // An unterminated `{{` is a parse error in Hurl, but `key_problem`
            // already reports that one against the row it appears in; saying it
            // twice, in two vocabularies, would be worse than saying it once.
            break;
        };
        if let Some(p) = placeholder_problem(&after[..close]) {
            out.push(p);
        }
        rest = &after[close + 2..];
    }
    out
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
        // A commented row has to stay on one line. A newline inside it would
        // end the comment, and everything after it would be read as Hurl —
        // which is how a disabled row carrying a multi-line value used to
        // break the file just as thoroughly as an enabled one.
        out.push_str(&line.replace(['\n', '\r'], " "));
        out.push('\n');
        return;
    }
    out.push_str(line);
    out.push('\n');
}

/// Append one `key: value` request-section row to `out` (see [`push_line`] for
/// the disabled-row handling). Shared by the Header, Cookies and Query
/// sections, which are otherwise identical.
fn push_kv_line(out: &mut String, row: &KvRow) {
    push_desc(out, &row.desc);
    // A row Hurl can't carry is emitted commented even when enabled: the line
    // would otherwise cost the user *every* request in the file on reload (see
    // `key_problem`/`value_problem`). Both front-ends refuse such a row at
    // input; this is the backstop for every other producer (Postman import, Raw
    // Mode, a hand-edited file), and it is deliberately lossy-but-visible — the
    // row survives in the text where the user can see and fix it, instead of
    // taking the collection with it.
    let writable = key_problem(&row.key).is_none() && value_problem(&row.value).is_none();
    push_line(
        out,
        &format!("{}: {}", row.key, row.value),
        row.enabled && writable,
    );
}

/// Emit a row's description as the `# @desc …` line above it, if it has one.
/// A multi-line note becomes one marker line per line, so re-reading it
/// reassembles the original text rather than swallowing the continuation as
/// prose.
fn push_desc(out: &mut String, desc: &str) {
    for line in desc.lines() {
        out.push_str(DESC_MARKER);
        out.push_str(line);
        out.push('\n');
    }
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum CommentAnchor {
    /// Above the entry's title / method line (file- or entry-leading comments).
    Lead,
    /// In the header region, between the request line and the first block.
    /// Also where an anchor this build doesn't recognise lands, so a comment
    /// written by a newer version stays with its request instead of failing
    /// the whole state file.
    #[default]
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
    #[serde(default, deserialize_with = "crate::persistence::lenient")]
    pub anchor: CommentAnchor,
    /// The comment line verbatim, including its leading `#` (e.g. `# note` or a
    /// `#####` banner), so decoration round-trips unchanged.
    pub text: String,
}

/// Force a string onto a single line, for the places where the Hurl file
/// format has only one.
///
/// A title is written as a `# ` comment directly above the method line, and a
/// comment ends at the newline. A title containing one therefore didn't stay a
/// title: everything after the newline landed *below* the `#`, where the
/// parser reads it as another request's method line. A Postman collection with
/// a multi-line request name (Postman allows it) imported as a valid-looking
/// file that grew an extra request out of nothing — and "Run All" would send
/// it. Collapsing here means the name loses its line break, visibly, rather
/// than the collection quietly gaining a request.
pub(crate) fn single_line(s: &str) -> String {
    if s.contains(['\n', '\r']) {
        s.replace(['\n', '\r'], " ").trim().to_string()
    } else {
        s.trim().to_string()
    }
}

/// A single request entry from a Hurl file, or a user-created request.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HurlEntry {
    /// Leading comment block describing this entry (its display name).
    pub title: String,
    pub method: String,
    pub url: String,
    pub headers: Vec<KvRow>,
    pub basic_auth: Option<(String, String)>,
    /// `[Form]` (all `Text`) or `[Multipart]` (any `File`) fields, chosen
    /// automatically by [`to_hurl`](HurlEntry::to_hurl). `#[serde(default)]`
    /// keeps older saved requests (which had a plain `form_params` shape)
    /// loadable — they simply start with no form fields.
    #[serde(default)]
    pub form_fields: Vec<FormField>,
    #[serde(default)]
    pub is_multipart: bool,
    pub queries: Vec<KvRow>,
    /// `[Cookies]` `(name, value)` pairs — syntactic sugar over a `Cookie:`
    /// header. `#[serde(default)]` keeps older saved states loadable.
    #[serde(default)]
    pub cookies: Vec<KvRow>,
    /// Request `[Options]` rows: `(name, value, enabled)` — e.g. `retry: 3`,
    /// `insecure: true`, `variable: host=example.net`. Behavioral per-request
    /// options honoured by the runner (and `hurl_core`). A disabled row
    /// round-trips as a `# name: value` comment, exactly like a disabled
    /// header. `#[serde(default)]` keeps older saved states loadable.
    #[serde(default)]
    pub options: Vec<KvRow>,
    /// The request body **as authored** — comments and all, when it is JSON
    /// that carries them. This is what every editor shows and edits; what goes
    /// on the wire and into a `.hurl` file is [`HurlEntry::body_wire`].
    ///
    /// Deliberately one field rather than an authored copy beside a stripped
    /// one: two copies means every writer has to remember to update both, and
    /// the one that forgets produces a body that silently disagrees with the
    /// comments describing it. `#[serde(rename)]` keeps the saved-state key as
    /// `body`, so the rename costs no migration.
    #[serde(rename = "body")]
    pub body_src: Option<String>,
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
    pub response_headers: Vec<KvRow>,
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
    /// The request's source text, held verbatim, when the file it came from
    /// could not be parsed at that point.
    ///
    /// A `.hurl` file that fails to parse used to open as *nothing at all* —
    /// one damaged request made every other request in the file unreachable,
    /// and the damage is often PaperBoy's own doing (a bad merge, a
    /// half-finished hand-edit, an escaping bug). Recovery keeps the requests
    /// that do parse and parks the text of the ones that don't in here, where
    /// it is displayed, written back out unchanged, and can be repaired in Raw
    /// Mode. Nothing is interpreted: an entry carrying this has no meaningful
    /// method, URL or body, so every path that would send it, edit it as
    /// fields, or reason about its parts must check
    /// [`is_unreadable`](HurlEntry::is_unreadable) first.
    ///
    /// `#[serde(default)]` keeps older saved states loadable.
    #[serde(default)]
    pub unparsed: Option<String>,
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

    /// A runtime identity for this entry, used only to tell whether the
    /// collection's entry *list* still matches the one on disk — see
    /// `Collection::structure_baseline`.
    ///
    /// Deliberately not derived from the entry's contents. The obvious
    /// alternative, fingerprinting each entry by its title (or its method and
    /// URL), fails at both ends: a `.hurl` file whose requests carry no `#`
    /// name gives every entry the *same* fingerprint, so reordering two of them
    /// looks like nothing happened, while editing a URL changes a fingerprint
    /// and so looks like the list itself was rearranged. A number that is
    /// stamped once and then just travels with the entry has neither problem —
    /// it survives every edit and is unique across entries.
    ///
    /// Zero means "not stamped yet", which is what a freshly built request has
    /// and what a `#[serde(default)]` read of an older saved state produces;
    /// it reads as "this entry was not in the list that was saved", which is
    /// exactly right for a request the user has just added. Stamps are handed
    /// out by `Collection::reset_structure_baseline` at the moments the list and
    /// the file agree.
    ///
    /// Runtime-only: `#[serde(skip)]` keeps it out of `state.json`, where it
    /// would be meaningless across runs.
    #[serde(skip)]
    pub uid: u64,
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

/// Why a name typed into the "extract to parameter" prompt can't be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParamNameError {
    /// Not a usable `{{name}}` at all (empty, or containing whitespace/braces).
    Invalid,
    /// Already declared by this request, with a *different* default. Reusing it
    /// would silently repoint this value at the other one's default, which is
    /// the sort of change that looks like it worked. Carries the existing
    /// default so the message can say what it would become.
    Conflict(String),
}

/// Whether `name` can be used to extract `value`, or why not.
///
/// Re-using a name the request already declares *with the same value* is not an
/// error — it is the point: two fields that carry the same file should end up
/// reading the same parameter rather than declaring it twice.
pub fn check_parameter_name(
    name: &str,
    value: &str,
    declared: &[(String, String)],
) -> Option<ParamNameError> {
    if !is_variable_name(name.trim()) {
        return Some(ParamNameError::Invalid);
    }
    let name = name.trim();
    match declared.iter().find(|(n, _)| n == name) {
        Some((_, existing)) if existing != value.trim() => {
            Some(ParamNameError::Conflict(existing.clone()))
        }
        _ => None,
    }
}

/// A parameter name to offer for `value`, given what the request already
/// `declared` as `(name, default)` pairs.
///
/// Derived from the value because that is what the user is looking at: a path
/// suggests its file stem (`./samples/example.pdf` ⇒ `EXAMPLE`), anything else
/// suggests itself, upper-cased and with runs of punctuation folded to `_` so
/// the result reads like the `{{SHOUTING}}` every other variable in a Hurl file
/// uses. Only a suggestion — the prompt it fills is fully editable — so it errs
/// towards something short and obviously wrong over something clever.
pub fn suggest_parameter_name(value: &str, declared: &[(String, String)]) -> String {
    let value = value.trim();
    // A declaration already holding this exact value is the answer: extracting
    // the same path from a second field should join the existing parameter, not
    // declare a near-duplicate beside it that then has to be kept in step.
    if let Some((name, _)) = declared.iter().find(|(_, v)| v == value) {
        return name.clone();
    }
    let taken: Vec<&String> = declared.iter().map(|(n, _)| n).collect();
    // A path's identity is its file name, not the directories above it, and the
    // extension is shared by every file it will be swapped for.
    let core = match value.rsplit(['/', '\\']).next() {
        Some(last) if value.contains('/') || value.contains('\\') => {
            last.split('.').next().unwrap_or(last)
        }
        _ => value,
    };
    let mut base = String::new();
    for ch in core.chars() {
        if ch.is_alphanumeric() {
            base.extend(ch.to_uppercase());
        } else if !base.ends_with('_') {
            base.push('_');
        }
    }
    let base = base.trim_matches('_');
    // A name has to survive being written as `{{NAME}}` and read back as an
    // identifier, so a leading digit or an empty result falls back rather than
    // producing something the user has to fix before they can use it.
    let base = if base.is_empty() || base.starts_with(|c: char| c.is_ascii_digit()) {
        "VALUE"
    } else {
        base
    };
    let base: String = base.chars().take(24).collect();
    let base = base.trim_end_matches('_').to_string();
    if !taken.iter().any(|t| **t == base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}_{n}"))
        .find(|c| !taken.iter().any(|t| **t == *c))
        .expect("an unused suffix always exists")
}

/// Whether `name` could be the name of a `{{name}}` variable Hurl will actually
/// resolve: non-empty, and made only of the characters
/// [`hurl_name_char`] accepts.
///
/// This used to be deliberately liberal — anything PaperBoy's own `{{ … }}`
/// pattern would match, on the reasoning that a name working everywhere else in
/// PaperBoy should not be refused here. The reasoning was sound and the premise
/// was wrong: `{{x.y}}` resolves in PaperBoy's preview but reaches the wire as
/// the value of `x`, because Hurl reads a name only as far as the first
/// character outside its own set and silently drops the rest (see
/// [`placeholder_problem`]). Offering such a name here meant helping the user
/// build a request that [`crate::request::truncated_placeholders`] then refuses
/// to send — caught, but a step too late to be useful.
pub fn is_variable_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(hurl_name_char)
}

/// Recognise a `# [Body] <n>` marker and return the line count it claims.
///
/// The count is what makes the block safe to claim: a body is arbitrary text
/// and may perfectly well contain a line reading `[Body]`, so a block closed by
/// an end marker could be terminated early by its own contents. Counting also
/// means a block whose lines have been added to or removed — by a merge, or by
/// hand — fails to validate rather than silently claiming the wrong lines.
pub(crate) fn parse_body_marker(line: &str) -> Option<usize> {
    let rest = line.trim_start().strip_prefix('#')?.trim_start();
    if !rest.get(..6)?.eq_ignore_ascii_case("[body]") {
        return None;
    }
    rest[6..].trim().parse::<usize>().ok()
}

/// Undo one line of `# [Body]` encoding: drop the marker and the single space
/// after it, so the body's own indentation comes back exactly as authored.
pub(crate) fn decode_body_line(line: &str) -> &str {
    let rest = line.trim_start().strip_prefix('#').unwrap_or("");
    rest.strip_prefix(' ').unwrap_or(rest)
}

/// Write an authored body as the `# [Body]` block that carries it through a
/// `.hurl` file.
///
/// Kept next to [`parse_body_marker`] and [`decode_body_line`] deliberately.
/// The two halves drifting apart is not a hypothetical: writing the block with
/// whatever line endings the body happened to have, while the reader threw the
/// `\r` away, made every save produce different bytes than the last.
fn encode_body_block(src: &str) -> String {
    let lines: Vec<&str> = src.split('\n').collect();
    let mut out = format!("# [Body] {}\n", lines.len());
    for l in lines {
        // Written with the line endings the wire body already has: deriving it
        // rebuilds the text line by line and so hands back LF regardless.
        let l = l.strip_suffix('\r').unwrap_or(l);
        // An empty line is bare `#`, so nothing in the file carries trailing
        // whitespace; decoding drops one space after the marker, which is how a
        // body's own indentation survives.
        if l.is_empty() {
            out.push_str("#\n");
        } else {
            out.push_str(&format!("# {l}\n"));
        }
    }
    out
}

impl HurlEntry {
    /// A request recovered from text that could not be parsed: kept verbatim,
    /// shown in the list, and written back out unchanged.
    ///
    /// The title is taken from the first comment line, if there is one, and
    /// otherwise from the first line with anything on it — enough for the user
    /// to recognise which request went wrong without pretending we understood
    /// it.
    pub fn unreadable(text: &str) -> Self {
        let title = text
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map(|l| l.trim_start_matches('#').trim())
            .filter(|l| !l.is_empty())
            .unwrap_or_default()
            .chars()
            .take(80)
            .collect();
        Self {
            title,
            unparsed: Some(text.to_string()),
            ..Default::default()
        }
    }

    /// Whether this is text we could not parse rather than a request we
    /// understand. Sending it, or editing it as fields, is meaningless.
    pub fn is_unreadable(&self) -> bool {
        self.unparsed.is_some()
    }

    /// The body as it goes on the wire and into a `.hurl` file: [`Self::body_src`]
    /// with its JSON comments stripped.
    ///
    /// Every path that sends a request or writes a file must go through here
    /// rather than reading the field, or it will ship the user's notes to their
    /// server. That is the reason the field is named `body_src` and not `body`:
    /// the rename makes the compiler ask each caller which one it meant.
    pub fn body_wire(&self) -> Option<Cow<'_, str>> {
        self.body_src.as_deref().map(json_comments::wire_body)
    }

    /// The leftover `# [Body]` block this request is carrying as prose, if any:
    /// the indices of the comment lines it occupies, and the body text it
    /// describes.
    ///
    /// A block that still reconciles with the body never reaches `comments` at
    /// all — the parser claims it — so anything found here is by definition a
    /// block that no longer describes what the request sends. That is why this
    /// needs no stored state and cannot go out of date: staleness is not
    /// remembered, it is the observable difference between a block the parser
    /// took and one it left behind.
    ///
    /// Nothing is deleted on the user's behalf, so these linger until they say
    /// otherwise. They are also what is left over when a body is deleted
    /// outright — the notes are not part of the body once they have come
    /// unstuck from it, so removing the body cannot take them with it.
    pub fn stale_body_notes(&self) -> Option<(Vec<usize>, String)> {
        let at_body: Vec<usize> = self
            .comments
            .iter()
            .enumerate()
            .filter(|(_, c)| c.anchor == CommentAnchor::Body)
            .map(|(i, _)| i)
            .collect();
        for (pos, &idx) in at_body.iter().enumerate() {
            let Some(n) = parse_body_marker(&self.comments[idx].text) else {
                continue;
            };
            // A marker claiming no lines describes no body; treating it as an
            // empty block would offer to replace a real body with nothing.
            if n == 0 {
                continue;
            }
            // A count that overruns what is left is a damaged marker, not the
            // end of the search: a hand-edited or badly merged block must not
            // hide a well-formed one below it, and the arithmetic must not
            // overflow on a count near the top of the range.
            let Some(end) = pos.checked_add(1).and_then(|s| s.checked_add(n)) else {
                continue;
            };
            let Some(lines) = at_body.get(pos + 1..end) else {
                continue;
            };
            if lines.len() != n {
                continue;
            }
            let text = lines
                .iter()
                .map(|&i| decode_body_line(&self.comments[i].text))
                .collect::<Vec<_>>()
                .join("\n");
            let mut claimed = vec![idx];
            claimed.extend_from_slice(lines);
            return Some((claimed, text));
        }
        None
    }

    /// Whether the leftover notes can be taken back as the body.
    ///
    /// The test is not "do the comments strip cleanly" but "would the result
    /// survive being written to a file and read back". Those are not the same
    /// question: text with no comments in it at all — prose, a stray word, a
    /// half-deleted block — strips to itself and still isn't something Hurl
    /// will accept as a body. Writing it would make the file unparseable, and
    /// an unparseable file reads back as an empty collection, so a single
    /// adopt could take every request in it down. The only honest way to know
    /// is to do the write and read it back, which is what this does.
    pub fn can_adopt_body_notes(&self) -> bool {
        self.adopted_notes()
            .is_some_and(|e| e.survives_being_written())
    }

    /// The entry this one would become if its leftover notes were adopted.
    fn adopted_notes(&self) -> Option<Self> {
        let (at, text) = self.stale_body_notes()?;
        let mut next = self.clone();
        next.body_src = Some(text);
        next.drop_comments(&at);
        Some(next)
    }

    /// Whether writing this entry out and reading it back gives the same
    /// request, rather than a parse error (which the parser reports by
    /// returning no entries at all).
    fn survives_being_written(&self) -> bool {
        let back = crate::hurl::parser::parse_hurl(&self.to_hurl());
        back.len() == 1 && back[0].body_wire() == self.body_wire()
    }

    /// Take the leftover notes back as the body, comments and all.
    ///
    /// This changes what the request sends — to whatever the notes strip down
    /// to — which is exactly why it is a deliberate action and never automatic.
    pub fn adopt_body_notes(&mut self) -> bool {
        let Some(next) = self.adopted_notes() else {
            return false;
        };
        if !next.survives_being_written() {
            return false;
        }
        *self = next;
        true
    }

    /// Throw the leftover notes away, keeping the body as it is.
    pub fn discard_body_notes(&mut self) -> bool {
        let Some((at, _)) = self.stale_body_notes() else {
            return false;
        };
        self.drop_comments(&at);
        true
    }

    fn drop_comments(&mut self, at: &[usize]) {
        let mut i = 0usize;
        self.comments.retain(|_| {
            let keep = !at.contains(&i);
            i += 1;
            keep
        });
    }

    /// Build an entry from user-entered form fields. Rows with a blank key are
    /// dropped and the rest are trimmed. An empty `body` becomes `None`.
    pub fn from_fields(
        name: &str,
        method: &str,
        url: &str,
        headers: Vec<KvRow>,
        body: &str,
    ) -> Self {
        let headers = headers
            .into_iter()
            .filter(|r| !r.key.trim().is_empty())
            .map(|r| KvRow {
                key: r.key.trim().to_string(),
                value: r.value.trim().to_string(),
                enabled: r.enabled,
                desc: r.desc,
            })
            .collect();
        let body = if body.trim().is_empty() {
            None
        } else {
            Some(body.to_string())
        };
        Self {
            title: single_line(name),
            method: method.to_string(),
            url: url.trim().to_string(),
            headers,
            body_src: body,
            ..Default::default()
        }
    }

    /// The request's declared **parameters**: the `variable: NAME=value` rows
    /// of its `[Options]` section, in written order.
    ///
    /// Hurl reads such a row as an assignment that wins over anything the
    /// caller passed in; PaperBoy reads it as a *default* — the value used only
    /// when nobody else binds the name (see
    /// [`crate::request::effective_vars`]). That flip is what lets one request
    /// serve both audiences: opened on its own it runs with the author's sample
    /// value, and driven from a PaperTrail loop (`FOR FILE IN FILES …`) it takes
    /// the loop's value, with neither side editing the other's file. The
    /// alternative — a bare `{{FILE}}` with no default — makes the request
    /// unusable outside the report, which is the state this replaces.
    ///
    /// Rows are matched case-insensitively on the option name (Hurl's own
    /// parser is case-sensitive here, but a request typed by hand in a form
    /// should not fail silently over a capital letter). A row with no `=`, an
    /// empty name, or a name that isn't a plausible variable identifier is
    /// skipped rather than reported: `[Options]` is a free-text grid the user
    /// may be halfway through typing, and refusing to send a request because a
    /// half-written option row exists would be worse than ignoring it.
    /// Disabled rows are skipped like every other disabled row.
    pub fn variable_defaults(&self) -> Vec<(String, String)> {
        self.options
            .iter()
            .filter_map(|r| Self::variable_default(r.enabled, &r.key, &r.value))
            .collect()
    }

    /// The parameter one `[Options]` row declares, if it declares one.
    ///
    /// Split out of [`HurlEntry::variable_defaults`] because the request
    /// editors need to read rows they are still editing — the terminal wizard
    /// holds them as text editors, not as a [`HurlEntry`] — and both must agree
    /// exactly on what counts as a declaration, or a row would read as a
    /// parameter in the form and not be one when sent.
    pub fn variable_default(enabled: bool, key: &str, value: &str) -> Option<(String, String)> {
        if !enabled || !key.trim().eq_ignore_ascii_case("variable") {
            return None;
        }
        let (name, value) = value.split_once('=')?;
        let name = name.trim();
        is_variable_name(name).then(|| (name.to_string(), value.trim().to_string()))
    }

    /// Whether this request declares a parameter named `name`
    /// (case-sensitively, as `{{NAME}}` interpolation is).
    pub fn declares_variable(&self, name: &str) -> bool {
        self.variable_defaults().iter().any(|(n, _)| n == name)
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
        let has_body = self
            .body_wire()
            .as_deref()
            .is_some_and(|b| !b.trim().is_empty())
            || !self.form_fields.is_empty();
        let has_content_length = self
            .headers
            .iter()
            .any(|r| r.key.eq_ignore_ascii_case("content-length"));
        if carries_body && !has_body && !has_content_length && !has_forms {
            self.headers.push(KvRow::new("Content-Length", "0"));
        }
    }

    /// Whether this request carries both a raw body and enabled form fields.
    ///
    /// Hurl builds both onto the same libcurl handle — the form is written with
    /// `post_fields_copy`, then the body overwrites it with a second
    /// `post_fields_copy` (a `[Multipart]` `httppost` is likewise overridden) —
    /// so the form silently never reaches the wire. Worse, Hurl picks the
    /// implicit `Content-Type` from the *form*, so the body goes out labelled
    /// `application/x-www-form-urlencoded` (or `multipart/form-data`) whatever
    /// it actually is. The request "succeeds" and the server is simply sent the
    /// wrong thing, which is the hardest kind of failure to trace back — so a
    /// front-end must treat this as an error rather than a hint.
    ///
    /// Any body at all counts, including one that is nothing but a stray space:
    /// libcurl checks the bytes, not whether they mean anything, so a
    /// whitespace body loses the form just as thoroughly as a real one. Only
    /// *enabled* fields count, since a disabled row is a comment and never
    /// reaches the wire (see the serializer).
    pub fn body_form_conflict(&self) -> bool {
        self.body_src.is_some() && self.form_fields.iter().any(|f| f.enabled)
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
        // An unreadable request is text we never understood, so there is
        // nothing to serialize from — writing it back exactly as it arrived is
        // both the only honest thing to do and what stops a save from turning
        // a damaged file into a lost one.
        if let Some(raw) = &self.unparsed {
            let mut out = raw.trim_end_matches(['\n', '\r']).to_string();
            out.push('\n');
            return out;
        }
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
            out.push_str(&single_line(&self.title));
            out.push('\n');
        }
        let method = if self.method.is_empty() {
            "GET"
        } else {
            self.method.as_str()
        };
        out.push_str(&format!("{method} {}\n", self.url));
        push_comments(&mut out, Headers);
        for row in &self.headers {
            push_kv_line(&mut out, row);
        }
        push_comments(&mut out, BasicAuth);
        if let Some((user, pass)) = &self.basic_auth {
            out.push_str(&format!("[BasicAuth]\n{user}: {pass}\n"));
        }
        push_comments(&mut out, Cookies);
        if !self.cookies.is_empty() {
            out.push_str("[Cookies]\n");
            for row in &self.cookies {
                push_kv_line(&mut out, row);
            }
        }
        push_comments(&mut out, Query);
        if !self.queries.is_empty() {
            out.push_str("[Query]\n");
            for row in &self.queries {
                push_kv_line(&mut out, row);
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
                push_desc(&mut out, &f.desc);
                // Commented when the row would break parsing, as in
                // `push_kv_line`. Only a Text field's value is checked: a
                // File/Base64File value is a path, and `escape_form_file_path`
                // already escapes the backslashes and newlines a path can hold.
                let writable = key_problem(&f.key).is_none()
                    && (f.kind != FormFieldKind::Text || value_problem(&f.value).is_none());
                push_line(&mut out, &form_field_line(f), f.enabled && writable);
            }
        }
        push_comments(&mut out, Options);
        if !self.options.is_empty() {
            out.push_str("[Options]\n");
            for row in &self.options {
                push_kv_line(&mut out, row);
            }
        }
        // The request body must follow every request `[Section]`: `hurl_core`
        // parses a request as headers → sections → body and stops at the body,
        // so a section emitted *after* the body would be read as the start of
        // the next entry (a parse error). Keeping the body last here lets a
        // request carry both a body and, say, `[Options]` and still round-trip.
        push_comments(&mut out, Body);
        // The wire body, never the authored one: what lands in the file has
        // to be strict JSON that every other Hurl runner accepts.
        if let Some(body) = self.body_wire() {
            // A body authored with comments is written twice — once as a
            // `# [Body]` block that Hurl ignores and PaperBoy reads back, and
            // once as the strict JSON that is actually sent. Duplication is the
            // price of the file staying valid: there is nowhere else in a
            // `.hurl` for a comment to live.
            //
            // The block is claimed by a line *count* rather than closed by an
            // end marker, so no line of body content can terminate it early
            // (`[Body]` is legal JSON text) and lines inserted by, say, a merge
            // are detected as damage instead of silently mis-claimed.
            if let Some(src) = self.body_src.as_deref().filter(|s| *s != body.as_ref()) {
                out.push_str(&encode_body_block(src));
            }
            out.push_str(&body);
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
        for row in &self.response_headers {
            push_kv_line(&mut out, row);
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
                .any(|r| r.key == "Content-Length" && r.value == "0")
        );
    }

    #[test]
    fn content_length_added_for_all_body_carrying_methods() {
        for m in ["POST", "PUT", "PATCH", "DELETE", "post", "Put"] {
            let mut e = entry(m);
            e.ensure_run_content_length();
            assert!(
                e.headers.iter().any(|r| r.key == "Content-Length"),
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
                    .any(|r| r.key.eq_ignore_ascii_case("content-length")),
                "did not expect Content-Length for {m}"
            );
        }
    }

    #[test]
    fn content_length_skipped_when_a_body_is_present() {
        let mut e = entry("POST");
        e.body_src = Some("{\"a\":1}".to_string());
        e.ensure_run_content_length();
        assert!(
            !e.headers
                .iter()
                .any(|r| r.key.eq_ignore_ascii_case("content-length"))
        );
    }

    /// libcurl checks the body's *bytes*, not whether they mean anything, so a
    /// body of one stray space replaces the form just as completely as a real
    /// one — which is exactly how someone loses a form without touching it.
    #[test]
    fn even_a_whitespace_body_conflicts_with_form_fields() {
        let mut e = HurlEntry {
            form_fields: vec![FormField {
                key: "grant_type".into(),
                enabled: true,
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(!e.body_form_conflict(), "form fields alone are fine");
        e.body_src = Some(" ".into());
        assert!(e.body_form_conflict(), "a space is still a body");
    }

    /// A disabled row is written out as a comment and never reaches the wire,
    /// so it can't be the thing a body is competing with.
    #[test]
    fn a_disabled_form_field_does_not_conflict_with_a_body() {
        let e = HurlEntry {
            body_src: Some("{}".into()),
            form_fields: vec![FormField {
                key: "grant_type".into(),
                enabled: false,
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(!e.body_form_conflict());
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
            desc: String::new(),
        }];
        e.ensure_run_content_length();
        assert!(
            !e.headers
                .iter()
                .any(|r| r.key.eq_ignore_ascii_case("content-length"))
        );
    }

    #[test]
    fn a_user_set_content_length_is_not_duplicated() {
        let mut e = entry("POST");
        e.headers.push(KvRow::toggled("content-length", "5", true));
        e.ensure_run_content_length();
        let count = e
            .headers
            .iter()
            .filter(|r| r.key.eq_ignore_ascii_case("content-length"))
            .count();
        assert_eq!(count, 1);
    }

    /// `HurlEntry` is serialised verbatim into `~/.config/paperboy/state.json`,
    /// so a state file written before rows grew a description (an array of
    /// `[key, value, enabled]`) must still load.
    #[test]
    fn a_row_saved_before_descriptions_existed_still_loads() {
        let legacy: KvRow = serde_json::from_str(r#"["X-Trace","on",false]"#)
            .expect("the legacy three-element form must still deserialise");
        assert_eq!(legacy.key, "X-Trace");
        assert_eq!(legacy.value, "on");
        assert!(!legacy.enabled);
        assert_eq!(legacy.desc, "", "with no note, of course");
    }

    #[test]
    fn a_described_row_round_trips_through_the_saved_state_format() {
        let row = KvRow {
            key: "X-Trace".into(),
            value: "on".into(),
            enabled: true,
            desc: "staging only".into(),
        };
        let back: KvRow = serde_json::from_str(&serde_json::to_string(&row).unwrap()).unwrap();
        assert_eq!(back.desc, "staging only");
        assert_eq!(back.key, "X-Trace");
    }

    /// A row keyed `[Body]` used to serialize to `[Body]: value`, which Hurl
    /// reads as a section header — and `parse_hurl` yields *no* entries for a
    /// file that doesn't parse, so a single such row silently destroyed every
    /// request in the collection on the next load. The writer now emits it
    /// commented so the file still parses and the row stays visible.
    #[test]
    fn a_bracket_key_never_breaks_the_collection_file() {
        for row_of in [
            (|r| HurlEntry {
                headers: vec![r],
                ..Default::default()
            }) as fn(KvRow) -> HurlEntry,
            |r| HurlEntry {
                cookies: vec![r],
                ..Default::default()
            },
            |r| HurlEntry {
                queries: vec![r],
                ..Default::default()
            },
            |r| HurlEntry {
                options: vec![r],
                ..Default::default()
            },
        ] {
            let mut e = row_of(KvRow::new("[Body]", "value"));
            e.method = "POST".into();
            e.url = "http://h/a".into();
            let text = e.to_hurl();
            assert!(
                text.contains("# [Body]: value"),
                "the row must be commented, got:\n{text}"
            );
            assert_eq!(
                crate::hurl::parse_hurl_error(&text),
                None,
                "must still parse:\n{text}"
            );
            assert_eq!(
                crate::hurl::parse_hurl(&text).len(),
                1,
                "the entry must survive:\n{text}"
            );
        }
    }

    /// The same guard for `[Form]`/`[Multipart]` field names, which share the
    /// section-line grammar.
    #[test]
    fn a_bracket_form_field_key_never_breaks_the_collection_file() {
        let e = HurlEntry {
            method: "POST".into(),
            url: "http://h/a".into(),
            form_fields: vec![FormField {
                key: "[Body]".into(),
                value: "v".into(),
                enabled: true,
                ..Default::default()
            }],
            ..Default::default()
        };
        let text = e.to_hurl();
        assert_eq!(crate::hurl::parse_hurl_error(&text), None, "\n{text}");
        assert_eq!(crate::hurl::parse_hurl(&text).len(), 1, "\n{text}");
    }

    /// One bad row must not take its *neighbours* with it — the regression that
    /// made this severe rather than cosmetic.
    #[test]
    fn a_bracket_key_does_not_lose_the_other_requests_in_the_file() {
        let bad = HurlEntry {
            method: "POST".into(),
            url: "http://h/a".into(),
            headers: vec![KvRow::new("[Body]", "value")],
            ..Default::default()
        };
        let good = HurlEntry {
            method: "GET".into(),
            url: "http://h/b".into(),
            ..Default::default()
        };
        let text = crate::hurl::collection_to_hurl(&[bad, good]);
        assert_eq!(crate::hurl::parse_hurl(&text).len(), 2, "\n{text}");
    }

    /// Only a *leading* bracket is a section-header ambiguity: `filter[name]`
    /// is an ordinary query key and must be left enabled — and, crucially, must
    /// come *back* as an enabled row. An earlier version of this test only
    /// asserted the file parsed, which it did while `split_kv` silently dropped
    /// the row on reload.
    #[test]
    fn a_bracket_inside_a_key_is_still_a_normal_row() {
        assert_eq!(key_problem("filter[name]"), None);
        assert_eq!(key_problem("[Body]"), Some(KeyProblem::LeadingBracket));
        assert_eq!(key_problem("  [Options]"), Some(KeyProblem::LeadingBracket));
        let e = HurlEntry {
            method: "GET".into(),
            url: "http://h/a".into(),
            queries: vec![KvRow::new("filter[name]", "x")],
            ..Default::default()
        };
        let text = e.to_hurl();
        assert!(text.contains("\nfilter[name]: x"), "\n{text}");
        assert_eq!(crate::hurl::parse_hurl_error(&text), None, "\n{text}");
        let back = crate::hurl::parse_hurl(&text);
        assert_eq!(
            back[0].queries,
            vec![("filter[name]".into(), "x".into(), true)]
        );
    }

    /// Names real APIs use that Hurl accepts but PaperBoy's reader used to
    /// refuse, silently deleting the row on the next load. The writer/reader
    /// pair must round-trip every one of them.
    #[test]
    fn permissive_key_shapes_round_trip() {
        for key in [
            "filter[name]",
            "{{VAR}}",
            "X-Ké",
            "-X-A",
            "_X_A",
            "$X",
            "X.A",
            "X-A]B",
            "X-{{Tenant}}-Id",
        ] {
            assert_eq!(key_problem(key), None, "{key} should be writable");
            let e = HurlEntry {
                method: "GET".into(),
                url: "http://h/a".into(),
                headers: vec![KvRow::new(key, "v")],
                ..Default::default()
            };
            let text = e.to_hurl();
            assert_eq!(crate::hurl::parse_hurl_error(&text), None, "{key}\n{text}");
            let back = crate::hurl::parse_hurl(&text);
            assert_eq!(
                back.first().map(|e| e.headers.clone()).unwrap_or_default(),
                vec![(key.to_string(), "v".to_string(), true)],
                "{key} must survive a save/load round trip:\n{text}"
            );
        }
    }

    /// The names that genuinely can't be carried. Each one either ends the name
    /// early, opens a comment, or splits the line in two, so the writer has to
    /// refuse them rather than emit a line the reader can't take back.
    #[test]
    fn unwritable_key_shapes_are_refused() {
        for key in [
            "X-A:B", "X-A#B", "X\"A", "X\\A", "X;A", "X,A", "X A", "X\tA", "X\nY", "X/A", "X@A",
            "X(A", "A}B", "A{B", "{V}", "X-{{V",
        ] {
            assert!(
                matches!(key_problem(key), Some(KeyProblem::Char(_))),
                "{key:?} must be refused"
            );
        }
        assert_eq!(key_problem(""), Some(KeyProblem::Empty));
        assert_eq!(key_problem("   "), Some(KeyProblem::Empty));
    }

    /// Values are far more permissive than names, and over-refusing them would
    /// block ordinary request data — a JSON blob, a URL with a fragment, a
    /// variable reference. Only the three characters that actually break the
    /// line are refused.
    #[test]
    fn only_line_breaking_characters_are_refused_in_a_value() {
        for value in [
            "plain",
            "a: b",
            "a # b",
            "[bracketed]",
            "{{VAR}}",
            "\"quoted\"",
            "a;b,c",
            "",
        ] {
            assert_eq!(value_problem(value), None, "{value:?} should be writable");
        }
        assert_eq!(value_problem("a\nb"), Some('\n'));
        assert_eq!(value_problem("a\tb"), Some('\t'));
        assert_eq!(value_problem("a\\b"), Some('\\'));
    }

    /// The writer/reader invariant, stated directly: for every row the writer
    /// accepts, the file parses *and* the row comes back byte-identical. This
    /// is the property that makes the guard trustworthy — the individual cases
    /// above are only the interesting samples of it.
    #[test]
    fn every_writable_row_survives_a_round_trip() {
        let keys = [
            "X-A",
            "filter[name]",
            "{{V}}",
            "X-Ké",
            "$X",
            "_A",
            "-A",
            "A.B",
            "A{{V}}B",
            "9",
        ];
        let values = ["v", "a: b", "a # b", "{{V}}", "[x]", "a;b", "\"q\""];
        for key in keys {
            for value in values {
                assert_eq!(key_problem(key), None, "{key:?}");
                assert_eq!(value_problem(value), None, "{value:?}");
                let e = HurlEntry {
                    method: "GET".into(),
                    url: "http://h/a".into(),
                    headers: vec![KvRow::new(key, value)],
                    ..Default::default()
                };
                let text = e.to_hurl();
                assert_eq!(
                    crate::hurl::parse_hurl_error(&text),
                    None,
                    "{key:?} / {value:?}\n{text}"
                );
                let back = crate::hurl::parse_hurl(&text);
                assert_eq!(
                    back.first().map(|e| e.headers.clone()).unwrap_or_default(),
                    vec![(key.to_string(), value.to_string(), true)],
                    "{key:?} / {value:?} must round-trip:\n{text}"
                );
            }
        }
    }

    /// A *disabled* row goes out as a `#` comment, so a newline in its value
    /// used to escape the comment and leave the tail of the value as a stray
    /// line of Hurl. The writer now folds the line breaks into spaces.
    #[test]
    fn a_disabled_row_with_a_multiline_value_keeps_the_file_parseable() {
        let e = HurlEntry {
            method: "GET".into(),
            url: "http://h/a".into(),
            headers: vec![KvRow {
                key: "X-A".into(),
                value: "one\nGET http://evil/".into(),
                enabled: false,
                desc: String::new(),
            }],
            ..Default::default()
        };
        let text = e.to_hurl();
        assert_eq!(crate::hurl::parse_hurl_error(&text), None, "\n{text}");
        assert_eq!(
            crate::hurl::parse_hurl(&text).len(),
            1,
            "the value must not become a second entry:\n{text}"
        );
    }

    /// The whole point of the rename: whatever the user authored, the file gets
    /// strict JSON. A `.hurl` carrying `//` is one no other Hurl runner will
    /// parse, so this is the invariant everything else rests on.
    #[test]
    fn a_commented_body_is_written_out_as_strict_json() {
        let mut e = HurlEntry {
            title: "t".into(),
            method: "POST".into(),
            url: "http://h/a".into(),
            ..Default::default()
        };
        e.body_src = Some("{\n  // who\n  \"id\": 1 // the caller\n}".into());

        assert_eq!(
            e.body_wire().as_deref(),
            Some("{\n  \"id\": 1\n}"),
            "the wire body has no commentary in it"
        );
        // The notes live on `#` lines, which Hurl ignores. Everything Hurl
        // actually reads must be free of them.
        let text = e.to_hurl();
        let hurl_reads: String = text
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n");
        // (`http://h/a` has its own `//`, so this looks for the notes.)
        assert!(
            !hurl_reads.contains("// who") && !hurl_reads.contains("// the caller"),
            "\n{hurl_reads}"
        );
        assert!(hurl_reads.contains("\"id\": 1"), "\n{text}");
        assert!(text.contains("#   // who"), "the note is kept:\n{text}");
    }

    /// A body that was never JSON keeps its slashes — stripping there would
    /// truncate real content at the first `//`.
    #[test]
    fn a_body_that_is_not_json_is_written_out_untouched() {
        let mut e = HurlEntry::default();
        e.body_src = Some("query { user // not a comment\n}".into());
        assert_eq!(e.body_wire().as_deref(), e.body_src.as_deref());
    }

    /// The field was renamed but the saved-state key must not be, or every
    /// existing `state.json` would come back with no bodies at all.
    #[test]
    fn renaming_the_field_did_not_rename_it_on_disk() {
        let mut e = HurlEntry::default();
        e.body_src = Some("{}".into());
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains("\"body\":\"{}\""), "{json}");

        let back: HurlEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back.body_src.as_deref(), Some("{}"));
    }

    /// Regression: a request *name* containing a newline used to serialize
    /// into a second line below the `# ` comment, where the parser reads it as
    /// another request's method line — so the collection silently grew an
    /// extra, runnable request, and "Run All" would send it. Postman allows
    /// multi-line names, so importing one was enough to trigger it.
    #[test]
    fn a_name_with_a_newline_cannot_invent_a_second_request() {
        let e = HurlEntry::from_fields(
            "line one\nGET http://evil.example.net",
            "POST",
            "https://x/y",
            vec![],
            "",
        );
        let text = e.to_hurl();
        let back = crate::hurl::parse_hurl(&text);
        assert_eq!(back.len(), 1, "one request in, one request out:\n{text}");
        assert_eq!(back[0].url, "https://x/y");
        assert_eq!(back[0].method, "POST");
        assert_eq!(
            back[0].title, "line one GET http://evil.example.net",
            "the name is kept, just flattened onto its one line"
        );
    }

    /// The writer defends the file even when the title was set directly rather
    /// than through `from_fields` — e.g. renamed in the UI with a pasted value.
    #[test]
    fn a_pasted_multi_line_title_still_serializes_to_one_comment() {
        let mut e = HurlEntry::from_fields("ok", "GET", "https://x/y", vec![], "");
        e.title = "first\r\nDELETE https://x/z".to_string();
        let back = crate::hurl::parse_hurl(&e.to_hurl());
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].method, "GET");
    }
}

#[cfg(test)]
mod placeholder_tests {
    use super::*;

    /// What `hurl_core` *actually* does with `{{inner}}`, discovered by asking
    /// it, rather than by reading its grammar and hoping. Returns the variable
    /// name Hurl resolves, or `None` if the file doesn't parse.
    ///
    /// This is the whole point of the test below: [`placeholder_problem`]
    /// encodes a rule about someone else's parser, so it is checked against
    /// that parser. If a future Hurl tightens the grammar (or fixes the silent
    /// truncation — `templatize` parses the expression out of a placeholder
    /// without checking it consumed all of it, which is arguably a bug
    /// upstream), this test fails and says so, instead of PaperBoy quietly
    /// refusing placeholders that had become perfectly valid.
    fn hurl_reads(inner: &str) -> Option<String> {
        let text = format!("GET http://h/{{{{{inner}}}}}\n");
        let file = hurl_core::parser::parse_hurl_file(&text).ok()?;
        let url = file.entries[0].request.url.to_string();
        let start = url.find("{{")?;
        let rest = &url[start + 2..];
        let end = rest.find("}}")?;
        Some(rest[..end].to_string())
    }

    #[test]
    fn the_truncation_rule_matches_what_hurl_actually_does() {
        // Fine: Hurl reads the whole name.
        for inner in [
            "TOKEN", " TOKEN ", "api_key", "api-key", "x1", "Ké",
            // Hurl's own two placeholder functions are ordinary names to this
            // test, and must not be flagged.
            "newUuid", "newDate",
        ] {
            assert_eq!(
                placeholder_problem(inner),
                None,
                "{inner:?} should be clean"
            );
            assert_eq!(
                hurl_reads(inner).as_deref(),
                Some(inner.trim_matches([' ', '\t'])),
                "hurl disagrees about {inner:?}"
            );
        }

        // Truncated: Hurl takes a prefix and discards the rest in silence.
        // `api.key` is the one that matters — PaperBoy's own substitution
        // resolves it, so preview and wire disagree with nothing to show for it.
        for (inner, read) in [
            ("api.key", "api"),
            (" api.key ", "api"),
            ("gen.uuid", "gen"),
            ("a b", "a"),
            ("hmac(k, m)", "hmac"),
            ("TOKEN!", "TOKEN"),
        ] {
            assert_eq!(
                placeholder_problem(inner),
                Some(PlaceholderProblem::Truncated {
                    written: format!("{{{{{inner}}}}}"),
                    read: read.to_string(),
                }),
                "{inner:?} should be truncated"
            );
            assert_eq!(
                hurl_reads(inner).as_deref(),
                Some(read),
                "hurl disagrees about {inner:?}"
            );
        }

        // Unparsable: either no name at all, or a character that ends the
        // template early (a tab, or the `#` that opens a comment) — so the
        // whole *file* fails to load, which is why this is worth naming at the
        // placeholder rather than leaving as "the collection is empty".
        for inner in ["$guid", "", " ", ".x", "!", "\tTOKEN\t", "TOKEN\t", "a#b"] {
            assert_eq!(
                placeholder_problem(inner),
                Some(PlaceholderProblem::Unparsable {
                    written: format!("{{{{{inner}}}}}"),
                }),
                "{inner:?} should be unparsable"
            );
            assert_eq!(hurl_reads(inner), None, "hurl disagrees about {inner:?}");
        }
    }

    /// The name offered to "extract to parameter" is held to Hurl's rule, not
    /// PaperBoy's looser one. `api.key` reads as a perfectly good variable name
    /// and resolves in the preview, but Hurl sends the value of `api` — so
    /// accepting it here would build a request PaperBoy then refuses to send.
    #[test]
    fn a_parameter_name_hurl_would_truncate_is_not_offered() {
        for good in ["TOKEN", "api_key", "api-key", "x1", "Ké"] {
            assert_eq!(check_parameter_name(good, "v", &[]), None, "{good:?}");
            assert!(is_variable_name(good), "{good:?}");
        }
        for bad in ["api.key", "$guid", "a b", "", "a{b"] {
            assert_eq!(
                check_parameter_name(bad, "v", &[]),
                Some(ParamNameError::Invalid),
                "{bad:?}"
            );
            assert!(!is_variable_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn every_placeholder_in_a_line_is_checked_in_order() {
        let found = placeholder_problems("{{ok}}/{{a.b}}?t={{ok2}}&u={{c.d}}");
        assert_eq!(
            found,
            vec![
                PlaceholderProblem::Truncated {
                    written: "{{a.b}}".into(),
                    read: "a".into()
                },
                PlaceholderProblem::Truncated {
                    written: "{{c.d}}".into(),
                    read: "c".into()
                },
            ]
        );
        assert!(placeholder_problems("nothing templated here").is_empty());
        // An unterminated `{{` is left to `key_problem`, which already reports
        // it against the row it appears in.
        assert!(placeholder_problems("{{ unterminated").is_empty());
    }
}
