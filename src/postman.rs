//! Best-effort import of a Postman collection (v2.1 JSON export) into our
//! [`HurlEntry`] model. The Hurl format doesn't cover every Postman feature
//! (pre-request scripts, …), but the request line, headers, query, body and
//! basic/bearer auth are mapped; Postman `{{var}}` placeholders share Hurl's
//! syntax so they carry over. The schema subset we care about is deserialized
//! into the typed structs below, every field optional/defaulted so partial
//! exports still import and anything unmodelled is ignored.

use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Deserializer};
use serde_json::Value;

use crate::hurl::{
    CommentAnchor, EntryComment, FormField, FormFieldKind, HurlEntry, KvRow, parse_hurl,
};

#[derive(Deserialize, Default)]
#[serde(default)]
struct Collection {
    item: Vec<Item>,
    /// Collection-level variables. Postman resolves `{{name}}` against these
    /// when nothing in the environment defines it, so they are part of what
    /// makes an exported collection runnable — see
    /// [`postman_collection_variables`].
    variable: Vec<Param>,
    /// The collection's default auth, inherited by every request that doesn't
    /// set its own.
    auth: Option<Auth>,
    #[serde(rename = "protocolProfileBehavior")]
    protocol_profile_behavior: Option<Profile>,
}

/// A folder (nested `item`s) or a leaf holding a `request`.
#[derive(Deserialize, Default)]
#[serde(default)]
struct Item {
    #[serde(deserialize_with = "de_str")]
    name: String,
    item: Option<Vec<Item>>,
    request: Option<Request>,
    /// A folder's default auth, overriding the collection's for everything
    /// beneath it. Only meaningful on folders; a leaf's auth lives on its
    /// `request`.
    auth: Option<Auth>,
    /// Pre-request / test scripts. Only `test` scripts are mined for
    /// `pm.<store>.set(...)` capture calls (see [`captures_from_events`]).
    event: Vec<Event>,
    #[serde(rename = "protocolProfileBehavior")]
    protocol_profile_behavior: Option<Profile>,
}

/// Postman's per-request send-time switches. Set at collection, folder or
/// request level, each inherited by everything below it until overridden.
#[derive(Clone, Copy, Default, Deserialize)]
#[serde(default)]
struct Profile {
    /// Send a body even on a method that normally has none. Postman *strips*
    /// the body from a GET unless this is set, so a request stored with both a
    /// GET and a body is not a request with a body — it's a leftover.
    #[serde(rename = "disableBodyPruning")]
    disable_body_pruning: Option<bool>,
    /// `false` means "don't verify the certificate" — Hurl's `insecure`.
    #[serde(rename = "strictSSL")]
    strict_ssl: Option<bool>,
}

impl Profile {
    /// This level's settings over the inherited ones, field by field: Postman
    /// overrides individually rather than replacing the whole block.
    fn over(self, parent: Profile) -> Profile {
        Profile {
            disable_body_pruning: self.disable_body_pruning.or(parent.disable_body_pruning),
            strict_ssl: self.strict_ssl.or(parent.strict_ssl),
        }
    }
}

/// A Postman `event` — a `prerequest` or `test` script attached to an item.
#[derive(Deserialize, Default)]
#[serde(default)]
struct Event {
    #[serde(deserialize_with = "de_str")]
    listen: String,
    script: Script,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Script {
    exec: Vec<String>,
}

#[derive(Deserialize)]
struct Request {
    #[serde(default = "get_method", deserialize_with = "de_method")]
    method: String,
    #[serde(default, deserialize_with = "de_url")]
    url: Url,
    #[serde(default)]
    header: Vec<Param>,
    auth: Option<Auth>,
    body: Option<Body>,
    /// Prose documenting the request. A bare string, or `{"content": …}` with
    /// a media type beside it.
    #[serde(default, deserialize_with = "de_description")]
    description: String,
}

/// A Postman description is a string or a `{"content": …, "type": …}` object;
/// anything else (including an explicit `null`) reads as no description rather
/// than failing the collection.
fn de_description<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::String(s) => s,
        Value::Object(m) => m
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        _ => String::new(),
    })
}

fn get_method() -> String {
    "GET".to_string()
}

/// Like [`de_str`], but a method that reads as blank (a `null`, or a structure
/// we can't stringify) falls back to the same `GET` an absent one does rather
/// than producing a request with no verb.
fn de_method<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let m = de_str(d)?;
    Ok(if m.trim().is_empty() { get_method() } else { m })
}

/// A Postman URL: the text as typed, plus the *path variables* declared for it.
///
/// Postman writes a path placeholder twice — once in `raw` as `/:batch_id`, and
/// once in `variable` as a key/value pair holding the value to substitute. Only
/// reading `raw` imported the colon form literally, so the request went out
/// asking for a batch actually named ":batch_id".
#[derive(Default)]
struct Url {
    raw: String,
    /// The `#fragment` cut off `raw`, kept only so the import can say it went.
    /// A fragment is not sent to the server — Postman doesn't send it either —
    /// and a `#` on a Hurl request line starts a comment, so leaving it in the
    /// URL meant the rest of the line vanished the next time the file was
    /// read, taking any query parameters after it with it.
    fragment: String,
    /// Declared path variables, in declaration order. Empty for the bare-string
    /// form of a URL, which has nowhere to put them.
    variables: Vec<Param>,
    /// The query parameters as Postman lists them. Only the *disabled* ones
    /// matter here — enabled ones are already in `raw` and get parsed out of
    /// it, but a switched-off parameter is left out of `raw` entirely, so it
    /// used to disappear rather than import switched off.
    queries: Vec<Param>,
}

/// Join a Postman URL part that may be a list (`host: ["api","example","com"]`)
/// or already a single string (`host: "api.example.com"`).
fn join_parts(v: Option<&Value>, sep: &str) -> String {
    match v {
        Some(Value::Array(parts)) => parts
            .iter()
            .map(|p| match p {
                Value::String(s) => s.clone(),
                // A path segment can be an object carrying a `:variable`.
                Value::Object(o) => o
                    .get("value")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                other => other.as_str().unwrap_or_default().to_string(),
            })
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>()
            .join(sep),
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    }
}

/// Rebuild a URL from the pieces Postman also stores it in.
///
/// `raw` is normally present and authoritative, but it is only a *cache* of
/// `protocol`/`host`/`path`/`port`; an export written by a script, or an older
/// or hand-edited one, can carry the pieces and no `raw` at all. Reading `raw`
/// alone then produced a request with **no URL whatsoever** — the whole target
/// silently gone — so the pieces are the fallback.
fn url_from_parts(m: &serde_json::Map<String, Value>) -> String {
    let host = join_parts(m.get("host"), ".");
    if host.is_empty() {
        return String::new();
    }
    let mut url = String::new();
    let protocol = m.get("protocol").and_then(Value::as_str).unwrap_or("");
    if !protocol.is_empty() {
        url.push_str(protocol);
        url.push_str("://");
    }
    url.push_str(&host);
    if let Some(port) = m
        .get("port")
        .and_then(Value::as_str)
        .filter(|p| !p.is_empty())
    {
        url.push(':');
        url.push_str(port);
    }
    let path = join_parts(m.get("path"), "/");
    if !path.is_empty() {
        if !path.starts_with('/') {
            url.push('/');
        }
        url.push_str(&path);
    }
    url
}

/// Add any *enabled* query parameter that `raw` doesn't already carry.
///
/// Postman keeps `raw` and `query[]` in step, so normally there is nothing to
/// do and `raw` wins. When they disagree — a stale or scripted export — the
/// parameter listed only in `query[]` used to be dropped on the assumption
/// that `raw` already had it, which silently changed the request. Matching on
/// the parameter *name* means a parameter that is in both is never duplicated.
fn merge_enabled_queries(raw: &str, queries: &[Param]) -> String {
    let existing: Vec<&str> = raw
        .split_once('?')
        .map(|(_, q)| {
            q.split('&')
                .map(|p| p.split_once('=').map_or(p, |(k, _)| k))
                .collect()
        })
        .unwrap_or_default();
    let missing: Vec<String> = queries
        .iter()
        .filter(|q| !q.disabled && !q.key.trim().is_empty())
        .filter(|q| !existing.contains(&q.key.as_str()))
        .map(|q| {
            if q.value.is_empty() {
                q.key.clone()
            } else {
                format!("{}={}", q.key, q.value)
            }
        })
        .collect();
    if missing.is_empty() {
        return raw.to_string();
    }
    let sep = if raw.contains('?') { '&' } else { '?' };
    format!("{raw}{sep}{}", missing.join("&"))
}

/// A Postman URL is a bare string or an object with a `raw` field; anything
/// else imports as an empty URL rather than failing the whole collection.
fn de_url<'de, D: Deserializer<'de>>(d: D) -> Result<Url, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::String(s) => {
            let (raw, fragment) = split_fragment(&s);
            Url {
                raw,
                fragment,
                variables: Vec::new(),
                queries: Vec::new(),
            }
        }
        Value::Object(m) => Url {
            raw: String::new(),
            fragment: String::new(),
            variables: m
                .get("variable")
                .cloned()
                .and_then(|v| serde_json::from_value::<Vec<Param>>(v).ok())
                .unwrap_or_default(),
            queries: m
                .get("query")
                .cloned()
                .and_then(|v| serde_json::from_value::<Vec<Param>>(v).ok())
                .unwrap_or_default(),
        }
        .with_raw_from(&m),
        _ => Url::default(),
    })
}

/// Split a URL into the part that is sent and the `#fragment` that isn't.
/// Only the first `#` counts, and a URL that is nothing but a fragment is left
/// alone — that is a template, not an address.
fn split_fragment(url: &str) -> (String, String) {
    match url.find('#') {
        Some(0) | None => (url.to_string(), String::new()),
        Some(i) => (url[..i].to_string(), url[i..].to_string()),
    }
}

impl Url {
    /// Fill in `raw` from the object form: `raw` when it has one, otherwise
    /// rebuilt from the pieces, then topped up with any enabled query
    /// parameter the text is missing, then split from its fragment.
    fn with_raw_from(mut self, m: &serde_json::Map<String, Value>) -> Self {
        let raw = m.get("raw").and_then(Value::as_str).unwrap_or("").trim();
        let base = if raw.is_empty() {
            url_from_parts(m)
        } else {
            raw.to_string()
        };
        let (kept, fragment) = split_fragment(&merge_enabled_queries(&base, &self.queries));
        self.raw = kept;
        self.fragment = fragment;
        self
    }
}

/// `basic` (→ `basic_auth`), `bearer` (→ a `Bearer` header), `apikey` (→ a
/// header or a query parameter) and `oauth2` (→ a generated token request, see
/// [`apply_oauth2`]) are mapped; credentials live in `key/value` lists keyed by
/// `username`/`password`/`token`/`key`/`value`/`in`.
#[derive(Clone, Deserialize, Default)]
#[serde(default)]
struct Auth {
    #[serde(rename = "type", deserialize_with = "de_str")]
    kind: String,
    basic: Vec<Param>,
    bearer: Vec<Param>,
    apikey: Vec<Param>,
    oauth2: Vec<Param>,
    awsv4: Vec<Param>,
}

impl Auth {
    fn field(list: &[Param], name: &str) -> String {
        list.iter()
            .find(|p| p.key == name)
            .map(|p| p.value.clone())
            .unwrap_or_default()
    }

    /// Whether this block turns auth *off* rather than describing some. Postman
    /// writes `{"type": "noauth"}` on a request that opts out of the auth it
    /// would otherwise inherit, so it has to beat the parent rather than being
    /// skipped as "nothing useful here".
    fn is_noauth(&self) -> bool {
        self.kind == "noauth"
    }

    /// Whether this block explicitly defers to the parent. Postman usually
    /// signals inheritance by omitting `auth` entirely, but the newer exports
    /// write it out as a type of its own.
    fn inherits(&self) -> bool {
        self.kind == "inherit" || self.kind.is_empty()
    }
}

/// Only `raw`, `urlencoded` and `formdata` modes are mapped.
#[derive(Deserialize, Default)]
#[serde(default)]
struct Body {
    #[serde(deserialize_with = "de_str")]
    mode: String,
    #[serde(deserialize_with = "de_str")]
    raw: String,
    urlencoded: Vec<Param>,
    formdata: Vec<Param>,
    /// `{"src": "/path/to/file"}` — the whole request body read from a file.
    file: FileBody,
    graphql: GraphQl,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct FileBody {
    #[serde(deserialize_with = "de_str")]
    src: String,
}

/// Postman's GraphQL body is the query and its variables kept apart. On the
/// wire GraphQL is an ordinary JSON POST, so this is a presentation split
/// rather than a protocol one — which is why it can be imported rather than
/// merely reported.
#[derive(Deserialize, Default)]
#[serde(default)]
struct GraphQl {
    #[serde(deserialize_with = "de_str")]
    query: String,
    /// Held as a *string* of JSON, not as JSON — but not always: an export
    /// written by anything other than Postman itself may put the object
    /// straight in. `de_str` coerced that to an empty string, so the request
    /// went out with its query and **no variables at all** — an operation with
    /// unbound arguments, silently. [`de_json_str`] takes either shape.
    #[serde(deserialize_with = "de_json_str")]
    variables: String,
}

/// A `{key, value, …}` entry shared by headers, auth and body params; the extra
/// fields only matter for form-data files (`src`/`type`/`contentType`).
///
/// Postman commonly emits an explicit `null` for string fields it leaves blank
/// (e.g. `"value": null` on a `file` form entry). `#[serde(default)]` only
/// fills in *absent* fields, not `null` ones, so the string fields use
/// [`de_str`] to coerce `null` to an empty string; otherwise a single `null`
/// would fail the whole collection import.
#[derive(Clone, Deserialize, Default)]
#[serde(default)]
struct Param {
    #[serde(deserialize_with = "de_str")]
    key: String,
    #[serde(deserialize_with = "de_str")]
    value: String,
    disabled: bool,
    #[serde(rename = "type", deserialize_with = "de_str")]
    kind: String,
    #[serde(deserialize_with = "de_str")]
    src: String,
    #[serde(rename = "contentType")]
    content_type: Option<String>,
    /// Postman's own per-row note. Imported into [`KvRow::desc`] so the
    /// documentation an exported collection carries isn't thrown away.
    #[serde(default, deserialize_with = "de_str")]
    description: String,
}

/// Deserialize a string field tolerantly. Postman's schema is only loosely
/// enforced by its own exporter, so a field documented as a string turns up as
/// anything: an explicit JSON `null` (which `#[serde(default)]` does *not*
/// handle), a number or bool from a hand-edited collection, or a nested
/// structure — real exports carry `{"key": "tokenRequestParams", "value": []}`
/// inside an oauth2 block. Because serde aborts the *whole* document on the
/// first type error, and [`convert_postman`] answers a failed parse with an
/// empty collection, being strict here silently emptied entire workspaces. So
/// scalars stringify and structures become empty rather than fatal.
/// A field that is a JSON *string* of JSON, or the JSON itself. An object or
/// array is re-serialized to the string form the caller expects; anything else
/// follows [`de_str`].
fn de_json_str<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::String(s) => s,
        v @ (Value::Object(_) | Value::Array(_)) => v.to_string(),
        Value::Null => String::new(),
        other => other.to_string(),
    })
}

fn de_str<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::String(s) => s,
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null | Value::Array(_) | Value::Object(_) => String::new(),
    })
}

impl Param {
    /// The row this parameter becomes, unless it is keyless.
    fn enabled_kve(&self) -> Option<KvRow> {
        (!self.key.is_empty()).then(|| KvRow {
            key: self.key.clone(),
            value: self.value.clone(),
            enabled: !self.disabled,
            desc: self.description.clone(),
        })
    }

    /// A form field — text, or a `File` using `src` as its path — unless the
    /// entry is disabled or keyless.
    fn form_field(&self) -> Option<FormField> {
        if self.disabled || self.key.is_empty() {
            return None;
        }
        Some(if self.kind == "file" {
            FormField {
                key: self.key.clone(),
                value: self.src.clone(),
                kind: FormFieldKind::File,
                content_type: self.content_type.clone(),
                // A file part Postman never had a file for (`src` empty — the
                // user picked the field but not the file, which their export
                // preserves) serializes to `key: file,;`, which is not valid
                // Hurl: the file would be written and then refuse to load,
                // taking the whole collection with it. Kept as a *disabled*
                // row, which round-trips as a comment — the field is still
                // there to be filled in, and the file still parses.
                enabled: !self.src.trim().is_empty(),
                desc: self.description.clone(),
                base64_prefix: None,
            }
        } else {
            FormField {
                key: self.key.clone(),
                value: self.value.clone(),
                kind: FormFieldKind::Text,
                content_type: None,
                base64_prefix: None,
                enabled: true,
                desc: self.description.clone(),
            }
        })
    }
}

/// Unwrap the `{"<key>": {…}}` envelope Postman puts around a collection or an
/// environment when it comes from the Postman API or an "Export all data"
/// account backup (each `Collections/*.json` there is `{"collection": {"info":
/// …, "item": …}}` and each `Environments/*.json` is `{"environment": {"name":
/// …, "values": …}}`), as opposed to the bare documents a single "Export
/// collection"/"Export environment" produces. `marker` is the field the inner
/// document must carry for the value to be treated as an envelope, so a
/// same-named field that happens to hold something else isn't mistaken for one.
/// Anything else is returned untouched.
fn unwrap_envelope(v: Value, key: &str, marker: &str) -> Value {
    match v {
        Value::Object(mut m) => match m.remove(key) {
            Some(inner @ Value::Object(_)) if inner.get(marker).is_some() => inner,
            // Not an envelope: put back what we took so the value is unchanged.
            other => {
                if let Some(other) = other {
                    m.insert(key.to_string(), other);
                }
                Value::Object(m)
            }
        },
        other => other,
    }
}

/// `true` when `content` looks like a Postman collection export (an `info`
/// block and an `item` array), as opposed to Hurl text. Both the bare and the
/// `{"collection": …}` enveloped shapes are recognized.
pub fn looks_like_postman(content: &str) -> bool {
    serde_json::from_str::<Value>(content)
        .map(|v| unwrap_envelope(v, "collection", "item"))
        .map(|v| v.get("info").is_some() && v.get("item").is_some())
        .unwrap_or(false)
}

/// Which of the two things Postman exports a file holds.
///
/// A user who picks "import an exported file" has one file and no idea which
/// of PaperBoy's two shelves it belongs on — Postman writes collections and
/// environments to the same `.json` extension. Deciding that from the content
/// is the importer's job, not theirs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportKind {
    Collection,
    Environment,
}

/// What kind of Postman export `content` is, or `None` if it is not one.
///
/// Collections are tested first: a document carrying both shapes is a
/// collection with variables, not an environment.
pub fn export_kind(content: &str) -> Option<ExportKind> {
    if looks_like_postman(content) {
        Some(ExportKind::Collection)
    } else if postman_env_values(content).is_some() {
        Some(ExportKind::Environment)
    } else {
        None
    }
}

/// A Postman environment export: a flat list of variables. Postman has no
/// notion of PaperBoy's provider references, so every value imports as a
/// literal — but a value that *is* written as `{{ op://… }}` / `{{ ssm:… }}`
/// still classifies as a secret reference once it reaches
/// [`crate::environment::parse_vars_pending`], exactly as in a `.vars` file.
#[derive(Deserialize, Default)]
#[serde(default)]
struct PostmanEnv {
    values: Vec<EnvValue>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct EnvValue {
    #[serde(deserialize_with = "de_str")]
    key: String,
    #[serde(deserialize_with = "de_str")]
    value: String,
    /// Absent (or `null`) means enabled — Postman only writes this out when a
    /// variable has been ticked off, so defaulting it to `false` (as
    /// `#[serde(default)]` would) would silently drop every variable.
    enabled: Option<bool>,
}

/// The `KEY`/value pairs of a Postman environment export, or `None` if
/// `content` isn't one. Both the bare `{"name": …, "values": […]}` shape and
/// the `{"environment": …}` envelope used by an account backup are accepted.
///
/// Variables Postman has disabled are dropped: they are the ones it would not
/// send, and a `.vars` environment has no "present but off" state to map them
/// onto. Keyless entries are dropped too, and a value carrying a newline is
/// flattened to a space — a `.vars` file is line-based, so keeping the break
/// would split one variable into two on the next save/reload.
pub fn postman_env_values(content: &str) -> Option<Vec<(String, String)>> {
    let v = serde_json::from_str::<Value>(content).ok()?;
    let v = unwrap_envelope(v, "environment", "values");
    // A collection also has no `values`, but check `item` too so a document
    // carrying both is never imported as an environment.
    if !v.get("values").is_some_and(Value::is_array) || v.get("item").is_some() {
        return None;
    }
    let env = serde_json::from_value::<PostmanEnv>(v).ok()?;
    Some(
        env.values
            .into_iter()
            .filter(|v| v.enabled.unwrap_or(true) && !v.key.trim().is_empty())
            .map(|v| {
                let value = v.value.replace(['\n', '\r'], " ");
                (v.key.trim().to_string(), value.trim().to_string())
            })
            .collect(),
    )
}

/// Parse a collection file's `content`: a Postman JSON export is imported,
/// anything else is treated as Hurl text.
pub fn parse_collection(content: &str) -> Vec<HurlEntry> {
    if looks_like_postman(content) {
        import_postman(content)
    } else {
        parse_hurl(content)
    }
}

/// Something a conversion could not carry across, recorded rather than
/// silently dropped so a migration off Postman knows what still needs doing by
/// hand. See [`convert_postman`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversionNote {
    /// The request (folder-prefixed) or `""` for the collection as a whole.
    pub item: String,
    pub detail: String,
}

/// Everything a Postman collection turns into: the requests, the
/// collection-level variables (which have nowhere to live in a `.hurl` file
/// and belong in a `.vars` alongside it), and what was lost on the way.
#[derive(Debug, Default)]
pub struct ConvertedCollection {
    pub entries: Vec<HurlEntry>,
    pub variables: Vec<(String, String)>,
    pub notes: Vec<ConversionNote>,
}

/// Convert a Postman collection JSON into `HurlEntry` values. Folders are
/// preserved by prefixing each request's title with its `/`-joined folder path
/// (e.g. "Auth/Tokens/Refresh") — the same convention plain Hurl collections
/// use (see [`crate::tree`]). Returns an empty vec if the JSON isn't a
/// recognizable collection.
pub fn import_postman(content: &str) -> Vec<HurlEntry> {
    convert_postman(content).entries
}

/// The full conversion, including the collection variables and the fidelity
/// notes [`import_postman`] throws away.
///
/// Auth is resolved here rather than per request, because Postman's is
/// inherited: a request with no `auth` block uses its folder's, a folder with
/// none uses the collection's, and `"type": "noauth"` at any level means "no
/// auth", not "ask my parent".
pub fn convert_postman(content: &str) -> ConvertedCollection {
    // A file that doesn't deserialize produces *no* requests, which looks
    // exactly like an empty collection — a shape of Postman JSON we mishandle
    // is therefore invisible unless it says so. Record it as a note so the
    // failure is reported rather than mistaken for "this collection is empty".
    let root = match serde_json::from_str::<Value>(content)
        .map(|v| unwrap_envelope(v, "collection", "item"))
        .and_then(serde_json::from_value::<Collection>)
    {
        Ok(root) => root,
        Err(e) => {
            return ConvertedCollection {
                notes: vec![ConversionNote {
                    item: String::new(),
                    detail: format!("collection could not be read: {e}"),
                }],
                ..ConvertedCollection::default()
            };
        }
    };
    let mut out = ConvertedCollection {
        variables: root
            .variable
            .iter()
            .filter(|v| !v.disabled && !v.key.trim().is_empty())
            .map(|v| {
                (
                    v.key.trim().to_string(),
                    v.value.replace(['\n', '\r'], " ").trim().to_string(),
                )
            })
            .collect(),
        ..ConvertedCollection::default()
    };
    let inherited = root.auth.as_ref().filter(|a| !a.inherits());
    let mut tokens = OAuthTokens::default();
    walk_items(
        &root.item,
        &mut Vec::new(),
        inherited,
        &[],
        root.protocol_profile_behavior.unwrap_or_default(),
        &mut tokens,
        &mut out,
    );
    out
}

/// Recursively collect requests, descending into folders (nodes carrying a
/// nested `item` array) and building up `path` as the folder breadcrumb so
/// each request's title can be prefixed with it. Folders take precedence when
/// a node unusually carries both `item` and `request`.
///
/// `inherited` is the nearest enclosing auth, already resolved — `None` once
/// some level has said `noauth`. `auth_path` is the folder breadcrumb of the
/// level that *declared* it, which is where a generated OAuth 2 token request
/// belongs: naming it after the first request that happens to use it would
/// bury a collection-wide token three folders deep.
fn walk_items(
    items: &[Item],
    path: &mut Vec<String>,
    inherited: Option<&Auth>,
    auth_path: &[String],
    profile: Profile,
    tokens: &mut OAuthTokens,
    out: &mut ConvertedCollection,
) {
    for it in items {
        if let Some(sub) = &it.item {
            let here = resolve_auth(it.auth.as_ref(), inherited);
            let declares_own = it.auth.as_ref().is_some_and(|a| !a.inherits());
            path.push(it.name.clone());
            let here_path = if declares_own {
                path.clone()
            } else {
                auth_path.to_vec()
            };
            let here_profile = it
                .protocol_profile_behavior
                .unwrap_or_default()
                .over(profile);
            walk_items(sub, path, here, &here_path, here_profile, tokens, out);
            path.pop();
        } else if let Some(req) = &it.request {
            let title = if path.is_empty() {
                it.name.clone()
            } else {
                format!("{}/{}", path.join("/"), it.name)
            };
            let auth = resolve_auth(req.auth.as_ref(), inherited);
            // A request declaring its own auth owns it, so its own folder is
            // where a token request for it belongs.
            let declares_own = req.auth.as_ref().is_some_and(|a| !a.inherits());
            let token_path: &[String] = if declares_own { path } else { auth_path };
            let profile = it
                .protocol_profile_behavior
                .unwrap_or_default()
                .over(profile);
            let mut entry = map_request(&title, req, &it.event, auth, profile);
            apply_path_variables(&title, &req.url, &mut entry, out);
            apply_oauth2(&title, token_path, auth, &mut entry, tokens, out);
            note_losses(&title, req, &it.event, auth, profile, &entry, out);
            for (name, fate) in rename_dynamic_variables(&mut entry) {
                let detail = match fate {
                    DynamicFate::Builtin(f) => format!(
                        "Postman generated `{{{{${name}}}}}` for you; Hurl generates the same \
                         thing, so it became `{{{{{f}}}}}` and still needs nothing supplied"
                    ),
                    DynamicFate::Computed(expr) => format!(
                        "Postman generated `{{{{${name}}}}}` for you; it is now computed by this \
                         request's `[Gen]` block as `{expr}`, once per send rather than once per \
                         use"
                    ),
                    DynamicFate::Supplied => format!(
                        "Postman generated `{{{{${name}}}}}` for you; nothing here can produce it, \
                         so it became the variable `{{{{{plain}}}}}`, which has to be supplied",
                        plain = name.replace('.', "_")
                    ),
                };
                out.notes.push(ConversionNote {
                    item: title.clone(),
                    detail,
                });
            }
            out.entries.push(entry);
        }
    }
}

/// Postman's OAuth 2 configuration, flattened out of its `key`/`value` list.
///
/// Postman fetches the token itself, behind the scenes, and never writes it to
/// the export — so an OAuth 2 collection used to import as a pile of requests
/// with no credentials on them at all. Hurl has no such machinery, but it
/// doesn't need any: a token request is just a request, and `[Captures]` feeds
/// its answer to the ones that follow. That is exactly the shape a hand-written
/// Hurl collection uses, so this generates it.
struct OAuth2 {
    access_token_url: String,
    grant_type: String,
    client_id: String,
    client_secret: String,
    username: String,
    password: String,
    scope: String,
    /// `header` (HTTP Basic, Postman's default) or `body` (credentials as form
    /// fields) — how the token endpoint expects the client to identify itself.
    client_authentication: String,
    /// Text placed before the token, e.g. `"Bearer "`. Postman stores the
    /// trailing space; exports that omit it fall back to `tokenType`.
    header_prefix: String,
    /// `header` (the default) or `queryParams`.
    add_token_to: String,
}

impl OAuth2 {
    fn read(auth: &Auth) -> Self {
        let f = |name: &str| Auth::field(&auth.oauth2, name);
        let prefix = match (f("headerPrefix"), f("tokenType")) {
            (p, _) if !p.trim().is_empty() => p,
            (_, t) if !t.trim().is_empty() => format!("{} ", t.trim()),
            _ => "Bearer ".to_string(),
        };
        OAuth2 {
            access_token_url: f("accessTokenUrl"),
            grant_type: f("grant_type"),
            client_id: f("clientId"),
            client_secret: f("clientSecret"),
            username: f("username"),
            password: f("password"),
            scope: f("scope"),
            client_authentication: f("client_authentication"),
            header_prefix: prefix,
            add_token_to: f("addTokenTo"),
        }
    }

    /// What makes two OAuth 2 blocks the same token. Folders repeat the whole
    /// configuration rather than referring to a shared one, so without this a
    /// collection with the same credentials on six folders would fetch six
    /// identical tokens.
    ///
    /// **Every field the token depends on has to be in here.** The credentials
    /// were once left out on the grounds that the endpoint and client
    /// identified the token — but a `password` grant's token *is* the user, and
    /// one client app with several test users is the normal shape of such a
    /// collection. Two folders logging in as different people therefore shared
    /// one token request, and every request in the second folder quietly went
    /// out as the first folder's user. The same omission merged two
    /// `client_credentials` blocks that shared a `client_id` but not a secret.
    fn identity(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}|{}|{}|{}",
            self.access_token_url,
            self.grant_type,
            self.client_id,
            self.client_secret,
            self.username,
            self.password,
            self.scope,
            self.client_authentication
        )
    }
}

/// Tokens generated so far: identity → the variable its value is captured into.
/// Threaded through the walk so the token request is emitted once, immediately
/// before the first request that needs it — which is also the order "Run All"
/// needs, since a collection is a script that runs top to bottom.
#[derive(Default)]
struct OAuthTokens {
    issued: Vec<(String, String)>,
}

impl OAuthTokens {
    fn var_for(&self, identity: &str) -> Option<&str> {
        self.issued
            .iter()
            .find(|(id, _)| id == identity)
            .map(|(_, var)| var.as_str())
    }

    /// A fresh capture name. The first token is plain `access_token` — the name
    /// the endpoint's own JSON uses and the one anybody reading the collection
    /// will expect; later ones are numbered rather than named after the folder,
    /// because a folder can be renamed and the variable would then lie.
    ///
    /// `taken` is every variable the collection already defines. A capture
    /// writes its variable at run time, so reusing a name the collection had
    /// meant the token request silently overwrote the user's own
    /// `access_token` — a name common enough that the collision is likely
    /// rather than exotic. Skipping past those names keeps both.
    fn next_var(&self, taken: &[(String, String)]) -> String {
        let used = |name: &str| {
            taken.iter().any(|(k, _)| k == name) || self.issued.iter().any(|(_, v)| v == name)
        };
        if self.issued.is_empty() && !used("access_token") {
            return "access_token".to_string();
        }
        // The second token is `access_token_2`, so start from the count (never
        // below 1) and step forward until the name is free.
        let mut n = self.issued.len().max(1);
        loop {
            n += 1;
            let candidate = format!("access_token_{n}");
            if !used(&candidate) {
                return candidate;
            }
        }
    }
}

/// Turn a Postman OAuth 2 block into a real request: a token request generated
/// once, plus the `Authorization` header (or query parameter) on every request
/// that inherits it.
///
/// Only the grants that are *just an HTTP POST* are generated —
/// `client_credentials` and `password`. `authorization_code`, `implicit` and
/// PKCE need a browser, a redirect and a human, none of which a file of
/// requests can carry, so they are reported rather than half-built.
fn apply_oauth2(
    title: &str,
    path: &[String],
    auth: Option<&Auth>,
    entry: &mut HurlEntry,
    tokens: &mut OAuthTokens,
    out: &mut ConvertedCollection,
) {
    let Some(auth) = auth.filter(|a| a.kind == "oauth2") else {
        return;
    };
    let cfg = OAuth2::read(auth);
    let mut note = |detail: String| {
        out.notes.push(ConversionNote {
            item: title.to_string(),
            detail,
        })
    };

    // A request that spells its own credentials out keeps them. Real exports do
    // this constantly — a hand-written token request sitting inside a folder
    // that also has OAuth 2 configured on it, so it ends up asking for a token
    // using a token it doesn't have yet. Appending ours as well would leave two
    // `Authorization` headers on the wire and let the collection's own,
    // deliberate choice lose to a generated one.
    let already_authorized = if cfg.add_token_to == "queryParams" {
        entry.queries.iter().any(|q| q.key == "access_token")
    } else {
        entry
            .headers
            .iter()
            .any(|h| h.key.eq_ignore_ascii_case("authorization"))
    };
    if already_authorized {
        note(
            "this request sets its own Authorization, so the folder's OAuth 2 token was not \
             added on top of it"
                .into(),
        );
        return;
    }

    // A folder may override only the presentation (`headerPrefix`) and leave
    // the token configuration to its parent. There is nothing to fetch, so
    // reuse whatever the enclosing level already issued.
    let var = if cfg.access_token_url.trim().is_empty() {
        match tokens.issued.last() {
            Some((_, var)) => var.clone(),
            None => {
                note(
                    "OAuth 2 auth with no token URL — Postman was holding a token it fetched \
                     elsewhere, which an export can't carry, so this request has no credentials"
                        .into(),
                );
                return;
            }
        }
    } else if !matches!(cfg.grant_type.as_str(), "client_credentials" | "password") {
        note(format!(
            "the OAuth 2 `{}` grant needs a browser redirect, which a file of requests can't \
             perform — fetch a token by hand and put it in a variable",
            cfg.grant_type
        ));
        return;
    } else {
        let identity = cfg.identity();
        match tokens.var_for(&identity) {
            Some(var) => var.to_string(),
            None => {
                let var = tokens.next_var(&out.variables);
                let (token_entry, missing) = token_request(&cfg, &var, path);
                if missing {
                    note(
                        "Postman keeps OAuth 2 client credentials outside the export, so the \
                         generated token request refers to `{{oauth_client_id}}` and \
                         `{{oauth_client_secret}}` — fill them in alongside the collection"
                            .into(),
                    );
                }
                out.entries.push(token_entry);
                tokens.issued.push((identity, var.clone()));
                var
            }
        }
    };

    if cfg.add_token_to == "queryParams" {
        entry
            .queries
            .push(KvRow::new("access_token", format!("{{{{{var}}}}}")));
    } else {
        entry.headers.push(KvRow::new(
            "Authorization",
            format!("{}{{{{{var}}}}}", cfg.header_prefix),
        ));
    }
}

/// Build the token request itself. Returns it plus whether the credentials had
/// to be stubbed out as variables because the export didn't carry them.
fn token_request(cfg: &OAuth2, var: &str, path: &[String]) -> (HurlEntry, bool) {
    let missing = cfg.client_id.trim().is_empty() && cfg.client_secret.trim().is_empty();
    let (id, secret) = if missing {
        (
            "{{oauth_client_id}}".to_string(),
            "{{oauth_client_secret}}".to_string(),
        )
    } else {
        (cfg.client_id.clone(), cfg.client_secret.clone())
    };

    let mut form: Vec<FormField> = Vec::new();
    let mut text = |key: &str, value: String| {
        form.push(FormField {
            key: key.to_string(),
            value,
            kind: FormFieldKind::Text,
            content_type: None,
            base64_prefix: None,
            enabled: true,
            desc: String::new(),
        })
    };
    text("grant_type", cfg.grant_type.clone());
    if !cfg.scope.trim().is_empty() {
        text("scope", cfg.scope.clone());
    }
    if cfg.grant_type == "password" {
        text("username", cfg.username.clone());
        text("password", cfg.password.clone());
    }
    // Postman's default is HTTP Basic ("header"); "body" sends the credentials
    // as ordinary form fields instead. Both are in the spec and endpoints
    // differ on which they accept, so the export's choice is honoured.
    let basic_auth = if cfg.client_authentication == "body" {
        text("client_id", id);
        text("client_secret", secret);
        None
    } else {
        Some((id, secret))
    };

    // The name is prefixed with the folder that declared the auth so it nests
    // beside the requests that use it, and reads as the first step of that
    // folder rather than a stray request at the top of the collection.
    let title = if path.is_empty() {
        "Get access token".to_string()
    } else {
        format!("{}/Get access token", path.join("/"))
    };

    let entry = HurlEntry {
        title,
        method: "POST".to_string(),
        url: cfg.access_token_url.clone(),
        form_fields: form,
        basic_auth,
        // Asserted, not merely hoped for: without it a failed token request
        // captures nothing and every request after it fails for a reason that
        // has scrolled off the screen.
        expected_status: Some(200),
        captures: vec![(var.to_string(), "jsonpath \"$.access_token\"".to_string())],
        ..Default::default()
    };
    (entry, missing)
}

/// Rewrite Postman's `/:name` path placeholders to `{{name}}`, and carry the
/// values it declared for them into the collection's variables.
///
/// Postman substitutes a path variable from `url.variable` at send time, so
/// importing `raw` alone produced a URL that asks the server for a resource
/// literally named ":batch_id". Hurl's equivalent is an ordinary `{{name}}`,
/// which keeps the request parameterised rather than baking one value in.
///
/// Only whole segments are rewritten, and only for names the export actually
/// declares: `:` is legal in a URL (`http://host:8080`, a `mailto:`), and
/// Postman itself only substitutes what is in the `variable` list.
///
/// A declared value is seeded into the collection's variables so the request
/// works as imported. The first value for a name wins — path variables are
/// per-request, so several requests can declare the same name with different
/// values, and there is exactly one `.vars` file for them to land in. A
/// conflict is reported rather than silently resolved, since guessing which
/// batch id was meant is not something an importer can do.
fn apply_path_variables(
    title: &str,
    url: &Url,
    entry: &mut HurlEntry,
    out: &mut ConvertedCollection,
) {
    let declared: Vec<&Param> = url
        .variables
        .iter()
        .filter(|v| !v.disabled && !v.key.trim().is_empty())
        .collect();
    if declared.is_empty() {
        return;
    }

    // Rewrite the path only. The query string can contain a bare `:` in a
    // value, and Postman never substitutes path variables there.
    let (path, query) = match entry.url.split_once('?') {
        Some((p, q)) => (p.to_string(), Some(q.to_string())),
        None => (entry.url.clone(), None),
    };
    let rewritten: Vec<String> = path
        .split('/')
        .map(|seg| match seg.strip_prefix(':') {
            Some(name) if declared.iter().any(|v| v.key.trim() == name) => {
                format!("{{{{{name}}}}}")
            }
            _ => seg.to_string(),
        })
        .collect();
    entry.url = match query {
        Some(q) => format!("{}?{}", rewritten.join("/"), q),
        None => rewritten.join("/"),
    };

    for var in declared {
        let key = var.key.trim().to_string();
        let value = var.value.replace(['\n', '\r'], " ").trim().to_string();
        match out.variables.iter().find(|(k, _)| *k == key) {
            Some((_, existing)) if *existing != value && !value.is_empty() => {
                out.notes.push(ConversionNote {
                    item: title.to_string(),
                    detail: format!(
                        "the path variable `{key}` is declared here as `{value}` but is already \
                         `{existing}` — a `.vars` file holds one value per name, so the first was \
                         kept"
                    ),
                });
            }
            Some(_) => {}
            None => out.variables.push((key, value)),
        }
    }
}

/// What became of one of Postman's *dynamic variables* — `{{$guid}}`,
/// `{{$timestamp}}`, `{{$randomInt}}` — the values it makes up at send time.
enum DynamicFate {
    /// Hurl generates the same thing itself, so the placeholder becomes its
    /// built-in and needs nothing else. The most portable outcome there is:
    /// stock `hurl` runs it with no variables supplied at all.
    Builtin(&'static str),
    /// A `# [Gen]` row now computes it. Carries the expression written into the
    /// block.
    Computed(&'static str),
    /// Nothing here produces it, so it becomes an ordinary variable the user
    /// has to supply.
    Supplied,
}

/// What PaperBoy can do about `$name`.
///
/// Only the handful whose meaning is exact and unambiguous are claimed. Guessing
/// at `$randomFirstName` would be worse than saying it has to be supplied: a
/// request that sends a plausible wrong value is harder to notice than one that
/// refuses to run.
fn dynamic_fate(name: &str) -> DynamicFate {
    match name {
        // Hurl's own generators, so no `# [Gen]` block is needed at all.
        "guid" | "randomUUID" => DynamicFate::Builtin("newUuid"),
        "isoTimestamp" => DynamicFate::Builtin("newDate"),
        // `$timestamp` is Unix *seconds*, which `newDate` is not — it renders
        // ISO 8601. Close enough to reach for by mistake, so it is spelled out.
        "timestamp" => DynamicFate::Computed("timestamp"),
        // Postman documents `$randomInt` as 0 to 1000 inclusive.
        "randomInt" => DynamicFate::Computed("random_int(0, 1000)"),
        _ => DynamicFate::Supplied,
    }
}

/// Rewrite Postman's dynamic variables into something Hurl can read, and where
/// possible into something that actually produces a value.
///
/// A `$` is not legal in a Hurl template name, and the failure was not local:
/// one `{{$guid}}` anywhere in a collection made the whole converted file fail
/// to parse ("parsing template variable"), so every request in it was lost, not
/// just the one that used it. Renaming is therefore not optional.
///
/// Where the value has an exact equivalent it is *supplied* rather than merely
/// renamed — `{{$guid}}` becomes Hurl's own `{{newUuid}}`, `{{$timestamp}}`
/// becomes a `# [Gen]` row — so the request runs on import instead of stopping
/// on a variable nobody can fill in. Everything else keeps its name without the
/// `$` (`{{$randomFirstName}}` → `{{randomFirstName}}`), which parses and stays
/// readable as the thing it was; dotted forms (`{{$processEnv.HOME}}`) fold
/// their dots into underscores for the same reason.
///
/// Returns each original `$name` and its fate, so every one can be noted.
fn rename_dynamic_variables(entry: &mut HurlEntry) -> Vec<(String, DynamicFate)> {
    static DYNAMIC_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\{\{\s*\$([A-Za-z_][A-Za-z0-9_.]*)\s*\}\}").unwrap());

    let mut found: Vec<(String, DynamicFate)> = Vec::new();
    let mut rows: Vec<(String, String)> = Vec::new();
    let mut fix = |text: &mut String| {
        if !text.contains("{{") {
            return;
        }
        let replaced = DYNAMIC_RE.replace_all(text, |caps: &regex::Captures| {
            let raw = &caps[1];
            let fate = dynamic_fate(raw);
            // The name a `$`-less Hurl template can carry.
            let plain = raw.replace('.', "_");
            let name = match fate {
                DynamicFate::Builtin(f) => f.to_string(),
                _ => plain.clone(),
            };
            if let DynamicFate::Computed(expr) = fate
                && !rows.iter().any(|(n, _)| *n == name)
            {
                rows.push((name.clone(), expr.to_string()));
            }
            if !found.iter().any(|(n, _)| n == raw) {
                found.push((raw.to_string(), fate));
            }
            format!("{{{{{name}}}}}")
        });
        if let std::borrow::Cow::Owned(new) = replaced {
            *text = new;
        }
    };

    fix(&mut entry.url);
    for row in entry
        .headers
        .iter_mut()
        .chain(entry.queries.iter_mut())
        .chain(entry.cookies.iter_mut())
    {
        fix(&mut row.value);
    }
    for f in &mut entry.form_fields {
        fix(&mut f.value);
    }
    if let Some(body) = entry.body_src.as_mut() {
        fix(body);
    }
    if let Some((user, pass)) = entry.basic_auth.as_mut() {
        fix(user);
        fix(pass);
    }
    // Appended rather than assigned: a converted entry could already carry a
    // block from elsewhere, and a name defined twice is a block that reads
    // differently depending on which row won.
    for (name, expr) in rows {
        if !entry.generators.iter().any(|(n, _)| *n == name) {
            entry.generators.push((name, expr));
        }
    }
    found
}

/// `value` unless it is blank, in which case `fallback`.
fn non_empty(value: String, fallback: &str) -> String {
    if value.trim().is_empty() {
        fallback.to_string()
    } else {
        value
    }
}

/// Which auth applies at this level: its own if it declares any, otherwise
/// whatever it inherits — and nothing at all if it opts out.
fn resolve_auth<'a>(own: Option<&'a Auth>, inherited: Option<&'a Auth>) -> Option<&'a Auth> {
    match own {
        Some(a) if a.is_noauth() => None,
        Some(a) if !a.inherits() => Some(a),
        _ => inherited,
    }
}

/// Record what this request couldn't bring with it. Deliberately conservative:
/// a note is only written where something in the export is genuinely not in the
/// output, so the report stays worth reading.
fn note_losses(
    title: &str,
    req: &Request,
    events: &[Event],
    auth: Option<&Auth>,
    profile: Profile,
    entry: &HurlEntry,
    out: &mut ConvertedCollection,
) {
    let mut note = |detail: String| {
        out.notes.push(ConversionNote {
            item: title.to_string(),
            detail,
        })
    };

    if let Some(auth) = auth
        && !matches!(
            auth.kind.as_str(),
            "basic" | "bearer" | "apikey" | "oauth2" | "awsv4"
        )
    {
        note(format!(
            "auth type `{}` has no Hurl equivalent and was dropped",
            auth.kind
        ));
    }
    if let Some(auth) = auth
        && auth.kind == "awsv4"
        && Auth::field(&auth.awsv4, "accessKey").trim().is_empty()
    {
        note(
            "AWS auth carried no keys — real exports keep them in variables or outside the file \
             — so the request signs with `{{aws_access_key_id}}` and `{{aws_secret_access_key}}`"
                .into(),
        );
    }
    if !req.url.fragment.is_empty() {
        note(format!(
            "the URL fragment `{}` was left off: a fragment is never sent to the server (Postman \
             doesn't send it either), and a `#` on the request line would comment out the rest \
             of the URL",
            req.url.fragment
        ));
    }
    // Must match `map_request`'s routing exactly: it sends the key to a query
    // parameter only for `in: "query"` and to a header otherwise, so testing
    // "anything that isn't a header" here told the user their key was in the
    // query string when it wasn't.
    if let Some(auth) = auth
        && auth.kind == "apikey"
        && Auth::field(&auth.apikey, "in") == "query"
    {
        note("API-key auth is sent in the query string; it was added as a query parameter".into());
    }
    let pruned =
        matches!(req.method.as_str(), "GET" | "HEAD") && profile.disable_body_pruning != Some(true);
    if let Some(b) = &req.body
        && pruned
        && !(b.raw.is_empty() && b.mode.is_empty())
    {
        note(format!(
            "the stored {} body was left out, because Postman would not have sent it on a {} \
             either — nothing turned its body pruning off",
            if b.mode.is_empty() {
                "request"
            } else {
                &b.mode
            },
            req.method
        ));
    } else if let Some(b) = &req.body {
        match b.mode.as_str() {
            "" | "raw" | "urlencoded" | "formdata" => {}
            // Hurl can send a whole body from a file, but PaperBoy's own
            // request model has no place to keep one — a `file,path;` body
            // serialises correctly and then reads back as nothing, so importing
            // it would produce a request that quietly loses its body the first
            // time the collection is reopened. Better to say so.
            "file" if b.file.src.trim().is_empty() => note(
                "the body is a file, and Postman never had one chosen — attach it here instead"
                    .into(),
            ),
            "file" => note(format!(
                "the body was the file `{}`; attach it here instead, as PaperBoy sends file \
                 bodies as form or multipart parts rather than as the whole body",
                b.file.src.trim()
            )),
            "graphql" if b.graphql.query.trim().is_empty() => {
                note("the GraphQL body held no query, so there was nothing to send".into())
            }
            "graphql" => {}
            mode => note(format!("body mode `{mode}` was dropped")),
        }
    }
    for f in &entry.form_fields {
        if f.kind == FormFieldKind::File && !f.enabled {
            note(format!(
                "the file part `{}` had no file chosen in Postman, so it is switched off until one is",
                f.key
            ));
        }
    }
    if events
        .iter()
        .any(|e| e.listen == "prerequest" && !e.script.exec.is_empty())
    {
        // Worth naming the `[Gen]` block here rather than only in the README:
        // this note is the moment the user learns the script is gone, and most
        // pre-request scripts are computing a nonce, a stamp or a signature,
        // which the block does. It is not offered as an automatic translation
        // because a script can do anything, and a wrong guess at a signature
        // sends a plausible-looking request that fails for no visible reason.
        note(
            "a pre-request script was dropped — Hurl cannot run one. If it was computing a \
             nonce, a timestamp or a signature, the request's `[Gen]` block can do that instead"
                .into(),
        );
    }
    let has_tests = events
        .iter()
        .any(|e| e.listen == "test" && !e.script.exec.is_empty());
    if has_tests && entry.captures.is_empty() {
        note("a test script was dropped — nothing in it reduced to a [Captures] entry".into());
    } else if has_tests {
        note("a test script was read for [Captures] only; its assertions were dropped".into());
    }
}

fn map_request(
    name: &str,
    req: &Request,
    events: &[Event],
    auth: Option<&Auth>,
    profile: Profile,
) -> HurlEntry {
    let mut headers: Vec<KvRow> = req.header.iter().filter_map(Param::enabled_kve).collect();
    let mut queries: Vec<KvRow> = Vec::new();
    let mut options: Vec<KvRow> = Vec::new();

    // Auth → basic_auth, or a header/query parameter. `auth` is already
    // resolved against the enclosing folder and collection by `walk_items`.
    let mut basic_auth = None;
    if let Some(auth) = auth {
        match auth.kind.as_str() {
            "basic" => {
                let u = Auth::field(&auth.basic, "username");
                let p = Auth::field(&auth.basic, "password");
                if !u.is_empty() || !p.is_empty() {
                    basic_auth = Some((u, p));
                }
            }
            "bearer" => {
                let t = Auth::field(&auth.bearer, "token");
                if !t.is_empty() {
                    headers.push(KvRow::new("Authorization", format!("Bearer {t}")));
                }
            }
            // An API key is a header or a query parameter with a configurable
            // name — both of which Hurl expresses directly, so this is the one
            // remaining common Postman auth type that maps without loss.
            "apikey" => {
                let key = Auth::field(&auth.apikey, "key");
                let value = Auth::field(&auth.apikey, "value");
                if !key.is_empty() {
                    let row = KvRow::new(key, value);
                    if Auth::field(&auth.apikey, "in") == "query" {
                        queries.push(row);
                    } else {
                        headers.push(row);
                    }
                }
            }
            // AWS Signature v4 is a signing algorithm, not a header PaperBoy
            // could write out: the signature covers the method, path, headers
            // and body, so it can only be computed at send time. curl does it,
            // Hurl exposes it as the `aws-sigv4` option, and the credentials
            // ride in `user` — so this maps exactly, which is worth doing for
            // an auth type that otherwise loses every request under it.
            "awsv4" => {
                let f = |name: &str| Auth::field(&auth.awsv4, name);
                // `aws:amz` is the provider pair every AWS endpoint uses.
                // Region and service are appended only when the export names
                // them, because curl infers both from the hostname and a blank
                // guess would be worse than no guess.
                let mut provider = "aws:amz".to_string();
                let (region, service) = (f("region"), f("service"));
                // The suffix needs the region: `aws:amz::s3` names an *empty*
                // region, which is a worse guess than none at all — the point
                // of leaving it off is to let curl infer both from the
                // hostname. So a service without a region is dropped too.
                if !region.trim().is_empty() {
                    provider.push(':');
                    provider.push_str(region.trim());
                    if !service.trim().is_empty() {
                        provider.push(':');
                        provider.push_str(service.trim());
                    }
                }
                options.push(KvRow::new("aws-sigv4", provider));

                // Postman usually stores these as collection variables rather
                // than in the auth block, and real exports leave the block
                // empty altogether — so fall back to named variables the user
                // can fill in rather than signing with nothing.
                let key = non_empty(f("accessKey"), "{{aws_access_key_id}}");
                let secret = non_empty(f("secretKey"), "{{aws_secret_access_key}}");
                options.push(KvRow::new("user", format!("{key}:{secret}")));

                let session = f("sessionToken");
                if !session.trim().is_empty() {
                    headers.push(KvRow::new("x-amz-security-token", session));
                }
            }
            _ => {}
        }
    }

    // Body: a raw body is kept verbatim; url-encoded / form-data fields become
    // form fields (file-type form-data fields become `File` fields).
    let mut form_fields = Vec::new();
    let mut body = String::new();
    // Postman strips the body from a body-less method unless `disableBodyPruning`
    // says otherwise, so a GET stored with a body is not a GET that sends one —
    // it's the remains of an edit. Importing it anyway would change what the
    // collection does, and some servers reject a GET with a body outright.
    let pruned =
        matches!(req.method.as_str(), "GET" | "HEAD") && profile.disable_body_pruning != Some(true);
    if let Some(b) = &req.body.as_ref().filter(|_| !pruned) {
        match b.mode.as_str() {
            "raw" => body = b.raw.clone(),
            "urlencoded" => {
                form_fields = b.urlencoded.iter().filter_map(Param::form_field).collect()
            }
            "formdata" => form_fields = b.formdata.iter().filter_map(Param::form_field).collect(),
            // GraphQL over HTTP is a JSON POST of `{query, variables}`; the two
            // halves are only kept apart for editing.
            "graphql" if !b.graphql.query.trim().is_empty() => {
                let mut doc = serde_json::Map::new();
                doc.insert("query".into(), Value::String(b.graphql.query.clone()));
                // Variables arrive as a string of JSON. Parsed, they nest as an
                // object the way the server expects; unparseable, they are left
                // out rather than sent as a quoted blob the server would reject.
                if let Ok(vars) = serde_json::from_str::<Value>(&b.graphql.variables)
                    && !vars.is_null()
                {
                    doc.insert("variables".into(), vars);
                }
                body = serde_json::to_string_pretty(&Value::Object(doc)).unwrap_or_default();
                if !headers
                    .iter()
                    .any(|h| h.key.eq_ignore_ascii_case("content-type"))
                {
                    headers.push(KvRow::new("Content-Type", "application/json"));
                }
            }
            _ => {}
        }
    }

    let mut entry = HurlEntry::from_fields(name, &req.method, &req.url.raw, headers, &body);
    entry.basic_auth = basic_auth;
    entry.form_fields = form_fields;
    // A parameter Postman has switched off is left out of the URL text, so
    // reading the text alone threw it away. It is part of the request as
    // documentation — the optional filter someone turns on now and again — and
    // PaperBoy has a switched-off row for exactly this.
    entry.queries.extend(
        req.url
            .queries
            .iter()
            .filter(|q| q.disabled)
            .filter_map(Param::enabled_kve),
    );
    entry.queries.extend(queries);
    // Postman's prose about the request. Kept as comments in the header
    // region rather than folded into the title: the title is the request's
    // *name* and is what every list in the app shows, so a paragraph in it
    // would be unreadable — but dropping it loses the only explanation of what
    // half these requests are for. `EntryComment` already round-trips.
    entry.comments.extend(
        req.description
            .replace("\r\n", "\n")
            .lines()
            .map(|line| EntryComment {
                anchor: CommentAnchor::Headers,
                text: if line.trim().is_empty() {
                    "#".to_string()
                } else {
                    format!("# {}", line.trim_end())
                },
            }),
    );
    entry.options.extend(options);
    // `strictSSL: false` is Postman being told not to verify the certificate,
    // which is Hurl's `insecure` — a real behavioural setting that would
    // otherwise import as a request that simply fails against the staging box
    // it was written for.
    if profile.strict_ssl == Some(false) {
        entry.options.push(KvRow::new("insecure", "true"));
    }
    // Captured variables from the request's `test` script (#24). A request that
    // gets captures serializes with a `HTTP *` line automatically; one with none
    // stays bare (a hand-added `[Captures]` later gives a clear "add HTTP *"
    // parse error, so we don't emit an unsolicited wildcard line).
    entry.captures = captures_from_events(events);
    entry
}

// `var/let/const X = pm.response.json()` — `X` is the parsed-body variable
// whose accessor chains map to jsonpaths.
static JSON_VAR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:var|let|const)\s+(\w+)\s*=\s*pm\.response\.json\s*\(\s*\)").unwrap()
});

// `pm.<store>.set("NAME", <value>)` for the variable stores Postman exposes.
static SET_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"pm\.(?:environment|collectionVariables|globals|variables)\.set\(\s*['"]([^'"]+)['"]\s*,\s*([^)]+)\)"#,
    )
    .unwrap()
});

/// Best-effort scrape of a request's `test` scripts into `[Captures]`: each
/// `pm.<store>.set("NAME", body['a']['b'])` (or `body.a.b`) call where `body`
/// is the `pm.response.json()` variable becomes `NAME = jsonpath "$.a.b"`.
/// Calls that don't reduce to a plain accessor chain are skipped rather than
/// failing the import.
fn captures_from_events(events: &[Event]) -> Vec<(String, String)> {
    let script = events
        .iter()
        .filter(|e| e.listen == "test")
        .flat_map(|e| e.script.exec.iter())
        .map(|l| l.trim_end_matches('\r'))
        .collect::<Vec<_>>()
        .join("\n");
    if script.is_empty() {
        return Vec::new();
    }
    // Read the script as code, not as text: a `pm.environment.set(...)` line
    // someone commented out is a capture they explicitly turned *off*, and one
    // quoted inside a string is documentation. Matching those produced a
    // capture that runs — and because captures feed the variables later
    // requests interpolate, a spurious `token` capture changes the bytes those
    // requests send. A wrong capture is far worse than a missing one.
    let (code, in_string) = strip_js_noise(&script);

    // Response-body variable name(s); default to the near-universal `jsonData`
    // when the script assigns the body to nothing we recognise.
    let mut roots: Vec<String> = JSON_VAR_RE
        .captures_iter(&code)
        .map(|c| c[1].to_string())
        .collect();
    if roots.is_empty() {
        roots.push("jsonData".to_string());
    }
    SET_RE
        .captures_iter(&code)
        .filter(|c| {
            // The call itself must be code. Its *arguments* are quoted, so
            // only the position the match starts at can be judged.
            c.get(0)
                .is_some_and(|m| !in_string.get(m.start()).copied().unwrap_or(false))
        })
        .filter_map(|c| {
            let path = accessor_to_jsonpath(c[2].trim(), &roots)?;
            Some((c[1].to_string(), format!("jsonpath \"{path}\"")))
        })
        .collect()
}

/// Blank out JavaScript comments and report which bytes sit inside a string
/// literal.
///
/// The returned text is the same length as the input — comments become spaces,
/// keeping newlines — so a match offset in it means the same thing in the
/// original. The scanner is deliberately small: it understands `//`, `/* */`,
/// `'`/`"`/backtick strings and backslash escapes, which is everything needed
/// to tell "this call is real code" from "this call is text". It does not try
/// to be a JavaScript parser — a regex over a scripting language is a
/// heuristic either way, and the point here is only to stop the obvious
/// false positives.
///
/// A regex literal (`/foo/`) is not recognised, so its contents are read as
/// code; that can only ever cause a `pm...set(...)` *inside a regex* to be
/// picked up, which is not a thing anybody writes.
fn strip_js_noise(script: &str) -> (String, Vec<bool>) {
    #[derive(PartialEq)]
    enum St {
        Code,
        Line,
        Block,
        Str(char),
    }
    let mut out = String::with_capacity(script.len());
    let mut in_string = Vec::with_capacity(script.len());
    let mut st = St::Code;
    let mut escaped = false;
    let mut chars = script.chars().peekable();
    while let Some(c) = chars.next() {
        let (keep, quoted) = match st {
            St::Code => match c {
                '/' if chars.peek() == Some(&'/') => {
                    st = St::Line;
                    (false, false)
                }
                '/' if chars.peek() == Some(&'*') => {
                    st = St::Block;
                    (false, false)
                }
                '\'' | '"' | '`' => {
                    st = St::Str(c);
                    escaped = false;
                    (true, true)
                }
                _ => (true, false),
            },
            St::Line => {
                if c == '\n' {
                    st = St::Code;
                    (true, false)
                } else {
                    (false, false)
                }
            }
            St::Block => {
                if c == '*' && chars.peek() == Some(&'/') {
                    // Consume the `/` too, as a blank.
                    chars.next();
                    out.push(' ');
                    in_string.push(false);
                    st = St::Code;
                }
                (c == '\n', false)
            }
            St::Str(delim) => {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == delim || (c == '\n' && delim != '`') {
                    // An unterminated quote at end of line is a typo, not a
                    // string that swallows the rest of the file.
                    st = St::Code;
                }
                (true, true)
            }
        };
        // A blanked character becomes a single space, which can shorten
        // `out` — that is fine, because the flags are pushed to match `out`,
        // and `out` is what the regexes are run over.
        let ch = if keep { c } else { ' ' };
        out.push(ch);
        for _ in 0..ch.len_utf8() {
            in_string.push(quoted);
        }
    }
    debug_assert_eq!(out.len(), in_string.len());
    (out, in_string)
}

/// Convert a JS accessor chain rooted at one of `roots`
/// (`body['a'].b["c"][0]`) into a jsonpath (`$.a.b.c[0]`). Returns `None` for
/// anything past a simple `.ident` / `['key']` / `[n]` chain (a method call,
/// arithmetic, …), so an unparseable capture is dropped instead of guessed.
fn accessor_to_jsonpath(expr: &str, roots: &[String]) -> Option<String> {
    let mut s = roots.iter().find_map(|r| {
        expr.strip_prefix(r.as_str())
            .filter(|rest| rest.is_empty() || rest.starts_with(['.', '[']))
    })?;
    let mut path = String::from("$");
    while !s.is_empty() {
        if let Some(rest) = s.strip_prefix('.') {
            let end = rest
                .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            if end == 0 {
                return None;
            }
            push_key(&mut path, &rest[..end]);
            s = &rest[end..];
        } else {
            let rest = s.strip_prefix('[')?;
            let close = rest.find(']')?;
            let key = rest[..close].trim();
            if let Some(k) = unquote(key) {
                push_key(&mut path, k);
            } else if !key.is_empty() && key.bytes().all(|b| b.is_ascii_digit()) {
                path.push_str(&format!("[{key}]"));
            } else {
                return None;
            }
            s = &rest[close + 1..];
        }
    }
    Some(path)
}

/// Append a jsonpath key: a plain identifier as `.name`, anything else bracket-
/// quoted (`['a-b']`) so the path stays valid.
fn push_key(path: &mut String, key: &str) {
    let simple = !key.is_empty()
        && !key.starts_with(|c: char| c.is_ascii_digit())
        && key.chars().all(|c| c.is_alphanumeric() || c == '_');
    if simple {
        path.push('.');
        path.push_str(key);
    } else {
        path.push_str(&format!("['{key}']"));
    }
}

/// Strip matching single or double quotes, returning the inner text.
fn unquote(s: &str) -> Option<&str> {
    let b = s.as_bytes();
    if b.len() >= 2 && (b[0] == b'\'' || b[0] == b'"') && b[b.len() - 1] == b[0] {
        Some(&s[1..s.len() - 1])
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The collections under `examples/postman/` are what a user is told to
    /// import to see this working. A mapping change that stopped producing the
    /// outcome the example's own description promises would be found by them,
    /// not by us, so they are converted here.
    mod shipped_examples {
        use super::*;
        use std::collections::HashMap;

        fn convert_example(file: &str) -> ConvertedCollection {
            let path = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/postman/");
            let json = std::fs::read_to_string(format!("{path}{file}"))
                .unwrap_or_else(|e| panic!("{file} is shipped and must be readable: {e}"));
            convert_postman(&json)
        }

        fn entry<'a>(c: &'a ConvertedCollection, title: &str) -> &'a HurlEntry {
            c.entries
                .iter()
                .find(|e| e.title == title)
                .unwrap_or_else(|| {
                    panic!(
                        "no request titled {title:?}; found {:?}",
                        c.entries.iter().map(|e| &e.title).collect::<Vec<_>>()
                    )
                })
        }

        #[test]
        fn the_dynamic_variable_example_demonstrates_all_three_outcomes() {
            let c = convert_example("dynamic-variables.postman_collection.json");

            let builtin = entry(&c, "Built in/A GUID and an ISO timestamp");
            let text = builtin.to_hurl();
            assert!(
                text.contains("{{newUuid}}") && text.contains("{{newDate}}"),
                "$guid and $isoTimestamp become Hurl's own placeholders: {text}"
            );
            assert!(
                builtin.generators.is_empty(),
                "a built-in needs no computed row"
            );

            let twice = entry(&c, "Built in/The same GUID twice");
            assert!(
                twice.generators.is_empty(),
                "and still none when used in two places"
            );

            let computed = entry(&c, "Computed/A Unix timestamp and a random integer");
            let names: Vec<&str> = computed
                .generators
                .iter()
                .map(|(n, _)| n.as_str())
                .collect();
            assert_eq!(
                computed.generators.len(),
                2,
                "one row per name however often it is used, not one per use: {names:?}"
            );
            let exprs: Vec<&str> = computed
                .generators
                .iter()
                .map(|(_, e)| e.as_str())
                .collect();
            assert!(
                exprs.contains(&"timestamp"),
                "$timestamp is Unix seconds: {exprs:?}"
            );
            assert!(
                exprs.iter().any(|e| e.starts_with("random_int(")),
                "$randomInt is a bounded integer: {exprs:?}"
            );

            let supplied = entry(&c, "Supplied/Faker data nothing can produce");
            assert!(
                supplied.generators.is_empty(),
                "nothing here can be honestly computed"
            );
            assert!(
                c.notes.iter().any(|n| n.item == supplied.title),
                "so the user is told to supply it instead"
            );
        }

        #[test]
        fn every_shipped_example_still_parses_as_hurl() {
            for file in [
                "dynamic-variables.postman_collection.json",
                "signed-requests.postman_collection.json",
            ] {
                let c = convert_example(file);
                assert!(!c.entries.is_empty(), "{file} converted to nothing");
                for e in &c.entries {
                    let text = e.to_hurl();
                    let back = crate::hurl::parse_hurl(&text);
                    assert_eq!(
                        back.len(),
                        1,
                        "{file}: {:?} did not survive a round trip:\n{text}",
                        e.title
                    );
                    assert_eq!(back[0].generators, e.generators, "{file}: {:?}", e.title);
                }
            }
        }

        /// `signed-requests.hurl` is the worked answer the README tells the
        /// user to compare their own block against, so its rows must actually
        /// evaluate. A `.hurl` file that no longer parses, or a function that
        /// has been renamed out from under it, would otherwise be found by
        /// whoever followed the instructions.
        #[test]
        fn the_worked_signing_example_parses_and_evaluates() {
            let path = concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/examples/postman/signed-requests.hurl"
            );
            let text = std::fs::read_to_string(path).expect("shipped example must be readable");
            let entries = crate::hurl::parse_hurl(&text);
            assert_eq!(entries.len(), 6, "six requests, one deliberately broken");

            let vars: HashMap<String, String> = [
                ("baseUrl", "https://postman-echo.com"),
                ("API_KEY", "EXAMPLE-KEY-id"),
                ("API_SECRET", "EXAMPLE-SECRET-not-a-real-key"),
                ("SINCE", "2026-01-01T00:00:00Z"),
            ]
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();

            for e in &entries {
                assert!(
                    !e.generators.is_empty(),
                    "{:?} is in this file to demonstrate a block",
                    e.title
                );
                let mut merged = vars.clone();
                let errors = crate::generators::expand(
                    &e.generators,
                    &mut merged,
                    &crate::generators::SystemSource::new(),
                );
                if e.title.starts_with("Deliberately broken") {
                    assert_eq!(errors.len(), 1, "the typo is the point of that request");
                    continue;
                }
                assert!(errors.is_empty(), "{:?}: {errors:?}", e.title);
            }

            // The known-answer request is the file's own proof, so check the
            // vector here too rather than only against a live server.
            let vector = &entries[4];
            let mut merged = vars.clone();
            crate::generators::expand(
                &vector.generators,
                &mut merged,
                &crate::generators::SystemSource::new(),
            );
            assert_eq!(
                merged.get("sig").map(String::as_str),
                Some("5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"),
                "RFC 4231 case 2"
            );
        }

        #[test]
        fn the_signing_example_keeps_the_scripts_it_cannot_run() {
            let c = convert_example("signed-requests.postman_collection.json");
            let signed = entry(&c, "HMAC-SHA256 over a nonce and a timestamp");
            assert!(
                signed.to_hurl().contains("{{sig}}"),
                "the signature placeholder is preserved for the [Gen] row to fill"
            );
            assert!(
                c.notes
                    .iter()
                    .any(|n| n.item == signed.title && n.detail.to_lowercase().contains("script")),
                "and the dropped pre-request script is reported, not silently lost: {:?}",
                c.notes
            );
        }
    }

    #[test]
    fn imports_requests_headers_and_body() {
        let json = r#"{
          "info": { "name": "demo", "schema": "https://schema.getpostman.com/..v2.1.0" },
          "item": [
            { "name": "folder", "item": [
              { "name": "login", "request": {
                  "method": "POST",
                  "url": { "raw": "{{url}}/login?next=1", "host": ["{{url}}"], "path": ["login"] },
                  "header": [
                    { "key": "Content-Type", "value": "application/json", "type": "text" },
                    { "key": "X-Off", "value": "no", "disabled": true }
                  ],
                  "body": { "mode": "raw", "raw": "{\"u\":\"a\"}" }
              }}
            ]},
            { "name": "form", "request": {
                "method": "POST",
                "url": "{{url}}/upload",
                "body": { "mode": "urlencoded", "urlencoded": [
                  { "key": "a", "value": "1" },
                  { "key": "f", "type": "file", "src": "x" }
                ]}
            }}
          ]
        }"#;
        assert!(looks_like_postman(json));
        let e = import_postman(json);
        assert_eq!(
            e.len(),
            2,
            "folders are flattened into requests, but their path is kept in the title"
        );

        assert_eq!(
            e[0].title, "folder/login",
            "the request's folder path is preserved in its title"
        );
        assert_eq!(e[0].method, "POST");
        assert_eq!(e[0].url, "{{url}}/login?next=1");
        assert_eq!(
            e[0].headers,
            vec![
                (
                    "Content-Type".to_string(),
                    "application/json".to_string(),
                    true
                ),
                ("X-Off".to_string(), "no".to_string(), false),
            ]
        );
        assert_eq!(e[0].body_src.as_deref(), Some("{\"u\":\"a\"}"));

        assert_eq!(e[1].title, "form");
        assert_eq!(
            e[1].form_fields,
            vec![
                FormField {
                    key: "a".into(),
                    value: "1".into(),
                    kind: FormFieldKind::Text,
                    content_type: None,
                    base64_prefix: None,
                    enabled: true,
                    desc: String::new(),
                },
                FormField {
                    key: "f".into(),
                    value: "x".into(),
                    kind: FormFieldKind::File,
                    content_type: None,
                    base64_prefix: None,
                    enabled: true,
                    desc: String::new(),
                },
            ],
            "text and file form-data fields are both imported"
        );
    }

    #[test]
    fn bearer_auth_becomes_a_header() {
        let json = r#"{"info":{},"item":[{"name":"x","request":{
            "method":"GET","url":"{{url}}/me",
            "auth":{"type":"bearer","bearer":[{"key":"token","value":"{{tok}}"}]}
        }}]}"#;
        let e = import_postman(json);
        assert_eq!(e.len(), 1);
        assert!(e[0].headers.contains(&KvRow::toggled(
            "Authorization".to_string(),
            "Bearer {{tok}}".to_string(),
            true
        )));
    }

    #[test]
    fn deeply_nested_folders_build_a_full_slash_separated_path() {
        let json = r#"{"info":{},"item":[
            { "name": "Auth", "item": [
                { "name": "Tokens", "item": [
                    { "name": "Refresh", "request": { "method": "POST", "url": "{{url}}/refresh" } }
                ]},
                { "name": "Login", "request": { "method": "POST", "url": "{{url}}/login" } }
            ]},
            { "name": "Health", "request": { "method": "GET", "url": "{{url}}/health" } }
        ]}"#;
        let e = import_postman(json);
        assert_eq!(e.len(), 3);
        assert_eq!(
            e[0].title, "Auth/Tokens/Refresh",
            "nesting three levels deep joins every folder name"
        );
        assert_eq!(e[1].title, "Auth/Login");
        assert_eq!(
            e[2].title, "Health",
            "a top-level request keeps its bare name"
        );
    }

    #[test]
    fn non_postman_json_is_not_detected() {
        assert!(!looks_like_postman("{\"foo\": 1}"));
        assert!(!looks_like_postman("GET http://x/y\nHTTP 200\n"));
    }

    /// Postman's account backup ("Export all data") and its API wrap each
    /// collection in a `{"collection": …}` envelope instead of exporting the
    /// bare `{"info": …, "item": …}` shape, so both must import.
    #[test]
    fn enveloped_collection_export_is_detected_and_imported() {
        let json = r#"{ "collection": {
          "info": { "name": "demo", "schema": "https://schema.getpostman.com/..v2.1.0" },
          "item": [
            { "name": "login", "request": { "method": "POST", "url": "{{url}}/login" } }
          ]
        }}"#;
        assert!(looks_like_postman(json));
        let e = import_postman(json);
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].title, "login");
        assert_eq!(e[0].method, "POST");
        assert_eq!(e[0].url, "{{url}}/login");
    }

    /// A `collection` key that isn't the envelope (no `item` inside) must not
    /// swallow the real document.
    #[test]
    fn collection_key_that_is_not_an_envelope_is_left_alone() {
        let json = r#"{
          "collection": "some-id",
          "info": { "name": "demo" },
          "item": [ { "name": "ping", "request": { "method": "GET", "url": "http://x/y" } } ]
        }"#;
        assert!(looks_like_postman(json));
        assert_eq!(import_postman(json).len(), 1);
    }

    #[test]
    fn form_field_with_explicit_null_value_still_imports() {
        // Postman routinely emits `"value": null` (and null `src`) for blank
        // `file` form entries. `#[serde(default)]` only covers *absent*
        // fields, so without null-tolerant deserialization a single null
        // would fail the whole collection import.
        let json = r#"{
            "info": {"name": "n"},
            "item": [
                {
                    "name": "upload",
                    "request": {
                        "method": "POST",
                        "url": "http://x/upload",
                        "body": {
                            "mode": "formdata",
                            "formdata": [
                                {"key": "doc", "value": "hi", "type": "text"},
                                {"key": "file", "type": "file", "value": null, "src": null},
                                {"key": "back", "type": "file", "src": "/tmp/a.png"}
                            ]
                        }
                    }
                }
            ]
        }"#;
        let entries = import_postman(json);
        assert_eq!(entries.len(), 1);
        let keys: Vec<&str> = entries[0]
            .form_fields
            .iter()
            .map(|f| f.key.as_str())
            .collect();
        assert_eq!(keys, ["doc", "file", "back"]);
        assert_eq!(entries[0].form_fields[2].value, "/tmp/a.png");
    }

    #[test]
    fn accessor_chains_become_jsonpaths() {
        let roots = vec!["jsonData".to_string()];
        let p = |e: &str| accessor_to_jsonpath(e, &roots);
        assert_eq!(p("jsonData['token']").as_deref(), Some("$.token"));
        assert_eq!(p("jsonData[\"token\"]").as_deref(), Some("$.token"));
        assert_eq!(p("jsonData.a.b").as_deref(), Some("$.a.b"));
        assert_eq!(p("jsonData['a']['b']").as_deref(), Some("$.a.b"));
        assert_eq!(p("jsonData.items[0].id").as_deref(), Some("$.items[0].id"));
        assert_eq!(p("jsonData['a-b']").as_deref(), Some("$['a-b']"));
        // A bare root (unlikely) and anything past a plain accessor chain
        // (method call, arithmetic) is dropped rather than mis-parsed.
        assert_eq!(p("jsonData").as_deref(), Some("$"));
        assert_eq!(p("jsonData.foo()"), None);
        assert_eq!(p("other['x']"), None);
    }

    #[test]
    fn test_script_set_calls_become_captures_with_wildcard_status() {
        let json = r#"{
          "info": {},
          "item": [
            { "name": "login", "request": { "method": "POST", "url": "{{url}}/login" },
              "event": [
                { "listen": "test", "script": { "exec": [
                    "var jsonData = pm.response.json();\r",
                    "pm.environment.set(\"token\", jsonData['token']);",
                    "pm.collectionVariables.set(\"sid\", jsonData.session.id);"
                ]}}
              ]
            }
          ]
        }"#;
        let e = import_postman(json);
        assert_eq!(e.len(), 1);
        assert_eq!(
            e[0].captures,
            vec![
                ("token".to_string(), "jsonpath \"$.token\"".to_string()),
                ("sid".to_string(), "jsonpath \"$.session.id\"".to_string()),
            ]
        );
        // A request that gained captures serializes with a `HTTP *` line (so
        // the [Captures] section parses); the capture rows follow it.
        let text = e[0].to_hurl();
        assert!(text.contains("HTTP *"), "wildcard status expected:\n{text}");
        assert!(text.contains("token: jsonpath \"$.token\""));
    }

    #[test]
    fn imported_request_without_captures_stays_bare() {
        let json =
            r#"{"info":{},"item":[{"name":"x","request":{"method":"GET","url":"{{u}}/a"}}]}"#;
        let e = import_postman(json);
        assert_eq!(e.len(), 1);
        assert!(e[0].captures.is_empty());
        // No captures/asserts → no unsolicited `HTTP *`; hand-adding a section
        // later surfaces a clear "add an HTTP line" parse error instead.
        assert!(
            !e[0].to_hurl().contains("HTTP"),
            "a capture-less import has no response line"
        );
    }

    /// Postman lets you document each header and body parameter. Those notes
    /// used to be dropped on import; they now land in the row's description.
    /// (Query parameters stay in the raw URL, so they have no row to carry.)
    #[test]
    fn postman_parameter_documentation_becomes_a_row_description() {
        let json = r#"{
          "info": { "name": "demo", "schema": "https://schema.getpostman.com/..v2.1.0" },
          "item": [
            { "name": "search", "request": {
                "method": "POST",
                "url": {
                  "raw": "{{url}}/search?q=cats",
                  "host": ["{{url}}"],
                  "path": ["search"],
                  "query": [ { "key": "q", "value": "cats" } ]
                },
                "header": [
                  { "key": "X-Trace", "value": "on", "description": "staging only" }
                ],
                "body": {
                  "mode": "urlencoded",
                  "urlencoded": [
                    { "key": "region", "value": "eu", "description": "which cluster" }
                  ]
                }
            }}
          ]
        }"#;
        let entries = import_postman(json);
        let e = &entries[0];
        assert_eq!(
            e.headers[0].desc, "staging only",
            "the header's Postman documentation should survive the import"
        );
        assert_eq!(
            e.form_fields[0].desc, "which cluster",
            "and so should a form field's"
        );
    }
}

#[cfg(test)]
mod description_tests {
    use super::*;

    fn one(request: &str) -> ConvertedCollection {
        convert_postman(&format!(
            r#"{{ "info": {{ "name": "d", "schema": "https://schema.getpostman.com/..v2.1.0" }},
                 "item": [ {{ "name": "r", "request": {request} }} ] }}"#
        ))
    }

    fn comments(e: &HurlEntry) -> Vec<&str> {
        e.comments.iter().map(|c| c.text.as_str()).collect()
    }

    /// 88 requests in the exports on hand carry prose, and it was the only
    /// explanation of what half of them are for.
    #[test]
    fn a_request_description_is_kept_as_comments() {
        let c = one(r#"{ "method": "GET", "url": "https://h/x",
                 "description": "Returns the current user.\n\nRequires the `read` scope." }"#);
        assert_eq!(
            comments(&c.entries[0]),
            vec![
                "# Returns the current user.",
                "#",
                "# Requires the `read` scope."
            ]
        );
    }

    /// The title is the request's *name* and is what every list in the app
    /// shows, so a paragraph must not end up in it.
    #[test]
    fn the_description_never_becomes_part_of_the_name() {
        let c = one(r#"{ "method": "GET", "url": "https://h/x", "description": "long prose" }"#);
        assert_eq!(c.entries[0].title, "r");
    }

    /// Postman writes the newer descriptions as an object with a media type.
    #[test]
    fn an_object_description_is_read_too() {
        let c = one(r##"{ "method": "GET", "url": "https://h/x",
                 "description": { "content": "Heading", "type": "text/markdown" } }"##);
        assert_eq!(comments(&c.entries[0]), vec!["# Heading"]);
    }

    /// A description that isn't there mustn't leave an empty comment behind.
    #[test]
    fn no_description_adds_nothing() {
        let c = one(r#"{ "method": "GET", "url": "https://h/x" }"#);
        assert!(c.entries[0].comments.is_empty());
    }

    /// Postman writes an explicit `null` for fields it leaves blank, which must
    /// not fail the whole import.
    #[test]
    fn a_null_description_is_survivable() {
        let c = one(r#"{ "method": "GET", "url": "https://h/x", "description": null }"#);
        assert_eq!(c.entries.len(), 1);
        assert!(c.entries[0].comments.is_empty());
    }
}

#[cfg(test)]
mod body_mode_tests {
    use super::*;

    fn post(body: &str) -> ConvertedCollection {
        convert_postman(&format!(
            r#"{{ "info": {{ "name": "d", "schema": "https://schema.getpostman.com/..v2.1.0" }},
                 "item": [ {{ "name": "r", "request": {{ "method": "POST",
                   "url": "https://h/x", "body": {body} }} }} ] }}"#
        ))
    }

    /// GraphQL over HTTP is an ordinary JSON POST; Postman only keeps the query
    /// and its variables apart so they can be edited separately. That made this
    /// a presentation difference the importer was treating as a protocol one.
    #[test]
    fn a_graphql_body_becomes_the_json_post_it_actually_is() {
        let c = post(
            r#"{ "mode": "graphql", "graphql": {
                 "query": "query Q($id: ID){ thing(id: $id) }",
                 "variables": "{ \"id\": \"7\" }" } }"#,
        );
        let e = &c.entries[0];
        let sent: serde_json::Value = serde_json::from_str(e.body_src.as_deref().unwrap()).unwrap();
        assert_eq!(sent["query"], "query Q($id: ID){ thing(id: $id) }");
        assert_eq!(
            sent["variables"]["id"], "7",
            "the variables nest as an object, not as the string Postman stores"
        );
        assert!(
            e.headers
                .iter()
                .any(|h| h.key.eq_ignore_ascii_case("content-type")
                    && h.value.contains("application/json"))
        );
        assert!(
            !c.notes.iter().any(|n| n.detail.contains("graphql")),
            "and nothing was lost to report: {:?}",
            c.notes
        );
    }

    /// Half-written variables are left out rather than sent as a quoted blob
    /// the server would reject.
    #[test]
    fn unparseable_graphql_variables_are_left_out() {
        let c = post(
            r#"{ "mode": "graphql", "graphql": { "query": "{ ping }",
                 "variables": "{ not json" } }"#,
        );
        let sent: serde_json::Value =
            serde_json::from_str(c.entries[0].body_src.as_deref().unwrap()).unwrap();
        assert_eq!(sent["query"], "{ ping }");
        assert!(sent.get("variables").is_none());
    }

    /// A `file,path;` body serialises as valid Hurl and then reads back as
    /// nothing, because PaperBoy's request model has nowhere to keep one. An
    /// import that vanished on the next reload would be worse than a note.
    #[test]
    fn a_file_body_is_reported_rather_than_imported_and_lost() {
        let c = post(r#"{ "mode": "file", "file": { "src": "./payload.bin" } }"#);
        assert_eq!(c.entries[0].body_src, None);
        assert!(
            c.notes.iter().any(|n| n.detail.contains("./payload.bin")),
            "the path is named so it can be attached by hand: {:?}",
            c.notes
        );
    }

    /// Both file bodies in the exports on hand are `{"src": ""}` — the mode was
    /// chosen and a file never was.
    #[test]
    fn a_file_body_with_no_file_is_reported_not_invented() {
        let c = post(r#"{ "mode": "file", "file": { "src": "" } }"#);
        assert_eq!(c.entries[0].body_src, None);
        assert!(
            c.notes
                .iter()
                .any(|n| n.detail.contains("never had one chosen")),
            "{:?}",
            c.notes
        );
    }
}

#[cfg(test)]
mod profile_behavior_tests {
    use super::*;

    fn convert(item: &str) -> ConvertedCollection {
        convert_postman(&format!(
            r#"{{ "info": {{ "name": "d", "schema": "https://schema.getpostman.com/..v2.1.0" }},
                 "item": [ {item} ] }}"#
        ))
    }

    /// Postman strips the body from a GET unless told not to, so a GET stored
    /// with a body is the remains of an edit, not a request that sends one.
    #[test]
    fn a_get_body_postman_would_not_have_sent_is_left_out() {
        let c = convert(
            r#"{ "name": "r", "request": { "method": "GET", "url": "https://h/x",
                 "body": { "mode": "raw", "raw": "{\"stale\":true}" } } }"#,
        );
        assert_eq!(c.entries[0].body_src, None);
        assert!(
            c.notes.iter().any(|n| n.detail.contains("body pruning")),
            "and it says why, since the text is visibly in the export: {:?}",
            c.notes
        );
    }

    /// ...but 318 requests in the exports on hand explicitly turn pruning off,
    /// and those really do send a body on a GET.
    #[test]
    fn disable_body_pruning_keeps_the_body() {
        let c = convert(
            r#"{ "name": "r", "protocolProfileBehavior": { "disableBodyPruning": true },
                 "request": { "method": "GET", "url": "https://h/x",
                 "body": { "mode": "raw", "raw": "{}" } } }"#,
        );
        assert_eq!(c.entries[0].body_src.as_deref(), Some("{}"));
    }

    /// A POST body is never pruned, whatever the setting says.
    #[test]
    fn a_post_body_is_untouched() {
        let c = convert(
            r#"{ "name": "r", "request": { "method": "POST", "url": "https://h/x",
                 "body": { "mode": "raw", "raw": "{}" } } }"#,
        );
        assert_eq!(c.entries[0].body_src.as_deref(), Some("{}"));
    }

    /// The setting is inherited, and Postman overrides field by field rather
    /// than replacing the whole block.
    #[test]
    fn a_folder_can_turn_pruning_off_for_everything_inside_it() {
        let c = convert(
            r#"{ "name": "F", "protocolProfileBehavior": { "disableBodyPruning": true },
                 "item": [ { "name": "r", "request": { "method": "GET", "url": "https://h/x",
                   "body": { "mode": "raw", "raw": "{}" } } } ] }"#,
        );
        assert_eq!(c.entries[0].body_src.as_deref(), Some("{}"));
    }

    /// `strictSSL: false` is a real behavioural setting; without it the request
    /// imports and then simply fails against the box it was written for.
    #[test]
    fn strict_ssl_off_becomes_the_insecure_option() {
        let c = convert(
            r#"{ "name": "r", "protocolProfileBehavior": { "strictSSL": false },
                 "request": { "method": "GET", "url": "https://h/x" } }"#,
        );
        assert!(
            c.entries[0]
                .options
                .contains(&KvRow::toggled("insecure", "true", true))
        );
    }

    /// The default must stay strict — silently disabling certificate checks
    /// would be the worst possible thing to get wrong here.
    #[test]
    fn certificate_checking_stays_on_by_default() {
        let c = convert(r#"{ "name": "r", "request": { "method": "GET", "url": "https://h/x" } }"#);
        assert!(c.entries[0].options.is_empty());
    }

    /// A switched-off parameter is left out of the URL text, so reading the
    /// text alone lost it. It is documentation — the optional filter someone
    /// turns on now and again — and there is a switched-off row for it.
    #[test]
    fn a_disabled_query_parameter_imports_switched_off() {
        let c = convert(
            r#"{ "name": "r", "request": { "method": "GET", "url": {
                 "raw": "https://h/x?page=2",
                 "query": [ { "key": "page", "value": "2" },
                            { "key": "verbose", "value": "true", "disabled": true } ] } } }"#,
        );
        let q = &c.entries[0].queries;
        assert!(
            q.contains(&KvRow::toggled("verbose", "true", false)),
            "the disabled parameter is kept, switched off: {q:?}"
        );
        assert_eq!(
            q.iter().filter(|r| r.key == "page").count(),
            0,
            "and the enabled one is not duplicated — it is already in the URL text"
        );
        assert_eq!(c.entries[0].url, "https://h/x?page=2");
    }
}

#[cfg(test)]
mod awsv4_tests {
    use super::*;

    fn one(auth: &str) -> ConvertedCollection {
        convert_postman(&format!(
            r#"{{
              "info": {{ "name": "d", "schema": "https://schema.getpostman.com/..v2.1.0" }},
              "item": [ {{ "name": "r", "request": {{ "method": "GET",
                "url": "https://api.example.com/v1/x", "auth": {auth} }} }} ]
            }}"#
        ))
    }

    fn option(e: &HurlEntry, name: &str) -> Option<String> {
        e.options
            .iter()
            .find(|o| o.key == name)
            .map(|o| o.value.clone())
    }

    /// A v4 signature covers the method, path, headers and body, so it can only
    /// be computed at send time — which is precisely what Hurl's `aws-sigv4`
    /// option asks curl to do. Every request under this auth used to import
    /// unsigned, with a note saying so.
    #[test]
    fn awsv4_auth_becomes_the_aws_sigv4_option() {
        let c = one(r#"{ "type": "awsv4", "awsv4": [
                 { "key": "accessKey", "value": "AKIA1" },
                 { "key": "secretKey", "value": "s3cret" },
                 { "key": "region", "value": "eu-west-1" },
                 { "key": "service", "value": "execute-api" } ] }"#);
        let e = &c.entries[0];
        assert_eq!(
            option(e, "aws-sigv4").as_deref(),
            Some("aws:amz:eu-west-1:execute-api")
        );
        assert_eq!(option(e, "user").as_deref(), Some("AKIA1:s3cret"));
    }

    /// curl works the region and service out from the hostname, so naming them
    /// blank would be worse than not naming them.
    #[test]
    fn an_unnamed_region_is_left_for_curl_to_infer() {
        let c = one(r#"{ "type": "awsv4", "awsv4": [
                        { "key": "accessKey", "value": "AKIA1" },
                        { "key": "secretKey", "value": "s3cret" } ] }"#);
        assert_eq!(
            option(&c.entries[0], "aws-sigv4").as_deref(),
            Some("aws:amz")
        );
    }

    /// Both AWS-signed collections in the exports on hand are exactly this:
    /// a bare `{"type": "awsv4"}`, with the keys kept somewhere else entirely.
    #[test]
    fn a_bare_aws_auth_block_signs_with_named_variables_and_says_so() {
        let c = one(r#"{ "type": "awsv4" }"#);
        assert_eq!(
            option(&c.entries[0], "user").as_deref(),
            Some("{{aws_access_key_id}}:{{aws_secret_access_key}}")
        );
        assert!(
            c.notes
                .iter()
                .any(|n| n.detail.contains("aws_access_key_id")),
            "the user is told where to put the keys: {:?}",
            c.notes
        );
        assert!(
            !c.notes.iter().any(|n| n.detail.contains("was dropped")),
            "and it is no longer reported as a lost auth type"
        );
    }

    /// Temporary credentials need the session token alongside the signature.
    #[test]
    fn a_session_token_rides_in_its_own_header() {
        let c = one(r#"{ "type": "awsv4", "awsv4": [
                        { "key": "accessKey", "value": "A" },
                        { "key": "secretKey", "value": "B" },
                        { "key": "sessionToken", "value": "tok" } ] }"#);
        assert!(c.entries[0].headers.contains(&KvRow::toggled(
            "x-amz-security-token",
            "tok",
            true
        )));
    }
}

#[cfg(test)]
mod oauth2_tests {
    use super::*;

    /// A folder that authenticates with client credentials, holding two
    /// requests — the real shape from the IDKit exports.
    fn folder_oauth2(extra: &str) -> String {
        format!(
            r#"{{
              "info": {{ "name": "d", "schema": "https://schema.getpostman.com/..v2.1.0" }},
              "item": [
                {{ "name": "Tenant API",
                   "auth": {{ "type": "oauth2", "oauth2": [
                     {{ "key": "accessTokenUrl", "value": "https://id.example.com/v1/token" }},
                     {{ "key": "grant_type", "value": "client_credentials" }},
                     {{ "key": "clientId", "value": "abc" }},
                     {{ "key": "clientSecret", "value": "shh" }},
                     {{ "key": "scope", "value": "read write" }},
                     {{ "key": "tokenType", "value": "Bearer" }}
                     {extra}
                   ] }},
                   "item": [
                     {{ "name": "list", "request": {{ "method": "GET", "url": "https://h/a" }} }},
                     {{ "name": "get", "request": {{ "method": "GET", "url": "https://h/b" }} }}
                   ] }}
              ]
            }}"#
        )
    }

    fn header(e: &HurlEntry, name: &str) -> Option<String> {
        e.headers
            .iter()
            .find(|h| h.key.eq_ignore_ascii_case(name))
            .map(|h| h.value.clone())
    }

    fn field(e: &HurlEntry, key: &str) -> Option<String> {
        e.form_fields
            .iter()
            .find(|f| f.key == key)
            .map(|f| f.value.clone())
    }

    /// The point of the feature: Postman fetches the token itself and never
    /// writes it to the export, so these requests used to import with no
    /// credentials at all. Hurl doesn't need the machinery — a token request is
    /// just a request.
    #[test]
    fn a_folders_client_credentials_auth_becomes_a_token_request() {
        let c = convert_postman(&folder_oauth2(""));
        let titles: Vec<&str> = c.entries.iter().map(|e| e.title.as_str()).collect();
        assert_eq!(
            titles,
            vec![
                "Tenant API/Get access token",
                "Tenant API/list",
                "Tenant API/get"
            ],
            "the token request is generated once, in the folder that declared \
             the auth, ahead of the requests that need it"
        );

        let token = &c.entries[0];
        assert_eq!(token.method, "POST");
        assert_eq!(token.url, "https://id.example.com/v1/token");
        assert_eq!(
            field(token, "grant_type").as_deref(),
            Some("client_credentials")
        );
        assert_eq!(field(token, "scope").as_deref(), Some("read write"));
        assert_eq!(
            token.basic_auth,
            Some(("abc".to_string(), "shh".to_string())),
            "Postman's default client authentication is HTTP Basic"
        );
        assert_eq!(
            token.captures,
            vec![(
                "access_token".to_string(),
                "jsonpath \"$.access_token\"".to_string()
            )]
        );
        assert_eq!(
            token.expected_status,
            Some(200),
            "without this a failed token request captures nothing and every \
             request after it fails for a reason that has scrolled away"
        );

        for e in &c.entries[1..] {
            assert_eq!(
                header(e, "Authorization").as_deref(),
                Some("Bearer {{access_token}}"),
                "{} must actually use the token",
                e.title
            );
        }
    }

    /// Folders repeat the whole configuration rather than pointing at a shared
    /// one, so the same credentials on six folders must not fetch six tokens.
    #[test]
    fn one_token_request_is_generated_per_distinct_configuration() {
        let c = convert_postman(
            r#"{
              "info": { "name": "d", "schema": "https://schema.getpostman.com/..v2.1.0" },
              "item": [
                { "name": "A", "auth": { "type": "oauth2", "oauth2": [
                    { "key": "accessTokenUrl", "value": "https://id/t" },
                    { "key": "grant_type", "value": "client_credentials" },
                    { "key": "clientId", "value": "same" } ] },
                  "item": [ { "name": "x", "request": { "method": "GET", "url": "https://h/x" } } ] },
                { "name": "B", "auth": { "type": "oauth2", "oauth2": [
                    { "key": "accessTokenUrl", "value": "https://id/t" },
                    { "key": "grant_type", "value": "client_credentials" },
                    { "key": "clientId", "value": "same" } ] },
                  "item": [ { "name": "y", "request": { "method": "GET", "url": "https://h/y" } } ] },
                { "name": "C", "auth": { "type": "oauth2", "oauth2": [
                    { "key": "accessTokenUrl", "value": "https://id/t" },
                    { "key": "grant_type", "value": "client_credentials" },
                    { "key": "clientId", "value": "other" } ] },
                  "item": [ { "name": "z", "request": { "method": "GET", "url": "https://h/z" } } ] }
              ]
            }"#,
        );
        let tokens: Vec<&str> = c
            .entries
            .iter()
            .filter(|e| e.title.ends_with("Get access token"))
            .map(|e| e.title.as_str())
            .collect();
        assert_eq!(
            tokens,
            vec!["A/Get access token", "C/Get access token"],
            "identical configurations share a token; a different client gets its own"
        );
        let used = |title: &str| {
            c.entries
                .iter()
                .find(|e| e.title == title)
                .and_then(|e| header(e, "Authorization"))
        };
        assert_eq!(used("A/x"), used("B/y"), "B reuses A's token");
        assert_eq!(used("C/z").as_deref(), Some("Bearer {{access_token_2}}"));
    }

    /// `client_authentication: body` puts the credentials in the form instead
    /// of the Basic header; endpoints differ on which they accept.
    #[test]
    fn body_client_authentication_sends_the_credentials_as_form_fields() {
        let c = convert_postman(&folder_oauth2(
            r#", { "key": "client_authentication", "value": "body" }"#,
        ));
        let token = &c.entries[0];
        assert_eq!(token.basic_auth, None);
        assert_eq!(field(token, "client_id").as_deref(), Some("abc"));
        assert_eq!(field(token, "client_secret").as_deref(), Some("shh"));
    }

    /// Postman can be told to put the token in the query string instead.
    #[test]
    fn add_token_to_query_params_uses_a_query_parameter() {
        let c = convert_postman(&folder_oauth2(
            r#", { "key": "addTokenTo", "value": "queryParams" }"#,
        ));
        let list = c
            .entries
            .iter()
            .find(|e| e.title == "Tenant API/list")
            .unwrap();
        assert_eq!(header(list, "Authorization"), None);
        assert!(
            list.queries
                .contains(&KvRow::toggled("access_token", "{{access_token}}", true))
        );
    }

    /// A browser redirect is not something a file of requests can perform, so
    /// this is reported rather than half-built.
    #[test]
    fn the_authorization_code_grant_is_reported_not_invented() {
        let c =
            convert_postman(&folder_oauth2("").replace("client_credentials", "authorization_code"));
        assert!(
            c.entries
                .iter()
                .all(|e| !e.title.ends_with("Get access token")),
            "nothing is generated for a flow that needs a human"
        );
        assert!(
            c.notes
                .iter()
                .any(|n| n.detail.contains("authorization_code")),
            "but the user is told why: {:?}",
            c.notes
        );
    }

    /// Postman keeps client credentials outside the export, which is most of
    /// the real exports. The request still has to be generated -- with the
    /// secrets as variables, where PaperBoy keeps secrets anyway.
    #[test]
    fn missing_credentials_become_variables_and_a_note() {
        let c = convert_postman(
            r#"{
              "info": { "name": "d", "schema": "https://schema.getpostman.com/..v2.1.0" },
              "item": [ { "name": "x", "request": { "method": "GET", "url": "https://h/x",
                "auth": { "type": "oauth2", "oauth2": [
                  { "key": "accessTokenUrl", "value": "https://id/t" },
                  { "key": "grant_type", "value": "client_credentials" },
                  { "key": "clientId", "value": "" },
                  { "key": "clientSecret", "value": "" } ] } } } ]
            }"#,
        );
        assert_eq!(
            c.entries[0].basic_auth,
            Some((
                "{{oauth_client_id}}".to_string(),
                "{{oauth_client_secret}}".to_string()
            ))
        );
        assert!(
            c.notes.iter().any(|n| n.detail.contains("oauth_client_id")),
            "and it says so rather than leaving a silently unusable request"
        );
    }

    /// A folder can override only the presentation and leave the token
    /// configuration to its parent; there is nothing to fetch.
    #[test]
    fn an_override_with_no_token_url_reuses_the_inherited_token() {
        let c = convert_postman(
            r#"{
              "info": { "name": "d", "schema": "https://schema.getpostman.com/..v2.1.0" },
              "auth": { "type": "oauth2", "oauth2": [
                { "key": "accessTokenUrl", "value": "https://id/t" },
                { "key": "grant_type", "value": "client_credentials" },
                { "key": "clientId", "value": "abc" } ] },
              "item": [
                { "name": "plain", "request": { "method": "GET", "url": "https://h/a" } },
                { "name": "F", "auth": { "type": "oauth2", "oauth2": [
                    { "key": "headerPrefix", "value": "Token " } ] },
                  "item": [ { "name": "y", "request": { "method": "GET", "url": "https://h/y" } } ] }
              ]
            }"#,
        );
        assert_eq!(
            c.entries
                .iter()
                .filter(|e| e.title.ends_with("Get access token"))
                .count(),
            1,
            "the override has no token URL of its own to fetch from"
        );
        let y = c.entries.iter().find(|e| e.title == "F/y").unwrap();
        assert_eq!(
            header(y, "Authorization").as_deref(),
            Some("Token {{access_token}}"),
            "but its prefix override is honoured"
        );
    }

    /// A collection-wide token belongs at the top, not wherever the first
    /// request that uses it happens to live.
    #[test]
    fn a_collection_wide_token_is_not_buried_in_a_folder() {
        let c = convert_postman(
            r#"{
              "info": { "name": "d", "schema": "https://schema.getpostman.com/..v2.1.0" },
              "auth": { "type": "oauth2", "oauth2": [
                { "key": "accessTokenUrl", "value": "https://id/t" },
                { "key": "grant_type", "value": "client_credentials" },
                { "key": "clientId", "value": "abc" } ] },
              "item": [ { "name": "Deep", "item": [ { "name": "Deeper", "item": [
                { "name": "x", "request": { "method": "GET", "url": "https://h/x" } } ] } ] } ]
            }"#,
        );
        assert_eq!(c.entries[0].title, "Get access token");
    }

    /// The `password` grant is also just a POST, so it is generated too.
    #[test]
    fn the_password_grant_is_generated_like_client_credentials() {
        let c = convert_postman(
            r#"{
              "info": { "name": "d", "schema": "https://schema.getpostman.com/..v2.1.0" },
              "item": [ { "name": "x", "request": { "method": "GET", "url": "https://h/x",
                "auth": { "type": "oauth2", "oauth2": [
                  { "key": "accessTokenUrl", "value": "https://id/t" },
                  { "key": "grant_type", "value": "password" },
                  { "key": "clientId", "value": "abc" },
                  { "key": "username", "value": "u" },
                  { "key": "password", "value": "p" } ] } } } ]
            }"#,
        );
        let token = &c.entries[0];
        assert_eq!(field(token, "grant_type").as_deref(), Some("password"));
        assert_eq!(field(token, "username").as_deref(), Some("u"));
        assert_eq!(field(token, "password").as_deref(), Some("p"));
    }
}

#[cfg(test)]
mod path_variable_tests {
    use super::*;

    fn one(url: &str) -> ConvertedCollection {
        convert_postman(&format!(
            r#"{{
              "info": {{ "name": "d", "schema": "https://schema.getpostman.com/..v2.1.0" }},
              "item": [ {{ "name": "r", "request": {{ "method": "GET", "url": {url} }} }} ]
            }}"#
        ))
    }

    /// The bug: importing `raw` alone asked the server for a batch literally
    /// named ":batch_id".
    #[test]
    fn a_declared_path_variable_becomes_a_hurl_variable() {
        let c = one(r#"{ "raw": "{{base}}/v1/batches/:batch_id/add",
                 "variable": [{ "key": "batch_id", "value": "se-28529731" }] }"#);
        assert_eq!(c.entries[0].url, "{{base}}/v1/batches/{{batch_id}}/add");
        assert!(
            c.variables
                .contains(&("batch_id".into(), "se-28529731".into())),
            "and the value Postman would have substituted comes with it"
        );
    }

    /// A colon is legal in a URL, and Postman only substitutes what it declares.
    #[test]
    fn an_undeclared_colon_segment_is_left_alone() {
        let c = one(r#"{ "raw": "http://localhost:8080/v1/:not_declared" }"#);
        assert_eq!(c.entries[0].url, "http://localhost:8080/v1/:not_declared");
        assert!(c.variables.is_empty());
    }

    /// Only whole segments — a port is not a path variable even when a
    /// same-named variable happens to be declared.
    #[test]
    fn a_port_is_never_mistaken_for_a_path_variable() {
        let c = one(r#"{ "raw": "http://host:8080/x/:id",
                 "variable": [{ "key": "id", "value": "7" }] }"#);
        assert_eq!(c.entries[0].url, "http://host:8080/x/{{id}}");
    }

    /// Rewriting the query string too would corrupt values that legitimately
    /// contain a colon.
    #[test]
    fn the_query_string_is_not_rewritten() {
        let c = one(r#"{ "raw": "https://h/x/:id?at=12:30&who=:id",
                 "variable": [{ "key": "id", "value": "7" }] }"#);
        assert_eq!(
            c.entries[0].url, "https://h/x/{{id}}?at=12:30&who=:id",
            "the path placeholder is rewritten; the colons after the `?` are not"
        );
    }

    /// Path variables are per-request but a `.vars` file has one value per
    /// name, so a clash has to be reported rather than quietly picked.
    #[test]
    fn two_requests_declaring_the_same_name_differently_are_reported() {
        let c = convert_postman(
            r#"{
              "info": { "name": "d", "schema": "https://schema.getpostman.com/..v2.1.0" },
              "item": [
                { "name": "a", "request": { "method": "GET", "url": {
                    "raw": "https://h/:id", "variable": [{ "key": "id", "value": "1" }] } } },
                { "name": "b", "request": { "method": "GET", "url": {
                    "raw": "https://h/:id", "variable": [{ "key": "id", "value": "2" }] } } }
              ]
            }"#,
        );
        assert_eq!(c.variables, vec![("id".to_string(), "1".to_string())]);
        assert!(
            c.notes
                .iter()
                .any(|n| n.item == "b" && n.detail.contains("id")),
            "the discarded second value is named, not swallowed: {:?}",
            c.notes
        );
    }

    /// ShipEngine's exports declare the name but leave the value blank. The
    /// request must still be parameterised — and the empty variable is exactly
    /// what the undefined-variable warning is for.
    #[test]
    fn a_declared_variable_with_no_value_still_parameterises_the_url() {
        let c = one(r#"{ "raw": "{{baseUrl}}/v1/batches/:batch_id",
                 "variable": [{ "key": "batch_id", "value": "", "description": "Batch ID" }] }"#);
        assert_eq!(c.entries[0].url, "{{baseUrl}}/v1/batches/{{batch_id}}");
        assert_eq!(c.variables, vec![("batch_id".to_string(), String::new())]);
    }
}

#[cfg(test)]
mod inheritance_tests {
    use super::*;

    /// A collection whose auth lives at the top, is overridden by one folder,
    /// switched off by one request, and left to inherit everywhere else.
    fn nested() -> &'static str {
        r#"{
          "info": { "name": "demo", "schema": "https://schema.getpostman.com/..v2.1.0" },
          "auth": { "type": "bearer", "bearer": [{ "key": "token", "value": "{{TOKEN}}" }] },
          "variable": [
            { "key": "base", "value": "https://api.example.com" },
            { "key": "off", "value": "x", "disabled": true },
            { "key": "", "value": "nameless" }
          ],
          "item": [
            { "name": "inherits", "request": { "method": "GET", "url": "{{base}}/a" } },
            { "name": "opts out", "request": {
                "method": "GET", "url": "{{base}}/b", "auth": { "type": "noauth" } } },
            { "name": "folder",
              "auth": { "type": "basic", "basic": [
                { "key": "username", "value": "u" }, { "key": "password", "value": "p" } ] },
              "item": [
                { "name": "deep", "request": { "method": "GET", "url": "{{base}}/c" } },
                { "name": "own", "request": { "method": "GET", "url": "{{base}}/d",
                    "auth": { "type": "apikey", "apikey": [
                      { "key": "key", "value": "X-Key" },
                      { "key": "value", "value": "{{KEY}}" } ] } } }
              ] }
          ]
        }"#
    }

    fn header(e: &HurlEntry, name: &str) -> Option<String> {
        e.headers
            .iter()
            .find(|h| h.key.eq_ignore_ascii_case(name))
            .map(|h| h.value.clone())
    }

    /// Postman applies the collection's auth to every request that doesn't
    /// declare its own, so a collection that authenticates once at the top must
    /// not import as sixty unauthenticated requests.
    #[test]
    fn a_request_without_auth_inherits_the_collections() {
        let c = convert_postman(nested());
        let e = &c.entries[0];
        assert_eq!(e.title, "inherits");
        assert_eq!(
            header(e, "Authorization").as_deref(),
            Some("Bearer {{TOKEN}}")
        );
    }

    /// `noauth` is Postman's way of saying "not even the inherited one", so it
    /// has to beat the parent rather than being ignored as an empty block.
    #[test]
    fn a_request_can_opt_out_of_the_inherited_auth() {
        let c = convert_postman(nested());
        let e = &c.entries[1];
        assert_eq!(e.title, "opts out");
        assert_eq!(header(e, "Authorization"), None);
        assert_eq!(e.basic_auth, None);
    }

    /// A folder's auth replaces the collection's for everything beneath it,
    /// however deep.
    #[test]
    fn a_folder_overrides_the_collection_for_everything_inside_it() {
        let c = convert_postman(nested());
        let e = c.entries.iter().find(|e| e.title == "folder/deep").unwrap();
        assert_eq!(e.basic_auth, Some(("u".to_string(), "p".to_string())));
        assert_eq!(
            header(e, "Authorization"),
            None,
            "the collection's bearer token doesn't leak past the folder"
        );
    }

    /// The request is the innermost level, so its own auth wins over the
    /// folder's as well as the collection's.
    #[test]
    fn a_requests_own_auth_beats_the_folder_it_is_in() {
        let c = convert_postman(nested());
        let e = c.entries.iter().find(|e| e.title == "folder/own").unwrap();
        assert_eq!(header(e, "X-Key").as_deref(), Some("{{KEY}}"));
        assert_eq!(e.basic_auth, None, "the folder's basic auth was replaced");
    }

    /// An API key can be sent in the query string instead of a header, which
    /// Hurl expresses directly — so it maps rather than being dropped.
    #[test]
    fn an_api_key_in_the_query_string_becomes_a_query_parameter() {
        let json = r#"{
          "info": { "name": "d", "schema": "x" },
          "item": [ { "name": "q", "request": { "method": "GET", "url": "https://x/y",
            "auth": { "type": "apikey", "apikey": [
              { "key": "key", "value": "api_key" },
              { "key": "value", "value": "abc" },
              { "key": "in", "value": "query" } ] } } } ]
        }"#;
        let c = convert_postman(json);
        let e = &c.entries[0];
        assert!(
            e.queries
                .iter()
                .any(|q| q.key == "api_key" && q.value == "abc"),
            "the key rides in the query string: {:?}",
            e.queries
        );
        assert_eq!(header(e, "api_key"), None, "and not in a header as well");
    }

    /// Collection variables are what make `{{base}}` resolve, so they have to
    /// come across — into a `.vars` file, since a `.hurl` has nowhere to put
    /// them. Disabled and nameless ones are dropped, as in an environment.
    #[test]
    fn collection_variables_are_extracted_for_a_vars_file() {
        let vars = convert_postman(nested()).variables;
        assert_eq!(
            vars,
            vec![("base".to_string(), "https://api.example.com".to_string())]
        );
    }

    /// Anything genuinely lost is recorded, so a migration knows what is left
    /// to do by hand instead of finding out at runtime.
    #[test]
    fn what_could_not_be_converted_is_reported() {
        let json = r#"{
          "info": { "name": "d", "schema": "x" },
          "item": [
            { "name": "oauth1", "request": { "method": "GET", "url": "https://x",
                "auth": { "type": "oauth1" } } },
            { "name": "gql", "request": { "method": "POST", "url": "https://x",
                "body": { "mode": "graphql" } } },
            { "name": "scripted", "request": { "method": "GET", "url": "https://x" },
              "event": [ { "listen": "prerequest",
                           "script": { "exec": ["pm.environment.set('t', Date.now())"] } } ] }
          ]
        }"#;
        let notes = convert_postman(json).notes;
        let for_item = |name: &str| {
            notes
                .iter()
                .filter(|n| n.item == name)
                .map(|n| n.detail.clone())
                .collect::<Vec<_>>()
        };
        assert!(
            for_item("oauth1")[0].contains("oauth1"),
            "the auth type that was lost is named: {notes:?}"
        );
        assert!(
            for_item("gql")[0].contains("GraphQL"),
            "an empty GraphQL body has nothing to send, and says so"
        );
        assert!(for_item("scripted")[0].contains("pre-request"));
    }

    /// The bug this whole guard exists for: one Postman dynamic variable
    /// anywhere in a collection produced a `.hurl` that would not parse
    /// ("parsing template variable"), so *every* request in the file was lost
    /// — silently, because the file itself looked fine on disk.
    #[test]
    fn a_generated_variable_is_renamed_so_the_file_still_parses() {
        let json = r#"{
          "info": { "name": "d", "schema": "x" },
          "item": [ { "name": "start", "request": { "method": "POST",
            "url": "https://x/{{$guid}}",
            "header": [ { "key": "X-Run", "value": "{{$timestamp}}" } ],
            "body": { "mode": "raw", "raw": "{\"id\": \"{{$processEnv.HOME}}\"}" } } } ]
        }"#;
        let converted = convert_postman(json);
        let hurl = crate::hurl::collection_to_hurl(&converted.entries);
        assert!(!hurl.contains("{{$"), "a `$` is not a legal name: {hurl}");
        assert!(
            hurl.contains("{{newUuid}}"),
            "a GUID is something Hurl generates itself: {hurl}"
        );
        assert!(hurl.contains("{{timestamp}}"), "{hurl}");
        assert!(
            hurl.contains("# [Gen] 1") && hurl.contains("# timestamp = timestamp"),
            "a Unix timestamp is computed rather than left to be supplied: {hurl}"
        );
        assert!(hurl.contains("{{processEnv_HOME}}"), "{hurl}");
        assert_eq!(
            crate::hurl::parse_hurl(&hurl).len(),
            1,
            "the converted file must read back: {:?}",
            crate::hurl::parse_hurl_error(&hurl)
        );
        // Each rename is reported: the value Postman used to make up now has
        // to come from somewhere.
        assert_eq!(converted.notes.len(), 3);
        assert!(
            converted
                .notes
                .iter()
                .any(|n| n.detail.contains("{{newUuid}}") && n.detail.contains("nothing supplied")),
            "the GUID note says it needs nothing: {:?}",
            converted.notes
        );
        assert!(
            converted
                .notes
                .iter()
                .any(|n| n.detail.contains("{{processEnv_HOME}}")
                    && n.detail.contains("has to be supplied")),
            "the one nothing can produce still says so: {:?}",
            converted.notes
        );
    }

    /// The values PaperBoy can now actually produce. Before the `# [Gen]`
    /// block existed every one of these imported as a variable nobody could
    /// fill in, so the request arrived unrunnable.
    #[test]
    fn the_generated_values_paperboy_can_produce_are_produced() {
        let json = r#"{
          "info": { "name": "d", "schema": "x" },
          "item": [ { "name": "start", "request": { "method": "POST",
            "url": "https://x/?n={{$randomInt}}&u={{$randomUUID}}",
            "header": [ { "key": "X-At", "value": "{{$isoTimestamp}}" } ] } } ]
        }"#;
        let converted = convert_postman(json);
        let entry = &converted.entries[0];
        assert_eq!(
            entry.generators,
            vec![("randomInt".to_string(), "random_int(0, 1000)".to_string())],
            "only the one Hurl can't generate itself needs a row"
        );
        assert!(entry.url.contains("{{newUuid}}"), "{}", entry.url);
        assert_eq!(entry.headers[0].value, "{{newDate}}");

        // Portability is the whole point of preferring the built-ins: the file
        // must still read back as Hurl, block and all.
        let hurl = crate::hurl::collection_to_hurl(&converted.entries);
        let back = crate::hurl::parse_hurl(&hurl);
        assert_eq!(back.len(), 1, "{:?}", crate::hurl::parse_hurl_error(&hurl));
        assert_eq!(back[0].generators, entry.generators, "the block survives");
    }

    /// One `$name` used twice is one row, not two — a name defined twice is a
    /// block whose meaning depends on which row won.
    #[test]
    fn a_generated_value_used_twice_declares_one_row() {
        let json = r#"{
          "info": { "name": "d", "schema": "x" },
          "item": [ { "name": "start", "request": { "method": "POST",
            "url": "https://x/?a={{$timestamp}}&b={{$timestamp}}",
            "header": [ { "key": "X-At", "value": "{{$timestamp}}" } ] } } ]
        }"#;
        let converted = convert_postman(json);
        assert_eq!(
            converted.entries[0].generators,
            vec![("timestamp".to_string(), "timestamp".to_string())]
        );
        assert_eq!(
            converted.notes.len(),
            1,
            "and it is reported once: {:?}",
            converted.notes
        );
    }

    /// Postman keeps a file part that never had a file chosen. It serialized to
    /// `key: file,;`, which is not valid Hurl — and again took the whole
    /// collection down with it.
    #[test]
    fn a_file_part_with_no_file_is_switched_off_rather_than_written_broken() {
        let json = r#"{
          "info": { "name": "d", "schema": "x" },
          "item": [ { "name": "upload", "request": { "method": "POST", "url": "https://x",
            "body": { "mode": "formdata", "formdata": [
              { "key": "document_id", "value": "1", "type": "text" },
              { "key": "front_side_file", "type": "file", "src": "" }
            ] } } } ]
        }"#;
        let converted = convert_postman(json);
        let hurl = crate::hurl::collection_to_hurl(&converted.entries);
        // The row survives as a comment (which parses); what must not appear
        // is a live `file,;` line, which does not.
        assert!(
            !hurl
                .lines()
                .any(|l| !l.trim_start().starts_with('#') && l.contains("file,;")),
            "{hurl}"
        );
        assert_eq!(
            crate::hurl::parse_hurl(&hurl).len(),
            1,
            "the converted file must read back: {:?}",
            crate::hurl::parse_hurl_error(&hurl)
        );
        // The field is still there, switched off, so it can be filled in.
        let back = &crate::hurl::parse_hurl(&hurl)[0];
        let part = back
            .form_fields
            .iter()
            .find(|f| f.key == "front_side_file")
            .expect("the part survives as a disabled row");
        assert!(!part.enabled);
        assert!(
            converted
                .notes
                .iter()
                .any(|n| n.detail.contains("front_side_file")),
            "the switched-off part is reported: {:?}",
            converted.notes
        );
    }

    /// A collection that converts cleanly must produce an empty report, so an
    /// empty report means something.
    #[test]
    fn a_clean_collection_reports_nothing() {
        let json = r#"{
          "info": { "name": "d", "schema": "x" },
          "item": [ { "name": "ok", "request": { "method": "GET", "url": "https://x",
            "header": [ { "key": "Accept", "value": "application/json" } ] } } ]
        }"#;
        assert_eq!(convert_postman(json).notes, vec![]);
    }
}

#[cfg(test)]
mod field_tolerance_tests {
    use super::*;

    /// A collection whose oauth2 block carries the request-parameter lists
    /// Postman writes out as *arrays* — `{"key": "tokenRequestParams",
    /// "value": []}`. Real exports (the IDKit workspaces) all have these.
    fn collection_with(param: &str) -> String {
        format!(
            r#"{{
              "info": {{ "name": "d", "schema": "https://schema.getpostman.com/..v2.1.0" }},
              "auth": {{ "type": "oauth2", "oauth2": [
                {{ "key": "accessTokenUrl", "value": "https://id.example.com/token" }},
                {{ "key": "grant_type", "value": "client_credentials" }},
                {{ "key": "clientId", "value": "abc" }},
                {{ "key": "clientSecret", "value": "shh" }},
                {param}
              ] }},
              "item": [
                {{ "name": "Ping", "request": {{ "method": "GET", "url": "https://x/ping" }} }}
              ]
            }}"#
        )
    }

    /// The regression: a non-string `value` anywhere aborted the *whole*
    /// document, and a failed parse imports as an empty collection — so one
    /// unexpected field silently emptied entire workspaces rather than
    /// degrading the one row it appeared on.
    #[test]
    fn array_valued_auth_param_does_not_empty_the_collection() {
        for value in [r#"[]"#, r#"[{"key": "a", "value": "b"}]"#, r#"{"a": 1}"#] {
            let json = collection_with(&format!(
                r#"{{ "key": "tokenRequestParams", "value": {value}, "type": "any" }}"#
            ));
            let out = convert_postman(&json);
            assert!(
                out.entries.iter().any(|e| e.title == "Ping"),
                "value {value} emptied the collection"
            );
            assert!(
                out.notes
                    .iter()
                    .all(|n| !n.detail.contains("could not be read")),
                "value {value} failed to parse"
            );
        }
    }

    /// Numbers and bools stringify rather than failing — hand-edited and
    /// third-party-generated collections write both.
    #[test]
    fn scalar_valued_fields_stringify() {
        let json = r#"{
          "info": { "name": "d", "schema": "https://schema.getpostman.com/..v2.1.0" },
          "item": [ { "name": "Ping", "request": {
            "method": "GET", "url": "https://x/ping",
            "header": [ { "key": "X-Retry", "value": 3 },
                        { "key": "X-Debug", "value": true } ] } } ]
        }"#;
        let entries = import_postman(json);
        let headers = &entries[0].headers;
        assert_eq!(
            headers.iter().find(|h| h.key == "X-Retry").unwrap().value,
            "3"
        );
        assert_eq!(
            headers.iter().find(|h| h.key == "X-Debug").unwrap().value,
            "true"
        );
    }

    /// A `null` method is still a `GET`, like an absent one.
    #[test]
    fn null_method_falls_back_to_get() {
        let json = r#"{
          "info": { "name": "d", "schema": "https://schema.getpostman.com/..v2.1.0" },
          "item": [ { "name": "Ping", "request": { "method": null, "url": "https://x/ping" } } ]
        }"#;
        assert_eq!(import_postman(json)[0].method, "GET");
    }

    /// When a collection genuinely can't be read, say so — an empty result on
    /// its own is indistinguishable from an empty collection.
    #[test]
    fn unreadable_collection_is_reported_rather_than_silently_empty() {
        let json = r#"{ "info": { "schema": "https://schema.getpostman.com/..v2.1.0" },
                        "item": "not a list" }"#;
        let out = convert_postman(json);
        assert!(out.entries.is_empty());
        assert_eq!(out.notes.len(), 1);
        assert!(out.notes[0].item.is_empty());
        assert!(out.notes[0].detail.contains("could not be read"));
    }

    // ---- regressions found by the Postman-conversion review ---------------

    /// One request carrying `auth`, for the auth-shape regressions below.
    fn with_auth(auth: &str) -> ConvertedCollection {
        convert_postman(&format!(
            r#"{{
              "info": {{ "name": "d", "schema": "https://schema.getpostman.com/..v2.1.0" }},
              "item": [ {{ "name": "r", "request": {{ "method": "GET",
                "url": "https://api.example.com/v1/x", "auth": {auth} }} }} ]
            }}"#
        ))
    }

    /// Two folders that log in as *different users* must not share one token.
    /// The token identity left the credentials out, so the second folder's
    /// requests silently went out as the first folder's user.
    #[test]
    fn two_users_on_one_client_each_get_their_own_token() {
        let json = r#"{"info":{"name":"d","schema":"x"},"item":[
          {"name":"Alice","auth":{"type":"oauth2","oauth2":[
            {"key":"accessTokenUrl","value":"https://id/t"},
            {"key":"grant_type","value":"password"},
            {"key":"clientId","value":"cli"},
            {"key":"username","value":"alice"},
            {"key":"password","value":"alice-pw"}]},
            "item":[{"name":"me","request":{"method":"GET","url":"https://h/me"}}]},
          {"name":"Bob","auth":{"type":"oauth2","oauth2":[
            {"key":"accessTokenUrl","value":"https://id/t"},
            {"key":"grant_type","value":"password"},
            {"key":"clientId","value":"cli"},
            {"key":"username","value":"bob"},
            {"key":"password","value":"bob-pw"}]},
            "item":[{"name":"me","request":{"method":"GET","url":"https://h/me"}}]}]}"#;
        let c = convert_postman(json);
        let tokens: Vec<_> = c
            .entries
            .iter()
            .filter(|e| e.title.ends_with("Get access token"))
            .collect();
        assert_eq!(tokens.len(), 2, "one token request per user");
        let users: Vec<&str> = tokens
            .iter()
            .filter_map(|t| t.form_fields.iter().find(|f| f.key == "username"))
            .map(|f| f.value.as_str())
            .collect();
        assert_eq!(users, vec!["alice", "bob"]);
        let bob = c.entries.iter().find(|e| e.title == "Bob/me").unwrap();
        let alice = c.entries.iter().find(|e| e.title == "Alice/me").unwrap();
        let token_of = |e: &HurlEntry| {
            e.headers
                .iter()
                .find(|h| h.key == "Authorization")
                .map(|h| h.value.clone())
                .unwrap_or_default()
        };
        assert_ne!(
            token_of(bob),
            token_of(alice),
            "each user's requests use their own captured token"
        );
    }

    /// The generated capture must not overwrite a variable the collection
    /// already defines: a capture is written at run time, so reusing the name
    /// silently replaced the user's own value.
    #[test]
    fn a_generated_token_never_takes_a_variable_name_already_in_use() {
        let json = r#"{"info":{"name":"d","schema":"x"},
          "variable":[{"key":"access_token","value":"a-preset-value"}],
          "item":[{"name":"F","auth":{"type":"oauth2","oauth2":[
            {"key":"accessTokenUrl","value":"https://id/t"},
            {"key":"grant_type","value":"client_credentials"},
            {"key":"clientId","value":"cli"}]},
            "item":[{"name":"x","request":{"method":"GET","url":"https://h/x"}}]}]}"#;
        let c = convert_postman(json);
        let tok = c
            .entries
            .iter()
            .find(|e| e.title.ends_with("Get access token"))
            .unwrap();
        let captured = &tok.captures[0].0;
        assert_ne!(
            captured, "access_token",
            "the user's variable is left alone"
        );
        assert!(
            c.variables
                .iter()
                .any(|(k, v)| k == "access_token" && v == "a-preset-value"),
            "and still holds its value"
        );
        let req = c.entries.iter().find(|e| e.title == "F/x").unwrap();
        assert!(
            req.headers
                .iter()
                .any(|h| h.key == "Authorization" && h.value.contains(captured)),
            "the request uses the generated name"
        );
    }

    /// A `pm.environment.set` that was commented out is a capture the script's
    /// author turned off. Running it anyway changes the variables later
    /// requests interpolate — a wrong capture is worse than a missing one.
    #[test]
    fn a_commented_out_capture_is_not_a_capture() {
        let script = r#"[
            "// pm.environment.set(\"token\", jsonData.secret)",
            "/* pm.environment.set(\"blocked\", jsonData.b) */",
            "console.log(\"pm.environment.set('quoted', jsonData.c)\")",
            "pm.environment.set(\"real\", jsonData.ok)"
        ]"#;
        let json = format!(
            r#"{{"info":{{"name":"d","schema":"x"}},"item":[
              {{"name":"t","request":{{"method":"GET","url":"https://h/x"}},
               "event":[{{"listen":"test","script":{{"exec":{script}}}}}]}}]}}"#
        );
        let e = import_postman(&json);
        let names: Vec<&str> = e[0].captures.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["real"], "only the live call is captured");
    }

    /// GraphQL variables are normally a string of JSON, but an export can put
    /// the object straight in — which used to send the operation with no
    /// variables bound at all.
    #[test]
    fn graphql_variables_given_as_an_object_are_kept() {
        let json = r#"{"info":{"name":"d","schema":"x"},"item":[
          {"name":"gql","request":{"method":"POST","url":"https://h/graphql",
            "body":{"mode":"graphql","graphql":{
              "query":"query($id:ID!){user(id:$id){name}}",
              "variables":{"id":"42"}}}}}]}"#;
        let e = import_postman(json);
        let body = e[0].body_src.clone().unwrap();
        let sent: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(sent["variables"]["id"], "42");
    }

    /// A URL stored only as its pieces still has to import as a URL; reading
    /// `raw` alone lost the whole target of the request.
    #[test]
    fn a_url_kept_only_as_pieces_is_rebuilt() {
        let json = r#"{"info":{"name":"d","schema":"x"},"item":[
          {"name":"s","request":{"method":"GET","url":{
            "protocol":"https","host":["api","example","com"],"port":"8443",
            "path":["v1","users"],"query":[{"key":"page","value":"2"}]}}}]}"#;
        let e = import_postman(json);
        assert_eq!(e[0].url, "https://api.example.com:8443/v1/users?page=2");
    }

    /// When `raw` and `query[]` disagree, an enabled parameter listed only in
    /// `query[]` used to be dropped on the assumption `raw` already had it.
    #[test]
    fn an_enabled_query_missing_from_the_url_text_is_added_once() {
        let json = r#"{"info":{"name":"d","schema":"x"},"item":[
          {"name":"d","request":{"method":"GET","url":{
            "raw":"https://h/y?page=1",
            "query":[{"key":"page","value":"1"},{"key":"token","value":"abc"}]}}}]}"#;
        let e = import_postman(json);
        assert_eq!(
            e[0].url, "https://h/y?page=1&token=abc",
            "the missing one is added, the shared one is not duplicated"
        );
    }

    /// A `#` on a Hurl request line starts a comment, so a fragment left in
    /// the URL took the rest of the line with it on the next read. It is not
    /// sent to a server anyway — drop it, but say so.
    #[test]
    fn a_url_fragment_is_dropped_and_reported() {
        let json = r#"{"info":{"name":"d","schema":"x"},"item":[
          {"name":"h","request":{"method":"GET",
            "url":"https://h/search?q=hurl#results"}}]}"#;
        let c = convert_postman(json);
        assert_eq!(c.entries[0].url, "https://h/search?q=hurl");
        assert!(
            c.notes.iter().any(|n| n.detail.contains("#results")),
            "the loss is reported, not silent: {:?}",
            c.notes
        );
        let back = crate::hurl::parse_hurl(&c.entries[0].to_hurl());
        assert_eq!(back.len(), 1, "and the file reads back as one request");
        assert_eq!(back[0].url, "https://h/search?q=hurl");
    }

    /// `aws:amz::s3` names an *empty* region, which is a worse guess than
    /// leaving both off and letting curl infer them from the hostname.
    #[test]
    fn an_aws_service_without_a_region_is_left_to_curl() {
        let c = with_auth(
            r#"{ "type": "awsv4", "awsv4": [
                 { "key": "accessKey", "value": "AKIA1" },
                 { "key": "service", "value": "s3" } ] }"#,
        );
        let sigv4 = c.entries[0]
            .options
            .iter()
            .find(|o| o.key == "aws-sigv4")
            .map(|o| o.value.clone());
        assert_eq!(sigv4.as_deref(), Some("aws:amz"));
    }

    /// The note has to describe what the conversion actually did: the key only
    /// goes to the query string for `in: "query"`.
    #[test]
    fn an_api_key_sent_as_a_header_is_not_announced_as_a_query_parameter() {
        let c = with_auth(
            r#"{ "type": "apikey", "apikey": [
                 { "key": "key", "value": "X-Api-Key" },
                 { "key": "value", "value": "secret" },
                 { "key": "in", "value": "cookie" } ] }"#,
        );
        assert!(
            !c.notes.iter().any(|n| n.detail.contains("query")),
            "no query-string claim for a key that went to a header: {:?}",
            c.notes
        );
    }
}
