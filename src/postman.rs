//! Best-effort import of a Postman collection (v2.1 JSON export) into our
//! [`HurlEntry`] model. The Hurl format doesn't cover every Postman feature
//! (pre-request scripts, …), but the request line, headers, query, body and
//! basic/bearer auth are mapped; Postman `{{var}}` placeholders share Hurl's
//! syntax so they carry over. The schema subset we care about is deserialized
//! into the typed structs below, every field optional/defaulted so partial
//! exports still import and anything unmodelled is ignored.

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
        Value::Object(m) => m.get("raw").and_then(Value::as_str).unwrap_or("").to_string(),
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
#[derive(Deserialize, Default)]
#[serde(default)]
struct Param {
    key: String,
    value: String,
    disabled: bool,
    #[serde(rename = "type")]
    kind: String,
    src: String,
    #[serde(rename = "contentType")]
    content_type: Option<String>,
}

impl Param {
    /// A `{key, value}` pair, unless the entry is disabled or keyless.
    fn enabled_kv(&self) -> Option<(String, String)> {
        (!self.disabled && !self.key.is_empty()).then(|| (self.key.clone(), self.value.clone()))
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
            }
        } else {
            FormField {
                key: self.key.clone(),
                value: self.value.clone(),
                kind: FormFieldKind::Text,
                content_type: None,
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
            out.push(map_request(&title, req));
        }
    }
}

fn map_request(name: &str, req: &Request) -> HurlEntry {
    let mut headers: Vec<(String, String)> =
        req.header.iter().filter_map(Param::enabled_kv).collect();

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
                    headers.push(("Authorization".to_string(), format!("Bearer {t}")));
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
            "urlencoded" => form_fields = b.urlencoded.iter().filter_map(Param::form_field).collect(),
            "formdata" => form_fields = b.formdata.iter().filter_map(Param::form_field).collect(),
            _ => {}
        }
    }

    let mut entry = HurlEntry::from_fields(name, &req.method, &req.url, headers, &body);
    entry.basic_auth = basic_auth;
    entry.form_fields = form_fields;
    entry
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
            vec![("Content-Type".to_string(), "application/json".to_string())]
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
                    content_type: None
                },
                FormField {
                    key: "f".into(),
                    value: "x".into(),
                    kind: FormFieldKind::File,
                    content_type: None
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
        assert!(
            e[0].headers
                .contains(&("Authorization".to_string(), "Bearer {{tok}}".to_string()))
        );
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
}
