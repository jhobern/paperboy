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

use crate::hurl::{FormField, FormFieldKind, HurlEntry, parse_hurl};

#[derive(Deserialize, Default)]
#[serde(default)]
struct Collection {
    item: Vec<Item>,
}

/// A folder (nested `item`s) or a leaf holding a `request`.
#[derive(Deserialize, Default)]
#[serde(default)]
struct Item {
    name: String,
    item: Option<Vec<Item>>,
    request: Option<Request>,
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

/// Only `basic` (→ `basic_auth`) and `bearer` (→ a `Bearer` header) are mapped;
/// credentials live in `key/value` lists keyed by `username`/`password`/`token`.
#[derive(Deserialize, Default)]
#[serde(default)]
struct Auth {
    #[serde(rename = "type")]
    kind: String,
    basic: Vec<Param>,
    bearer: Vec<Param>,
}

impl Auth {
    fn field(list: &[Param], name: &str) -> String {
        list.iter()
            .find(|p| p.key == name)
            .map(|p| p.value.clone())
            .unwrap_or_default()
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
#[derive(Deserialize, Default)]
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
}

/// Deserialize a string field tolerantly: an explicit JSON `null` (which
/// `#[serde(default)]` does *not* handle) becomes an empty string.
fn de_str<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(Option::<String>::deserialize(d)?.unwrap_or_default())
}

impl Param {
    /// A `{key, value}` pair, unless the entry is  keyless.
    fn enabled_kve(&self) -> Option<(String, String, bool)> {
        (!self.key.is_empty()).then(|| (self.key.clone(), self.value.clone(), !self.disabled))
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
            }
        } else {
            FormField {
                key: self.key.clone(),
                value: self.value.clone(),
                kind: FormFieldKind::Text,
                content_type: None,
                base64_prefix: None,
                enabled: true,
            }
        })
    }
}

/// `true` when `content` looks like a Postman collection export (an `info`
/// block and an `item` array), as opposed to Hurl text.
pub fn looks_like_postman(content: &str) -> bool {
    serde_json::from_str::<Value>(content)
        .map(|v| v.get("info").is_some() && v.get("item").is_some())
        .unwrap_or(false)
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

/// Convert a Postman collection JSON into `HurlEntry` values. Folders are
/// preserved by prefixing each request's title with its `/`-joined folder path
/// (e.g. "Auth/Tokens/Refresh") — the same convention plain Hurl collections
/// use (see [`crate::tree`]). Returns an empty vec if the JSON isn't a
/// recognizable collection.
pub fn import_postman(content: &str) -> Vec<HurlEntry> {
    let Ok(root) = serde_json::from_str::<Collection>(content) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    walk_items(&root.item, &mut Vec::new(), &mut out);
    out
}

/// Recursively collect requests, descending into folders (nodes carrying a
/// nested `item` array) and building up `path` as the folder breadcrumb so
/// each request's title can be prefixed with it. Folders take precedence when
/// a node unusually carries both `item` and `request`.
fn walk_items(items: &[Item], path: &mut Vec<String>, out: &mut Vec<HurlEntry>) {
    for it in items {
        if let Some(sub) = &it.item {
            path.push(it.name.clone());
            walk_items(sub, path, out);
            path.pop();
        } else if let Some(req) = &it.request {
            let title = if path.is_empty() {
                it.name.clone()
            } else {
                format!("{}/{}", path.join("/"), it.name)
            };
            out.push(map_request(&title, req, &it.event));
        }
    }
}

fn map_request(name: &str, req: &Request, events: &[Event]) -> HurlEntry {
    let mut headers: Vec<(String, String, bool)> =
        req.header.iter().filter_map(Param::enabled_kve).collect();

    // Auth → basic_auth, or a `Bearer` Authorization header for bearer tokens.
    let mut basic_auth = None;
    if let Some(auth) = &req.auth {
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
                    headers.push(("Authorization".to_string(), format!("Bearer {t}"), true));
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
                    enabled: true
                },
                FormField {
                    key: "f".into(),
                    value: "x".into(),
                    kind: FormFieldKind::File,
                    content_type: None,
                    base64_prefix: None,
                    enabled: true
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
        assert!(e[0].headers.contains(&(
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
}
