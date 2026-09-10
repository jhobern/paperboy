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
// The assert vocabulary is shared with the response-side builder: an assert
// imported from a Postman test and one built by pointing at a response are the
// same thing said twice, and one emitter keeps them spelled identically.
use crate::probe::{Predicate, Subject, assert_line, push_key};

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
    /// Scripts attached to the collection itself. Postman runs these before (or
    /// after) *every* request in it, so they are read here and threaded down —
    /// see [`walk_items`].
    event: Vec<Event>,
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
    /// Pre-request / test scripts. On a folder these apply to everything
    /// inside it, exactly as the collection's do (see [`walk_items`]);
    /// `prerequest` scripts are mined for the assignments a `# [Gen]` block can
    /// carry (see [`generators_from_events`]) and `test` scripts for
    /// `pm.<store>.set(...)` captures and `pm.expect(...)` assertions (see
    /// [`captures_from_events`], [`asserts_from_events`]).
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
#[derive(Clone, Deserialize, Default)]
#[serde(default)]
struct Event {
    #[serde(deserialize_with = "de_str")]
    listen: String,
    script: Script,
    /// Set when this script was declared by an enclosing folder or by the
    /// collection rather than by the request itself. Never deserialized — it is
    /// stamped on by [`inherited_events`] as the script is carried down.
    #[serde(skip)]
    inherited: bool,
    /// The folder breadcrumb of whoever declared it (empty for the collection
    /// itself), so a note about an inherited script can be filed against the
    /// thing that holds it. Ninety-five requests sharing one folder script
    /// share one problem, and ninety-five copies of the note describing it read
    /// as ninety-five problems.
    #[serde(skip)]
    owner: String,
}

#[derive(Clone, Deserialize, Default)]
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
    let mut existing: Vec<&str> = raw
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
        .filter(|q| {
            // Matched off one for one rather than by name alone. A key is
            // allowed to appear more than once -- `?tag=a&tag=b` is how a list
            // is sent -- and treating the name as seen once and for all
            // dropped every repeat after the first, quietly narrowing the
            // request to one value of a set.
            match existing.iter().position(|k| *k == q.key.as_str()) {
                Some(i) => {
                    existing.remove(i);
                    false
                }
                None => true,
            }
        })
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
    /// `"secret"` for a value Postman masks in its own UI. Nothing here can
    /// keep it masked -- a `.vars` file is plain text and so is the session
    /// state -- but it is worth saying so on the way in (see
    /// [`postman_env_secret_keys`]).
    #[serde(rename = "type", default, deserialize_with = "de_str")]
    kind: String,
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
    Some(
        env_values(content)?
            .into_iter()
            .map(|(k, v, _)| (k, v))
            .collect(),
    )
}

/// The keys Postman had marked secret, in the order they appear.
///
/// PaperBoy has nowhere to *keep* a secret literal: a `.vars` file is plain
/// text, and so is the session state it is remembered in. Importing one is
/// therefore a decision about where a secret lives, and the import says so
/// rather than quietly copying it into a second plaintext file -- with the
/// provider references (`{{ op://… }}`, `{{ ssm:… }}`) that *are* the answer
/// named in the note.
pub fn postman_env_secret_keys(content: &str) -> Vec<String> {
    env_values(content)
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, _, secret)| *secret)
        .map(|(k, _, _)| k)
        .collect()
}

/// Every `{{name}}` in an environment value that PaperBoy could not work out:
/// `(variable, the name it asked for)`.
///
/// After [`resolve_env_refs`] has done what it can, a leftover reference is one
/// of two things -- a variable defined somewhere this file doesn't reach (a
/// collection variable, a Postman global), or a loop -- and either way it is
/// worth a word on the way in, because a `.vars` value is not a template and
/// nothing downstream will ever expand it.
pub fn postman_env_unresolved_refs(content: &str) -> Vec<(String, String)> {
    env_values(content)
        .unwrap_or_default()
        .into_iter()
        .flat_map(|(k, v, _)| {
            ENV_REF_RE
                .captures_iter(&v)
                .map(|c| c[1].trim().to_string())
                .filter(|n| !is_provider_reference(n))
                .map(|n| (k.clone(), n))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// A `{{ … }}` anywhere in a value. Postman writes them without spaces, but a
/// hand-edited environment may not, and the moustache is the same one Hurl uses.
static ENV_REF_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{\{\s*([^{}]+?)\s*\}\}").unwrap());

/// Whether `inner` names one of PaperBoy's providers rather than another
/// variable. These are resolved at load time by shelling out to a CLI (see
/// [`crate::environment`]) and must survive the import untouched.
fn is_provider_reference(inner: &str) -> bool {
    inner.starts_with("op://") || inner.starts_with("ssm:") || inner.starts_with("env:")
}

/// Expand `{{name}}` references between an environment's own variables.
///
/// Postman treats a variable's value as a template and resolves it at the
/// moment it is used, so `base_url = {{scheme}}://{{host}}` is ordinary and
/// works. A `.vars` value is not a template -- it is the value -- and nothing
/// downstream expands one, so an imported `{{host}}` would sit there as text
/// (or, when it is the whole value, be classified as an unrecognised provider
/// reference and shown as unresolved). Working them out here is the only place
/// the answer is still known.
///
/// Repeated until nothing changes so a chain resolves, and bounded because a
/// loop otherwise never settles: `a = {{b}}, b = {{a}}` reaches a state where
/// each names itself, which no pass can improve on, and stops. What is left
/// behind is reported by [`postman_env_unresolved_refs`] rather than guessed
/// at.
fn resolve_env_refs(values: &mut [(String, String, bool)]) {
    for _ in 0..8 {
        let known: Vec<(String, String)> = values
            .iter()
            .map(|(k, v, _)| (k.clone(), v.clone()))
            .collect();
        let mut changed = false;
        for (key, value, _) in values.iter_mut() {
            let next = ENV_REF_RE
                .replace_all(value, |c: &regex::Captures| {
                    let name = c[1].trim();
                    // A value naming itself has nowhere to go; leaving the
                    // reference visible is better than an empty string.
                    if name == key || is_provider_reference(name) {
                        return c[0].to_string();
                    }
                    known
                        .iter()
                        .find(|(k, _)| k == name)
                        .map(|(_, v)| v.clone())
                        .unwrap_or_else(|| c[0].to_string())
                })
                .into_owned();
            if next != *value {
                *value = next;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
}

/// The shared read behind [`postman_env_values`] and
/// [`postman_env_secret_keys`]: key, value, and whether Postman called it a
/// secret.
fn env_values(content: &str) -> Option<Vec<(String, String, bool)>> {
    let v = serde_json::from_str::<Value>(content).ok()?;
    let v = unwrap_envelope(v, "environment", "values");
    // A collection also has no `values`, but check `item` too so a document
    // carrying both is never imported as an environment.
    if !v.get("values").is_some_and(Value::is_array) || v.get("item").is_some() {
        return None;
    }
    let env = serde_json::from_value::<PostmanEnv>(v).ok()?;
    let mut values: Vec<(String, String, bool)> = env
        .values
        .into_iter()
        .filter(|v| v.enabled.unwrap_or(true) && !v.key.trim().is_empty())
        .map(|v| {
            let value = v.value.replace(['\n', '\r'], " ");
            let secret = v.kind == "secret";
            (v.key.trim().to_string(), value.trim().to_string(), secret)
        })
        .collect();
    resolve_env_refs(&mut values);
    Some(values)
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
        &inherited_events(&[], &root.event, &[]),
        root.protocol_profile_behavior.unwrap_or_default(),
        &mut tokens,
        &mut out,
    );
    out
}

/// The scripts in force inside a folder (or the collection): everything handed
/// down from above, then this level's own, stamped as inherited because from
/// here on they belong to somebody else.
///
/// Order matters and is Postman's: collection first, then each folder outside
/// in, then the request. A `prerequest` that sets a variable an inner one reads
/// has to have run first, and the same holds for the `[Gen]` rows they become.
fn inherited_events(from_above: &[Event], own: &[Event], owner: &[String]) -> Vec<Event> {
    from_above
        .iter()
        .cloned()
        .chain(own.iter().cloned().map(|mut e| {
            e.inherited = true;
            e.owner = owner.join("/");
            e
        }))
        .collect()
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
///
/// `events` are the scripts every request at this level inherits — the
/// collection's, plus those of each folder passed through. Postman runs a
/// folder's `prerequest` before *every* request inside it, so a folder script
/// computing an id belongs to each of those requests just as surely as one
/// written on the request itself; reading only the leaf's own `event` list lost
/// the whole thing silently.
fn walk_items(
    items: &[Item],
    path: &mut Vec<String>,
    inherited: Option<&Auth>,
    auth_path: &[String],
    events: &[Event],
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
            let here_events = inherited_events(events, &it.event, path);
            walk_items(
                sub,
                path,
                here,
                &here_path,
                &here_events,
                here_profile,
                tokens,
                out,
            );
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
            let events: Vec<Event> = events.iter().cloned().chain(it.event.clone()).collect();
            let mut entry = map_request(&title, req, &events, auth, profile);
            apply_path_variables(&title, &req.url, &mut entry, out);
            apply_oauth2(&title, token_path, auth, &mut entry, tokens, out);
            note_losses(&title, req, &events, auth, profile, &entry, out);
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
        // …and `$randomAlphaNumeric` as exactly one character, which is easy
        // to misread as "some" and is why the length is written out.
        "randomAlphaNumeric" => DynamicFate::Computed("random_alnum(1)"),
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
    // The `[Gen]` rows this entry already carries — from its own pre-request
    // script — so a dynamic variable does not silently reuse one of them.
    let existing: Vec<(String, String)> = entry.generators.clone();
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
                DynamicFate::Computed(expr) => {
                    // The row that computes this value. If a row of the wanted
                    // name already exists — here or in a `[Gen]` block this
                    // entry carries — with a *different* expression, pointing
                    // the placeholder at it would resolve to that other value.
                    // `{{$timestamp}}` (Unix seconds) folding onto a script's
                    // millisecond `timestamp` row is exactly the silent
                    // wrong-value the note then mis-describes. So a fresh name
                    // (`timestamp_1`) is taken and the placeholder points at it.
                    let taken = |n: &str| {
                        existing
                            .iter()
                            .chain(rows.iter())
                            .find(|(name, _)| name == n)
                            .map(|(_, e)| e.clone())
                    };
                    let mut candidate = plain.clone();
                    let mut suffix = 0;
                    loop {
                        match taken(&candidate) {
                            None => {
                                rows.push((candidate.clone(), expr.to_string()));
                                break;
                            }
                            // Same expression already: reuse the row rather than
                            // add a duplicate.
                            Some(e) if e == expr => break,
                            Some(_) => {
                                suffix += 1;
                                candidate = format!("{plain}_{suffix}");
                            }
                        }
                    }
                    candidate
                }
                DynamicFate::Supplied => plain.clone(),
            };
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
    // Scripts. The notes are worded around what *did* carry over, because a
    // partly-translated script is the common case and "dropped" would be a lie
    // about the half that wasn't. A script the request inherited is filed
    // against the folder holding it instead, once — the alternative is the same
    // sentence repeated under every request in the folder, which reads as many
    // problems rather than the one it is.
    //
    // Crucially the kept/residue is computed *per owner*, not over the merged
    // script. The converters run over folder+request together, so when the
    // *folder's* part is what failed to convert, blaming the leftover on the
    // request put "; the rest of it was dropped" under sixteen of FMS's
    // ninety-eight requests whose own scripts convert completely. Each owning
    // event is reduced on its own and filed against whoever wrote it — the same
    // thing the `setNextRequest` loop below already does deliberately.
    let scope = |owner: &Option<String>| match owner {
        None => "this request's",
        Some(_) => "this folder's",
    };
    let reach = |owner: &Option<String>| match owner {
        None => "",
        Some(_) => ", and it runs for every request inside",
    };
    for (owner, group) in owner_groups(events, "prerequest") {
        let (rows, residue) = generators_from_events(&group);
        let detail = if rows.is_empty() {
            // Worth naming the `[Gen]` block even when nothing translated: this
            // note is the moment the user learns the script is gone, and most
            // pre-request scripts are computing a nonce, a stamp or a
            // signature, which the block does.
            format!(
                "{} pre-request script was dropped — Hurl cannot run one{}. If it was computing \
                 a nonce, a timestamp or a signature, a request's `[Gen]` block can do that \
                 instead",
                scope(&owner),
                reach(&owner)
            )
        } else {
            let names: Vec<&str> = rows.iter().map(|(n, _)| n.as_str()).collect();
            let rest = if residue {
                "; the rest of it was dropped, since Hurl cannot run a script"
            } else {
                ""
            };
            format!(
                "{} pre-request script now computes {} in the `[Gen]` block of each request it \
                 covers, once per send{rest}",
                scope(&owner),
                and_list(&names)
            )
        };
        push_note(out, title, owner, detail);
    }
    for (owner, group) in owner_groups(events, "test") {
        let captures = captures_from_events(&group);
        let (status, asserts, residue) = asserts_from_events(&group);
        let mut kept: Vec<String> = Vec::new();
        if !captures.is_empty() {
            kept.push(format!("{} [Captures]", captures.len()));
        }
        if status.is_some() {
            kept.push("the status it expects".to_string());
        }
        if !asserts.is_empty() {
            kept.push(format!("{} [Asserts]", asserts.len()));
        }
        let detail = if kept.is_empty() {
            format!(
                "{} test script was dropped — nothing in it reduced to a Hurl capture or \
                 assertion",
                scope(&owner)
            )
        } else {
            let rest = if residue {
                "; the rest of it was dropped"
            } else {
                ""
            };
            format!(
                "{} test script became {}{rest}",
                scope(&owner),
                and_list(&kept.iter().map(String::as_str).collect::<Vec<_>>())
            )
        };
        push_note(out, title, owner, detail);
    }
    // Said separately, and for both script kinds, because it is not a lost
    // assertion but a lost *order*: a collection whose scripts choose what runs
    // next does not do the same thing when it is run top to bottom, and the
    // requests themselves look perfectly correct while it happens.
    // Read one event at a time rather than the concatenated script, so the note
    // is filed against whoever actually wrote the call — the same per-owner
    // reduction the script notes above now do. Filing an inherited jump against
    // the request that carries its own assertions alongside it would claim the
    // folder's problem as the request's, which is how one folder script once
    // produced eighteen identical notes on a real collection.
    for e in events {
        if e.listen != "prerequest" && e.listen != "test" {
            continue;
        }
        let script = e
            .script
            .exec
            .iter()
            .map(|l| l.trim_end_matches('\r'))
            .collect::<Vec<_>>()
            .join("\n");
        if !script.contains("setNextRequest") {
            continue;
        }
        let owner = e.inherited.then(|| e.owner.clone());
        for detail in next_request_fates(&script, title) {
            push_note(out, title, owner.clone(), detail);
        }
    }
}

// `pm.execution.setNextRequest(` and the older `postman.setNextRequest(`.
static NEXT_CALL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:pm\.execution|postman|pm)\.setNextRequest\s*\(").unwrap());

/// What each `setNextRequest` in a script was doing, said in terms the reader
/// can act on.
///
/// This is a lost *order*, not a lost assertion: the requests themselves look
/// perfectly correct while the collection quietly does something else. PaperBoy
/// runs a collection in file order, and PaperTrail — which does drive requests
/// in a written order — has no branching either, so none of these can be
/// converted. What they can be is named: the four shapes below are four
/// different problems with four different fixes, and the one note they used to
/// share sent every reader looking for the wrong one.
fn next_request_fates(script: &str, title: &str) -> Vec<String> {
    let (code, in_string) = strip_js_noise(script);
    let conditional = conditional_mask(&code);
    // The request's own name as the script would have written it: Postman
    // addresses a request by its bare name, while `title` is the breadcrumb
    // the import builds from the folders around it.
    let own = title.rsplit('/').next().unwrap_or(title).trim();

    let mut out: Vec<String> = Vec::new();
    let mut push = |d: String| {
        if !out.contains(&d) {
            out.push(d);
        }
    };
    for call in find_calls(&code, &in_string, &NEXT_CALL_RE) {
        let Some(arg) = call.args.first().map(|a| a.trim()) else {
            continue;
        };
        let guarded = conditional.get(call.start).copied().unwrap_or(false)
            || !starts_statement(&code, call.start);
        match unquote(arg) {
            Some(name) if name.trim() == own => push(format!(
                "this request ran itself again (`setNextRequest(\"{own}\")`) — that is a polling \
                 loop, which Hurl writes as `[Options] retry: <n>` plus the assert that has to \
                 pass in the end, rather than as a repeated request"
            )),
            Some(name) if guarded => push(format!(
                "a script sometimes jumped to `{name}` instead of carrying on; PaperBoy runs a \
                 collection in file order and has no way to say \"only sometimes\", so check \
                 whether `{name}` is where it needs to be"
            )),
            Some(name) => push(format!(
                "a script always ran `{name}` next, whatever follows this request in the file; \
                 move `{name}` after this request, or write the order out in a PaperTrail flow \
                 (`REQUEST` lines run in the order you write them)"
            )),
            None if arg == "null" => push(
                "a script stopped the run here (`setNextRequest(null)`); nothing after this \
                 request ran, and in PaperBoy it will"
                    .into(),
            ),
            None => push(
                "a script chose the next request by a name it worked out as it ran, so what runs \
                 next isn't in the file at all; PaperBoy runs a collection in file order"
                    .into(),
            ),
        }
    }
    out
}

/// File a note against the request, or — when the script that caused it came
/// from an enclosing folder — against that folder, and only once. `owner` is
/// `None` for a script the request declares itself, otherwise the breadcrumb of
/// the folder (or collection) that does; an empty breadcrumb is the collection.
fn push_note(out: &mut ConvertedCollection, title: &str, owner: Option<String>, detail: String) {
    let item = owner.unwrap_or_else(|| title.to_string());
    if out
        .notes
        .iter()
        .any(|n| n.item == item && n.detail == detail)
    {
        return;
    }
    out.notes.push(ConversionNote { item, detail });
}

/// The `listen` scripts in force here, grouped by who wrote them, so each part
/// can be reduced on its own and its note filed against its owner.
///
/// Each group is `(owner, events)`: `None` for the request's own scripts (the
/// note reads "this request's" and is filed against the request), or
/// `Some(breadcrumb)` for a folder's or the collection's (the note reads "this
/// folder's" and is filed against that folder, once for every request that
/// inherits it). Groups with no actual code are dropped — Postman writes an
/// empty script tab on nearly every request, and a note for each is noise that
/// buries the two that matter.
fn owner_groups(events: &[Event], listen: &str) -> Vec<(Option<String>, Vec<Event>)> {
    let mut groups: Vec<(Option<String>, Vec<Event>)> = Vec::new();
    for e in events.iter().filter(|e| e.listen == listen) {
        let key = e.inherited.then(|| e.owner.clone());
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, group)) => group.push(e.clone()),
            None => groups.push((key, vec![e.clone()])),
        }
    }
    groups
        .into_iter()
        .filter(|(_, group)| has_script(group, listen))
        .collect()
}
/// `a`, `a and b`, `a, b and c` — the readable spelling of a short list.
fn and_list(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [one] => one.to_string(),
        [head @ .., last] => format!("{} and {last}", head.join(", ")),
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
    // The rest of the `test` script: the status it checked and the assertions
    // that reduce to Hurl queries. Only the unconditional ones — see
    // [`asserts_from_events`].
    let (status, asserts, _) = asserts_from_events(events);
    entry.expected_status = status;
    entry.asserts = asserts;
    // And the `prerequest` script's assignments, as the `[Gen]` block that
    // computes them per send.
    let (generators, _) = generators_from_events(events);
    entry.generators = generators;
    entry
}

/// The text of every `listen` script in `events`, in the order Postman runs
/// them: the collection's, then each enclosing folder's outside in, then the
/// request's own.
fn script_text(events: &[Event], listen: &str) -> String {
    events
        .iter()
        .filter(|e| e.listen == listen)
        .flat_map(|e| e.script.exec.iter())
        .map(|l| l.trim_end_matches('\r'))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether any `listen` script actually holds code.
///
/// Postman writes `"exec": [""]` for a script tab that was opened and never
/// typed into, and a collection somebody has clicked through carries one on
/// nearly every request. Testing the *vector* for emptiness therefore announced
/// a dropped pre-request script for every one of them — which is worse than
/// saying nothing, because the two notes that mattered were then buried in
/// thirty about scripts that do not exist.
fn has_script(events: &[Event], listen: &str) -> bool {
    !script_text(events, listen).trim().is_empty()
}

// `var/let/const X = pm.response.json()` — `X` is the parsed-body variable
// whose accessor chains map to jsonpaths.
// `var/let/const X = pm.response.json()`, and the older
// `JSON.parse(responseBody)` that means exactly the same thing -- a legacy
// collection names its body variable that way, and the common name it picks
// (`jsonData`) is only a root here by coincidence.
static JSON_VAR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?:var|let|const)\s+(\w+)\s*=\s*(?:pm\.response\.json\s*\(\s*\)|JSON\.parse\s*\(\s*responseBody\s*\))",
    )
    .unwrap()
});

/// Best-effort scrape of a request's `test` scripts into `[Captures]`: each
/// `pm.<store>.set("NAME", body['a']['b'])` (or `body.a.b`) call where `body`
/// is the `pm.response.json()` variable becomes `NAME = jsonpath "$.a.b"`.
/// Calls that don't reduce to a plain accessor chain are skipped rather than
/// failing the import.
///
/// This uses [`find_calls`] and [`capture_from_call`] — the *same* balanced-
/// paren parser, root list and conditional gate [`asserts_from_events`] uses to
/// decide which `set` calls it has "covered". The two once disagreed: the
/// coverage side added `pm.response.json()` as a root and read the whole call,
/// while this side used a `[^)]+` regex that stopped at the first `)`, so
/// `pm.response.json().token` was marked covered and then captured *nothing* —
/// no capture, and no note either, on a report that claimed a clean import. A
/// single shared decision keeps coverage and conversion from ever diverging.
fn captures_from_events(events: &[Event]) -> Vec<(String, String)> {
    let script = script_text(events, "test");
    if script.trim().is_empty() {
        return Vec::new();
    }
    // Read the script as code, not as text: a `pm.environment.set(...)` line
    // someone commented out is a capture they explicitly turned *off*, and one
    // quoted inside a string is documentation. Matching those produced a
    // capture that runs — and because captures feed the variables later
    // requests interpolate, a spurious `token` capture changes the bytes those
    // requests send. A wrong capture is far worse than a missing one.
    let (code, in_string) = strip_js_noise(&script);
    let conditional = conditional_mask(&code);
    let roots = capture_roots(&code);

    let mut caps: Vec<(String, String)> = Vec::new();
    for call in find_calls(&code, &in_string, &SET_CALL_RE) {
        let Some((name, query)) = capture_from_call(&call, &code, &conditional, &roots) else {
            continue;
        };
        // The script ran top to bottom and the last write won. A name captured
        // twice does not read as two rows: `[Captures]` would store the second
        // over the first anyway, and a note saying "2 [Captures]" for one
        // stored value is a lie. The row is replaced, exactly as the `[Gen]`
        // path does for the same reason.
        match caps.iter_mut().find(|(n, _)| *n == name) {
            Some(row) => row.1 = query,
            None => caps.push((name, query)),
        }
    }
    caps
}

/// The response-body roots a `test` script's accessor chains resolve against:
/// every `var body = pm.response.json()` name, plus the near-universal
/// `jsonData` and the bare `pm.response.json()` (the body with no variable in
/// between). [`captures_from_events`] and [`asserts_from_events`] must build
/// this identically, or a `set` one covers is not a capture the other emits.
fn capture_roots(code: &str) -> Vec<BodyRoot> {
    let mut roots: Vec<BodyRoot> = JSON_VAR_RE
        .captures_iter(code)
        .map(|c| BodyRoot::whole(&c[1]))
        .collect();
    roots.push(BodyRoot::whole("jsonData"));
    roots.push(BodyRoot::whole("pm.response.json()"));

    // A name standing for *part* of the body -- `const data = jsonData.data;`
    // and then `set("id", data.id)` -- is an ordinary way to write a script
    // against a response that nests everything one level down, and every call
    // through such a name used to be dropped as unreadable. Resolved in passes
    // so an alias of an alias (`const first = data.items[0]`) resolves too;
    // the list stops growing quickly, and the cap is only there so a
    // pathological script cannot spin.
    for _ in 0..4 {
        let found: Vec<BodyRoot> = ALIAS_RE
            .captures_iter(code)
            .filter_map(|c| {
                let name = c[1].to_string();
                if roots.iter().any(|r| r.name == name) {
                    return None;
                }
                // Declared twice with different meanings is a question the
                // text cannot answer, so the name is not treated as a root at
                // all rather than resolved to whichever came first.
                if ALIAS_RE
                    .captures_iter(code)
                    .filter(|d| d[1] == name)
                    .count()
                    > 1
                {
                    return None;
                }
                let prefix = accessor_to_jsonpath(&compact_code(&c[2]), &roots)?;
                Some(BodyRoot { name, prefix })
            })
            .collect();
        if found.is_empty() {
            break;
        }
        roots.extend(found);
    }
    roots
}

/// A name a script's accessor chains can be rooted at, and where in the
/// response body it stands for: `$` for the body itself, or the path of the
/// part of it the name was assigned.
#[derive(Debug, Clone)]
struct BodyRoot {
    name: String,
    prefix: String,
}

impl BodyRoot {
    fn whole(name: &str) -> Self {
        BodyRoot {
            name: name.to_string(),
            prefix: "$".to_string(),
        }
    }
}

// `var/let/const X = <chain>` where the chain is an accessor chain off some
// other name -- the declaration that makes `X` stand for part of the body.
static ALIAS_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?:var|let|const)\s+(\w+)\s*=\s*((?:pm\.response\.json\s*\(\s*\)|[A-Za-z_$][\w$]*)(?:\s*\.\s*\w+|\s*\[[^\]\[]*\])+)",
    )
    .unwrap()
});

/// The `[Captures]` row a single `pm.<store>.set(...)` call becomes, or `None`
/// when it cannot become one — the sole authority on which `set` calls turn
/// into captures.
///
/// A call is refused (and so left as residue, to be noted) when it is guarded
/// by a conditional (Hurl captures unconditionally, so a guarded one would run
/// where the guard said it should not — and, worse, error whenever the guarded
/// path was not taken), when its name is not one Hurl can carry (an invalid
/// name is written into the file and then silently deleted the first time it is
/// reopened), or when its value is not a plain accessor chain.
fn capture_from_call(
    call: &CallSite,
    code: &str,
    conditional: &[bool],
    roots: &[BodyRoot],
) -> Option<(String, String)> {
    if call.args.len() != 2 {
        return None;
    }
    if conditional.get(call.start).copied().unwrap_or(false) || !starts_statement(code, call.start)
    {
        return None;
    }
    let name = unquote(call.args[0].trim())?;
    // A name Hurl's grammar rejects is written into `[Captures]` and then
    // dropped the first time the file is reopened, while the note claims it
    // became a capture. The `[Gen]` path already filters the same way.
    if !crate::hurl::is_variable_name(name) {
        return None;
    }
    let path = accessor_to_jsonpath(&compact_code(call.args[1]), roots)?;
    Some((name.to_string(), format!("jsonpath \"{path}\"")))
}

/// One real (unquoted, uncommented) call found in a script: the byte range it
/// occupies and its already-split top-level arguments.
struct CallSite<'a> {
    /// Start of the call's name, e.g. the `p` of `pm.expect(`.
    start: usize,
    /// One past its closing `)`.
    end: usize,
    args: Vec<&'a str>,
}

/// Every call in `code` whose `<name>(` matches `head` — which must end at the
/// `(` — paired with its balanced argument list.
///
/// A regex cannot do this part on its own: `pm.expect(pm.response.json().a)`
/// closes three parens before the one that ends the call, and an argument
/// pattern of `[^)]+` stops at the first of them. That was harmless for the
/// accessor chains [`captures_from_events`] was written for and wrong for
/// anything holding a call, which is most of what a script assigns.
fn find_calls<'a>(code: &'a str, in_string: &[bool], head: &Regex) -> Vec<CallSite<'a>> {
    let mut out = Vec::new();
    for m in head.find_iter(code) {
        if in_string.get(m.start()).copied().unwrap_or(false) {
            continue;
        }
        let open = m.end() - 1;
        let Some(close) = matching_paren(code, open) else {
            continue;
        };
        out.push(CallSite {
            start: m.start(),
            end: close + 1,
            args: split_args(&code[open + 1..close]),
        });
    }
    out
}

/// Index of the `)` closing the `(` at `open`, skipping anything quoted.
fn matching_paren(code: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (i, c) in code[open..].char_indices() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' | '`' => quote = Some(c),
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Split an argument list on its top-level commas — those outside any nested
/// brackets and outside any string.
fn split_args(inner: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut start = 0usize;
    for (i, c) in inner.char_indices() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' | '`' => quote = Some(c),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                out.push(&inner[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    if !inner[start..].trim().is_empty() || !out.is_empty() {
        out.push(&inner[start..]);
    }
    out
}

/// `code` with every whitespace character removed, so a value can be recognised
/// by shape without a table of every way to space it.
fn compact(code: &str) -> String {
    code.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Like [`compact`], but leaves the inside of string literals alone.
///
/// Whitespace only ever muddies the *shape* of an expression, so it is safe to
/// drop between tokens — but a space inside a quoted key or value is data.
/// `pm.expect(jsonData['full name'])` names a field that really is called
/// `full name`, and `pm.response.headers.get('X Weird')` a header that really
/// has a space; compacting straight through the quotes turned both into a query
/// for a field that does not exist, and the assert then failed against a
/// perfectly correct response. This keeps everything inside `'…'`, `"…"` or
/// `` `…` `` verbatim while stripping the whitespace around it.
fn compact_code(code: &str) -> String {
    let mut out = String::with_capacity(code.len());
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for c in code.chars() {
        if let Some(q) = quote {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' | '`' => {
                quote = Some(c);
                out.push(c);
            }
            _ if c.is_whitespace() => {}
            _ => out.push(c),
        }
    }
    out
}

/// Whether any script API call was left untranslated — the signal that the note
/// has to say the rest was dropped. `covered` holds the byte ranges already
/// accounted for.
///
/// Both spellings are looked for: `pm.` is today's sandbox, `postman.` the
/// older one it replaced. A collection written against the old API and left
/// alone since (which is most of what people have to import) is nothing but
/// `postman.` calls, so looking only for `pm.` reported a script that had been
/// entirely dropped as having nothing left in it.
fn has_uncovered_pm_code(code: &str, in_string: &[bool], covered: &[(usize, usize)]) -> bool {
    ["pm.", "postman."].iter().any(|api| {
        code.match_indices(api).any(|(i, _)| {
            !in_string.get(i).copied().unwrap_or(false)
                && !covered.iter().any(|(s, e)| i >= *s && i < *e)
        })
    })
}

/// A call that stores a variable, in either spelling: `pm.<store>.set(` for the
/// stores today's sandbox exposes, and the older
/// `postman.setEnvironmentVariable(` / `postman.setGlobalVariable(` that
/// preceded them. The two take the same two arguments, a name and a value, so
/// everything downstream reads them identically -- and a collection that has
/// not been touched since the old API was current still imports its captures
/// and its `# [Gen]` rows rather than losing every one of them.
static SET_CALL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?:pm\.(?:environment|collectionVariables|globals|variables)\.set|postman\.set(?:Environment|Global)Variable)\s*\(",
    )
    .unwrap()
});

// `var/let/const X = require('uuid')`, so `X.v4()` can be read as a UUID.
static UUID_REQUIRE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:var|let|const)\s+(\w+)\s*=\s*require\s*\(\s*['"]uuid['"]\s*\)"#).unwrap()
});

/// The `# [Gen]` block a `prerequest` script reduces to, and whether anything
/// in it did not reduce.
///
/// Each `pm.<store>.set("NAME", <value>)` whose value has an exact `[Gen]`
/// equivalent becomes a row. That is the whole of the translation, and
/// deliberately so: a pre-request script can do anything, a wrong guess at what
/// one produces sends a plausible-looking request that fails for no visible
/// reason, and the note names what was left behind so the user can write the
/// rest themselves.
///
/// Folder and collection scripts arrive here too (see [`walk_items`]), which is
/// the case that matters most — a folder that stamps every request inside it
/// with a fresh id is the commonest pre-request script there is, and it used to
/// vanish without a word.
fn generators_from_events(events: &[Event]) -> (Vec<(String, String)>, bool) {
    let script = script_text(events, "prerequest");
    if script.trim().is_empty() {
        return (Vec::new(), false);
    }
    let (code, in_string) = strip_js_noise(&script);
    let conditional = conditional_mask(&code);
    let mut aliases: Vec<String> = UUID_REQUIRE_RE
        .captures_iter(&code)
        .map(|c| c[1].to_string())
        .collect();
    aliases.push("uuid".to_string());
    // The `require` line is bookkeeping for a call we do translate, so it is
    // not something left behind.
    let mut covered: Vec<(usize, usize)> = UUID_REQUIRE_RE
        .find_iter(&code)
        .map(|m| (m.start(), m.end()))
        .collect();

    let mut rows: Vec<(String, String)> = Vec::new();
    for call in find_calls(&code, &in_string, &SET_CALL_RE) {
        if call.args.len() != 2 {
            continue;
        }
        // Only unconditional sets become rows, exactly as `[Asserts]` takes only
        // unconditional assertions and for the same reason. Postman's ubiquitous
        // `if (!pm.environment.get('id')) { pm.environment.set('id', uuid()); }`
        // means "set it *once*"; taken unconditionally it becomes a fresh id on
        // every send. A guarded set is left uncovered, so it is noted rather than
        // silently made unconditional.
        if conditional.get(call.start).copied().unwrap_or(false)
            || !starts_statement(&code, call.start)
        {
            continue;
        }
        let Some(name) = unquote(call.args[0].trim()) else {
            continue;
        };
        // A name Hurl reads only part of would be set here and truncated in the
        // placeholder that reads it, which is the failure `[Gen]` names exist to
        // avoid.
        if !crate::hurl::is_variable_name(name) {
            continue;
        }
        let Some(expr) = gen_expression(call.args[1].trim(), &aliases) else {
            continue;
        };
        match rows.iter_mut().find(|(n, _)| n == name) {
            // The script ran top to bottom and the last write won. A block with
            // the same name twice does not: its meaning would depend on which
            // row won, so the row is replaced rather than repeated.
            Some(row) => row.1 = expr,
            None => rows.push((name.to_string(), expr)),
        }
        covered.push((call.start, call.end));
    }
    let residue = has_uncovered_pm_code(&code, &in_string, &covered);
    (rows, residue)
}

/// The `# [Gen]` expression a script's assigned value means *exactly*, or
/// `None` when nothing here means exactly the same thing.
///
/// Short on purpose. Every entry is a value with one obvious equivalent, and
/// the cost of being wrong is asymmetric: a request that refuses to run is
/// found in a second, a request that sends a plausible wrong id is found by
/// whoever reads the server's logs next week.
fn gen_expression(value: &str, uuid_aliases: &[String]) -> Option<String> {
    // A literal is exactly itself: `pm.environment.set("retries", 0)` becomes a
    // row that evaluates to 0 on every send, which is what the script did.
    if let Some(text) = unquote(value) {
        // `[Gen]` has no escape syntax, so a literal carrying a quote or a
        // backslash cannot be written down as one and is left to the note.
        return (!text.contains(['"', '\\'])).then(|| format!("\"{text}\""));
    }
    if is_hurl_number(value) {
        return Some(value.to_string());
    }
    // Whitespace-free, so `new Date().getTime()` and `new Date( ).getTime( )`
    // are one case rather than two. (Which is why the patterns below read
    // `newDate()`.)
    let c = compact(value);
    if uuid_aliases.iter().any(|a| c == format!("{a}.v4()"))
        || c == "require('uuid').v4()"
        || c == "require(\"uuid\").v4()"
        || c == "uuidv4()"
    {
        return Some("uuid".to_string());
    }
    match c.as_str() {
        "Date.now()" | "newDate().getTime()" | "newDate().valueOf()" => {
            Some("timestamp_ms".to_string())
        }
        "Math.floor(Date.now()/1000)"
        | "Math.round(Date.now()/1000)"
        | "Math.floor(newDate().getTime()/1000)"
        | "Math.round(newDate().getTime()/1000)" => Some("timestamp".to_string()),
        "newDate().toISOString()" => Some("iso8601".to_string()),
        _ => replaced_dynamic(&c),
    }
}

/// `pm.variables.replaceIn('{{$guid}}')` — the supported way for a script to
/// ask for one of Postman's dynamic variables. The same handful
/// [`dynamic_fate`] claims are claimed here, for the same reasons.
fn replaced_dynamic(compacted: &str) -> Option<String> {
    let inner = compacted
        .strip_prefix("pm.variables.replaceIn(")?
        .strip_suffix(')')?;
    let name = unquote(inner)?.strip_prefix("{{$")?.strip_suffix("}}")?;
    // Read off `dynamic_fate` rather than repeated, so the two cannot come to
    // disagree about which names PaperBoy claims. The difference is only that
    // this is a *generator expression*, where Hurl's own `{{newUuid}}` is not
    // available — so a `Builtin` is spelled as the equivalent function.
    match dynamic_fate(name) {
        DynamicFate::Builtin("newUuid") => Some("uuid".to_string()),
        DynamicFate::Builtin("newDate") => Some("iso8601".to_string()),
        DynamicFate::Builtin(_) => None,
        DynamicFate::Computed(expr) => Some(expr.to_string()),
        DynamicFate::Supplied => None,
    }
}

// `pm.expect(` — the Chai entry point nearly every Postman assertion goes
// through.
static EXPECT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"pm\.expect\s*\(").unwrap());

// `pm.response.to.have.status(` — the other spelling of a status check.
static STATUS_CALL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"pm\.response\.to\.have\.status\s*\(").unwrap());

// `pm.test("name", () => {` — the wrapper the assertions sit in. Only the head
// is ever matched: covering the whole call would swallow everything inside it,
// including whatever we failed to translate.
static TEST_CALL_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"pm\.test\s*\(").unwrap());

/// The `HTTP <status>` line and `[Asserts]` a `test` script reduces to, plus
/// whether anything in it did not reduce.
///
/// Only *unconditional* assertions are taken. A `pm.expect` inside an `if` is
/// an assertion that sometimes does not apply, and Hurl has no way to say
/// "check this only when…" — importing it as an unconditional assert would turn
/// a passing collection into a failing one, which is the fastest way to teach
/// somebody to ignore assertion failures. Those are reported as dropped
/// instead, along with everything else that does not reduce.
fn asserts_from_events(events: &[Event]) -> (Option<u16>, Vec<String>, bool) {
    let script = script_text(events, "test");
    if script.trim().is_empty() {
        return (None, Vec::new(), false);
    }
    let (code, in_string) = strip_js_noise(&script);
    let conditional = conditional_mask(&code);

    // Built identically to [`captures_from_events`], so that whether a `set`
    // call is "covered" here and whether it becomes a capture there are one
    // decision, not two that can drift apart.
    let roots = capture_roots(&code);

    // Scaffolding that is not "something we failed to translate": the wrapper,
    // the body variable, and the `pm.<store>.set` calls `captures_from_events`
    // has already turned into `[Captures]`.
    let mut covered: Vec<(usize, usize)> = TEST_CALL_RE
        .find_iter(&code)
        .chain(JSON_VAR_RE.find_iter(&code))
        .map(|m| (m.start(), m.end()))
        .collect();
    // A `set` call counts as covered only when it actually becomes a capture —
    // the very same test `captures_from_events` applies. Marking a call covered
    // that then captured nothing (a guarded set, an invalid name, or a value
    // `[^)]+` cut off at the first `)`) left no residue and so no note, on a
    // report that claimed a clean conversion.
    for call in find_calls(&code, &in_string, &SET_CALL_RE) {
        if capture_from_call(&call, &code, &conditional, &roots).is_some() {
            covered.push((call.start, call.end));
        }
    }

    let mut status = None;
    let mut asserts: Vec<String> = Vec::new();

    for call in find_calls(&code, &in_string, &STATUS_CALL_RE) {
        if conditional.get(call.start).copied().unwrap_or(false)
            || !starts_statement(&code, call.start)
        {
            continue;
        }
        let Some(code_num) = call.args.first().and_then(|a| a.trim().parse::<u16>().ok()) else {
            continue;
        };
        // Only the status actually adopted is covered. A second, *different*
        // status is a check the collection made that the import would silently
        // drop — so it is left as residue, and the note says the rest was
        // dropped, rather than quietly asserting the weaker of the two.
        if status.get_or_insert(code_num) == &code_num {
            covered.push((call.start, call.end));
        }
    }

    for call in find_calls(&code, &in_string, &EXPECT_RE) {
        if conditional.get(call.start).copied().unwrap_or(false)
            || !starts_statement(&code, call.start)
            || call.args.len() != 1
        {
            continue;
        }
        let tail_end = statement_end(&code, call.end);
        let Some(subject) = expect_subject(&compact_code(call.args[0]), &roots) else {
            continue;
        };
        // A deep equality against an object or array literal. Hurl has no
        // predicate that takes a document, so it is spelled out one leaf at a
        // time — which the reader gets the better of, since a failure then
        // names the field that differed.
        if let Subject::Json(path) = &subject
            && let Some(value) = deep_expectation(&code[call.end..tail_end])
        {
            let lines = crate::probe::deep_equality(path, &value);
            // An empty expansion is `{}` or a document of nothing but
            // expressions: saying nothing about the body is not the assertion
            // that was written, so it is left as residue to be noted.
            if !lines.is_empty() {
                for (s, p) in lines {
                    if let Some(line) = assert_line(s, p)
                        && !asserts.contains(&line)
                    {
                        asserts.push(line);
                    }
                }
                covered.push((call.start, tail_end));
                continue;
            }
        }
        let Some(predicate) = parse_tail(&code[call.end..tail_end]) else {
            continue;
        };
        match (subject, predicate) {
            (Subject::Status, Predicate::Eq(v)) => match v.parse::<u16>() {
                // As above: a second, differing status is left uncovered so it
                // is noted, rather than silently discarded in favour of the
                // first.
                Ok(n) => {
                    if status.get_or_insert(n) == &n {
                        covered.push((call.start, tail_end));
                    }
                    continue;
                }
                Err(_) => continue,
            },
            (subject, predicate) => match assert_line(subject, predicate) {
                Some(line) => {
                    if !asserts.contains(&line) {
                        asserts.push(line);
                    }
                }
                None => continue,
            },
        }
        covered.push((call.start, tail_end));
    }

    let residue = has_uncovered_pm_code(&code, &in_string, &covered);
    (status, asserts, residue)
}

/// What `pm.expect(<expr>)` is asserting about, for the expressions that name
/// something Hurl can query.
fn expect_subject(compacted: &str, roots: &[BodyRoot]) -> Option<Subject> {
    match compacted {
        "pm.response.code" => return Some(Subject::Status),
        "pm.response.responseTime" => return Some(Subject::Duration),
        _ => {}
    }
    if let Some(rest) = compacted.strip_prefix("pm.response.headers.get(")
        && let Some(inner) = rest.strip_suffix(')')
        && let Some(name) = unquote(inner)
    {
        return Some(Subject::Header(name.to_string()));
    }
    // `.length` is a property of the value, not a key in it: read as a key it
    // would produce `$.errors.length`, a path that matches nothing and an
    // assert that fails for a reason having nothing to do with the response.
    if let Some(head) = compacted.strip_suffix(".length") {
        return accessor_to_jsonpath(head, roots).map(Subject::JsonCount);
    }
    accessor_to_jsonpath(compacted, roots).map(Subject::Json)
}

/// The document a Chai deep-equality tail compares against, when the tail is
/// one and the document is written out in full.
///
/// Only the *deep* spellings count. `.to.equal({…})` in Chai is reference
/// equality, which no two separately-parsed documents ever satisfy, so a
/// collection using it is asserting something that was already failing — and
/// importing it as a deep equality would quietly change what the test means.
fn deep_expectation(tail: &str) -> Option<Value> {
    // `.to.deep.include` is deliberately absent: on an array it means "contains
    // this element", which is not what a per-index expansion says.
    let (head, inner) = split_chai_call(tail)?;
    matches!(
        head.as_str(),
        ".to.eql(" | ".to.deep.equal(" | ".to.deep.eql("
    )
    .then(|| js_literal(inner))
    .flatten()
}

/// Split a Chai tail into its whitespace-free call chain (up to and including
/// the opening bracket) and the argument text *as written*.
///
/// The argument is deliberately not compacted: `compact` removes whitespace
/// everywhere, including inside string literals, so reading
/// `.to.equal("Not Found")` off compacted text asserts `"NotFound"` — an
/// assert that fails on a response that was correct.
fn split_chai_call(tail: &str) -> Option<(String, &str)> {
    let t = tail.trim().trim_end_matches(';').trim_end();
    let inner = t.strip_suffix(')')?;
    let open = inner.find('(')?;
    Some((compact(&inner[..=open]), &inner[open + 1..]))
}

/// Read a Chai tail (`.to.equal("x")`, `.is.not.empty`) as a predicate.
fn parse_tail(tail: &str) -> Option<Predicate> {
    // Longest first: `.to.not.equal(` also starts with `.to.`.
    let calls: [(&str, fn(String) -> Predicate); 12] = [
        (".to.not.be.equal(", Predicate::Ne),
        (".to.not.equal(", Predicate::Ne),
        (".to.not.eql(", Predicate::Ne),
        (".to.deep.equal(", Predicate::Eq),
        (".to.deep.eql(", Predicate::Eq),
        (".to.be.equal(", Predicate::Eq),
        (".to.equal(", Predicate::Eq),
        (".to.eql(", Predicate::Eq),
        (".to.include(", Predicate::Contains),
        (".to.contain(", Predicate::Contains),
        (".to.be.above(", Predicate::Gt),
        (".to.be.below(", Predicate::Lt),
    ];
    if let Some((call, inner)) = split_chai_call(tail) {
        for (head, make) in calls {
            if call == head {
                return hurl_literal(inner).map(make);
            }
        }
    }
    match compact(tail).trim_end_matches(';') {
        ".to.be.empty" | ".is.empty" => Some(Predicate::Empty(true)),
        ".to.not.be.empty" | ".is.not.empty" => Some(Predicate::Empty(false)),
        _ => None,
    }
}

/// A JavaScript literal as the Hurl value it is identical to, or `None` for an
/// expression whose value is not knowable from the text.
fn hurl_literal(value: &str) -> Option<String> {
    let value = value.trim();
    if let Some(text) = unquote(value) {
        // Hurl quoted strings take `\"` and `\\`, but a literal needing them is
        // rare enough that dropping the assert (and saying so) beats getting the
        // escaping subtly wrong. `{{` is refused for the same reason: Hurl would
        // read it as a template and compare against a substituted value, so a
        // literal carrying one is left to the note rather than mis-asserted.
        return (!text.contains(['"', '\\']) && !text.contains("{{"))
            .then(|| format!("\"{text}\""));
    }
    if matches!(value, "true" | "false" | "null") {
        return Some(value.to_string());
    }
    // Only a number Hurl's predicate grammar can actually read. `f64::parse`
    // also accepts `1e3`, `.5`, `Infinity` and `NaN`, none of which Hurl will
    // parse — the assert was written, the note claimed it, and it vanished (or
    // broke the whole file) the first time the collection was reopened.
    is_hurl_number(value).then(|| value.to_string())
}

/// Whether `value` is a number Hurl's predicate grammar accepts: an optional
/// leading `-`, one or more digits, and — if there is a `.` — one or more
/// digits after it. No exponent, no bare `.5`, no `Infinity`/`NaN`.
fn is_hurl_number(value: &str) -> bool {
    let digits = value.strip_prefix('-').unwrap_or(value);
    let mut parts = digits.splitn(2, '.');
    let int = parts.next().unwrap_or("");
    if int.is_empty() || !int.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    match parts.next() {
        None => true,
        Some(frac) => !frac.is_empty() && frac.bytes().all(|b| b.is_ascii_digit()),
    }
}

/// Whether the call at `start` begins its own statement: nothing but whitespace
/// between it and the last `;`, `{`, `}` or newline.
///
/// This is what keeps a conditional assertion out even when it has no block of
/// its own — `if (x) pm.expect(...)` and `x ? pm.expect(...) : y` both put
/// something in front of the call, and both mean "sometimes".
fn starts_statement(code: &str, start: usize) -> bool {
    // "Or the start of the script": a boundary that is *missing* used to read
    // as "nothing in front of it", so the very first line of a script was
    // exempt from the whole rule — `if (ok) pm.expect(…)` written on line one
    // imported as an unconditional assert.
    let boundary = code[..start].rfind([';', '{', '}', '\n']);
    let from = boundary.map_or(0, |i| i + 1);
    if !code[from..start].trim().is_empty() {
        return false;
    }
    // A brace-less guard split over two lines is still a guard:
    //
    // ```js
    // if (jsonData.ok)
    //     pm.expect(...)
    // ```
    //
    // The newline in front of the call looks like the end of a statement, but
    // it is the middle of one. When the boundary is a newline, look back past
    // the whitespace to the real code before it, and if that is the head of a
    // brace-less `if`/`for`/`while`/`else`/`do`, the call is its guarded body.
    if boundary.is_some_and(|i| code.as_bytes()[i] == b'\n') {
        let head = code[..boundary.unwrap()].trim_end();
        if ends_with_guard_head(head) {
            return false;
        }
    }
    true
}

/// Whether `head` — the code on the line(s) before a brace-less body — ends in
/// a guard whose body is what follows: `else`, `do`, or `if`/`for`/`while (…)`.
fn ends_with_guard_head(head: &str) -> bool {
    let word_before = |s: &str| -> String {
        s.chars()
            .rev()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    };
    // A bare `else` or `do` with only its body to come.
    let last_word = word_before(head);
    if matches!(last_word.as_str(), "else" | "do") {
        return true;
    }
    // `if (...)`, `for (...)`, `while (...)`: the keyword is what sits before
    // the parenthesised condition this `)` closes.
    if head.ends_with(')')
        && let Some(open) = matching_open_paren(head)
    {
        let keyword = word_before(head[..open].trim_end());
        return matches!(keyword.as_str(), "if" | "for" | "while");
    }
    false
}

/// The index of the `(` matched by the final `)` of `s`, scanning back and
/// counting depth. Strings have already been left in place by
/// [`strip_js_noise`], so a `)` quoted in one is a rare source of a wrong
/// match — acceptable for a heuristic whose only job is to spot a guard head.
fn matching_open_paren(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    if bytes.last() != Some(&b')') {
        return None;
    }
    let mut depth = 0i32;
    for i in (0..bytes.len()).rev() {
        match bytes[i] {
            b')' => depth += 1,
            b'(' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// One past the end of the statement beginning at `from`: the next `;` or
/// newline outside any brackets or strings.
fn statement_end(code: &str, from: usize) -> usize {
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (i, c) in code[from..].char_indices() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' | '`' => quote = Some(c),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' if depth > 0 => depth -= 1,
            ';' | '\n' if depth == 0 => return from + i,
            ')' | '}' if depth == 0 => return from + i,
            _ => {}
        }
    }
    code.len()
}

/// For each byte of `code`, whether reaching it is conditional on something
/// only the run knows.
///
/// Everything inside a `{ … }` counts as conditional except the body of a
/// `pm.test("…", () => { … })` callback, which runs whenever the script does.
/// The exception is deliberately that narrow. A block guarded by `if` obviously
/// only sometimes runs, but so does the body of a helper —
///
/// ```js
/// const assertMatched = (body) => { pm.expect(body.Result).to.equal("Matched"); };
/// ```
///
/// — which runs only where it is called, and in the collection this was written
/// for it is called in one arm of an if/else whose other arm asserts the
/// opposite. Taking both produced a request asserting that one field equalled
/// two different strings: an assertion that can never pass, on a request that
/// was working perfectly.
fn conditional_mask(code: &str) -> Vec<bool> {
    let mut mask = vec![false; code.len()];
    let mut blocks: Vec<bool> = Vec::new();
    let mut parens: Vec<usize> = Vec::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (i, c) in code.char_indices() {
        let inside = blocks.iter().any(|c| *c);
        for m in mask.iter_mut().skip(i).take(c.len_utf8()) {
            *m = inside;
        }
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' | '`' => quote = Some(c),
            '(' => parens.push(i),
            ')' => {
                parens.pop();
            }
            '{' => blocks.push(!opens_test_callback(code, &parens, i)),
            '}' => {
                blocks.pop();
            }
            _ => {}
        }
    }
    mask
}

/// Whether the `{` at `at` opens the callback body of a `pm.test(...)`.
///
/// `parens` holds the still-open `(`s, so the innermost is the call this block
/// is an argument of — which is what tells a test callback from an arrow
/// function assigned to a variable, whose own parameter list closed before the
/// `{`.
fn opens_test_callback(code: &str, parens: &[usize], at: usize) -> bool {
    let Some(&open) = parens.last() else {
        return false;
    };
    let name: String = code[..open]
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.')
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    if !matches!(name.as_str(), "pm.test" | "pm.it") {
        return false;
    }
    // The text between `pm.test(` and this `{` must be *only* a callback
    // header — the name argument, a comma, and the callback's own signature —
    // and nothing more. Testing merely that it `contains("function(")` and
    // `ends_with(')')` matched the *inner* `{` of an `if` inside a
    // `function () {}` callback too (`("t",function(){if(jsonData.ok)` both
    // contains `function(` and ends with `)`), so the guard block was taken as
    // unconditional and everything in it imported unconditionally. An arrow
    // header ends with `=>`; a `function` header ends right after its own
    // parameter list.
    static TEST_CALLBACK_HEADER: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^\(.*,(?:async)?function\*?\([^)]*\)$").unwrap());
    let between = compact(&code[open..at]);
    between.ends_with("=>") || TEST_CALLBACK_HEADER.is_match(&between)
}

/// Blank out JavaScript comments and regex literals, and report which bytes sit
/// inside a string literal.
///
/// The returned text is the same length as the input — comments and regex
/// literals become spaces, keeping newlines — so a match offset in it means the
/// same thing in the original. The scanner is deliberately small: it
/// understands `//`, `/* */`, `'`/`"`/backtick strings, backslash escapes and
/// `/regex/` literals, which is everything needed to tell "this call is real
/// code" from "this is text or a pattern". It does not try to be a JavaScript
/// parser — a regex over a scripting language is a heuristic either way, and
/// the point here is only to stop the obvious false positives.
///
/// Blanking regex literals is not cosmetic. A pattern like `/\{([^}]*)\}/` holds
/// one `{` and two `}`; read as code, the extra `}` pops the enclosing `if`
/// block in [`conditional_mask`], and every statement after it reads as
/// unconditional. The mirror — an unmatched `{` in `const re = /\{/;` — hides
/// every assertion after it instead. A `/` is taken to open a regex only in
/// expression position (at the start, or after `(`, `,`, `=`, `:`, `[`, `!`,
/// `&`, `|`, `{`, `;`, `?`, or `return`); after a value or `)` it is division.
fn strip_js_noise(script: &str) -> (String, Vec<bool>) {
    #[derive(PartialEq)]
    enum St {
        Code,
        Line,
        Block,
        Str(char),
        Regex,
    }
    let mut out = String::with_capacity(script.len());
    let mut in_string = Vec::with_capacity(script.len());
    let mut st = St::Code;
    let mut escaped = false;
    let mut regex_class = false;
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
                '/' if regex_position(&out, &in_string) => {
                    st = St::Regex;
                    escaped = false;
                    regex_class = false;
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
            St::Regex => {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == '[' {
                    regex_class = true;
                } else if c == ']' {
                    regex_class = false;
                } else if c == '\n' {
                    // An unterminated regex at end of line is a typo (or a `/`
                    // we misjudged); don't swallow the rest of the script.
                    st = St::Code;
                } else if c == '/' && !regex_class {
                    // End of the pattern. Its flags (`gimsuy`) are letters that
                    // would read back as an identifier, so blank them too.
                    while chars.peek().is_some_and(|p| p.is_ascii_alphabetic()) {
                        chars.next();
                        out.push(' ');
                        in_string.push(false);
                    }
                    st = St::Code;
                }
                (false, false)
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

/// Whether a `/` reached with this code emitted so far opens a regex literal
/// rather than being a division. True in expression position: at the very
/// start, after an operator or opener, or after a keyword like `return` that a
/// value follows — false after a value, an identifier, or a closing bracket.
fn regex_position(out: &str, in_string: &[bool]) -> bool {
    // The last emitted character that is real code (not inside a string, not a
    // blanked comment/regex, not whitespace).
    let last = out
        .char_indices()
        .rev()
        .find(|&(i, ch)| !ch.is_whitespace() && !in_string.get(i).copied().unwrap_or(false))
        .map(|(_, ch)| ch);
    match last {
        None => true,
        Some(c) if "(,=:[!&|{;?".contains(c) => true,
        Some(c) if c.is_alphanumeric() || c == '_' || c == '$' => {
            // A keyword that a value follows means expression position; a plain
            // identifier or number means the `/` divides it.
            let word: String = out
                .chars()
                .rev()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            matches!(
                word.as_str(),
                "return"
                    | "typeof"
                    | "instanceof"
                    | "in"
                    | "of"
                    | "do"
                    | "else"
                    | "case"
                    | "void"
                    | "delete"
                    | "new"
                    | "throw"
                    | "yield"
                    | "await"
            )
        }
        Some(_) => false,
    }
}

/// Convert a JS accessor chain rooted at one of `roots`
/// (`body['a'].b["c"][0]`) into a jsonpath (`$.a.b.c[0]`). Returns `None` for
/// anything past a simple `.ident` / `['key']` / `[n]` chain (a method call,
/// arithmetic, …), so an unparseable capture is dropped instead of guessed.
fn accessor_to_jsonpath(expr: &str, roots: &[BodyRoot]) -> Option<String> {
    // Longest name first: `data` and `dataItems` both begin the same way, and
    // matching the shorter one would leave `Items[0]` as the chain.
    let mut by_len: Vec<&BodyRoot> = roots.iter().collect();
    by_len.sort_by_key(|r| std::cmp::Reverse(r.name.len()));
    let (root, mut s) = by_len.iter().find_map(|r| {
        expr.strip_prefix(r.name.as_str())
            .filter(|rest| rest.is_empty() || rest.starts_with(['.', '[']))
            .map(|rest| (*r, rest))
    })?;
    let mut path = root.prefix.clone();
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

/// Strip matching single or double quotes, returning the inner text.
/// A JavaScript object or array literal as the JSON value it stands for, or
/// `None` for anything whose value the text alone doesn't fix.
///
/// `pm.expect(x).to.eql({ id: 1, name: 'a' })` is a perfectly ordinary Postman
/// assertion, and its argument is *nearly* JSON: what stops `serde_json`
/// reading it is unquoted keys, single quotes and a trailing comma. Those three
/// are normalised here. Anything else — a nested expression, a template
/// literal, a function call — makes the whole literal unreadable rather than
/// half-read, because half a deep equality is an assertion nobody wrote.
fn js_literal(src: &str) -> Option<Value> {
    let src = src.trim();
    if !(src.starts_with('{') || src.starts_with('[')) {
        return None;
    }
    let mut out = String::with_capacity(src.len());
    let mut chars = src.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' | '\'' => {
                // Re-emit as a double-quoted JSON string, escaping what JSON
                // requires and un-escaping the `\'` that only mattered inside
                // single quotes.
                let str_start = out.len();
                out.push('"');
                let mut closed = false;
                while let Some((_, d)) = chars.next() {
                    match d {
                        '\\' => {
                            let (_, e) = chars.next()?;
                            match e {
                                '\'' => out.push('\''),
                                '"' => out.push_str("\\\""),
                                other => {
                                    out.push('\\');
                                    out.push(other);
                                }
                            }
                        }
                        d if d == c => {
                            closed = true;
                            break;
                        }
                        '"' => out.push_str("\\\""),
                        '\n' => return None,
                        other => out.push(other),
                    }
                }
                if !closed {
                    return None;
                }
                out.push('"');
                // A `{{…}}` inside the string would be read back as a Hurl
                // template when the leaf became an assert, comparing the
                // response against a *substituted* value (or erroring on an
                // undefined variable) for a body that was correct. `[Gen]`/
                // `[Asserts]` have no way to escape it, so the whole literal is
                // declined and left to the note.
                if out[str_start..].contains("{{") {
                    return None;
                }
            }
            // An unquoted key: a bare identifier immediately followed by `:`.
            c if c.is_ascii_alphabetic() || c == '_' || c == '$' => {
                let start = i;
                let mut end = i + c.len_utf8();
                while let Some(&(j, d)) = chars.peek() {
                    if d.is_ascii_alphanumeric() || d == '_' || d == '$' {
                        end = j + d.len_utf8();
                        chars.next();
                    } else {
                        break;
                    }
                }
                let word = &src[start..end];
                let followed_by_colon = src[end..].trim_start().starts_with(':');
                match word {
                    // Bare words that are values, not keys.
                    "true" | "false" | "null" if !followed_by_colon => out.push_str(word),
                    _ if followed_by_colon => {
                        out.push('"');
                        out.push_str(word);
                        out.push('"');
                    }
                    // A bare identifier used as a *value* is a variable, whose
                    // contents the export doesn't say.
                    _ => return None,
                }
            }
            // A trailing comma before the closing bracket is legal JS and not
            // legal JSON.
            ',' => {
                if src[i + 1..].trim_start().starts_with(['}', ']']) {
                    continue;
                }
                out.push(',');
            }
            other => out.push(other),
        }
    }
    serde_json::from_str(&out).ok()
}

/// A single string literal's inner text, or `None` when `s` is not exactly one
/// string.
///
/// It is not enough that the first and last characters are the same quote: `'a'
/// + x + 'b'` starts and ends with `'` yet is a *concatenation*, and reading it
/// as the one string `a' + x + 'b` fed a wrong value straight into a `[Gen]`
/// row or an assert. So the opening quote is scanned to its real terminator
/// (respecting `\` escapes); only when that terminator is the final character
/// is this one string.
fn unquote(s: &str) -> Option<&str> {
    let bytes = s.as_bytes();
    let quote = *bytes.first()?;
    if quote != b'\'' && quote != b'"' {
        return None;
    }
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate().skip(1) {
        if escaped {
            escaped = false;
        } else if b == b'\\' {
            escaped = true;
        } else if b == quote {
            // The closing quote must be the last byte, or what follows it
            // (`+ x + '…'`, `.trim()`, …) makes this more than one string.
            return (i == bytes.len() - 1).then(|| &s[1..i]);
        }
    }
    None
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
        let roots = vec![BodyRoot::whole("jsonData")];
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

    /// A key is allowed to appear more than once -- `?tag=a&tag=b` is how a
    /// list is sent -- and the merge used to treat a name as accounted for the
    /// first time it saw it, quietly narrowing the request to one value.
    #[test]
    fn a_query_parameter_repeated_in_the_list_keeps_every_value() {
        let json = r#"{"info":{"name":"d","schema":"x"},"item":[
          {"name":"d","request":{"method":"GET","url":{
            "raw":"https://h/y?tag=a",
            "query":[{"key":"tag","value":"a"},{"key":"tag","value":"b"}]}}}]}"#;
        let e = import_postman(json);
        assert_eq!(
            e[0].url, "https://h/y?tag=a&tag=b",
            "the second value of the pair is still sent"
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

/// Postman's pre-request and test scripts, and how much of each one a Hurl
/// request can be made to state on its own.
#[cfg(test)]
mod script_tests {
    use super::*;

    /// Postman writes `"exec": [""]` for a script tab that was opened and left
    /// empty. Treating that as a script produced a note about losing something
    /// that was never there — on a third of a real collection's requests.
    #[test]
    fn an_empty_script_tab_is_not_reported_as_a_lost_script() {
        let json = r#"{"info":{"name":"d","schema":"x"},"item":[
          {"name":"x","event":[{"listen":"prerequest","script":{"exec":[""]}},
                               {"listen":"test","script":{"exec":["",""]}}],
           "request":{"method":"GET","url":"https://h/x"}}]}"#;
        let c = convert_postman(json);
        assert!(
            !c.notes.iter().any(|n| n.detail.contains("script")),
            "nothing was lost, so nothing is claimed: {:?}",
            c.notes
        );
    }

    /// A folder's scripts run for every request inside it, so they have to be
    /// carried down; only the request's own `event` used to be read, which lost
    /// them entirely and without a word.
    #[test]
    fn a_folder_pre_request_script_reaches_every_request_inside_it() {
        let json = r#"{"info":{"name":"d","schema":"x"},"item":[
          {"name":"F","event":[{"listen":"prerequest","script":{"exec":[
             "pm.environment.set('transaction_id', require('uuid').v4());",
             "pm.environment.set('retries', 0);"]}}],
           "item":[{"name":"a","request":{"method":"POST","url":"https://h/a"}},
                   {"name":"b","request":{"method":"POST","url":"https://h/b"}}]}]}"#;
        let c = convert_postman(json);
        for e in &c.entries {
            assert_eq!(
                e.generators,
                vec![
                    ("transaction_id".to_string(), "uuid".to_string()),
                    ("retries".to_string(), "0".to_string())
                ],
                "{} computes the folder's values",
                e.title
            );
        }
        let script_notes: Vec<&ConversionNote> = c
            .notes
            .iter()
            .filter(|n| n.detail.contains("pre-request script"))
            .collect();
        assert_eq!(
            script_notes.len(),
            1,
            "one folder script is one note, not one per request: {:?}",
            c.notes
        );
        assert_eq!(
            script_notes[0].item, "F",
            "filed against the folder that holds it"
        );
    }

    /// The whole point of the translation: the shapes a pre-request script
    /// reaches for most often are exactly the ones `[Gen]` covers.
    #[test]
    fn the_usual_pre_request_computations_become_generators() {
        let json = r#"{"info":{"name":"d","schema":"x"},"item":[
          {"name":"x","event":[{"listen":"prerequest","script":{"exec":[
             "pm.environment.set('id', uuidv4());",
             "pm.collectionVariables.set('ms', Date.now());",
             "pm.variables.set('secs', Math.floor(Date.now() / 1000));",
             "pm.environment.set('when', new Date().toISOString());",
             "pm.environment.set('guid', pm.variables.replaceIn('{{$guid}}'));",
             "pm.environment.set('tries', 0);",
             "pm.environment.set('who', 'alice');"]}}],
           "request":{"method":"GET","url":"https://h/x"}}]}"#;
        let c = convert_postman(json);
        assert_eq!(
            c.entries[0].generators,
            vec![
                ("id".to_string(), "uuid".to_string()),
                ("ms".to_string(), "timestamp_ms".to_string()),
                ("secs".to_string(), "timestamp".to_string()),
                ("when".to_string(), "iso8601".to_string()),
                ("guid".to_string(), "uuid".to_string()),
                ("tries".to_string(), "0".to_string()),
                ("who".to_string(), "\"alice\"".to_string()),
            ]
        );
        let back = crate::hurl::parse_hurl(&c.entries[0].to_hurl());
        assert_eq!(
            back[0].generators, c.entries[0].generators,
            "and the block reads back"
        );
    }

    /// A test script's status check is the one assertion nearly every Postman
    /// collection has, and Hurl states it on the request line.
    #[test]
    fn a_status_assertion_becomes_the_expected_status() {
        let json = r#"{"info":{"name":"d","schema":"x"},"item":[
          {"name":"x","event":[{"listen":"test","script":{"exec":[
             "pm.test('bad request', function () {",
             "    pm.response.to.have.status(400);",
             "});"]}}],
           "request":{"method":"POST","url":"https://h/x"}}]}"#;
        let c = convert_postman(json);
        assert_eq!(c.entries[0].expected_status, Some(400));
    }

    /// Body checks inside a `pm.test` callback are unconditional, so they can
    /// be stated as `[Asserts]`. `.length` is a count in Hurl, not a path.
    #[test]
    fn body_checks_inside_a_test_callback_become_asserts() {
        let json = r#"{"info":{"name":"d","schema":"x"},"item":[
          {"name":"x","event":[{"listen":"test","script":{"exec":[
             "pm.test('shape', () => {",
             "    const b = pm.response.json();",
             "    pm.expect(b.status).to.eql('Matched');",
             "    pm.expect(b.ModelState['body.Image']).to.not.be.empty;",
             "    pm.expect(b.errors.length).to.equal(1);",
             "});"]}}],
           "request":{"method":"POST","url":"https://h/x"}}]}"#;
        let c = convert_postman(json);
        assert_eq!(
            c.entries[0].asserts,
            vec![
                "jsonpath \"$.status\" == \"Matched\"".to_string(),
                "jsonpath \"$.ModelState['body.Image']\" not isEmpty".to_string(),
                "jsonpath \"$.errors\" count == 1".to_string(),
            ]
        );
        let back = crate::hurl::parse_hurl(&c.entries[0].to_hurl());
        assert_eq!(back[0].asserts, c.entries[0].asserts, "and they read back");
    }

    /// The trap that a first pass fell into: a helper that asserts one thing
    /// and a helper that asserts its opposite are both *called* from branches,
    /// so hoisting either into `[Asserts]` makes the request fail whichever way
    /// the response goes. Only a `pm.test` callback body is unconditional.
    #[test]
    fn assertions_the_script_only_sometimes_runs_are_not_hoisted() {
        let json = r#"{"info":{"name":"d","schema":"x"},"item":[
          {"name":"x","event":[{"listen":"test","script":{"exec":[
             "const matched = (b) => { pm.expect(b.result).to.eql('Matched'); };",
             "const notMatched = (b) => { pm.expect(b.result).to.eql('NotMatched'); };",
             "const b = pm.response.json();",
             "if (pm.environment.get('expect') === 'yes') { matched(b); } else { notMatched(b); }",
             "if (b.code) pm.expect(b.code).to.eql(2);"]}}],
           "request":{"method":"POST","url":"https://h/x"}}]}"#;
        let c = convert_postman(json);
        assert!(
            c.entries[0].asserts.is_empty(),
            "nothing here always holds: {:?}",
            c.entries[0].asserts
        );
        assert!(
            c.notes
                .iter()
                .any(|n| n.detail.contains("test script was dropped")),
            "and the user is told, so they can assert it by hand: {:?}",
            c.notes
        );
    }

    /// A collection whose scripts pick the next request does not do the same
    /// thing run top to bottom, and every request in it still looks right.
    ///
    /// A *guarded* jump is the "only sometimes" case, which neither file order
    /// nor a PaperTrail flow can express.
    #[test]
    fn a_script_choosing_the_next_request_is_reported_as_lost_order() {
        let notes = next_request_notes(
            "x",
            "if (pm.response.code === 202) pm.execution.setNextRequest('poll');",
        );
        assert!(
            notes
                .iter()
                .any(|d| d.contains("sometimes jumped to `poll`") && d.contains("file order")),
            "{notes:?}"
        );
    }

    /// The four shapes are four different problems. Lumping them into one note
    /// sent the reader looking for the wrong fix — most of all for a polling
    /// loop, which Hurl expresses directly and which the old note never
    /// mentioned.
    #[test]
    fn a_request_that_reran_itself_is_named_as_a_polling_loop() {
        let notes = next_request_notes(
            "get_result",
            "if (!done) { pm.execution.setNextRequest('get_result'); }",
        );
        assert!(
            notes
                .iter()
                .any(|d| d.contains("polling loop") && d.contains("retry")),
            "{notes:?}"
        );
    }

    /// An unconditional jump *is* an order, and an order is the one thing here
    /// that can be written down — so the note says where.
    #[test]
    fn an_unconditional_jump_is_reported_as_an_order_to_write_down() {
        let notes = next_request_notes("submit", "pm.execution.setNextRequest('get_result');");
        assert!(
            notes
                .iter()
                .any(|d| d.contains("always ran `get_result` next") && d.contains("PaperTrail")),
            "{notes:?}"
        );
    }

    /// `setNextRequest(null)` ends the run. On import the requests after it
    /// start running, which is a behaviour change in the direction of doing
    /// *more*, so it is worth its own sentence.
    #[test]
    fn stopping_the_run_is_reported_as_the_requests_after_it_now_running() {
        let notes = next_request_notes("x", "pm.execution.setNextRequest(null);");
        assert!(
            notes.iter().any(|d| d.contains("stopped the run here")),
            "{notes:?}"
        );
    }

    /// A name built at run time isn't in the file at all, so there is nothing
    /// to reorder — a different problem from a jump to a name we can see.
    #[test]
    fn a_computed_next_request_name_is_reported_as_not_being_in_the_file() {
        let notes = next_request_notes("x", "pm.execution.setNextRequest('Test Case ' + (n + 1));");
        assert!(
            notes.iter().any(|d| d.contains("worked out as it ran")),
            "{notes:?}"
        );
    }

    /// The old note fired once per script; these fire once per distinct shape,
    /// so a loop written twice doesn't read as two findings.
    #[test]
    fn the_same_jump_twice_is_one_note() {
        let notes = next_request_notes(
            "x",
            "pm.execution.setNextRequest('a');\npm.execution.setNextRequest('a');",
        );
        assert_eq!(notes.len(), 1, "{notes:?}");
    }

    /// Postman documents `$randomAlphaNumeric` as *one* character. Left
    /// unclaimed it became a variable the user had to supply, on a request that
    /// otherwise ran on import.
    #[test]
    fn random_alpha_numeric_is_computed_rather_than_left_to_be_supplied() {
        let json = r#"{"info":{"name":"d","schema":"x"},"item":[
          {"name":"x","request":{"method":"GET","url":"https://h/x?c={{$randomAlphaNumeric}}"}}]}"#;
        let c = convert_postman(json);
        assert!(
            c.entries[0].generators.contains(&(
                "randomAlphaNumeric".to_string(),
                "random_alnum(1)".to_string()
            )),
            "{:?}",
            c.entries[0].generators
        );
    }

    /// The `pm.variables.replaceIn` table used to be a second, hand-kept copy
    /// of the same list, so a name added to one was silently absent from the
    /// other.
    #[test]
    fn replace_in_claims_the_same_names_as_a_plain_placeholder() {
        let json = r#"{"info":{"name":"d","schema":"x"},"item":[
          {"name":"x","event":[{"listen":"prerequest","script":{"exec":[
             "pm.environment.set('c', pm.variables.replaceIn('{{$randomAlphaNumeric}}'));"]}}],
           "request":{"method":"GET","url":"https://h/x"}}]}"#;
        let c = convert_postman(json);
        assert!(
            c.entries[0]
                .generators
                .contains(&("c".to_string(), "random_alnum(1)".to_string())),
            "{:?}",
            c.entries[0].generators
        );
    }

    // ── Deep equality ──────────────────────────────────────────────────

    /// `.to.eql({…})` is an ordinary Postman assertion and its argument is not
    /// a scalar, so the whole test used to be dropped. Hurl has no predicate
    /// that takes a document, so it is spelled out one leaf at a time — which
    /// also makes the failure name the field that differed.
    #[test]
    fn a_deep_equality_against_an_object_becomes_one_assert_per_leaf() {
        let asserts = asserts_of(
            "pm.test('t', () => { pm.expect(pm.response.json().user).to.eql({ id: 7, name: 'Ada' }); });",
        );
        assert!(
            asserts.contains(&"jsonpath \"$.user.id\" == 7".to_string()),
            "{asserts:?}"
        );
        assert!(
            asserts.contains(&"jsonpath \"$.user.name\" == \"Ada\"".to_string()),
            "{asserts:?}"
        );
    }

    /// An array's length is pinned as well as its elements: per-index asserts
    /// alone pass on a longer list that happens to start the same way, which is
    /// not what a deep equality says.
    #[test]
    fn a_deep_equality_against_an_array_pins_its_length_too() {
        let asserts = asserts_of(
            "pm.test('t', () => { pm.expect(pm.response.json().ids).to.eql([1, 2]); });",
        );
        assert!(
            asserts.contains(&"jsonpath \"$.ids\" count == 2".to_string()),
            "{asserts:?}"
        );
        assert!(
            asserts.contains(&"jsonpath \"$.ids[0]\" == 1".to_string()),
            "{asserts:?}"
        );
    }

    /// A literal holding a variable says nothing the file can be checked
    /// against, and half a deep equality is an assertion nobody wrote — so it
    /// is dropped whole and noted, not partly imported.
    #[test]
    fn a_deep_equality_holding_an_expression_is_not_half_imported() {
        let asserts = asserts_of(
            "pm.test('t', () => { pm.expect(pm.response.json().user).to.eql({ id: expectedId }); });",
        );
        assert!(asserts.is_empty(), "{asserts:?}");
    }

    /// Chai's `.to.equal` on an object is *reference* equality, which two
    /// separately-parsed documents never satisfy. Importing it as a deep
    /// equality would quietly change what the test means.
    #[test]
    fn a_shallow_equal_against_an_object_is_not_read_as_a_deep_one() {
        let asserts = asserts_of(
            "pm.test('t', () => { pm.expect(pm.response.json().user).to.equal({ id: 7 }); });",
        );
        assert!(asserts.is_empty(), "{asserts:?}");
    }

    /// `compact` drops whitespace everywhere, including inside string
    /// literals, so reading a tail off compacted text turned
    /// `.to.equal("Not Found")` into an assert for `"NotFound"` — one that
    /// fails on a perfectly correct response.
    #[test]
    fn a_space_inside_an_expected_string_survives() {
        let asserts = asserts_of(
            "pm.test('t', () => { pm.expect(pm.response.json().msg).to.equal('Not Found'); });",
        );
        assert_eq!(
            asserts,
            vec!["jsonpath \"$.msg\" == \"Not Found\"".to_string()]
        );
    }

    fn asserts_of(script: &str) -> Vec<String> {
        let json = format!(
            r#"{{"info":{{"name":"d","schema":"x"}},"item":[
              {{"name":"x","event":[{{"listen":"test","script":{{"exec":[{}]}}}}],
               "request":{{"method":"GET","url":"https://h/x"}}}}]}}"#,
            serde_json::to_string(script).unwrap()
        );
        convert_postman(&json).entries[0].asserts.clone()
    }

    /// The jump lives in the folder's script, but each request also carries a
    /// test script of its own — so the "does this request have a script?"
    /// question said the folder's problem belonged to every request under it.
    /// On the collection this was found in, one folder script produced
    /// eighteen identical notes.
    #[test]
    fn an_inherited_jump_is_reported_once_against_the_folder() {
        let json = r#"{"info":{"name":"d","schema":"x"},"item":[
          {"name":"F","event":[{"listen":"test","script":{"exec":[
             "pm.execution.setNextRequest('poll');"]}}],
           "item":[
             {"name":"a","event":[{"listen":"test","script":{"exec":[
                "pm.test('t', () => { pm.response.to.have.status(200); });"]}}],
              "request":{"method":"GET","url":"https://h/a"}},
             {"name":"b","event":[{"listen":"test","script":{"exec":[
                "pm.test('t', () => { pm.response.to.have.status(200); });"]}}],
              "request":{"method":"GET","url":"https://h/b"}}]}]}"#;
        let c = convert_postman(json);
        let jumps: Vec<&ConversionNote> = c
            .notes
            .iter()
            .filter(|n| n.detail.contains("`poll`"))
            .collect();
        assert_eq!(jumps.len(), 1, "{:?}", c.notes);
        assert_eq!(jumps[0].item, "F", "filed against the folder that wrote it");
    }

    fn next_request_notes(title: &str, script: &str) -> Vec<String> {
        super::next_request_fates(script, title)
    }
}

/// Regressions for fourteen defects the Postman importer once had, each of
/// which either sent or asserted a value the collection never meant, or filed
/// a loss against the wrong item. Every test asserts the *correct* behaviour
/// and keeps a negative control alongside it, so a fix that over-corrects fails
/// here just as surely as one that under-corrects.
#[cfg(test)]
mod defect_regressions {
    use super::*;

    /// Postman treats an environment value as a template and expands it when it
    /// is used, so `base_url = {{scheme}}://{{host}}` is ordinary there. A
    /// `.vars` value is not a template and nothing downstream expands one, so
    /// the reference has to be worked out on the way in -- and a value that was
    /// *only* a reference used to arrive classified as an unrecognised provider
    /// reference and shown as unresolved.
    #[test]
    fn an_environment_value_referring_to_another_is_worked_out_on_import() {
        let env = r#"{"values":[
            {"key":"scheme","value":"https"},
            {"key":"host","value":"api.example.com"},
            {"key":"base_url","value":"{{scheme}}://{{host}}/v1"},
            {"key":"same","value":"{{host}}"},
            {"key":"secret","value":"{{ op://Vault/api/token }}"}
        ]}"#;
        let got = postman_env_values(env).unwrap();
        let value = |k: &str| {
            got.iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.clone())
                .unwrap()
        };
        assert_eq!(value("base_url"), "https://api.example.com/v1");
        assert_eq!(
            value("same"),
            "api.example.com",
            "a value that is nothing but a reference is the referenced value"
        );
        assert_eq!(
            value("secret"),
            "{{ op://Vault/api/token }}",
            "a provider reference is not a variable reference and is left alone"
        );
        assert!(postman_env_unresolved_refs(env).is_empty());
    }

    /// Chains resolve; loops and names this file cannot reach are left visible
    /// and reported, because an empty string here is a request that goes to the
    /// wrong place rather than one that plainly fails.
    #[test]
    fn a_chain_resolves_and_what_cannot_be_resolved_is_reported() {
        let env = r#"{"values":[
            {"key":"a","value":"{{b}}"},
            {"key":"b","value":"{{c}}"},
            {"key":"c","value":"end"},
            {"key":"loop1","value":"{{loop2}}"},
            {"key":"loop2","value":"{{loop1}}"},
            {"key":"elsewhere","value":"{{from_the_collection}}/path"}
        ]}"#;
        let got = postman_env_values(env).unwrap();
        let value = |k: &str| {
            got.iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.clone())
                .unwrap()
        };
        assert_eq!(value("a"), "end", "a chain is followed to the end");
        assert!(
            value("elsewhere").contains("{{from_the_collection}}"),
            "an unreachable name is left as written: {}",
            value("elsewhere")
        );
        let unresolved: Vec<String> = postman_env_unresolved_refs(env)
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert!(
            unresolved.contains(&"elsewhere".to_string()),
            "{unresolved:?}"
        );
        assert!(
            unresolved.contains(&"loop1".to_string()) && unresolved.contains(&"loop2".to_string()),
            "a loop settles rather than spinning, and says so: {unresolved:?}"
        );
    }

    fn j(s: &str) -> String {
        s.lines()
            .map(|l| serde_json::to_string(l).unwrap())
            .collect::<Vec<_>>()
            .join(",")
    }

    /// One request with an optional `prerequest` and/or `test` script, a URL
    /// and a title (the breadcrumb Postman would address it by).
    fn conv(pre: &str, test: &str, url: &str, title: &str) -> ConvertedCollection {
        let mut ev = Vec::new();
        if !pre.is_empty() {
            ev.push(format!(
                r#"{{"listen":"prerequest","script":{{"exec":[{}]}}}}"#,
                j(pre)
            ));
        }
        if !test.is_empty() {
            ev.push(format!(
                r#"{{"listen":"test","script":{{"exec":[{}]}}}}"#,
                j(test)
            ));
        }
        convert_postman(&format!(
            r#"{{"info":{{"name":"d","schema":"x"}},"item":[
              {{"name":"{title}","event":[{}],"request":{{"method":"GET","url":"{url}"}}}}]}}"#,
            ev.join(",")
        ))
    }

    // P1 — an `if` inside a `function () {}` test callback is conditional.
    #[test]
    fn a_guard_inside_a_function_test_callback_is_not_hoisted() {
        let c = conv(
            "",
            "pm.test('t', function () {\n  if (jsonData.ok) {\n    pm.expect(jsonData.name).to.equal('Ada');\n  }\n});",
            "https://h/x",
            "x",
        );
        assert!(
            c.entries[0].asserts.is_empty(),
            "guarded assert hoisted: {:?}",
            c.entries[0].asserts
        );
        // A guarded status line is likewise not adopted.
        let st = conv(
            "",
            "pm.test('t', function () {\n  if (ok) {\n    pm.response.to.have.status(500);\n  }\n});",
            "https://h/x",
            "x",
        );
        assert_eq!(st.entries[0].expected_status, None);
        // And a guarded jump reads as a sometimes-jump, not an always-jump.
        let nx = conv(
            "",
            "pm.test('t', function () {\n  if (bad) {\n    postman.setNextRequest('Retry');\n  }\n});",
            "https://h/x",
            "Login",
        );
        assert!(
            nx.notes
                .iter()
                .any(|n| n.detail.contains("sometimes jumped to `Retry`")),
            "{:?}",
            nx.notes
        );
        // Control: a plain `function () {}` callback body is still taken — the
        // fix must narrow, not disable, callback detection.
        let plain = conv(
            "",
            "pm.test('t', function () {\n  pm.expect(jsonData.name).to.equal('Ada');\n});",
            "https://h/x",
            "x",
        );
        assert_eq!(
            plain.entries[0].asserts,
            vec!["jsonpath \"$.name\" == \"Ada\"".to_string()]
        );
        // Control: the arrow spelling of the same guard was always refused.
        let arrow = conv(
            "",
            "pm.test('t', () => {\n  if (jsonData.ok) {\n    pm.expect(jsonData.name).to.equal('Ada');\n  }\n});",
            "https://h/x",
            "x",
        );
        assert!(arrow.entries[0].asserts.is_empty());
    }

    // P2 — a brace-less guard split over two lines is still a guard.
    #[test]
    fn a_brace_less_guard_split_across_lines_is_still_conditional() {
        let c = conv(
            "",
            "if (jsonData.ok)\n    pm.expect(jsonData.name).to.equal('Ada');",
            "https://h/x",
            "x",
        );
        assert!(
            c.entries[0].asserts.is_empty(),
            "{:?}",
            c.entries[0].asserts
        );
        let e = conv(
            "",
            "if (jsonData.ok) {\n} else\n  pm.expect(jsonData.name).to.equal('Ada');",
            "https://h/x",
            "x",
        );
        assert!(
            e.entries[0].asserts.is_empty(),
            "{:?}",
            e.entries[0].asserts
        );
        let f = conv(
            "",
            "for (const x of jsonData.items)\n  pm.expect(jsonData.name).to.equal('Ada');",
            "https://h/x",
            "x",
        );
        assert!(
            f.entries[0].asserts.is_empty(),
            "{:?}",
            f.entries[0].asserts
        );
        // Control: an ordinary statement after a newline is still taken.
        let ok = conv(
            "",
            "const a = 1;\npm.expect(jsonData.name).to.equal('Ada');",
            "https://h/x",
            "x",
        );
        assert_eq!(
            ok.entries[0].asserts,
            vec!["jsonpath \"$.name\" == \"Ada\"".to_string()]
        );
        // Control: the same guard on one line was always refused.
        let one = conv(
            "",
            "if (jsonData.ok) pm.expect(jsonData.name).to.equal('Ada');",
            "https://h/x",
            "x",
        );
        assert!(one.entries[0].asserts.is_empty());
    }

    // P3 — `unquote` accepts one string, never a concatenation.
    #[test]
    fn a_string_concatenation_is_not_read_as_one_string() {
        assert_eq!(unquote("'Not' + ' ' + 'Found'"), None);
        assert_eq!(unquote("'Found'"), Some("Found"));
        assert_eq!(unquote("'it\\'s'"), Some("it\\'s"));
        // The concatenated predicate is declined, not asserted as a wrong value.
        let a = conv(
            "",
            "pm.expect(jsonData.name).to.equal('Not' + ' ' + 'Found');",
            "https://h/x",
            "x",
        );
        assert!(
            a.entries[0].asserts.is_empty(),
            "{:?}",
            a.entries[0].asserts
        );
        assert!(a.notes.iter().any(|n| n.detail.contains("dropped")));
        // A concatenated `[Gen]` value — actually *sent* — is declined too.
        let g = conv(
            "pm.environment.set('key', 'abc' + '-' + 'def');",
            "",
            "https://h/x",
            "x",
        );
        assert!(
            g.entries[0].generators.is_empty(),
            "{:?}",
            g.entries[0].generators
        );
        // Control: a plain quoted value still converts.
        let ok = conv(
            "pm.environment.set('key', 'abcdef');",
            "",
            "https://h/x",
            "x",
        );
        assert_eq!(
            ok.entries[0].generators,
            vec![("key".to_string(), "\"abcdef\"".to_string())]
        );
    }

    // P4 — a space inside a quoted subject key or header name survives.
    #[test]
    fn a_space_in_a_subject_key_or_header_name_survives() {
        let k = conv(
            "",
            "pm.expect(jsonData['full name']).to.equal('Ada');",
            "https://h/x",
            "x",
        );
        assert_eq!(
            k.entries[0].asserts,
            vec!["jsonpath \"$['full name']\" == \"Ada\"".to_string()]
        );
        let h = conv(
            "",
            "pm.expect(pm.response.headers.get('X Weird')).to.equal('a');",
            "https://h/x",
            "x",
        );
        assert_eq!(
            h.entries[0].asserts,
            vec!["header \"X Weird\" == \"a\"".to_string()]
        );
        // Control: the predicate side already kept its spaces.
        let p = conv(
            "",
            "pm.expect(jsonData.msg).to.equal('Not Found');",
            "https://h/x",
            "x",
        );
        assert_eq!(
            p.entries[0].asserts,
            vec!["jsonpath \"$.msg\" == \"Not Found\"".to_string()]
        );
    }

    // P5 — a capture over `pm.response.json()` is actually captured, not marked
    // covered and silently lost.
    #[test]
    fn a_capture_over_response_json_is_captured_not_lost() {
        let c = conv(
            "",
            "pm.test('ok', function () { pm.response.to.have.status(200); });\npm.environment.set('token', pm.response.json().token);",
            "https://h/x",
            "x",
        );
        assert_eq!(
            c.entries[0].captures,
            vec![("token".to_string(), "jsonpath \"$.token\"".to_string())]
        );
        assert_eq!(c.entries[0].expected_status, Some(200));
        // Coverage and conversion cannot diverge: what is covered here really is
        // a capture, so the note counts it.
        assert!(
            c.notes.iter().any(|n| n.detail.contains("[Captures]")),
            "{:?}",
            c.notes
        );
    }

    /// The sandbox `pm.` replaced. A collection written against it and left
    /// alone since -- which is most of what there is to import -- had every one
    /// of its captures dropped, and the note said the script had nothing left
    /// in it.
    #[test]
    fn the_older_postman_api_still_becomes_captures() {
        let c = conv(
            "",
            "var jsonData = JSON.parse(responseBody);\npostman.setEnvironmentVariable('token', jsonData.token);",
            "https://h/x",
            "x",
        );
        assert_eq!(
            c.entries[0].captures,
            vec![("token".to_string(), "jsonpath \"$.token\"".to_string())]
        );
    }

    /// `JSON.parse(responseBody)` is the older spelling of
    /// `pm.response.json()`, and a script is free to give it any name it likes.
    #[test]
    fn a_legacy_body_variable_under_any_name_is_a_root() {
        let c = conv(
            "",
            "var body = JSON.parse(responseBody);\npostman.setEnvironmentVariable('id', body.user.id);",
            "https://h/x",
            "x",
        );
        assert_eq!(
            c.entries[0].captures,
            vec![("id".to_string(), "jsonpath \"$.user.id\"".to_string())]
        );
    }

    /// A name standing for part of the body is an ordinary way to write a
    /// script against a response that nests everything one level down; every
    /// call through one used to be dropped as unreadable.
    #[test]
    fn a_name_standing_for_part_of_the_body_is_a_root_too() {
        let c = conv(
            "",
            "const body = pm.response.json();\nconst data = body.data;\nconst first = data.items[0];\npm.environment.set('id', first.id);\npm.environment.set('total', data.total);",
            "https://h/x",
            "x",
        );
        assert_eq!(
            c.entries[0].captures,
            vec![
                (
                    "id".to_string(),
                    "jsonpath \"$.data.items[0].id\"".to_string()
                ),
                ("total".to_string(), "jsonpath \"$.data.total\"".to_string()),
            ]
        );
    }

    /// Two declarations of one name mean two different things at two points in
    /// the script, and the text alone cannot say which applies where. Answering
    /// anyway would capture from the wrong part of the body.
    #[test]
    fn a_name_declared_twice_is_not_treated_as_a_root() {
        let c = conv(
            "",
            "const body = pm.response.json();\nconst d = body.a;\nconst d = body.b;\npm.environment.set('id', d.id);",
            "https://h/x",
            "x",
        );
        assert!(
            c.entries[0].captures.is_empty(),
            "an ambiguous name was resolved anyway: {:?}",
            c.entries[0].captures
        );
    }

    // P6 — `[Gen]` rows and `[Captures]` are only taken from unconditional code.
    #[test]
    fn a_conditional_generator_or_capture_is_left_as_residue() {
        let g = conv(
            "if (!pm.environment.get('id')) { pm.environment.set('id', require('uuid').v4()); }",
            "",
            "https://h/x",
            "x",
        );
        assert!(
            g.entries[0].generators.is_empty(),
            "guarded set-once became an unconditional row: {:?}",
            g.entries[0].generators
        );
        assert!(g.notes.iter().any(|n| n.detail.contains("dropped")));
        let e = conv(
            "if (a) {\n  pm.environment.set('id', 1);\n} else {\n  pm.environment.set('id', 2);\n}",
            "",
            "https://h/x",
            "x",
        );
        assert!(
            e.entries[0].generators.is_empty(),
            "an if/else silently picked a branch: {:?}",
            e.entries[0].generators
        );
        let c = conv(
            "",
            "if (pm.response.code === 200) {\n  pm.environment.set('token', jsonData.token);\n}",
            "https://h/x",
            "x",
        );
        assert!(
            c.entries[0].captures.is_empty(),
            "a guarded capture would error whenever the guard was not taken: {:?}",
            c.entries[0].captures
        );
        assert!(c.notes.iter().any(|n| n.detail.contains("dropped")));
        // Controls: the unconditional forms still convert.
        let okg = conv(
            "pm.environment.set('id', require('uuid').v4());",
            "",
            "https://h/x",
            "x",
        );
        assert_eq!(
            okg.entries[0].generators,
            vec![("id".to_string(), "uuid".to_string())]
        );
        let okc = conv(
            "",
            "pm.environment.set('token', jsonData.token);",
            "https://h/x",
            "x",
        );
        assert_eq!(
            okc.entries[0].captures,
            vec![("token".to_string(), "jsonpath \"$.token\"".to_string())]
        );
    }

    // P7 — a capture name Hurl cannot carry is refused, not written and later
    // silently deleted.
    #[test]
    fn an_invalid_capture_name_is_refused() {
        let c = conv(
            "",
            "pm.environment.set('my token', jsonData.token);",
            "https://h/x",
            "x",
        );
        assert!(
            c.entries[0].captures.is_empty(),
            "{:?}",
            c.entries[0].captures
        );
        assert!(c.notes.iter().any(|n| n.detail.contains("dropped")));
        let hurl = crate::hurl::collection_to_hurl(&c.entries);
        assert!(
            !hurl.contains("my token: jsonpath"),
            "an invalid name was written into the file: {hurl}"
        );
        assert_eq!(crate::hurl::parse_hurl(&hurl).len(), 1);
        // Control: a valid name is captured.
        let ok = conv(
            "",
            "pm.environment.set('token', jsonData.token);",
            "https://h/x",
            "x",
        );
        assert_eq!(
            ok.entries[0].captures,
            vec![("token".to_string(), "jsonpath \"$.token\"".to_string())]
        );
    }

    // P8 — a dynamic variable does not fold onto an existing, differing `[Gen]`
    // row and take its value.
    #[test]
    fn a_dynamic_variable_does_not_reuse_a_differing_gen_row() {
        let c = conv(
            "pm.environment.set('timestamp', Date.now());",
            "",
            "https://h/x?t={{$timestamp}}",
            "x",
        );
        assert_eq!(
            c.entries[0].generators,
            vec![
                ("timestamp".to_string(), "timestamp_ms".to_string()),
                ("timestamp_1".to_string(), "timestamp".to_string()),
            ]
        );
        assert_eq!(c.entries[0].url, "https://h/x?t={{timestamp_1}}");
        assert!(
            c.notes.iter().any(|n| n
                .detail
                .contains("computed by this request's `[Gen]` block as `timestamp`")),
            "{:?}",
            c.notes
        );
        // Control: with no clash, the natural name is used.
        let ok = conv("", "", "https://h/x?t={{$timestamp}}", "x");
        assert_eq!(
            ok.entries[0].generators,
            vec![("timestamp".to_string(), "timestamp".to_string())]
        );
        assert_eq!(ok.entries[0].url, "https://h/x?t={{timestamp}}");
    }

    // P9 — a brace inside a regex literal does not shift the block stack.
    #[test]
    fn a_brace_in_a_regex_literal_does_not_shift_the_block_stack() {
        let c = conv(
            "",
            "if (bad) {\n    const m = pm.response.text().match(/\\{([^}]*)\\}/);\n    pm.expect(jsonData.a).to.equal(1);\n}",
            "https://h/x",
            "x",
        );
        assert!(
            c.entries[0].asserts.is_empty(),
            "the extra `}}` popped the if and hoisted a guarded assert: {:?}",
            c.entries[0].asserts
        );
        // The mirror: an unmatched `{` in a folder script must not hide the
        // request's own assertions.
        let json = format!(
            r#"{{"info":{{"name":"d","schema":"x"}},"item":[
              {{"name":"F","event":[{{"listen":"test","script":{{"exec":[{}]}}}}],
                "item":[{{"name":"r","event":[{{"listen":"test","script":{{"exec":[{}]}}}}],
                  "request":{{"method":"GET","url":"https://h/x"}}}}]}}]}}"#,
            j("const re = /\\{/;"),
            j("pm.expect(jsonData.a).to.equal(1);")
        );
        let f = convert_postman(&json);
        assert_eq!(
            f.entries[0].asserts,
            vec!["jsonpath \"$.a\" == 1".to_string()]
        );
        // Control: a `/` that is really division is not read as a regex.
        let d = conv(
            "",
            "const half = total / 2;\nif (bad) {\n  pm.expect(jsonData.a).to.equal(1);\n}",
            "https://h/x",
            "x",
        );
        assert!(
            d.entries[0].asserts.is_empty(),
            "{:?}",
            d.entries[0].asserts
        );
    }

    // P10 — a number Hurl's grammar cannot read is not asserted or emitted.
    #[test]
    fn a_number_hurl_cannot_read_is_declined() {
        for src in ["1e3", ".5", "Infinity", "NaN"] {
            let c = conv(
                "",
                &format!("pm.expect(jsonData.a).to.equal({src});"),
                "https://h/x",
                "x",
            );
            assert!(
                c.entries[0].asserts.is_empty(),
                "{src} was asserted: {:?}",
                c.entries[0].asserts
            );
        }
        // Controls: ordinary numbers still convert and read back.
        for (src, line) in [
            ("200", "jsonpath \"$.a\" == 200"),
            ("1.5", "jsonpath \"$.a\" == 1.5"),
            ("-3", "jsonpath \"$.a\" == -3"),
        ] {
            let c = conv(
                "",
                &format!("pm.expect(jsonData.a).to.equal({src});"),
                "https://h/x",
                "x",
            );
            assert_eq!(c.entries[0].asserts, vec![line.to_string()], "{src}");
            let back = crate::hurl::parse_hurl(&crate::hurl::collection_to_hurl(&c.entries));
            assert_eq!(back[0].asserts, c.entries[0].asserts, "{src} round-trips");
        }
        // The generator side: an unreadable number is declined, and every row
        // the importer does emit passes `generators::check`.
        let g = conv("pm.environment.set('n', 1e3);", "", "https://h/x", "x");
        assert!(
            g.entries[0].generators.is_empty(),
            "{:?}",
            g.entries[0].generators
        );
        let ok = conv("pm.environment.set('n', 5);", "", "https://h/x", "x");
        assert_eq!(
            ok.entries[0].generators,
            vec![("n".to_string(), "5".to_string())]
        );
        assert!(crate::generators::check(&ok.entries[0].generators).is_empty());
    }

    // P11 — a second, differing expected status is kept as residue, not
    // silently discarded.
    #[test]
    fn a_second_differing_status_is_noted_not_dropped_silently() {
        let c = conv(
            "",
            "pm.test('a', () => { pm.response.to.have.status(200); });\npm.test('b', () => { pm.expect(pm.response.code).to.equal(201); });",
            "https://h/x",
            "x",
        );
        assert_eq!(c.entries[0].expected_status, Some(200));
        assert!(
            c.notes
                .iter()
                .any(|n| n.detail.contains("the rest of it was dropped")),
            "the losing status left no note: {:?}",
            c.notes
        );
        // Control: a second *matching* status is not residue.
        let ok = conv(
            "",
            "pm.test('a', () => { pm.response.to.have.status(200); });\npm.test('b', () => { pm.expect(pm.response.code).to.equal(200); });",
            "https://h/x",
            "x",
        );
        assert_eq!(ok.entries[0].expected_status, Some(200));
        assert!(
            !ok.notes
                .iter()
                .any(|n| n.detail.contains("the rest of it was dropped")),
            "{:?}",
            ok.notes
        );
    }

    // P12 — an inherited script's losses are filed against the folder that
    // wrote them, not the request that only inherits them.
    #[test]
    fn a_folder_scripts_residue_is_filed_against_the_folder() {
        let json = format!(
            r#"{{"info":{{"name":"d","schema":"x"}},"item":[
              {{"name":"F","event":[{{"listen":"test","script":{{"exec":[{}]}}}}],
                "item":[{{"name":"r","event":[{{"listen":"test","script":{{"exec":[{}]}}}}],
                  "request":{{"method":"GET","url":"https://h/x"}}}}]}}]}}"#,
            j("const t = pm.environment.get('x');\npm.cookies.clear();"),
            j("pm.test('t', () => { pm.expect(jsonData.a).to.equal(1); });")
        );
        let c = convert_postman(&json);
        assert_eq!(c.notes.len(), 2, "{:?}", c.notes);
        // The request's own note is clean — its script converted completely.
        let req = c
            .notes
            .iter()
            .find(|n| n.item == "F/r")
            .expect("request note missing");
        assert!(
            req.detail
                .contains("this request's test script became 1 [Asserts]"),
            "{:?}",
            req
        );
        assert!(
            !req.detail.contains("the rest of it was dropped"),
            "the folder's loss was blamed on the request: {:?}",
            req
        );
        // The folder's own loss is filed against the folder.
        let folder = c
            .notes
            .iter()
            .find(|n| n.item == "F")
            .expect("folder note missing");
        assert!(
            folder
                .detail
                .contains("this folder's test script was dropped"),
            "{:?}",
            folder
        );
    }

    // P13 — a `{{` in an assert literal is refused, not read back as a Hurl
    // template.
    #[test]
    fn a_template_looking_literal_is_not_asserted() {
        let c = conv(
            "",
            "pm.expect(jsonData.a).to.equal('id {{x}} here');",
            "https://h/x",
            "x",
        );
        assert!(
            c.entries[0].asserts.is_empty(),
            "{:?}",
            c.entries[0].asserts
        );
        let d = conv(
            "",
            "pm.expect(jsonData).to.eql({ a: 'x {{y}} z' });",
            "https://h/x",
            "x",
        );
        assert!(
            d.entries[0].asserts.is_empty(),
            "{:?}",
            d.entries[0].asserts
        );
        // Control: an ordinary string is asserted.
        let ok = conv(
            "",
            "pm.expect(jsonData.a).to.equal('plain');",
            "https://h/x",
            "x",
        );
        assert_eq!(
            ok.entries[0].asserts,
            vec!["jsonpath \"$.a\" == \"plain\"".to_string()]
        );
    }

    // P14 — a capture name defined twice keeps only the last write, as the
    // generator path does — not two rows the file cannot hold.
    #[test]
    fn a_capture_name_defined_twice_keeps_only_the_last() {
        let c = conv(
            "",
            "pm.environment.set('id', jsonData.a);\npm.environment.set('id', jsonData.b);",
            "https://h/x",
            "x",
        );
        assert_eq!(
            c.entries[0].captures,
            vec![("id".to_string(), "jsonpath \"$.b\"".to_string())]
        );
        assert!(
            c.notes.iter().any(|n| n.detail.contains("1 [Captures]")),
            "{:?}",
            c.notes
        );
    }

    /// Controls that must keep converting correctly — if any of these changes,
    /// a fix has over-reached.
    #[test]
    fn controls_still_convert_correctly() {
        let a = conv(
            "",
            "pm.test('t', () => {\n  pm.expect(pm.response.code).to.equal(400);\n  pm.expect(pm.response.json()).to.eql({ Message: 'The request is invalid.', ModelState: { 'body.Image': ['too big',] } });\n});",
            "https://h/x",
            "x",
        );
        assert_eq!(a.entries[0].expected_status, Some(400));
        assert_eq!(
            a.entries[0].asserts,
            vec![
                "jsonpath \"$.Message\" == \"The request is invalid.\"".to_string(),
                "jsonpath \"$.ModelState['body.Image']\" count == 1".to_string(),
                "jsonpath \"$.ModelState['body.Image'][0]\" == \"too big\"".to_string(),
            ]
        );
        // A helper called from top level is still conditional (only a `pm.test`
        // callback body is unconditional).
        let helper = conv(
            "",
            "const check = (b) => { pm.expect(b.a).to.equal('one'); };\ncheck(jsonData);",
            "https://h/x",
            "x",
        );
        assert!(helper.entries[0].asserts.is_empty());
        // Chai `.to.equal` on an object is reference equality, never a deep one.
        let equal_obj = conv(
            "",
            "pm.expect(jsonData).to.equal({ a: 1 });",
            "https://h/x",
            "x",
        );
        assert!(equal_obj.entries[0].asserts.is_empty());
        // `.to.eql({})` asserts nothing about the body, so it is left as residue.
        let empty_obj = conv("", "pm.expect(jsonData).to.eql({});", "https://h/x", "x");
        assert!(empty_obj.entries[0].asserts.is_empty());
        assert!(
            empty_obj
                .notes
                .iter()
                .any(|n| n.detail.contains("nothing in it reduced"))
        );
    }
}
