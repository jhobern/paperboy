//! Best-effort import of a Postman collection (v2.1 JSON export) into our
//! [`HurlEntry`] model. The Hurl format doesn't cover every Postman feature
//! (multipart file uploads, pre-request scripts, …), but the request line,
//! headers, query, body and basic/bearer auth are mapped. Postman `{{var}}`
//! placeholders happen to use the same syntax as Hurl, so they carry over.
//!
//! Parsing uses `serde_json` (a `Value` tree) rather than a bespoke parser, and
//! tolerates schema variations by probing fields defensively.

use serde_json::Value;

use crate::hurl::{FormField, FormFieldKind, HurlEntry, parse_hurl};

/// `true` when `content` looks like a Postman collection export (an `info` block
/// and an `item` array), as opposed to Hurl text.
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
/// preserved by prefixing each request's title with its folder path, joined
/// by `/` (e.g. a request named "Login" inside folder "Auth" becomes
/// "Auth/Login", and further nesting works the same way, e.g.
/// "Auth/Tokens/Refresh") — the same convention used to represent folders in
/// plain Hurl collections (see [`crate::tree`]), so both import paths feed
/// the same folder-aware UI. Returns an empty vec if the JSON isn't a
/// recognizable collection.
pub fn import_postman(content: &str) -> Vec<HurlEntry> {
    let Ok(root) = serde_json::from_str::<Value>(content) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(items) = root.get("item").and_then(Value::as_array) {
        let mut path: Vec<String> = Vec::new();
        walk_items(items, &mut path, &mut out);
    }
    out
}

/// Recursively collect requests, descending into folders (items with a nested
/// `item` array instead of a `request`) and building up `path` as the current
/// folder breadcrumb so each request's title can be prefixed with it.
fn walk_items(items: &[Value], path: &mut Vec<String>, out: &mut Vec<HurlEntry>) {
    for it in items {
        let name = it.get("name").and_then(Value::as_str).unwrap_or("");
        if let Some(sub) = it.get("item").and_then(Value::as_array) {
            path.push(name.to_string());
            walk_items(sub, path, out);
            path.pop();
        } else if let Some(req) = it.get("request") {
            let title = if path.is_empty() {
                name.to_string()
            } else {
                format!("{}/{name}", path.join("/"))
            };
            out.push(map_request(&title, req));
        }
    }
}

fn map_request(name: &str, req: &Value) -> HurlEntry {
    let method = req
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("GET")
        .to_string();
    let url = req.get("url").map(url_raw).unwrap_or_default();

    let mut headers: Vec<(String, String)> = req
        .get("header")
        .and_then(Value::as_array)
        .map(|hs| hs.iter().filter_map(enabled_kv).collect())
        .unwrap_or_default();

    // Auth → basic_auth, or an Authorization header for bearer tokens.
    let mut basic_auth = None;
    if let Some(auth) = req.get("auth") {
        match auth.get("type").and_then(Value::as_str) {
            Some("basic") => {
                let u = auth_field(auth, "basic", "username");
                let p = auth_field(auth, "basic", "password");
                if !u.is_empty() || !p.is_empty() {
                    basic_auth = Some((u, p));
                }
            }
            Some("bearer") => {
                let t = auth_field(auth, "bearer", "token");
                if !t.is_empty() {
                    headers.push(("Authorization".to_string(), format!("Bearer {t}")));
                }
            }
            _ => {}
        }
    }

    // Body: a raw body is kept verbatim; url-encoded / form-data fields become
    // form fields (file-type form-data fields become `File` fields, using
    // Postman's `src` as the path).
    let mut form_fields = Vec::new();
    let mut body = String::new();
    if let Some(b) = req.get("body") {
        match b.get("mode").and_then(Value::as_str) {
            Some("raw") => {
                body = b
                    .get("raw")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string()
            }
            Some("urlencoded") => form_fields = form_fields_array(b.get("urlencoded")),
            Some("formdata") => form_fields = form_fields_array(b.get("formdata")),
            _ => {}
        }
    }

    let mut entry = HurlEntry::from_fields(name, &method, &url, headers, &body);
    entry.basic_auth = basic_auth;
    entry.form_fields = form_fields;
    entry
}

/// A Postman URL is either a bare string or `{ "raw": "…", … }`.
fn url_raw(url: &Value) -> String {
    match url {
        Value::String(s) => s.clone(),
        _ => url
            .get("raw")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    }
}

/// A `{key, value, disabled?}` entry, unless disabled or keyless.
fn enabled_kv(e: &Value) -> Option<(String, String)> {
    if e.get("disabled").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let key = e.get("key").and_then(Value::as_str)?;
    if key.is_empty() {
        return None;
    }
    let value = e.get("value").and_then(Value::as_str).unwrap_or("");
    Some((key.to_string(), value.to_string()))
}

/// Collect enabled `{key, value}` text fields, or `{key, src, type:"file"}`
/// file fields, from a body param array (Postman formdata/urlencoded).
fn form_fields_array(v: Option<&Value>) -> Vec<FormField> {
    v.and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(form_field_entry).collect())
        .unwrap_or_default()
}

/// One `{key, value|src, type?, disabled?}` body-param entry, unless disabled
/// or keyless.
fn form_field_entry(e: &Value) -> Option<FormField> {
    if e.get("disabled").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let key = e.get("key").and_then(Value::as_str)?;
    if key.is_empty() {
        return None;
    }
    if e.get("type").and_then(Value::as_str) == Some("file") {
        let src = e.get("src").and_then(Value::as_str).unwrap_or("");
        Some(FormField {
            key: key.to_string(),
            value: src.to_string(),
            kind: FormFieldKind::File,
            content_type: e
                .get("contentType")
                .and_then(Value::as_str)
                .map(str::to_string),
        })
    } else {
        let value = e.get("value").and_then(Value::as_str).unwrap_or("");
        Some(FormField {
            key: key.to_string(),
            value: value.to_string(),
            kind: FormFieldKind::Text,
            content_type: None,
        })
    }
}

/// Look up an auth field, e.g. `auth.basic[key==username].value`.
fn auth_field(auth: &Value, kind: &str, field: &str) -> String {
    auth.get(kind)
        .and_then(Value::as_array)
        .and_then(|arr| {
            arr.iter()
                .find(|e| e.get("key").and_then(Value::as_str) == Some(field))
                .and_then(|e| e.get("value").and_then(Value::as_str))
        })
        .unwrap_or("")
        .to_string()
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
