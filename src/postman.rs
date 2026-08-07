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

use crate::hurl::{FormField, FormFieldKind, HurlEntry, KvRow, parse_hurl};

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
}

/// A folder (nested `item`s) or a leaf holding a `request`.
#[derive(Deserialize, Default)]
#[serde(default)]
struct Item {
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
}

/// A Postman `event` — a `prerequest` or `test` script attached to an item.
#[derive(Deserialize, Default)]
#[serde(default)]
struct Event {
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
    #[serde(default = "get_method")]
    method: String,
    #[serde(default, deserialize_with = "de_url")]
    url: String,
    #[serde(default)]
    header: Vec<Param>,
    auth: Option<Auth>,
    body: Option<Body>,
}

fn get_method() -> String {
    "GET".to_string()
}

/// A Postman URL is a bare string or an object with a `raw` field; anything
/// else imports as an empty URL rather than failing the whole collection.
fn de_url<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::String(s) => s,
        Value::Object(m) => m
            .get("raw")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        _ => String::new(),
    })
}

/// `basic` (→ `basic_auth`), `bearer` (→ a `Bearer` header) and `apikey` (→ a
/// header or a query parameter) are mapped; credentials live in `key/value`
/// lists keyed by `username`/`password`/`token`/`key`/`value`/`in`.
#[derive(Clone, Deserialize, Default)]
#[serde(default)]
struct Auth {
    #[serde(rename = "type")]
    kind: String,
    basic: Vec<Param>,
    bearer: Vec<Param>,
    apikey: Vec<Param>,
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
    mode: String,
    raw: String,
    urlencoded: Vec<Param>,
    formdata: Vec<Param>,
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

/// Deserialize a string field tolerantly: an explicit JSON `null` (which
/// `#[serde(default)]` does *not* handle) becomes an empty string.
fn de_str<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(Option::<String>::deserialize(d)?.unwrap_or_default())
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
                base64_prefix: None,
                enabled: true,
                desc: self.description.clone(),
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
    let Ok(root) = serde_json::from_str::<Value>(content)
        .map(|v| unwrap_envelope(v, "collection", "item"))
        .and_then(serde_json::from_value::<Collection>)
    else {
        return ConvertedCollection::default();
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
    walk_items(&root.item, &mut Vec::new(), inherited, &mut out);
    out
}

/// Recursively collect requests, descending into folders (nodes carrying a
/// nested `item` array) and building up `path` as the folder breadcrumb so
/// each request's title can be prefixed with it. Folders take precedence when
/// a node unusually carries both `item` and `request`.
///
/// `inherited` is the nearest enclosing auth, already resolved — `None` once
/// some level has said `noauth`.
fn walk_items(
    items: &[Item],
    path: &mut Vec<String>,
    inherited: Option<&Auth>,
    out: &mut ConvertedCollection,
) {
    for it in items {
        if let Some(sub) = &it.item {
            let here = resolve_auth(it.auth.as_ref(), inherited);
            path.push(it.name.clone());
            walk_items(sub, path, here, out);
            path.pop();
        } else if let Some(req) = &it.request {
            let title = if path.is_empty() {
                it.name.clone()
            } else {
                format!("{}/{}", path.join("/"), it.name)
            };
            let auth = resolve_auth(req.auth.as_ref(), inherited);
            let entry = map_request(&title, req, &it.event, auth);
            note_losses(&title, req, &it.event, auth, &entry, out);
            out.entries.push(entry);
        }
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
        && !matches!(auth.kind.as_str(), "basic" | "bearer" | "apikey")
    {
        note(format!(
            "auth type `{}` has no Hurl equivalent and was dropped",
            auth.kind
        ));
    }
    if let Some(auth) = auth
        && auth.kind == "apikey"
        && !matches!(Auth::field(&auth.apikey, "in").as_str(), "" | "header")
    {
        note("API-key auth is sent in the query string; it was added as a query parameter".into());
    }
    if let Some(b) = &req.body
        && !matches!(b.mode.as_str(), "" | "raw" | "urlencoded" | "formdata")
    {
        note(format!("body mode `{}` was dropped", b.mode));
    }
    if events
        .iter()
        .any(|e| e.listen == "prerequest" && !e.script.exec.is_empty())
    {
        note("a pre-request script was dropped — Hurl has no equivalent".into());
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

fn map_request(name: &str, req: &Request, events: &[Event], auth: Option<&Auth>) -> HurlEntry {
    let mut headers: Vec<KvRow> = req.header.iter().filter_map(Param::enabled_kve).collect();
    let mut queries: Vec<KvRow> = Vec::new();

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
            _ => {}
        }
    }

    // Body: a raw body is kept verbatim; url-encoded / form-data fields become
    // form fields (file-type form-data fields become `File` fields).
    let mut form_fields = Vec::new();
    let mut body = String::new();
    if let Some(b) = &req.body {
        match b.mode.as_str() {
            "raw" => body = b.raw.clone(),
            "urlencoded" => {
                form_fields = b.urlencoded.iter().filter_map(Param::form_field).collect()
            }
            "formdata" => form_fields = b.formdata.iter().filter_map(Param::form_field).collect(),
            _ => {}
        }
    }

    let mut entry = HurlEntry::from_fields(name, &req.method, &req.url, headers, &body);
    entry.basic_auth = basic_auth;
    entry.form_fields = form_fields;
    entry.queries.extend(queries);
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
    // Response-body variable name(s); default to the near-universal `jsonData`
    // when the script assigns the body to nothing we recognise.
    let mut roots: Vec<String> = JSON_VAR_RE
        .captures_iter(&script)
        .map(|c| c[1].to_string())
        .collect();
    if roots.is_empty() {
        roots.push("jsonData".to_string());
    }
    SET_RE
        .captures_iter(&script)
        .filter_map(|c| {
            let path = accessor_to_jsonpath(c[2].trim(), &roots)?;
            Some((c[1].to_string(), format!("jsonpath \"{path}\"")))
        })
        .collect()
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
        assert_eq!(e[0].body.as_deref(), Some("{\"u\":\"a\"}"));

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
                "method": "GET",
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
            { "name": "oauth", "request": { "method": "GET", "url": "https://x",
                "auth": { "type": "oauth2" } } },
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
            for_item("oauth")[0].contains("oauth2"),
            "the auth type that was lost is named: {notes:?}"
        );
        assert!(for_item("gql")[0].contains("graphql"));
        assert!(for_item("scripted")[0].contains("pre-request"));
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
