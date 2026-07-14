//! Framework-agnostic request logic shared by the GUI and the terminal UI:
//! app-level default variables (`AppVars`) and the Hurl-collection request
//! building / running, so both front-ends behave identically.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;

use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Map, Value, json};

use crate::collection::Collection;
use crate::environment::{EnvUpdate, Environment, ValueSource, substitute};
use crate::http::ApiResponse;
use crate::hurl::{
    FormField, HurlEntry, collection_to_hurl, run_hurl, stage_out_of_scope_form_files,
};

/// The top-bar Base URL. It seeds the URL field when composing a new request,
/// but is intentionally NOT injected as a `{{ BASE_URL }}` substitution
/// variable — `BASE_URL` must come from the environment (or a capture) so that
/// `{{ BASE_URL }}` stays unresolved when the environment doesn't define it.
#[derive(Clone)]
pub struct AppVars {
    pub base_url: String,
}

impl Default for AppVars {
    fn default() -> Self {
        Self {
            base_url: "http://127.0.0.1:8080".to_string(),
        }
    }
}

/// Build the variable map used to substitute `{{ VAR }}` placeholders: the
/// collection's environment file, then values captured from prior responses
/// (each layer overrides the previous, so a fresh capture wins). The top-bar
/// Base URL is deliberately excluded — `{{ BASE_URL }}` resolves only when the
/// environment (or a capture) supplies `BASE_URL`.
pub fn collection_vars(
    env: Option<&Environment>,
    captures: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut vars = HashMap::new();
    if let Some(env) = env {
        for v in &env.vars {
            vars.insert(v.key.clone(), v.value.clone());
        }
    }
    for (k, v) in captures {
        vars.insert(k.clone(), v.clone());
    }
    vars
}

/// How a `{{ VAR }}` substitution should be coloured, reflecting whether its
/// value is available yet. Drives both the request preview / list colouring and
/// the environment-panel status dot so they agree.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SubstKind {
    /// A plain literal environment value (shown substituted, cyan).
    Literal,
    /// Resolved from an external source — env var, 1Password, SSM — or an
    /// initialised capture (shown substituted, green).
    Loaded,
    /// A secret reference still being fetched in the background (kept as
    /// `{{ VAR }}`, orange).
    Pending,
    /// Failed to resolve, or a capture not yet initialised from a response
    /// (kept as `{{ VAR }}`, red).
    Failed,
}

/// How one referenced variable should be rendered when substituted.
pub struct SubstInfo {
    /// `Some(value)` to substitute the value; `None` to keep the `{{ VAR }}`
    /// placeholder (its value isn't available yet).
    pub shown: Option<String>,
    pub kind: SubstKind,
}

/// Classify every variable a collection's requests might reference, so
/// `{{ VAR }}` placeholders can be substituted and colour-coded by whether they
/// are loaded. Resolution order (later wins): collection `[Captures]` names
/// (not-yet-captured → red) → environment variables → captured values (green).
/// Resolved secrets are shown masked; their real value is never exposed.
pub fn subst_map(col: &Collection, env: Option<&Environment>) -> HashMap<String, SubstInfo> {
    let mut out: HashMap<String, SubstInfo> = HashMap::new();

    // Every `[Captures]` name defined in the collection that hasn't produced a
    // value yet is "not initialised" → red placeholder.
    for e in &col.entries {
        for (name, _) in &e.captures {
            out.insert(
                name.clone(),
                SubstInfo {
                    shown: None,
                    kind: SubstKind::Failed,
                },
            );
        }
    }

    if let Some(env) = env {
        for v in &env.vars {
            let info = if v.loading {
                SubstInfo {
                    shown: None,
                    kind: SubstKind::Pending,
                }
            } else if v.resolved {
                match v.source {
                    ValueSource::Literal => SubstInfo {
                        shown: Some(v.value.clone()),
                        kind: SubstKind::Literal,
                    },
                    _ => SubstInfo {
                        shown: Some(v.display_value()),
                        kind: SubstKind::Loaded,
                    },
                }
            } else {
                SubstInfo {
                    shown: None,
                    kind: SubstKind::Failed,
                }
            };
            out.insert(v.key.clone(), info);
        }
    }

    // A capture that has produced a value is loaded → green (overrides env).
    for (k, val) in &col.captures {
        out.insert(
            k.clone(),
            SubstInfo {
                shown: Some(val.clone()),
                kind: SubstKind::Loaded,
            },
        );
    }

    out
}

/// The request text with every *known* `{{ VAR }}` replaced by its substituted
/// value (placeholders whose value isn't available are kept). Used to measure
/// the displayed length for horizontal-scroll clamping in the list.
pub fn subst_display(text: &str, map: &HashMap<String, SubstInfo>) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(open) = rest.find("{{") {
        let Some(close_rel) = rest[open + 2..].find("}}") else {
            break;
        };
        let close = open + 2 + close_rel;
        let end = close + 2;
        let inner = rest[open + 2..close].trim();
        out.push_str(&rest[..open]);
        match map.get(inner).and_then(|i| i.shown.as_ref()) {
            Some(val) => out.push_str(val),
            None => out.push_str(&rest[open..end]),
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// Pretty-printed JSON of the request in its RAW, editable form: `{{ VAR }}`
/// placeholders are kept intact (never substituted) so the user edits the
/// original template, and basic auth is shown as a readable `basic_auth` object
/// rather than an encoded `Authorization` header. A JSON body is inlined as a
/// value; otherwise it is kept as a string. This is the text shown in the
/// editor and the source for the substituted preview; the wire request is
/// re-derived (substituted + encoded) from the entry by [`resolve_request`].
pub fn build_request_json(entry: &HurlEntry) -> String {
    let mut headers_map: Map<String, Value> = Map::new();
    for (k, v) in &entry.headers {
        headers_map.insert(k.clone(), json!(v));
    }

    let query_params: Map<String, Value> = entry
        .query_params
        .iter()
        .map(|(k, v)| (k.clone(), json!(v)))
        .collect();

    let cookies_map: Map<String, Value> = entry
        .cookies
        .iter()
        .map(|(k, v)| (k.clone(), json!(v)))
        .collect();

    let form_fields: Vec<Value> = entry
        .form_fields
        .iter()
        .map(|f| {
            let mut o = Map::new();
            o.insert("key".into(), json!(f.key));
            o.insert("value".into(), json!(f.value));
            o.insert(
                "type".into(),
                json!(match f.kind {
                    crate::hurl::FormFieldKind::Text => "text",
                    crate::hurl::FormFieldKind::File => "file",
                }),
            );
            if let Some(ct) = &f.content_type {
                o.insert("content_type".into(), json!(ct));
            }
            Value::Object(o)
        })
        .collect();

    let body_value: Value = match &entry.body {
        None => Value::Null,
        Some(raw) => serde_json::from_str(raw).unwrap_or(json!(raw)),
    };

    let mut obj: Map<String, Value> = Map::new();
    obj.insert("method".into(), json!(entry.method));
    obj.insert("url".into(), json!(entry.url));
    if let Some((user, pass)) = &entry.basic_auth {
        let mut ba = Map::new();
        ba.insert("user".into(), json!(user));
        ba.insert("pass".into(), json!(pass));
        obj.insert("basic_auth".into(), Value::Object(ba));
    }
    if !headers_map.is_empty() {
        obj.insert("headers".into(), Value::Object(headers_map));
    }
    if !cookies_map.is_empty() {
        obj.insert("cookies".into(), Value::Object(cookies_map));
    }
    if !query_params.is_empty() {
        obj.insert("query_params".into(), Value::Object(query_params));
    }
    if !form_fields.is_empty() {
        obj.insert("form_fields".into(), Value::Array(form_fields));
    }
    if !body_value.is_null() {
        obj.insert("body".into(), body_value);
    }

    serde_json::to_string_pretty(&Value::Object(obj)).unwrap_or_else(|_| "{}".into())
}

/// A JSON scalar rendered as plain text for a header/cookie/query-param value:
/// a string is used as-is, `null` becomes empty, anything else (a number,
/// bool, etc. — not something [`build_request_json`] itself ever emits, but
/// tolerated on a hand-edited round trip) is rendered via its own JSON text.
fn value_as_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Parse the JSON text produced by [`build_request_json`] back into a
/// [`HurlEntry`], carrying over everything that view intentionally doesn't
/// expose (`title`, `expected_status`, `captures`, `asserts`, `user_added`)
/// unchanged from `base` — exactly like the Edit Request wizard already does
/// for the fields *it* doesn't expose. Used by the Main panel's Raw JSON Mode
/// editor (Shift+J), the JSON-text counterpart to Raw Mode's Hurl-text editor.
/// Errs with a short, human-readable reason on anything that isn't a JSON
/// object with at least a `method` and a `url`.
pub fn apply_request_json(base: &HurlEntry, text: &str) -> Result<HurlEntry, String> {
    let value: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let obj = value
        .as_object()
        .ok_or_else(|| "expected a JSON object".to_string())?;

    let method = obj
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(|| "\"method\" must be a string".to_string())?
        .to_string();
    let url = obj
        .get("url")
        .and_then(Value::as_str)
        .ok_or_else(|| "\"url\" must be a string".to_string())?
        .to_string();

    let basic_auth = match obj.get("basic_auth") {
        Some(Value::Object(ba)) => Some((
            ba.get("user").map(value_as_text).unwrap_or_default(),
            ba.get("pass").map(value_as_text).unwrap_or_default(),
        )),
        _ => None,
    };

    let as_pairs = |key: &str| -> Vec<(String, String)> {
        match obj.get(key) {
            Some(Value::Object(m)) => m
                .iter()
                .map(|(k, v)| (k.clone(), value_as_text(v)))
                .collect(),
            _ => Vec::new(),
        }
    };
    let headers = as_pairs("headers");
    let cookies = as_pairs("cookies");
    let query_params = as_pairs("query_params");

    let form_fields: Vec<FormField> = match obj.get("form_fields") {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| {
                let o = item.as_object()?;
                Some(FormField {
                    key: o.get("key").map(value_as_text).unwrap_or_default(),
                    value: o.get("value").map(value_as_text).unwrap_or_default(),
                    kind: match o.get("type").and_then(Value::as_str) {
                        Some("file") => crate::hurl::FormFieldKind::File,
                        _ => crate::hurl::FormFieldKind::Text,
                    },
                    content_type: o
                        .get("content_type")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                })
            })
            .collect(),
        _ => Vec::new(),
    };

    let body = match obj.get("body") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(v) => Some(serde_json::to_string_pretty(v).unwrap_or_default()),
    };

    let mut entry = base.clone();
    entry.method = method;
    entry.url = url;
    entry.basic_auth = basic_auth;
    entry.headers = headers;
    entry.cookies = cookies;
    entry.query_params = query_params;
    entry.form_fields = form_fields;
    entry.body = body;
    Ok(entry)
}

/// Which textual representation of a request the Main (Request) panel shows
/// by default, and copies whole when the panel has focus with nothing
/// selected: the pretty-printed JSON preview, or the actual Hurl text. An
/// app-wide preference (Settings → Preferences → Default Request View) that
/// applies to every request, not just the one currently selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum RequestView {
    #[default]
    Json,
    Hurl,
}

/// The wire request resolved for the selected entry: `{{ VAR }}` placeholders
/// substituted and basic auth encoded into an `Authorization` header.
pub struct ResolvedRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub cookies: Vec<(String, String)>,
    pub form_fields: Vec<FormField>,
    pub body: Option<String>,
}

/// Resolve the wire request to send for the selected entry: always rebuilt from
/// the entry (the source of truth) with `{{ VAR }}` placeholders substituted and
/// basic auth encoded into an `Authorization` header. Editor changes are applied
/// to the entry when committed, so this always reflects the current request.
pub fn resolve_request(col: &Collection, env: Option<&Environment>) -> Option<ResolvedRequest> {
    let entry = col.entries.get(col.selected_entry)?;
    let vars = collection_vars(env, &col.captures);
    let method = entry.method.clone();
    let url = substitute(&entry.url, &vars);
    let mut headers: Vec<(String, String)> = entry
        .headers
        .iter()
        .map(|(k, v)| (k.clone(), substitute(v, &vars)))
        .collect();
    if let Some((user, pass)) = &entry.basic_auth {
        let cred = STANDARD.encode(format!(
            "{}:{}",
            substitute(user, &vars),
            substitute(pass, &vars)
        ));
        headers.push(("Authorization".to_string(), format!("Basic {cred}")));
    }
    let cookies: Vec<(String, String)> = entry
        .cookies
        .iter()
        .map(|(k, v)| (substitute(k, &vars), substitute(v, &vars)))
        .collect();
    let form_fields: Vec<FormField> = entry
        .form_fields
        .iter()
        .map(|f| FormField {
            key: substitute(&f.key, &vars),
            value: substitute(&f.value, &vars),
            kind: f.kind,
            content_type: f.content_type.as_deref().map(|ct| substitute(ct, &vars)),
        })
        .collect();
    let body = entry.body.as_deref().map(|b| substitute(b, &vars));
    Some(ResolvedRequest {
        method,
        url,
        headers,
        cookies,
        form_fields,
        body,
    })
}

/// The result of running the collection's selected entry, routed back to its
/// collection: captured values (if any) plus a snapshot of the response
/// actually received, so it can be remembered per-entry (see
/// `HurlEntry::last_response`) rather than only in the shared "live" state.
pub struct CaptureUpdate {
    pub col_id: u64,
    pub entry_idx: usize,
    pub values: HashMap<String, String>,
    pub response: ApiResponse,
}

/// The result of a "Run All" (Alt+F5) pass over an entire collection, routed
/// back to its collection on the main thread.
pub struct BatchRunUpdate {
    pub col_id: u64,
    /// Per-entry pass/fail, in the same order as `Collection::entries`.
    /// `None` for an entry the runner never reached (e.g. the whole file
    /// failed to parse before any entry ran).
    pub results: Vec<Option<bool>>,
    /// Every value captured across the whole run, merged in entry order (a
    /// later entry's capture of the same name wins) — the exact multi-entry
    /// equivalent of the single-entry `CaptureUpdate::values`.
    pub captures: HashMap<String, String>,
    /// Per-entry response snapshot, in the same order as `Collection::entries`
    /// and `results`. `None` for an entry the runner never reached — its
    /// previous `HurlEntry::last_response` (if any) is left untouched rather
    /// than cleared, since that's still the last response actually received
    /// for it.
    pub responses: Vec<Option<ApiResponse>>,
}

/// Build the Hurl entry to run for the selected entry, honoring an edited
/// request-JSON buffer for the request line/headers/body while keeping the
/// entry's `[Captures]`/`[Asserts]` (which the JSON model doesn't carry).
/// Returned unserialized (rather than as Hurl text directly) so the caller
/// can stage any out-of-scope `[Form]`/`[Multipart]` file fields first (see
/// [`stage_out_of_scope_form_files`]).
fn run_content(col: &Collection, env: Option<&Environment>) -> Option<HurlEntry> {
    let base = col.entries.get(col.selected_entry)?;
    let resolved = resolve_request(col, env)?;
    Some(HurlEntry {
        title: String::new(),
        method: resolved.method,
        url: resolved.url,
        headers: resolved.headers,
        basic_auth: None, // already encoded into `headers` by resolve_request
        form_fields: resolved.form_fields,
        query_params: base.query_params.clone(),
        cookies: resolved.cookies,
        body: resolved.body,
        expected_status: base.expected_status,
        captures: base.captures.clone(),
        asserts: base.asserts.clone(),
        user_added: base.user_added,
        modified: base.modified,
        last_run: base.last_run,
        last_response: None,
    })
}

/// Human-readable error for the one Hurl request shape that can never be
/// built: a `[Form]`/`[Multipart]` section together with a raw `[Body]`.
/// Hurl's own parser rejects this (a body ends the entry, so a following
/// `[Form]`/`[Multipart]` header is a syntax error) but with a cryptic
/// message; detecting it ourselves lets us surface something the user can
/// act on directly on the status bar.
const BODY_FORM_CONFLICT_ERROR: &str = "Can't send: a request can't have both a Body and Form/Multipart fields (Hurl doesn't support combining them) — remove one.";

/// Run the collection's selected entry on a background thread via the Hurl
/// runner, mapping the result (status, body, headers, `[Asserts]`, error) into
/// the shared `ApiResponse` (used for the "in flight" spinner while sending)
/// **and** returning a `Receiver` that carries the same finished response
/// (plus any captured values) tagged with the collection id and the entry's
/// index, so [`drain_capture_updates`] can remember it as that specific
/// entry's [`HurlEntry::last_response`]. Returns `None` when the request
/// can't be built at all (e.g. the Body/Form conflict) — nothing ran, so
/// there's nothing to route back.
pub fn run_collection(
    col: &Collection,
    env: Option<&Environment>,
    state: Arc<Mutex<ApiResponse>>,
) -> Option<Receiver<CaptureUpdate>> {
    if let Some(entry) = col.entries.get(col.selected_entry)
        && entry.body.is_some()
        && !entry.form_fields.is_empty()
    {
        let mut r = state.lock().unwrap();
        r.loading = false;
        r.error = BODY_FORM_CONFLICT_ERROR.to_string();
        return None;
    }
    let mut run_entry = run_content(col, env)?;
    let vars = collection_vars(env, &col.captures);
    let col_id = col.id;
    let entry_idx = col.selected_entry;
    let file_root = col
        .path
        .as_ref()
        .and_then(|p| p.parent().map(std::path::PathBuf::from));

    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        // Copy any `[Form]`/`[Multipart]` files that live outside file_root
        // into a temp directory alongside it first — otherwise Hurl's own
        // sandbox rejects them regardless of how the path is written. A
        // no-op (no copies, same file_root) when everything's already in
        // scope. Falls back to running unstaged on any I/O error so Hurl's
        // own error (e.g. "no such file") still surfaces normally.
        let mut entries = [run_entry.clone()];
        let staged_dir =
            stage_out_of_scope_form_files(&mut entries, file_root.as_deref()).unwrap_or_default();
        if staged_dir.is_some() {
            run_entry = entries[0].clone();
        }
        let run_root = staged_dir.as_deref().or(file_root.as_deref());
        let content = run_entry.to_hurl();

        let out = run_hurl(&content, &vars, run_root);
        if let Some(dir) = &staged_dir {
            let _ = std::fs::remove_dir_all(dir);
        }
        let mut r = state.lock().unwrap();
        r.loading = false;
        match out.entries.into_iter().next() {
            Some(eo) => {
                r.status = eo.status;
                r.status_text = eo.status_text;
                r.body = Arc::from(eo.body);
                r.headers = eo.headers;
                r.assert_results = eo.asserts;
                // Surface a transport failure / failed assert on the status bar.
                r.error = eo.error.or(out.error).unwrap_or_default();
                let values: HashMap<String, String> = eo.captures.into_iter().collect();
                let _ = tx.send(CaptureUpdate {
                    col_id,
                    entry_idx,
                    values,
                    response: r.clone(),
                });
            }
            None => {
                // Parse error, or nothing ran.
                r.error = out.error.unwrap_or_else(|| "no response".to_string());
            }
        }
    });
    Some(rx)
}

/// Run every entry in the collection, in order, in a single Hurl execution —
/// mirroring the CLI's batch mode, so Hurl's own cookie jar and `[Captures]`
/// chaining apply across the whole run exactly as they would from the command
/// line. Always returns a `Receiver` (except when the collection is empty or
/// a Body/Form conflict blocks it outright) since the caller needs it to
/// learn per-entry pass/fail and each entry's own response, regardless of
/// whether anything was captured.
pub fn run_all_entries(
    col: &Collection,
    env: Option<&Environment>,
    state: Arc<Mutex<ApiResponse>>,
) -> Option<Receiver<BatchRunUpdate>> {
    if col.entries.is_empty() {
        return None;
    }
    if let Some(bad) = col
        .entries
        .iter()
        .find(|e| e.body.is_some() && !e.form_fields.is_empty())
    {
        let mut r = state.lock().unwrap();
        r.loading = false;
        r.error = format!("{} ({})", BODY_FORM_CONFLICT_ERROR, bad.title);
        return None;
    }
    let content = collection_to_hurl(&col.entries);
    let vars = collection_vars(env, &col.captures);
    let col_id = col.id;
    let file_root = col
        .path
        .as_ref()
        .and_then(|p| p.parent().map(std::path::PathBuf::from));
    let total = col.entries.len();
    let mut run_entries = col.entries.clone();

    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        // See the matching comment in `run_collection`: bring any
        // out-of-scope `[Form]`/`[Multipart]` files into a temp directory
        // alongside file_root first, re-serializing only if staging
        // actually happened (the common case is a no-op).
        let staged_dir = stage_out_of_scope_form_files(&mut run_entries, file_root.as_deref())
            .unwrap_or_default();
        let run_root = staged_dir.as_deref().or(file_root.as_deref());
        let content = if staged_dir.is_some() {
            collection_to_hurl(&run_entries)
        } else {
            content
        };

        let out = run_hurl(&content, &vars, run_root);
        if let Some(dir) = &staged_dir {
            let _ = std::fs::remove_dir_all(dir);
        }
        let mut r = state.lock().unwrap();
        r.loading = false;
        let mut results: Vec<Option<bool>> = vec![None; total];
        let mut captures: HashMap<String, String> = HashMap::new();
        let mut responses: Vec<Option<ApiResponse>> = vec![None; total];
        match out.entries.last() {
            Some(last) => {
                r.status = last.status;
                r.status_text = last.status_text.clone();
                r.body = Arc::from(last.body.as_str());
                r.headers = last.headers.clone();
                r.assert_results = last.asserts.clone();
                r.error = last
                    .error
                    .clone()
                    .or_else(|| out.error.clone())
                    .unwrap_or_default();
            }
            None => {
                r.error = out
                    .error
                    .clone()
                    .unwrap_or_else(|| "no response".to_string());
            }
        }
        for (i, eo) in out.entries.iter().enumerate().take(total) {
            results[i] = Some(eo.ok);
            captures.extend(eo.captures.iter().cloned());
            responses[i] = Some(ApiResponse {
                status: eo.status,
                status_text: eo.status_text.clone(),
                body: Arc::from(eo.body.as_str()),
                loading: false,
                error: eo.error.clone().unwrap_or_default(),
                headers: eo.headers.clone(),
                assert_results: eo.asserts.clone(),
            });
        }
        let _ = tx.send(BatchRunUpdate {
            col_id,
            results,
            captures,
            responses,
        });
    });
    Some(rx)
}

/// Drain completed single-entry run results: merge captured values into the
/// collection's capture map, invalidate its cached preview so newly captured
/// values flow into subsequent requests, and remember the response on the
/// entry itself (`HurlEntry::last_response`) so the Response pane keeps
/// showing that entry's own last response even after the user selects a
/// different one. Returns `true` while any run is still in flight (so the UI
/// keeps repainting).
pub fn drain_capture_updates(
    pending: &mut Vec<Receiver<CaptureUpdate>>,
    collections: &mut [Collection],
) -> bool {
    if pending.is_empty() {
        return false;
    }
    let mut still = Vec::new();
    for rx in std::mem::take(pending) {
        let mut disconnected = false;
        loop {
            match rx.try_recv() {
                Ok(update) => {
                    for col in collections.iter_mut() {
                        if col.id == update.col_id {
                            for (k, v) in &update.values {
                                col.captures.insert(k.clone(), v.clone());
                            }
                            col.invalidate_request_json();
                            if let Some(entry) = col.entries.get_mut(update.entry_idx) {
                                entry.last_response = Some(update.response.clone());
                            }
                        }
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        if !disconnected {
            still.push(rx);
        }
    }
    *pending = still;
    !pending.is_empty()
}

/// Environment variable names referenced (via `{{ KEY }}`) anywhere in `entry`.
pub fn entry_referenced_keys(entry: &HurlEntry) -> std::collections::HashSet<String> {
    let mut keys = std::collections::HashSet::new();
    let mut add = |text: &str| keys.extend(crate::environment::referenced_keys(text));

    add(&entry.url);
    for (k, v) in &entry.headers {
        add(k);
        add(v);
    }
    for (k, v) in &entry.query_params {
        add(k);
        add(v);
    }
    for f in &entry.form_fields {
        add(&f.key);
        add(&f.value);
    }
    for (k, v) in &entry.cookies {
        add(k);
        add(v);
    }
    if let Some((u, p)) = &entry.basic_auth {
        add(u);
        add(p);
    }
    if let Some(body) = &entry.body {
        add(body);
    }
    keys
}

/// Secret variables the selected entry needs but that haven't resolved yet.
/// While this is non-empty the request must not be sent. Returns the blocking
/// variable names, sorted for stable display.
pub fn pending_request_keys(col: &Collection, env: Option<&Environment>) -> Vec<String> {
    let Some(env) = env else { return Vec::new() };
    let Some(entry) = col.entries.get(col.selected_entry) else {
        return Vec::new();
    };
    let referenced = entry_referenced_keys(entry);
    let mut blocking: Vec<String> = env
        .vars
        .iter()
        .filter(|v| v.is_pending() && referenced.contains(&v.key))
        .map(|v| v.key.clone())
        .collect();
    blocking.sort();
    blocking.dedup();
    blocking
}

/// Same as [`pending_request_keys`], but across every entry in the collection
/// — used to gate "Run All", which sends every request, not just the selected
/// one.
pub fn pending_request_keys_all(col: &Collection, env: Option<&Environment>) -> Vec<String> {
    let Some(env) = env else { return Vec::new() };
    let mut referenced = std::collections::HashSet::new();
    for entry in &col.entries {
        referenced.extend(entry_referenced_keys(entry));
    }
    let mut blocking: Vec<String> = env
        .vars
        .iter()
        .filter(|v| v.is_pending() && referenced.contains(&v.key))
        .map(|v| v.key.clone())
        .collect();
    blocking.sort();
    blocking.dedup();
    blocking
}

/// Drain background secret-resolution results, applying each to the matching
/// Global Environment and invalidating every collection's cached preview (any
/// of them might reference it, linked or active-global). Disconnected
/// channels are dropped. Returns `true` while any resolution is still in
/// flight. Shared by both front-ends' per-frame/per-tick update loop.
pub fn drain_env_updates(
    pending: &mut Vec<Receiver<EnvUpdate>>,
    global_envs: &mut [Environment],
    collections: &mut [Collection],
) -> bool {
    if pending.is_empty() {
        return false;
    }
    let mut still = Vec::new();
    for rx in std::mem::take(pending) {
        let mut disconnected = false;
        loop {
            match rx.try_recv() {
                Ok(update) => {
                    for env in global_envs.iter_mut() {
                        if env.id == update.env_id {
                            env.apply_update(&update);
                            for col in collections.iter_mut() {
                                col.invalidate_request_json();
                            }
                        }
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        if !disconnected {
            still.push(rx);
        }
    }
    *pending = still;
    !pending.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collection::Collection;
    use crate::environment::{EnvVar, Environment, ValueSource};
    use crate::hurl::HurlEntry;

    fn me_entry() -> HurlEntry {
        HurlEntry {
            method: "GET".into(),
            url: "{{ BASE_URL }}/me".into(),
            headers: vec![("Authorization".into(), "Bearer {{ API_TOKEN }}".into())],
            ..Default::default()
        }
    }

    fn env_token(value: &str, resolved: bool) -> Environment {
        Environment {
            id: 0,
            name: "e".into(),
            vars: vec![EnvVar {
                key: "API_TOKEN".into(),
                value: value.into(),
                source: ValueSource::OnePassword,
                resolved,
                loading: false,
                original_value: value.into(),
                modified: false,
                user_added: false,
                raw: String::new(),
            }],
            path: None,
            git_origin: None,
        }
    }

    fn auth_header(col: &Collection, env: Option<&Environment>) -> String {
        let headers = resolve_request(col, env).unwrap().headers;
        headers
            .into_iter()
            .find(|(k, _)| k == "Authorization")
            .unwrap()
            .1
    }

    /// The send path is always rebuilt from the entry + current environment, so
    /// reloading an environment (e.g. once an `op://` secret exists) is reflected
    /// in the request without any cache to invalidate.
    #[test]
    fn reloading_environment_refreshes_the_sent_request() {
        let col = Collection::new("c".into(), vec![me_entry()]);

        // 1st load: the op:// reference doesn't resolve yet → sent unresolved.
        let env = env_token("{{ op://Eng/demo-api/token }}", false);
        assert!(
            auth_header(&col, Some(&env)).contains("op://"),
            "unresolved ref is sent while missing"
        );

        // Secret now exists; reload the environment (resolved).
        let env = env_token("real-secret", true);

        assert_eq!(auth_header(&col, Some(&env)), "Bearer real-secret");
    }

    fn secret_var(key: &str, resolved: bool, loading: bool) -> EnvVar {
        EnvVar {
            key: key.into(),
            value: "{{ op://x }}".into(),
            source: ValueSource::OnePassword,
            resolved,
            loading,
            original_value: "{{ op://x }}".into(),
            modified: false,
            user_added: false,
            raw: String::new(),
        }
    }

    fn env_with(vars: Vec<EnvVar>) -> Environment {
        Environment {
            id: 1,
            name: "e".into(),
            vars,
            path: None,
            git_origin: None,
        }
    }

    #[test]
    fn pending_secret_blocks_the_referencing_request() {
        let entry = HurlEntry {
            method: "GET".into(),
            url: "{{ BASE_URL }}/{{ API_TOKEN }}".into(),
            ..Default::default()
        };
        let col = Collection::new("c".into(), vec![entry]);
        let env = env_with(vec![secret_var("API_TOKEN", false, true)]);

        assert_eq!(
            pending_request_keys(&col, Some(&env)),
            vec!["API_TOKEN".to_string()]
        );
    }

    #[test]
    fn resolved_secret_does_not_block() {
        let entry = HurlEntry {
            method: "GET".into(),
            url: "{{ BASE_URL }}/{{ API_TOKEN }}".into(),
            ..Default::default()
        };
        let col = Collection::new("c".into(), vec![entry]);
        let env = env_with(vec![secret_var("API_TOKEN", true, false)]);

        assert!(pending_request_keys(&col, Some(&env)).is_empty());
    }

    #[test]
    fn unreferenced_pending_secret_does_not_block() {
        let entry = HurlEntry {
            method: "GET".into(),
            url: "{{ BASE_URL }}/plain".into(),
            ..Default::default()
        };
        let col = Collection::new("c".into(), vec![entry]);
        let env = env_with(vec![secret_var("OTHER", false, true)]);

        assert!(
            pending_request_keys(&col, Some(&env)).is_empty(),
            "request doesn't use the loading secret"
        );
    }

    #[test]
    fn pending_request_keys_all_checks_every_entry_not_just_the_selected_one() {
        let first = HurlEntry {
            method: "GET".into(),
            url: "{{ BASE_URL }}/plain".into(),
            ..Default::default()
        };
        let second = HurlEntry {
            method: "GET".into(),
            url: "{{ BASE_URL }}/{{ API_TOKEN }}".into(),
            ..Default::default()
        };
        let mut col = Collection::new("c".into(), vec![first, second]);
        col.selected_entry = 0; // the pending secret is only used by entry 1
        let env = env_with(vec![secret_var("API_TOKEN", false, true)]);

        assert!(
            pending_request_keys(&col, Some(&env)).is_empty(),
            "the selected entry alone doesn't reference it"
        );
        assert_eq!(
            pending_request_keys_all(&col, Some(&env)),
            vec!["API_TOKEN".to_string()],
            "Run All must check every entry, not just the selected one"
        );
    }

    // ── Captures ──────────────────────────────────────────────────────────

    #[test]
    fn collection_vars_includes_captures_overriding_env() {
        let env = env_with(vec![EnvVar {
            key: "access_token".into(),
            value: "from-env".into(),
            source: ValueSource::Literal,
            resolved: true,
            loading: false,
            original_value: "from-env".into(),
            modified: false,
            user_added: false,
            raw: String::new(),
        }]);
        let mut captures = HashMap::new();
        captures.insert("access_token".to_string(), "from-capture".to_string());

        let vars = collection_vars(Some(&env), &captures);
        assert_eq!(
            vars.get("access_token").unwrap(),
            "from-capture",
            "a fresh capture wins over env"
        );
    }

    /// The top-bar Base URL must not act as a `{{ BASE_URL }}` source: when the
    /// environment doesn't define `BASE_URL`, the placeholder stays unresolved.
    #[test]
    fn base_url_is_not_substituted_from_app_vars() {
        let env = env_with(vec![EnvVar {
            key: "API_TOKEN".into(),
            value: "t".into(),
            source: ValueSource::Literal,
            resolved: true,
            loading: false,
            original_value: "t".into(),
            modified: false,
            user_added: false,
            raw: String::new(),
        }]);
        let vars = collection_vars(Some(&env), &HashMap::new());
        assert!(
            !vars.contains_key("BASE_URL"),
            "BASE_URL is not injected from the app"
        );
        assert_eq!(
            substitute("{{ BASE_URL }}/me", &vars),
            "{{ BASE_URL }}/me",
            "an unresolved {{ BASE_URL }} is left intact so the user notices"
        );
    }

    #[test]
    fn drain_capture_updates_routes_to_the_matching_collection() {
        let c1 = Collection::new("c1".into(), vec![]);
        let mut c2 = Collection::new("c2".into(), vec![HurlEntry::default()]);
        // c2 has a cached preview that must be invalidated when captures arrive.
        c2.request_json_for = Some(0);
        let target = c2.id;

        let (tx, rx) = mpsc::channel();
        let mut values = HashMap::new();
        values.insert("access_token".to_string(), "tok".to_string());
        let response = ApiResponse {
            status: 200,
            body: "ok".into(),
            ..Default::default()
        };
        tx.send(CaptureUpdate {
            col_id: target,
            entry_idx: 0,
            values,
            response,
        })
        .unwrap();
        drop(tx); // sender gone -> receiver disconnects after the one message

        let mut pending = vec![rx];
        let mut cols = [c1, c2];
        // Drain a few passes so the queued message is consumed.
        for _ in 0..3 {
            drain_capture_updates(&mut pending, &mut cols);
        }

        assert_eq!(
            cols[1].captures.get("access_token").unwrap(),
            "tok",
            "matching collection updated"
        );
        assert!(cols[0].captures.is_empty(), "other collection untouched");
        assert_eq!(cols[1].request_json_for, None, "target preview invalidated");
        assert_eq!(
            cols[1].entries[0].last_response.as_ref().map(|r| r.status),
            Some(200),
            "the entry remembers its own last response"
        );
    }

    /// `run_all_entries` always returns a receiver for a non-empty collection
    /// (unlike `run_collection`, which only returns one when there are
    /// captures) — the caller needs it purely to learn per-entry pass/fail.
    /// Doesn't wait for the background run to finish (this environment's test
    /// network hangs rather than fails fast on unroutable hosts — see
    /// `tui::tests::app_in_main_pane`'s doc comment) — only that the run was
    /// actually kicked off.
    #[test]
    fn run_all_entries_returns_a_receiver_for_a_non_empty_collection() {
        let e1 = HurlEntry {
            method: "GET".into(),
            url: "http://192.0.2.1/one".into(),
            ..Default::default()
        };
        let e2 = HurlEntry {
            method: "GET".into(),
            url: "http://192.0.2.1/two".into(),
            ..Default::default()
        };
        let col = Collection::new("c".into(), vec![e1, e2]);
        let state = Arc::new(Mutex::new(ApiResponse::default()));

        assert!(
            run_all_entries(&col, None, state).is_some(),
            "a non-empty collection must start a batch run"
        );
    }

    #[test]
    fn run_all_entries_rejects_a_body_form_conflict_naming_the_offending_entry() {
        let ok_entry = HurlEntry {
            method: "GET".into(),
            url: "http://192.0.2.1/ok".into(),
            ..Default::default()
        };
        let bad_entry = HurlEntry {
            title: "Bad One".into(),
            method: "POST".into(),
            url: "http://192.0.2.1/bad".into(),
            body: Some("{}".into()),
            form_fields: vec![FormField {
                key: "f".into(),
                value: "v".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let col = Collection::new("c".into(), vec![ok_entry, bad_entry]);
        let state = Arc::new(Mutex::new(ApiResponse::default()));

        let rx = run_all_entries(&col, None, state.clone());

        assert!(rx.is_none(), "must not start a run that can never be built");
        let r = state.lock().unwrap();
        assert!(!r.loading);
        assert!(
            r.error.contains("Body") && r.error.contains("Form") && r.error.contains("Bad One")
        );
    }

    // ── Substitution status classification (colour coding) ────────────────

    #[test]
    fn subst_map_classifies_variables_by_status() {
        use crate::environment::SECRET_MASK;
        let env = env_with(vec![
            EnvVar {
                key: "HOST".into(),
                value: "h".into(),
                source: ValueSource::Literal,
                resolved: true,
                loading: false,
                original_value: "h".into(),
                modified: false,
                user_added: false,
                raw: String::new(),
            },
            EnvVar {
                key: "PORT".into(),
                value: "8080".into(),
                source: ValueSource::ProcessEnv,
                resolved: true,
                loading: false,
                original_value: "8080".into(),
                modified: false,
                user_added: false,
                raw: String::new(),
            },
            secret_var("TOK", false, true),  // loading (pending)
            secret_var("BAD", false, false), // failed
            EnvVar {
                key: "SECRET".into(),
                value: "real".into(),
                source: ValueSource::OnePassword,
                resolved: true,
                loading: false,
                original_value: "real".into(),
                modified: false,
                user_added: false,
                raw: String::new(),
            },
        ]);
        let mut col = Collection::new(
            "c".into(),
            vec![HurlEntry {
                captures: vec![("token".into(), "jsonpath \"$.t\"".into())],
                ..Default::default()
            }],
        );
        col.captures.insert("post_id".into(), "42".into());

        let m = subst_map(&col, Some(&env));
        assert!(matches!(m["HOST"].kind, SubstKind::Literal));
        assert_eq!(
            m["HOST"].shown.as_deref(),
            Some("h"),
            "literal is substituted"
        );
        assert!(matches!(m["PORT"].kind, SubstKind::Loaded));
        assert_eq!(
            m["PORT"].shown.as_deref(),
            Some("8080"),
            "env-var value is substituted"
        );
        assert!(
            matches!(m["TOK"].kind, SubstKind::Pending) && m["TOK"].shown.is_none(),
            "loading secret is pending, kept"
        );
        assert!(
            matches!(m["BAD"].kind, SubstKind::Failed) && m["BAD"].shown.is_none(),
            "failed secret is red, kept"
        );
        assert!(matches!(m["SECRET"].kind, SubstKind::Loaded));
        assert_eq!(
            m["SECRET"].shown.as_deref(),
            Some(SECRET_MASK),
            "a resolved secret is masked, not revealed"
        );
        assert!(
            matches!(m["token"].kind, SubstKind::Failed) && m["token"].shown.is_none(),
            "uninitialised capture is red, kept"
        );
        assert!(matches!(m["post_id"].kind, SubstKind::Loaded));
        assert_eq!(
            m["post_id"].shown.as_deref(),
            Some("42"),
            "an initialised capture is substituted"
        );
    }

    #[test]
    fn subst_display_substitutes_known_and_keeps_unavailable() {
        let env = env_with(vec![
            EnvVar {
                key: "HOST".into(),
                value: "example.test".into(),
                source: ValueSource::Literal,
                resolved: true,
                loading: false,
                original_value: "example.test".into(),
                modified: false,
                user_added: false,
                raw: String::new(),
            },
            secret_var("TOK", false, true), // loading -> kept
        ]);
        let col = Collection::new("c".into(), vec![]);
        let m = subst_map(&col, Some(&env));
        assert_eq!(
            subst_display("{{ HOST }}/{{ TOK }}/{{ NOPE }}", &m),
            "example.test/{{ TOK }}/{{ NOPE }}",
            "known values are substituted; pending and unknown placeholders are kept",
        );
    }
}
