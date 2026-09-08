//! Framework-agnostic request logic shared by the GUI and the terminal UI:
//! app-level default variables (`AppVars`) and the Hurl-collection request
//! building / running, so both front-ends behave identically.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;

use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::collection::Collection;
use crate::environment::{EnvUpdate, Environment, ValueSource, substitute};
use crate::generators::GenError;
use crate::http::ApiResponse;
use crate::hurl::{
    EntryOutcome, FormField, HurlEntry, KvRow, RunOutput, RunStatus, collection_to_hurl,
    expand_base64_form_fields, run_hurl, stage_out_of_scope_form_files,
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
    /// Produced by the request's `# [Gen]` block at send time.
    ///
    /// Kept as `{{ VAR }}` rather than substituted, because the value doesn't
    /// exist yet and inventing one for the preview would mean a new random
    /// number or timestamp on every frame — a flickering preview that also
    /// wouldn't be what gets sent. Coloured as loaded, since it *will* have a
    /// value; a row that can't evaluate is reported separately (see
    /// [`generator_problems`]).
    Computed,
    /// Referenced by the request but defined nowhere at all — no environment
    /// variable, no `[Captures]` name, no captured value (kept as `{{ VAR }}`,
    /// red).
    ///
    /// Kept apart from [`SubstKind::Failed`] because the two ask for different
    /// things: a failed variable exists and can be retried, while an undefined
    /// one is usually a typo or a missing environment and has to be *added*.
    /// Before this existed an undefined placeholder matched nothing in the
    /// substitution map and was drawn as ordinary body text, so the one kind of
    /// broken variable the user could do nothing about was also the only one
    /// that looked completely fine.
    Undefined,
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

    // Names the `# [Gen]` block will compute at send time. Inserted before the
    // captures below so a capture of the same name — which is a real value the
    // preview can show — still wins.
    for e in &col.entries {
        for (name, _) in &e.generators {
            out.insert(
                name.clone(),
                SubstInfo {
                    shown: None,
                    kind: SubstKind::Computed,
                },
            );
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

/// A header/cookie/query-param value together with its enabled flag, as it
/// appears in the Raw JSON editor. An **enabled** entry serializes as a bare
/// scalar (`"X-Foo": "bar"`) so the common case stays clean and hand-editable;
/// a **disabled** entry serializes as a `[value, false]` pair so the flag
/// survives a round trip through the editor. On parse it tolerates either
/// shape: any bare scalar (string, number, bool, null) is treated as enabled,
/// while a `[value, enabled?, desc?]` array carries an explicit flag
/// (defaulting to enabled when omitted) and an optional description.
#[derive(Clone)]
struct KvValue {
    value: String,
    enabled: bool,
    /// The row's description. Carried through so the Code view round-trips a
    /// note rather than silently dropping it on the way back.
    desc: String,
}

impl serde::Serialize for KvValue {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        // The plain-string form is kept for the overwhelmingly common
        // "enabled, no note" row, so the Code view stays readable; the array
        // grows a third element only when there is actually a note to carry.
        if self.enabled && self.desc.is_empty() {
            s.serialize_str(&self.value)
        } else if self.desc.is_empty() {
            (&self.value, self.enabled).serialize(s)
        } else {
            (&self.value, self.enabled, &self.desc).serialize(s)
        }
    }
}

impl<'de> serde::Deserialize<'de> for KvValue {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(match Value::deserialize(d)? {
            Value::Array(arr) => KvValue {
                value: arr.first().map(value_as_text).unwrap_or_default(),
                enabled: arr.get(1).and_then(Value::as_bool).unwrap_or(true),
                desc: arr.get(2).map(value_as_text).unwrap_or_default(),
            },
            other => KvValue {
                value: value_as_text(&other),
                enabled: true,
                desc: String::new(),
            },
        })
    }
}

/// A header/cookie/query-param/basic-auth value. Serializes as a JSON string;
/// on parse it tolerantly coerces any hand-edited scalar (number, bool, null)
/// to text.
#[derive(Clone, Default)]
struct TextValue(String);

impl serde::Serialize for TextValue {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> serde::Deserialize<'de> for TextValue {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(TextValue(value_as_text(&Value::deserialize(d)?)))
    }
}

/// A JSON scalar as plain text: strings as-is, `null` as empty, anything else
/// via its JSON text.
fn value_as_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Form field `type`, lower-cased. Unknown or missing values parse as `Text`.
#[derive(Serialize, Default, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum FormKind {
    #[default]
    Text,
    File,
    #[serde(rename = "base64file")]
    Base64File,
}

impl<'de> serde::Deserialize<'de> for FormKind {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(d)?;
        Ok(match v.as_str() {
            Some("file") => FormKind::File,
            Some("base64file") => FormKind::Base64File,
            _ => FormKind::Text,
        })
    }
}

/// One `form_fields` entry (fields alphabetical, matching [`RequestJson`]).
#[derive(Serialize, Deserialize)]
struct FormFieldJson {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    base64_prefix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    content_type: Option<String>,
    #[serde(default)]
    key: TextValue,
    #[serde(rename = "type", default)]
    kind: FormKind,
    #[serde(default)]
    value: TextValue,
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    enabled: bool,
    /// The field's note. Omitted from the JSON entirely when empty, which is
    /// almost always, so the Code view isn't cluttered by it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    desc: String,
}

fn default_true() -> bool {
    true
}

fn is_true(b: &bool) -> bool {
    *b
}

impl From<&FormField> for FormFieldJson {
    fn from(f: &FormField) -> Self {
        Self {
            base64_prefix: f.base64_prefix.clone(),
            content_type: f.content_type.clone(),
            key: TextValue(f.key.clone()),
            kind: match f.kind {
                crate::hurl::FormFieldKind::File => FormKind::File,
                crate::hurl::FormFieldKind::Base64File => FormKind::Base64File,
                crate::hurl::FormFieldKind::Text => FormKind::Text,
            },
            value: TextValue(f.value.clone()),
            enabled: f.enabled,
            desc: f.desc.clone(),
        }
    }
}

impl From<FormFieldJson> for FormField {
    fn from(f: FormFieldJson) -> Self {
        Self {
            key: f.key.0,
            value: f.value.0,
            kind: match f.kind {
                FormKind::File => crate::hurl::FormFieldKind::File,
                FormKind::Base64File => crate::hurl::FormFieldKind::Base64File,
                FormKind::Text => crate::hurl::FormFieldKind::Text,
            },
            content_type: f.content_type,
            base64_prefix: f.base64_prefix,
            enabled: f.enabled,
            desc: f.desc,
        }
    }
}

/// `basic_auth` object; both fields default so a hand-edit dropping one still
/// parses.
#[derive(Serialize, Deserialize, Default)]
struct BasicAuthJson {
    #[serde(default)]
    pass: TextValue,
    #[serde(default)]
    user: TextValue,
}

/// Serde mirror of the Raw JSON editor's request shape. Fields are alphabetical
/// so the pretty output stays byte-identical to serde_json's default
/// `BTreeMap` ordering; header/cookie/param maps dedupe+sort keys; `body` is a
/// raw JSON value so JSON bodies inline while anything else stays a string.
#[derive(Serialize, Deserialize)]
struct RequestJson {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    basic_auth: Option<BasicAuthJson>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    body: Option<Value>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    cookies: BTreeMap<String, KvValue>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    form_fields: Vec<FormFieldJson>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    headers: BTreeMap<String, KvValue>,
    method: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    query_params: BTreeMap<String, KvValue>,
    url: String,
}

fn rows_to_map(rows: &[KvRow]) -> BTreeMap<String, KvValue> {
    rows.iter()
        .map(|r| {
            (
                r.key.clone(),
                KvValue {
                    value: r.value.clone(),
                    enabled: r.enabled,
                    desc: r.desc.clone(),
                },
            )
        })
        .collect()
}

fn map_to_rows(map: BTreeMap<String, KvValue>) -> Vec<KvRow> {
    map.into_iter()
        .map(|(key, kv)| KvRow {
            key,
            value: kv.value,
            enabled: kv.enabled,
            desc: kv.desc,
        })
        .collect()
}

/// Pretty-printed JSON of the request in its RAW, editable form: `{{ VAR }}`
/// placeholders are kept intact and basic auth is shown as a readable
/// `basic_auth` object (not an encoded header). The wire request is re-derived
/// (substituted + encoded) from the entry by [`resolve_entry`].
pub fn build_request_json(entry: &HurlEntry) -> String {
    let dto = RequestJson {
        basic_auth: entry.basic_auth.as_ref().map(|(user, pass)| BasicAuthJson {
            pass: TextValue(pass.clone()),
            user: TextValue(user.clone()),
        }),
        body: entry.body_src.as_deref().map(|raw| {
            serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_string()))
        }),
        cookies: rows_to_map(&entry.cookies),
        form_fields: entry.form_fields.iter().map(FormFieldJson::from).collect(),
        headers: rows_to_map(&entry.headers),
        method: entry.method.clone(),
        query_params: rows_to_map(&entry.queries),
        url: entry.url.clone(),
    };
    serde_json::to_string_pretty(&dto).unwrap_or_else(|_| "{}".into())
}

/// Parse the JSON from [`build_request_json`] back into a [`HurlEntry`],
/// carrying over the fields this view doesn't expose (`title`,
/// `expected_status`, `captures`, `asserts`, `user_added`) unchanged from
/// `base`. Errs on anything that isn't an object with a `method` and `url`.
pub fn apply_request_json(base: &HurlEntry, text: &str) -> Result<HurlEntry, String> {
    let dto: RequestJson = serde_json::from_str(text).map_err(|e| e.to_string())?;

    let body = match dto.body {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s),
        Some(v) => Some(serde_json::to_string_pretty(&v).unwrap_or_default()),
    };

    let mut entry = base.clone();
    entry.method = dto.method;
    entry.url = dto.url;
    entry.basic_auth = dto.basic_auth.map(|ba| (ba.user.0, ba.pass.0));
    entry.headers = map_to_rows(dto.headers);
    entry.cookies = map_to_rows(dto.cookies);
    entry.queries = map_to_rows(dto.query_params);
    entry.form_fields = dto.form_fields.into_iter().map(FormField::from).collect();
    entry.body_src = body;
    Ok(entry)
}

/// Which textual representation of a request the Main (Request) panel shows
/// by default, and copies whole when the panel has focus with nothing
/// selected: the pretty-printed JSON preview, or the actual Hurl text. An
/// app-wide preference (Settings → Preferences → Default Request View) that
/// applies to every request, not just the one currently selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum RequestView {
    Json,
    #[default]
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

/// Resolve one arbitrary `HurlEntry`'s `{{ VAR }}` placeholders against `vars`,
/// folding any `basic_auth` into an `Authorization` header. Callers pass the
/// entry they want (e.g. a collection's *selected* entry with the collection's
/// own vars); the report interpreter reuses it to resolve a request chosen by
/// name with its own scoped vars.
pub fn resolve_entry(entry: &HurlEntry, vars: &HashMap<String, String>) -> ResolvedRequest {
    let method = entry.method.clone();
    let url = substitute(&entry.url, vars);
    let mut headers: Vec<(String, String)> = entry
        .headers
        .iter()
        .filter(|r| r.enabled)
        .map(|r| (r.key.clone(), substitute(&r.value, vars)))
        .collect();
    if let Some((user, pass)) = &entry.basic_auth {
        let cred = STANDARD.encode(format!(
            "{}:{}",
            substitute(user, vars),
            substitute(pass, vars)
        ));
        headers.push(("Authorization".to_string(), format!("Basic {cred}")));
    }
    let cookies: Vec<(String, String)> = entry
        .cookies
        .iter()
        .filter(|r| r.enabled)
        .map(|r| (substitute(&r.key, vars), substitute(&r.value, vars)))
        .collect();
    let form_fields: Vec<FormField> = entry
        .form_fields
        .iter()
        .filter(|f| f.enabled)
        .map(|f| FormField {
            key: substitute(&f.key, vars),
            value: substitute(&f.value, vars),
            kind: f.kind,
            content_type: f.content_type.as_deref().map(|ct| substitute(ct, vars)),
            base64_prefix: f.base64_prefix.as_deref().map(|p| substitute(p, vars)),
            enabled: f.enabled,
            desc: String::new(),
        })
        .collect();

    // Resolving builds the request that actually goes out, so the comments
    // come off here and never reach the wire.
    let body = entry.body_wire().as_deref().map(|b| substitute(b, vars));
    ResolvedRequest {
        method,
        url,
        headers,
        cookies,
        form_fields,
        body,
    }
}

/// The result of running the collection's selected entry, routed back to its
/// collection: captured values (if any) plus a snapshot of the response
/// actually received, so it can be remembered per-entry (see
/// `HurlEntry::last_response`) rather than only in the shared "live" state.
pub struct CaptureUpdate {
    pub col_id: u64,
    pub entry_idx: usize,
    /// Whether the runner considered this entry a pass (status expectation,
    /// asserts and transport all satisfied) — mirrors `EntryOutcome::ok`, so
    /// the front-end can stamp the entry's pass/fail marker without re-deriving
    /// it from the response.
    pub ok: bool,
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
/// Build the concrete `HurlEntry` to run from a `base` entry and its already
/// resolved request line/headers/body/form fields, keeping the base's
/// `[Query]`/`[Captures]`/`[Asserts]`/`[Reports]`/expected-status metadata.
/// Shared by [`run_content`] and the report interpreter so both assemble the
/// run entry identically.
fn to_run_entry(base: &HurlEntry, resolved: ResolvedRequest) -> HurlEntry {
    let is_multipart = resolved
        .form_fields
        .iter()
        .any(|form| form.kind.is_multipart());
    HurlEntry {
        // Stamped when the collection adopts these entries as its baseline.
        uid: 0,
        unparsed: None,
        title: String::new(),
        method: resolved.method,
        url: resolved.url,
        headers: resolved
            .headers
            .iter()
            .map(|(k, v)| KvRow::new(k.clone(), v.clone()))
            .collect(),
        basic_auth: None, // already encoded into `headers` by resolve_entry
        form_fields: resolved.form_fields,
        is_multipart,
        queries: base.queries.clone(),
        cookies: resolved
            .cookies
            .iter()
            .map(|(k, v)| KvRow::new(k.clone(), v.clone()))
            .collect(),
        // Per-request `[Options]` (retry, insecure, delay, …) genuinely affect
        // the run, so carry them through to the executed entry.
        options: base.options.clone(),
        body_src: resolved.body,
        expected_status: base.expected_status,
        // Expected response version/headers/body are real (implicit) asserts in
        // the source `.hurl`, so preserve them on the run entry too — dropping
        // them would silently skip assertions the request author wrote.
        response_version: base.response_version.clone(),
        response_headers: base.response_headers.clone(),
        response_body: base.response_body.clone(),
        captures: base.captures.clone(),
        asserts: base.asserts.clone(),
        reports: base.reports.clone(),
        // Generators have already been evaluated into the variable set by
        // `effective_vars_reporting`, and their placeholders are ordinary `{{name}}`
        // references that Hurl resolves from it. Carrying the definitions onto
        // the run entry would re-emit the block as a comment in text nobody
        // reads back, and risk them being evaluated twice.
        generators: Vec::new(),
        // A transient copy that's only executed, never serialized — comments
        // don't affect the run.
        comments: Vec::new(),
        user_added: base.user_added,
        modified: base.modified,
        last_run: base.last_run,
        last_response: None,
    }
}

/// The variables a request actually runs with: those it is given, plus its own
/// declared parameter defaults, plus its `# [Gen]` rows evaluated over both.
/// Also returns whatever went wrong in the block, so the caller can say so
/// (see [`generator_problems`]) rather than sending a request whose signature
/// is still `{{sig}}`.
///
/// A computed value needs no separate secret handling even when it is derived
/// from one: it is only ever put into this map, which goes to `run_hurl` as
/// variables and is dropped afterwards. It reaches no preview (a generator name
/// renders as [`SubstKind::Computed`], keeping its braces rather than showing a
/// value) and no `state.json`. That is the same transient path a resolved
/// `op://` secret already takes, so an HMAC of a secret is no more exposed than
/// the secret was.
pub fn effective_vars_reporting<'a>(
    base: &HurlEntry,
    vars: &'a HashMap<String, String>,
) -> (Cow<'a, HashMap<String, String>>, Vec<GenError>) {
    let defaults = base.variable_defaults();
    if defaults.is_empty() && base.generators.is_empty() {
        return (Cow::Borrowed(vars), Vec::new());
    }
    let mut merged = vars.clone();
    for (name, value) in defaults {
        if merged.contains_key(&name) {
            continue;
        }
        let value = substitute(&value, &merged);
        merged.insert(name, value);
    }
    // Evaluated last so a generator may read a declared parameter, and bound
    // over anything of the same name: a row that computes `nonce` is a
    // statement that *this* is where `nonce` comes from.
    let errors = crate::generators::expand(
        &base.generators,
        &mut merged,
        &crate::generators::SystemSource::new(),
    );
    (Cow::Owned(merged), errors)
}

/// Remove the `variable:` rows from a run entry's `[Options]`, having already
/// folded them into the variable set via [`effective_vars_reporting`].
///
/// Left in, they would undo the default semantics for everything
/// [`resolve_entry`] does not substitute in Rust — `[Captures]` and `[Asserts]`
/// templates are resolved by Hurl itself, and Hurl treats the option as an
/// assignment that beats the passed-in variables. A report binding `FILE` would
/// then see its value in the URL and the request's sample value in an assert,
/// which is the sort of half-applied override that takes a day to find.
///
/// Dropping them also closes the documented oddity that a `variable:` option
/// leaks into *subsequent* entries of the same file, unlike every other option.
fn strip_variable_options(entry: &mut HurlEntry) {
    entry
        .options
        .retain(|r| !(r.enabled && r.key.trim().eq_ignore_ascii_case("variable")));
}

/// The whole-file counterpart of [`strip_variable_options`]: drop only the
/// `variable:` rows whose name the caller has **already bound**, and report
/// whether anything was removed.
///
/// A whole-collection run (the TUI's "Run All" in batch mode, `paperboy -c …`)
/// hands the serialized file to Hurl without resolving it in Rust first, so
/// Hurl applies the remaining defaults itself — which is exactly what is wanted
/// for a name nobody bound. Removing just the bound ones therefore produces the
/// same "default unless overridden" reading as the single-request path, with no
/// second implementation of the precedence rule.
///
/// One residual difference is Hurl's, not ours: a surviving default still leaks
/// into the entries *after* it in the same file (the documented exception to
/// per-entry options). That is what a hand-written `.hurl` does today, so it is
/// left alone rather than silently changed.
pub fn strip_bound_variable_options(
    entries: &mut [HurlEntry],
    vars: &HashMap<String, String>,
) -> bool {
    let mut stripped = false;
    for entry in entries {
        let bound: Vec<String> = entry
            .variable_defaults()
            .into_iter()
            .filter(|(name, _)| vars.contains_key(name))
            .map(|(name, _)| name)
            .collect();
        if bound.is_empty() {
            continue;
        }
        entry.options.retain(|r| {
            let is_bound_default = r.enabled
                && r.key.trim().eq_ignore_ascii_case("variable")
                && r.value
                    .split_once('=')
                    .is_some_and(|(name, _)| bound.iter().any(|b| b == name.trim()));
            !is_bound_default
        });
        stripped = true;
    }
    stripped
}

/// Run one already-chosen `base` entry with `vars` through the full per-request
/// pipeline used for a normal single-request send — base64-form expansion →
/// out-of-scope form-file staging → content-length defaulting → `to_hurl` →
/// [`run_hurl`] — and return the raw [`RunOutput`]. Front-end agnostic (no
/// `ApiResponse`/threads), so both [`run_collection`] and the report interpreter
/// execute a request through exactly the same code path.
///
/// `extra_captures` are appended to the entry's `[Captures]` before running
/// (used by the report interpreter to evaluate `[Reports]`/`WITH` fields as
/// transient captures); pass an empty slice for a plain send. A base64/staging
/// failure is surfaced as `RunOutput { entries: [], error: Some(..) }`.
pub fn run_resolved_entry(
    base: &HurlEntry,
    vars: &HashMap<String, String>,
    file_root: Option<&std::path::Path>,
    extra_captures: &[(String, String)],
) -> RunOutput {
    run_resolved_entry_reporting(base, vars, file_root, extra_captures).0
}

/// [`run_resolved_entry`], also handing back what the request's `# [Gen]` block
/// computed for *this* send.
///
/// A generator is the pre-request script's job: a request that signs itself
/// computes a `nonce` the request after it is expected to echo. Re-evaluating
/// the block to find that value would produce a different `uuid` and a later
/// `timestamp` than the one actually sent, so the values have to travel out of
/// the send that made them. The caller merges them where captures go
/// ([`CaptureUpdate::values`]), which is memory only — a computed value still
/// reaches no `state.json`, since it may be an HMAC of a secret and is in any
/// case usually good for one request.
pub fn run_resolved_entry_reporting(
    base: &HurlEntry,
    vars: &HashMap<String, String>,
    file_root: Option<&std::path::Path>,
    extra_captures: &[(String, String)],
) -> (RunOutput, HashMap<String, String>) {
    // Declared parameters are resolved *here*, not left to Hurl's own late
    // binding, because everything downstream works on resolved text: an
    // unresolved `{{FILE}}` in a `[Multipart]` file path would reach
    // `stage_out_of_scope_form_files` as a literal filename, fail to stage, and
    // then be rejected by Hurl's file sandbox when it finally did resolve.
    // Text the file could not be read at has no method, URL or body to send.
    // Handing it to the runner would produce a parse error naming a line
    // number in a document the user never sees, so say what is actually wrong.
    if base.is_unreadable() {
        return (
            RunOutput {
                entries: vec![],
                error: Some(UNREADABLE_REQUEST_ERROR.to_string()),
            },
            HashMap::new(),
        );
    }
    let (vars, gen_errors) = effective_vars_reporting(base, vars);
    // A failed `[Gen]` row is left unbound, so going on would send the request
    // with `{{sig}}` where the signature should be — a 401 whose cause is three
    // screens away. The interactive send refuses for this reason; a report run
    // has to refuse too, or the one path nobody is watching becomes the one
    // that lies. Reported in English like `UNREADABLE_REQUEST_ERROR`: this is
    // the front-end-agnostic layer and has no `Strings`, and the alternative —
    // a `RunOutput` that carries the errors structurally — is a wider change
    // than the message is worth.
    if !gen_errors.is_empty() {
        let english = crate::i18n::Strings::for_language(&crate::i18n::Language::English);
        return (
            RunOutput {
                entries: vec![],
                error: Some(crate::i18n::describe_gen_errors(&english, &gen_errors).join("; ")),
            },
            HashMap::new(),
        );
    }
    // Read off the block's results *before* the borrow of `vars` ends: these
    // are the values this send actually used, not what a second evaluation
    // would produce.
    let generated: HashMap<String, String> = base
        .generators
        .iter()
        .filter_map(|(name, _)| vars.get(name).map(|v| (name.clone(), v.clone())))
        .collect();
    let resolved = resolve_entry(base, &vars);
    let mut run_entry = to_run_entry(base, resolved);
    run_entry.captures.extend(extra_captures.iter().cloned());
    strip_variable_options(&mut run_entry);

    let mut entries = [run_entry];
    if let Err(e) = expand_base64_form_fields(&mut entries, file_root) {
        return (
            RunOutput {
                entries: vec![],
                error: Some(format!("Base64 file error: {e}")),
            },
            generated,
        );
    }
    let staged_dir = stage_out_of_scope_form_files(&mut entries, file_root).unwrap_or_default();
    let mut run_entry = entries.into_iter().next().unwrap();
    run_entry.ensure_run_content_length();
    let run_root = staged_dir.as_deref().or(file_root);

    let content = run_entry.to_hurl();
    let out = run_hurl(&content, &vars, run_root);
    if let Some(dir) = &staged_dir {
        let _ = std::fs::remove_dir_all(dir);
    }
    (out, generated)
}

/// Why a request that could not be read cannot be sent. Front-end agnostic, so
/// the terminal, the GUI and the report interpreter all say the same thing.
pub const UNREADABLE_REQUEST_ERROR: &str = "This request could not be read from the file, so there is nothing to send. \
     Open it in Raw Mode (Shift+H) to repair the Hurl text.";

/// Human-readable error for the one request shape that must never be sent: a
/// `[Form]`/`[Multipart]` section together with a raw body.
///
/// Hurl builds both onto the same libcurl handle, so the body overwrites the
/// form and the fields never leave the machine — while the `Content-Type` is
/// still chosen from the form, so the body goes out mislabelled. Nothing
/// errors: the request returns a perfectly good response to something the user
/// never asked for. Both front-ends refuse such a request before it gets here
/// (see [`body_form_conflicts`]); this is the backstop for every other caller,
/// and for a `.hurl` edited by hand.
const BODY_FORM_CONFLICT_ERROR: &str = "Can't send: a request can't have both a Body and Form/Multipart fields (Hurl sends the body and silently drops the fields) — remove one.";

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
    let base = col.entries.get(col.selected_entry)?;
    if base.body_form_conflict() {
        let mut r = state.lock().unwrap();
        r.loading = false;
        r.error = BODY_FORM_CONFLICT_ERROR.to_string();
        return None;
    }
    let base = base.clone();
    let vars = collection_vars(env, &col.captures);
    let col_id = col.id;
    let entry_idx = col.selected_entry;
    let file_root = col
        .path
        .as_ref()
        .and_then(|p| p.parent().map(std::path::PathBuf::from));

    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        // The whole per-request pipeline (base64-form expansion, out-of-scope
        // form-file staging, content-length defaulting, serialize + run) lives
        // in the shared, front-end-agnostic `run_resolved_entry` so this send
        // and the report interpreter stay in exact lockstep. A base64/staging
        // failure comes back as `RunOutput { entries: [], error }` and surfaces
        // via the `None` arm below.
        let (out, generated) =
            run_resolved_entry_reporting(&base, &vars, file_root.as_deref(), &[]);
        let mut r = state.lock().unwrap();
        r.loading = false;
        match out.entries.into_iter().next() {
            Some(eo) => {
                r.status = eo.status;
                r.status_text = eo.status_text;
                r.body = Arc::from(eo.body);
                r.headers = eo.headers;
                r.assert_results = eo.asserts;
                r.duration_ms = Some(eo.duration_ms);
                // Surface a transport failure / failed assert on the status bar.
                r.error = eo.error.or(out.error).unwrap_or_default();
                // The block's values go back with the captures, so the next
                // request sees a `nonce` this one computed exactly the way it
                // sees a token this one captured. A `[Captures]` row of the
                // same name is the later, more specific statement and wins.
                let mut values: HashMap<String, String> = generated;
                values.extend(eo.captures);
                let _ = tx.send(CaptureUpdate {
                    col_id,
                    entry_idx,
                    ok: eo.ok,
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

/// Build the standalone `ApiResponse` the Response pane shows for one entry of
/// a "Run All" pass (used by both the batch and streaming paths).
fn entry_response(eo: &EntryOutcome) -> ApiResponse {
    ApiResponse {
        status: eo.status,
        status_text: eo.status_text.clone(),
        body: Arc::from(eo.body.as_str()),
        loading: false,
        error: eo.error.clone().unwrap_or_default(),
        headers: eo.headers.clone(),
        assert_results: eo.asserts.clone(),
        duration_ms: Some(eo.duration_ms),
    }
}

/// Run every entry in the collection, in order.
///
/// Two modes, mirroring the CLI:
/// - **Streaming** (`batch == false`, the default): each entry runs on its
///   own and results are pushed out as they finish, so the Requests list
///   stamps each pass/fail marker live. Hurl's automatic cookie jar does
///   *not* carry from one request to the next in this mode (an explicit
///   `[Cookies]` section is unaffected) — the caller raises a status-bar
///   warning about that when a streaming Run All starts.
/// - **Batch** (`batch == true`): the whole collection runs in one Hurl
///   execution, so the cookie jar and `[Captures]` chaining apply across the
///   entire run exactly as they would from the command line; a single update
///   is sent when it finishes.
///
/// Always returns a `Receiver` (except when the collection is empty or a
/// Body/Form conflict blocks it outright) since the caller needs it to learn
/// per-entry pass/fail and each entry's own response, regardless of whether
/// anything was captured.
pub fn run_all_entries(
    col: &Collection,
    env: Option<&Environment>,
    state: Arc<Mutex<ApiResponse>>,
    batch: bool,
) -> Option<Receiver<BatchRunUpdate>> {
    if col.entries.is_empty() {
        return None;
    }
    if let Some(bad) = col.entries.iter().find(|e| e.body_form_conflict()) {
        let mut r = state.lock().unwrap();
        r.loading = false;
        r.error = format!("{} ({})", BODY_FORM_CONFLICT_ERROR, bad.title);
        return None;
    }
    let vars = collection_vars(env, &col.captures);
    let col_id = col.id;
    let file_root = col
        .path
        .as_ref()
        .and_then(|p| p.parent().map(std::path::PathBuf::from));
    let total = col.entries.len();
    // Requests the file could not be read at are skipped rather than sent.
    // They are text, not requests, so there is nothing to send — and putting
    // them in the run document would fail the *whole* run to parse, which is
    // the all-or-nothing failure recovery exists to end. Their positions are
    // kept so every result still lands on the request it came from.
    let run_positions: Vec<usize> = col
        .entries
        .iter()
        .enumerate()
        .filter(|(_, e)| !e.is_unreadable())
        .map(|(i, _)| i)
        .collect();
    if run_positions.is_empty() {
        return None;
    }
    let mut run_entries: Vec<HurlEntry> = run_positions
        .iter()
        .map(|&i| col.entries[i].clone())
        .collect();

    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        // See the matching comment in `run_collection`: bring any
        // out-of-scope `[Form]`/`[Multipart]` files into a temp directory
        // alongside file_root first. `collection_to_hurl` is (re)computed
        // after expansion + staging so it reflects both.
        // See `run_collection`: expand Base64File fields to their sent Text
        // form before staging, surfacing a read failure as an error.
        if let Err(e) = expand_base64_form_fields(&mut run_entries, file_root.as_deref()) {
            let mut r = state.lock().unwrap();
            r.loading = false;
            r.error = format!("Base64 file error: {e}");
            return;
        }
        let staged_dir = stage_out_of_scope_form_files(&mut run_entries, file_root.as_deref())
            .unwrap_or_default();
        let run_root = staged_dir.as_deref().or(file_root.as_deref());
        for e in &mut run_entries {
            e.ensure_run_content_length();
        }
        // A request's own `[Options] variable:` rows are defaults, so any name
        // the environment/captures already bind must not be re-assigned by the
        // request itself (Hurl's own reading). The rest stay in and Hurl
        // applies them, which is what a default should do.
        strip_bound_variable_options(&mut run_entries, &vars);
        let content = collection_to_hurl(&run_entries);

        let mut results: Vec<Option<bool>> = vec![None; total];
        let mut captures: HashMap<String, String> = HashMap::new();
        let mut responses: Vec<Option<ApiResponse>> = vec![None; total];

        // `# [Gen]` blocks. Same split as the headless runner: batch has no
        // per-request moment to evaluate anything in, so everything is bound
        // once up front (and a name two requests compute collapses to one
        // value — the caller warns before starting such a run); streaming
        // evaluates each block in its own window, which is also what lets a
        // generator read a value an earlier request captured.
        let mut vars = vars;
        if batch {
            let blocks = expand_batch_generators(
                &run_entries,
                &vars,
                &crate::generators::SystemSource::new(),
            );
            // Bound *and* reported back as captures: a computed value is as
            // much a result of the run as a `[Captures]` row, and the request
            // after it — run on its own afterwards — needs to see it.
            captures.extend(blocks.bound.iter().map(|(k, v)| (k.clone(), v.clone())));
            vars.extend(blocks.bound);
        }
        let out = if batch {
            run_hurl(&content, &vars, run_root)
        } else {
            // Streaming: run each entry on its own and push a cumulative
            // snapshot after every one, so the Requests list stamps each
            // pass/fail marker the instant that entry finishes rather than
            // only once the whole run is done. The poll side drains every
            // queued message per frame, so the intermediate snapshots simply
            // supersede one another. (Cookies set by one request don't carry
            // to the next in this mode — the caller warns about that.)
            // Hurl reports which request each outcome belongs to, and that is
            // not the outcome's ordinal: `[Options] repeat`/`retry` make one
            // request produce several. Trusting the ordinal slid every later
            // result up and dropped the last one off the end.
            let gen_entries = run_entries.clone();
            // Written by the before-each-entry hook and read by the
            // after-each-entry one; both run on this thread, one at a time, so
            // a `RefCell` is enough to let the two closures share it.
            let generated: Rc<RefCell<HashMap<String, String>>> = Rc::default();
            let record_gen = Rc::clone(&generated);
            crate::hurl::run::run_hurl_streaming_with(
                &content,
                &vars,
                run_root,
                |i, known| {
                    let Some(entry) = gen_entries.get(i) else {
                        return Vec::new();
                    };
                    if entry.generators.is_empty() {
                        return Vec::new();
                    }
                    let mut merged = known.clone();
                    crate::generators::expand(
                        &entry.generators,
                        &mut merged,
                        &crate::generators::SystemSource::new(),
                    );
                    let bound: Vec<(String, String)> = entry
                        .generators
                        .iter()
                        .filter_map(|(name, _)| merged.get(name).map(|v| (name.clone(), v.clone())))
                        .collect();
                    record_gen.borrow_mut().extend(bound.iter().cloned());
                    bound
                },
                |eo| {
                    if let Some(&at) = run_positions.get(eo.entry_index) {
                        results[at] = Some(eo.ok);
                        // Computed values first so a `[Captures]` row of the same
                        // name — the later, more specific statement — still wins.
                        captures.extend(
                            generated
                                .borrow()
                                .iter()
                                .map(|(k, v)| (k.clone(), v.clone())),
                        );
                        captures.extend(eo.captures.iter().cloned());
                        responses[at] = Some(entry_response(eo));
                    }
                    let _ = tx.send(BatchRunUpdate {
                        col_id,
                        results: results.clone(),
                        captures: captures.clone(),
                        responses: responses.clone(),
                    });
                },
            )
        };
        if let Some(dir) = &staged_dir {
            let _ = std::fs::remove_dir_all(dir);
        }
        let mut r = state.lock().unwrap();
        r.loading = false;
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
        // Batch ran the whole collection in one call, so fill the per-entry
        // vectors from the final result set and send a single update.
        // (Streaming already emitted its final cumulative snapshot above.)
        if batch {
            for eo in out.entries.iter() {
                // Keyed by the request Hurl says produced this outcome, not by
                // the outcome's position: see the streaming path above.
                let Some(&at) = run_positions.get(eo.entry_index) else {
                    continue;
                };
                results[at] = Some(eo.ok);
                captures.extend(eo.captures.iter().cloned());
                responses[at] = Some(entry_response(eo));
            }
            let _ = tx.send(BatchRunUpdate {
                col_id,
                results,
                captures,
                responses,
            });
        }
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
                                // The send finished — stamp the pass/fail marker
                                // and clear the "sending" (Running) state so the
                                // Response pane stops showing the spinner for
                                // this entry (only the still-in-flight entry does).
                                entry.last_run = if update.ok {
                                    RunStatus::Passed
                                } else {
                                    RunStatus::Failed
                                };
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

/// Every piece of an entry's text that reaches the wire *as a Hurl template* —
/// so anything scanning for `{{…}}` sees exactly the places a placeholder is
/// substituted, and no more.
///
/// Shared by [`entry_referenced_keys`] and [`entry_placeholder_problems`]
/// deliberately: the set of fields "a variable can appear in" and the set it is
/// "checked in" drifting apart would mean a placeholder that resolves but is
/// never validated, which is the exact bug the validation exists to catch.
fn for_each_wire_text(entry: &HurlEntry, mut visit: impl FnMut(&str)) {
    visit(&entry.url);
    for r in entry.headers.iter().chain(&entry.queries) {
        visit(&r.key);
        visit(&r.value);
    }
    for f in &entry.form_fields {
        visit(&f.key);
        visit(&f.value);
    }
    for r in &entry.cookies {
        visit(&r.key);
        visit(&r.value);
    }
    if let Some((u, p)) = &entry.basic_auth {
        visit(u);
        visit(p);
    }
    // A `{{ var }}` written inside a comment is never sent, so it doesn't
    // count as a use of that variable.
    if let Some(body) = entry.body_wire() {
        visit(&body);
    }
}

/// Environment variable names referenced (via `{{ KEY }}`) anywhere in `entry`.
pub fn entry_referenced_keys(entry: &HurlEntry) -> std::collections::HashSet<String> {
    let mut keys = std::collections::HashSet::new();
    for_each_wire_text(entry, |text| {
        keys.extend(crate::environment::referenced_keys(text))
    });
    keys
}

/// Placeholders in `entry` that Hurl would read differently from PaperBoy — see
/// [`placeholder_problems`](crate::hurl::placeholder_problems). Deduplicated,
/// keeping the order written, since the same `{{ api.key }}` typed into three
/// headers is one mistake and should be said once.
pub fn entry_placeholder_problems(entry: &HurlEntry) -> Vec<crate::hurl::PlaceholderProblem> {
    let mut out: Vec<crate::hurl::PlaceholderProblem> = Vec::new();
    for_each_wire_text(entry, |text| {
        for p in crate::hurl::placeholder_problems(text) {
            if !out.contains(&p) {
                out.push(p);
            }
        }
    });
    out
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

/// Every variable name the collection can supply a value for, whether or not it
/// has one yet: the environment's variables, every `[Captures]` name any request
/// defines, and anything already captured this session.
///
/// A `[Captures]` name counts as defined even before it holds a value, because
/// a collection that logs in and then uses `{{ token }}` is correct — the value
/// arrives mid-run. Treating those as undefined would put a warning on every
/// well-formed collection, which is the fastest way to teach someone to ignore
/// warnings.
fn defined_keys(col: &Collection, env: Option<&Environment>) -> std::collections::HashSet<String> {
    let mut defined: std::collections::HashSet<String> = std::collections::HashSet::new();
    if let Some(env) = env {
        defined.extend(env.vars.iter().map(|v| v.key.clone()));
    }
    for entry in &col.entries {
        defined.extend(entry.captures.iter().map(|(name, _)| name.clone()));
        // A `# [Gen]` row defines its name just as surely as a capture does —
        // it is computed rather than fetched, but `{{sig}}` is not a typo.
        defined.extend(entry.generators.iter().map(|(name, _)| name.clone()));
    }
    defined.extend(col.captures.keys().cloned());
    defined
}

/// Variables the selected request references that nothing defines — the typo'd
/// `{{ tokn }}`, or the whole environment nobody remembered to activate.
///
/// Unlike [`pending_request_keys`] this does **not** block the run: sending a
/// literal `{{ tokn }}` is legal (Hurl will do it), and a front-end may have
/// good reason to. It is reported instead, loudly, because the failure it
/// causes otherwise surfaces as an unexplained 401 several steps later.
/// Sorted for stable display.
pub fn undefined_request_keys(col: &Collection, env: Option<&Environment>) -> Vec<String> {
    let Some(entry) = col.entries.get(col.selected_entry) else {
        return Vec::new();
    };
    let defined = defined_keys(col, env);
    let mut out: Vec<String> = entry_referenced_keys(entry)
        .into_iter()
        .filter(|k| !defined.contains(k))
        .collect();
    out.sort();
    out
}

/// [`undefined_request_keys`] across every entry in the collection — used by
/// "Run All", which sends all of them.
pub fn undefined_request_keys_all(col: &Collection, env: Option<&Environment>) -> Vec<String> {
    let defined = defined_keys(col, env);
    let mut out: Vec<String> = col
        .entries
        .iter()
        .flat_map(entry_referenced_keys)
        .filter(|k| !defined.contains(k))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// The name of the selected request if it carries both a raw body and enabled
/// form fields, which Hurl cannot send together — see
/// [`HurlEntry::body_form_conflict`](crate::hurl::HurlEntry::body_form_conflict).
///
/// Unlike [`undefined_request_keys`] this *does* block the run. An undefined
/// variable is sent literally and fails visibly; this one succeeds while
/// quietly dropping every form field, so the only way for a user to find out is
/// to notice that the server behaved as though the fields were never there.
pub fn body_form_conflicts(col: &Collection) -> Vec<String> {
    col.entries
        .get(col.selected_entry)
        .filter(|e| e.body_form_conflict())
        .map(|e| vec![request_label(e)])
        .unwrap_or_default()
}

/// [`body_form_conflicts`] across every entry — used by "Run All".
pub fn body_form_conflicts_all(col: &Collection) -> Vec<String> {
    col.entries
        .iter()
        .filter(|e| e.body_form_conflict())
        .map(request_label)
        .collect()
}

/// Placeholders in the selected request that Hurl reads differently from
/// PaperBoy, rendered for display as `written → read`.
///
/// Blocking, for the same reason as [`body_form_conflicts`] and not the same
/// reason as [`undefined_request_keys`]: an undefined variable goes on the wire
/// literally and fails loudly, but `{{ api.key }}` goes on the wire as the
/// value of `api` and the server answers as though it were asked a sensible
/// question. There is no version of that a user finds by reading the response.
pub fn truncated_placeholders(col: &Collection) -> Vec<String> {
    col.entries
        .get(col.selected_entry)
        .map(|e| describe_placeholder_problems(&entry_placeholder_problems(e)))
        .unwrap_or_default()
}

/// [`truncated_placeholders`] across every entry — used by "Run All".
pub fn truncated_placeholders_all(col: &Collection) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for e in &col.entries {
        for d in describe_placeholder_problems(&entry_placeholder_problems(e)) {
            if !out.contains(&d) {
                out.push(d);
            }
        }
    }
    out
}

/// Anything wrong with the selected request's `# [Gen]` block, described for a
/// status message.
///
/// Reported rather than blocking, unlike [`truncated_placeholders`]. A row that
/// fails binds nothing, so its `{{sig}}` goes on the wire literally and comes
/// back a loud 401 — the [`undefined_request_keys`] situation, not the
/// [`body_form_conflicts`] one. The reason is still worth saying at the moment
/// it happens, because "401" is a poor way to learn you misspelled `hmac_sha256`.
pub fn generator_problems(col: &Collection, env: Option<&Environment>) -> Vec<GenError> {
    let Some(entry) = col.entries.get(col.selected_entry) else {
        return Vec::new();
    };
    describe_generator_errors(entry, env, &col.captures)
}

/// [`generator_problems`] across every entry — used by "Run All".
pub fn generator_problems_all(col: &Collection, env: Option<&Environment>) -> Vec<GenError> {
    let mut out: Vec<GenError> = Vec::new();
    for entry in &col.entries {
        for d in describe_generator_errors(entry, env, &col.captures) {
            if !out.contains(&d) {
                out.push(d);
            }
        }
    }
    out
}

/// Everything a whole-file run needs to know about the `# [Gen]` blocks it is
/// about to carry.
///
/// A block belongs to one request and is evaluated per send, which a streaming
/// run can honour (each entry gets its own window) but a batch run cannot: batch
/// is a single Hurl call over the whole file, so there is no "before this
/// request" moment to evaluate anything in. Everything is therefore evaluated
/// once, up front, against the environment alone — and a name two requests each
/// compute collapses to one value for both, which is the [`collisions`] list.
///
/// [`collisions`]: BatchGenerators::collisions
pub struct BatchGenerators {
    /// The names bound for the run. Where two requests compute the same name
    /// the first request's value is kept, matching the order Hurl would have
    /// run them in.
    pub bound: HashMap<String, String>,
    /// What failed, per request, paired with that request's title so the
    /// caller can say which one it was.
    pub errors: Vec<(String, Vec<GenError>)>,
    /// Names computed by more than one request. In a streaming run each of
    /// those requests gets its own value; in a batch run they share the first,
    /// so the caller warns rather than silently sending one request's nonce
    /// with another's signature.
    pub collisions: Vec<String>,
}

/// Evaluate every entry's `# [Gen]` block once, for a batch (whole-file) run.
///
/// Shared by the headless runner and "Run All" in batch mode so the two cannot
/// drift on which value a name ends up with.
pub fn expand_batch_generators(
    entries: &[crate::hurl::HurlEntry],
    vars: &HashMap<String, String>,
    src: &dyn crate::generators::GenSource,
) -> BatchGenerators {
    let mut bound = HashMap::new();
    let mut errors = Vec::new();
    let mut collisions: Vec<String> = Vec::new();
    // Which request first claimed each name, so a second claim is recognised
    // as a collision rather than as the same request being listed twice (a
    // repeated name *within* one block is that block's own business).
    let mut claimed: HashMap<String, usize> = HashMap::new();

    for (i, e) in entries.iter().enumerate() {
        if e.generators.is_empty() {
            continue;
        }
        // Each block is evaluated against the environment plus what earlier
        // blocks bound — not against the run's own captures, which do not
        // exist yet in a batch run.
        let mut merged = vars.clone();
        merged.extend(
            bound
                .iter()
                .map(|(k, v): (&String, &String)| (k.clone(), v.clone())),
        );
        let errs = crate::generators::expand(&e.generators, &mut merged, src);
        if !errs.is_empty() {
            errors.push((e.title.clone(), errs));
        }
        for (name, _) in &e.generators {
            match claimed.get(name) {
                Some(&first) if first != i => {
                    if !collisions.contains(name) {
                        collisions.push(name.clone());
                    }
                    continue;
                }
                _ => {}
            }
            claimed.entry(name.clone()).or_insert(i);
            if let Some(v) = merged.get(name) {
                bound.entry(name.clone()).or_insert_with(|| v.clone());
            }
        }
    }

    BatchGenerators {
        bound,
        errors,
        collisions,
    }
}

/// The `# [Gen]` names more than one request in the collection computes.
///
/// Only a batch run has to care (see [`BatchGenerators::collisions`]); the
/// front-ends call this to warn before starting one.
pub fn generator_collisions(col: &Collection) -> Vec<String> {
    let mut claimed: Vec<&str> = Vec::new();
    let mut out: Vec<String> = Vec::new();
    for e in &col.entries {
        let mut seen_here: Vec<&str> = Vec::new();
        for (name, _) in &e.generators {
            if seen_here.contains(&name.as_str()) {
                continue;
            }
            seen_here.push(name);
            if claimed.contains(&name.as_str()) {
                if !out.contains(name) {
                    out.push(name.clone());
                }
            } else {
                claimed.push(name);
            }
        }
    }
    out
}

fn describe_generator_errors(
    entry: &crate::hurl::HurlEntry,
    env: Option<&Environment>,
    captures: &HashMap<String, String>,
) -> Vec<GenError> {
    if entry.generators.is_empty() {
        return Vec::new();
    }
    let vars = collection_vars(env, captures);
    // Every failure this reports is deterministic — a syntax error, an unknown
    // function, a bad reference — so evaluating here and again at send time
    // cannot disagree, even though the random and time values will differ.
    effective_vars_reporting(entry, &vars).1
}

/// Render placeholder problems for a status message. A truncation says what it
/// becomes (`{{api.key}} → api`), because the whole difficulty of the bug is
/// that the text looks right; an unparsable one has no "becomes" to show.
fn describe_placeholder_problems(problems: &[crate::hurl::PlaceholderProblem]) -> Vec<String> {
    use crate::hurl::PlaceholderProblem as P;
    problems
        .iter()
        .map(|p| match p {
            P::Truncated { written, read } => format!("{written} → {read}"),
            P::Unparsable { written } => written.clone(),
        })
        .collect()
}

/// How a request is named in a message about it: its title, or its method and
/// URL when it hasn't been given one (an untitled request is still worth
/// pointing at).
fn request_label(e: &crate::hurl::HurlEntry) -> String {
    if e.title.trim().is_empty() {
        format!("{} {}", e.method, e.url)
    } else {
        e.title.clone()
    }
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
    use crate::hurl::{FormField, FormFieldKind, HurlEntry};

    #[test]
    fn request_json_is_alphabetically_ordered_and_round_trips() {
        let entry = HurlEntry {
            method: "POST".into(),
            url: "http://example.com/api".into(),
            // Deliberately out of order to prove keys come out sorted.
            headers: vec![
                KvRow::toggled("X-Zed", "z", true),
                KvRow::toggled("Authorization", "Bearer t", true),
            ],
            cookies: vec![KvRow::toggled("session", "abc", true)],
            queries: vec![KvRow::toggled("page", "2", true)],
            basic_auth: Some(("alice".into(), "secret".into())),
            form_fields: vec![
                FormField {
                    key: "name".into(),
                    value: "widget".into(),
                    kind: FormFieldKind::Text,
                    content_type: None,
                    base64_prefix: None,
                    enabled: true,
                    desc: String::new(),
                },
                FormField {
                    key: "file".into(),
                    value: "./a.bin".into(),
                    kind: FormFieldKind::File,
                    content_type: Some("application/octet-stream".into()),
                    base64_prefix: None,
                    enabled: true,
                    desc: String::new(),
                },
            ],
            body_src: Some(r#"{"a":1}"#.into()),
            ..Default::default()
        };

        let json = build_request_json(&entry);
        let expected = r#"{
  "basic_auth": {
    "pass": "secret",
    "user": "alice"
  },
  "body": {
    "a": 1
  },
  "cookies": {
    "session": "abc"
  },
  "form_fields": [
    {
      "key": "name",
      "type": "text",
      "value": "widget"
    },
    {
      "content_type": "application/octet-stream",
      "key": "file",
      "type": "file",
      "value": "./a.bin"
    }
  ],
  "headers": {
    "Authorization": "Bearer t",
    "X-Zed": "z"
  },
  "method": "POST",
  "query_params": {
    "page": "2"
  },
  "url": "http://example.com/api"
}"#;
        assert_eq!(json, expected);

        // Re-parsing yields the same request fields (headers/cookies/params are
        // sorted by key on the round trip, matching the serialized object).
        let back = apply_request_json(&HurlEntry::default(), &json).unwrap();
        assert_eq!(back.method, "POST");
        assert_eq!(back.url, "http://example.com/api");
        assert_eq!(back.basic_auth, Some(("alice".into(), "secret".into())));
        assert_eq!(
            back.headers,
            vec![
                ("Authorization".to_string(), "Bearer t".to_string(), true),
                ("X-Zed".to_string(), "z".to_string(), true),
            ]
        );
        assert_eq!(back.form_fields.len(), 2);
        assert_eq!(back.form_fields[1].kind, FormFieldKind::File);
        assert_eq!(back.body_src.as_deref(), Some("{\n  \"a\": 1\n}"));
    }

    #[test]
    fn apply_request_json_tolerates_non_string_scalars_and_unknown_form_type() {
        let base = HurlEntry::default();
        let text = r#"{
            "method": "GET",
            "url": "http://x",
            "headers": { "X-Count": 5 },
            "form_fields": [ { "key": "k", "value": "v", "type": "weird" } ]
        }"#;
        let entry = apply_request_json(&base, text).unwrap();
        assert_eq!(
            entry.headers,
            vec![("X-Count".to_string(), "5".to_string(), true)]
        );
        assert_eq!(entry.form_fields[0].kind, FormFieldKind::Text);
    }

    #[test]
    fn apply_request_json_rejects_input_without_method_or_url() {
        let base = HurlEntry::default();
        assert!(apply_request_json(&base, "[]").is_err());
        assert!(apply_request_json(&base, r#"{"url":"http://x"}"#).is_err());
    }

    fn me_entry() -> HurlEntry {
        HurlEntry {
            method: "GET".into(),
            url: "{{ BASE_URL }}/me".into(),
            headers: vec![KvRow::new("Authorization", "Bearer {{ API_TOKEN }}")],
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
        let vars = collection_vars(env, &col.captures);
        let headers = resolve_entry(&col.entries[col.selected_entry], &vars).headers;
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

    // ── Undefined variables ───────────────────────────────────────────────

    fn plain_var(key: &str, value: &str) -> EnvVar {
        EnvVar {
            key: key.into(),
            value: value.into(),
            source: ValueSource::Literal,
            resolved: true,
            loading: false,
            original_value: value.into(),
            modified: false,
            user_added: false,
            raw: String::new(),
        }
    }

    #[test]
    fn a_variable_no_one_defines_is_reported() {
        let entry = HurlEntry {
            method: "GET".into(),
            url: "{{ BASE_URL }}/{{ tokn }}".into(),
            ..Default::default()
        };
        let col = Collection::new("c".into(), vec![entry]);
        let env = env_with(vec![plain_var("BASE_URL", "http://x")]);

        assert_eq!(
            undefined_request_keys(&col, Some(&env)),
            vec!["tokn".to_string()],
            "the typo is named; the variable that exists is not"
        );
    }

    #[test]
    fn a_capture_name_counts_as_defined_before_it_has_a_value() {
        // The "log in, then use {{ token }}" shape: the value only exists
        // mid-run, but the collection is correct and must not be flagged.
        let login = HurlEntry {
            method: "POST".into(),
            url: "http://x/login".into(),
            captures: vec![("token".into(), "jsonpath \"$.token\"".into())],
            ..Default::default()
        };
        let use_it = HurlEntry {
            method: "GET".into(),
            url: "http://x/me?t={{ token }}".into(),
            ..Default::default()
        };
        let mut col = Collection::new("c".into(), vec![login, use_it]);
        col.selected_entry = 1;

        assert!(
            undefined_request_keys(&col, None).is_empty(),
            "a captured name is defined even before the capture has run"
        );
    }

    #[test]
    fn a_pending_secret_is_defined_not_undefined() {
        // It has a source and is on its way — that's WaitingSecrets' job to
        // report, and double-reporting it would be noise.
        let entry = HurlEntry {
            method: "GET".into(),
            url: "{{ API_TOKEN }}".into(),
            ..Default::default()
        };
        let col = Collection::new("c".into(), vec![entry]);
        let env = env_with(vec![secret_var("API_TOKEN", false, true)]);

        assert!(undefined_request_keys(&col, Some(&env)).is_empty());
    }

    #[test]
    fn with_no_environment_active_every_referenced_variable_is_undefined() {
        let entry = HurlEntry {
            method: "GET".into(),
            url: "{{ BASE_URL }}/x".into(),
            headers: vec![KvRow::toggled(
                "Authorization",
                "Bearer {{ API_KEY }}",
                true,
            )],
            ..Default::default()
        };
        let col = Collection::new("c".into(), vec![entry]);

        assert_eq!(
            undefined_request_keys(&col, None),
            vec!["API_KEY".to_string(), "BASE_URL".to_string()],
            "sorted, and headers are scanned as well as the URL"
        );
    }

    #[test]
    fn undefined_request_keys_all_checks_every_entry_and_dedupes() {
        let first = HurlEntry {
            method: "GET".into(),
            url: "{{ nope }}/a".into(),
            ..Default::default()
        };
        let second = HurlEntry {
            method: "GET".into(),
            url: "{{ nope }}/b".into(),
            ..Default::default()
        };
        let mut col = Collection::new("c".into(), vec![first, second]);
        col.selected_entry = 0;

        assert_eq!(
            undefined_request_keys_all(&col, None),
            vec!["nope".to_string()],
            "reported once, not once per entry that uses it"
        );
    }

    // ── Captures ──────────────────────────────────────────────────────────

    fn entry_with_generators(url: &str, rows: &[(&str, &str)]) -> HurlEntry {
        let mut e = HurlEntry {
            method: "GET".into(),
            url: url.into(),
            ..Default::default()
        };
        e.generators = rows
            .iter()
            .map(|(n, x)| (n.to_string(), x.to_string()))
            .collect();
        e
    }

    // ── Whole-file `# [Gen]` handling ──────────────────────────────────

    /// A batch run has one variable set for the whole file, so two requests
    /// that each compute `nonce` cannot each have their own. Silently picking
    /// one is how a signature ends up computed over the other request's nonce,
    /// so the collision is named and the caller warns.
    #[test]
    fn two_requests_computing_one_name_collide_in_a_batch_run() {
        let entries = vec![
            entry_with_generators("https://x/", &[("nonce", "\"first\"")]),
            entry_with_generators("https://y/", &[("nonce", "\"second\"")]),
        ];
        let blocks = expand_batch_generators(
            &entries,
            &HashMap::new(),
            &crate::generators::SystemSource::new(),
        );
        assert_eq!(blocks.collisions, vec!["nonce".to_string()]);
        assert_eq!(
            blocks.bound.get("nonce").map(String::as_str),
            Some("first"),
            "the first request in the file keeps its value"
        );
        assert!(blocks.errors.is_empty(), "{:?}", blocks.errors);
    }

    /// Only a *second request* claiming the name is a collision. One request
    /// listing a name twice is that block's own business, and reporting it as
    /// a batch-only hazard would send the user looking for a request that
    /// isn't there.
    #[test]
    fn one_request_is_never_in_collision_with_itself() {
        let entries = vec![entry_with_generators(
            "https://x/",
            &[("n", "\"a\""), ("n", "\"b\"")],
        )];
        let blocks = expand_batch_generators(
            &entries,
            &HashMap::new(),
            &crate::generators::SystemSource::new(),
        );
        assert!(blocks.collisions.is_empty(), "{:?}", blocks.collisions);
    }

    /// A later block may read what an earlier one computed — the same reading
    /// streaming gives, so moving between the two modes doesn't change which
    /// names resolve.
    #[test]
    fn a_later_gen_block_can_read_an_earlier_one_in_batch() {
        let entries = vec![
            entry_with_generators("https://x/", &[("base", "\"abc\"")]),
            entry_with_generators("https://y/", &[("derived", "base")]),
        ];
        let blocks = expand_batch_generators(
            &entries,
            &HashMap::new(),
            &crate::generators::SystemSource::new(),
        );
        assert_eq!(blocks.bound.get("derived").map(String::as_str), Some("abc"));
    }

    /// Failures are reported per request: "one of them is wrong" is not a
    /// report when the file has thirty requests in it.
    #[test]
    fn a_failing_block_is_reported_against_its_own_request() {
        let mut bad = entry_with_generators("https://y/", &[("sig", "nope()")]);
        bad.title = "Sign".into();
        let entries = vec![entry_with_generators("https://x/", &[("n", "uuid")]), bad];
        let blocks = expand_batch_generators(
            &entries,
            &HashMap::new(),
            &crate::generators::SystemSource::new(),
        );
        assert_eq!(blocks.errors.len(), 1);
        assert_eq!(blocks.errors[0].0, "Sign");
    }

    /// The pre-flight warning the front-ends show, which reads the collection
    /// rather than evaluating anything.
    #[test]
    fn generator_collisions_names_only_what_two_requests_share() {
        let col = Collection::new(
            "c".into(),
            vec![
                entry_with_generators("https://x/", &[("nonce", "uuid"), ("ts", "timestamp")]),
                entry_with_generators("https://y/", &[("nonce", "uuid")]),
            ],
        );
        assert_eq!(generator_collisions(&col), vec!["nonce".to_string()]);
    }

    /// A computed value is a result of the send, not a detail of it: the
    /// request after the one that signed itself has to be able to echo the
    /// nonce, so the value has to travel out of the send that made it rather
    /// than be recomputed (which would produce a different `uuid`).
    #[test]
    fn a_send_hands_back_what_its_gen_block_computed() {
        let entry = entry_with_generators("https://127.0.0.1:1/", &[("nonce", "\"fixed\"")]);
        let (_out, generated) = run_resolved_entry_reporting(&entry, &HashMap::new(), None, &[]);
        assert_eq!(
            generated.get("nonce").map(String::as_str),
            Some("fixed"),
            "the connection failing doesn't unmake the value it was sent with"
        );
    }

    /// A name the `# [Gen]` block computes is defined by that block. Before
    /// `defined_keys` knew about generators, a request that signed itself
    /// correctly was still reported as referring to an undefined variable.
    #[test]
    fn a_generated_name_is_not_reported_as_undefined() {
        let col = Collection::new(
            "c".into(),
            vec![entry_with_generators(
                "https://x/?n={{nonce}}",
                &[("nonce", "uuid")],
            )],
        );
        assert!(
            undefined_request_keys(&col, None).is_empty(),
            "the block defines nonce"
        );
        assert!(
            generator_problems(&col, None).is_empty(),
            "and the row evaluates"
        );
    }

    /// A row that cannot evaluate is *reported*, not blocked: nothing binds
    /// `sig`, so `{{sig}}` goes out literally and the server rejects it loudly.
    /// The report exists to name the actual mistake instead of leaving the user
    /// to infer it from a 401.
    #[test]
    fn a_generator_row_that_cannot_evaluate_is_reported() {
        let col = Collection::new(
            "c".into(),
            vec![entry_with_generators(
                "https://x/?s={{sig}}",
                &[("sig", "hmac_sha526(k, m)")],
            )],
        );
        let problems = generator_problems(&col, None);
        assert_eq!(problems.len(), 1, "one bad row, one report");
        assert!(
            matches!(
                &problems[0],
                crate::generators::GenError::UnknownFunction { name, function }
                    if name == "sig" && function == "hmac_sha526"
            ),
            "the report names the row and the misspelling: {:?}",
            problems[0]
        );
    }

    /// Every failing row is reported, not just the first, so a block with two
    /// mistakes takes one round of fixing rather than two.
    #[test]
    fn every_failing_generator_row_is_reported() {
        let col = Collection::new(
            "c".into(),
            vec![entry_with_generators(
                "https://x/",
                &[("a", "nope()"), ("b", "uuid"), ("c", "{{")],
            )],
        );
        let problems = generator_problems(&col, None);
        let rows: Vec<&str> = problems.iter().map(|e| e.row()).collect();
        assert_eq!(rows, vec!["a", "c"], "b is fine and says nothing");
    }

    /// `generator_problems` only looks at the selected request; `_all` looks at
    /// every one, matching the two run commands they back.
    #[test]
    fn generator_problems_follow_the_selection() {
        let mut col = Collection::new(
            "c".into(),
            vec![
                entry_with_generators("https://x/", &[("a", "uuid")]),
                entry_with_generators("https://y/", &[("b", "nope()")]),
            ],
        );
        col.selected_entry = 0;
        assert!(
            generator_problems(&col, None).is_empty(),
            "entry 0 is sound"
        );
        assert_eq!(
            generator_problems_all(&col, None).len(),
            1,
            "Run All still sees entry 1's mistake"
        );
    }

    /// A generated name keeps its braces in the preview rather than showing a
    /// value: the value doesn't exist until the request is sent, and inventing
    /// one per frame would flicker and still not be what goes on the wire.
    #[test]
    fn a_generated_name_previews_as_computed() {
        let col = Collection::new(
            "c".into(),
            vec![entry_with_generators("https://x/", &[("nonce", "uuid")])],
        );
        let map = subst_map(&col, None);
        let info = map.get("nonce").expect("the generator name is known");
        assert_eq!(info.kind, SubstKind::Computed);
        assert!(info.shown.is_none(), "no value is invented for the preview");
    }

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
            ok: true,
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
            run_all_entries(&col, None, state, false).is_some(),
            "a non-empty collection must start a streaming run"
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
            body_src: Some("{}".into()),
            form_fields: vec![FormField {
                key: "f".into(),
                value: "v".into(),
                enabled: true,
                ..Default::default()
            }],
            ..Default::default()
        };
        let col = Collection::new("c".into(), vec![ok_entry, bad_entry]);
        let state = Arc::new(Mutex::new(ApiResponse::default()));

        let rx = run_all_entries(&col, None, state.clone(), false);

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

    // --- request-level parameter defaults ---------------------------------

    /// A request that declares a parameter and is opened on its own runs with
    /// the author's sample value — the whole point of the feature: the request
    /// stays usable outside the report that drives it.
    #[test]
    fn a_declared_parameter_supplies_its_default_when_nobody_binds_it() {
        let entry = param_entry("FILE", "./samples/invoice.pdf");
        let vars = HashMap::new();

        let effective = effective_vars_reporting(&entry, &vars).0;

        assert_eq!(
            effective.get("FILE"),
            Some(&"./samples/invoice.pdf".to_string()),
        );
        assert_eq!(
            resolve_entry(&entry, &effective).form_fields[0].value,
            "./samples/invoice.pdf",
            "the default reaches the multipart file field as a real path",
        );
    }

    /// The same request driven from a PaperTrail loop takes the loop's value.
    /// Hurl's own reading of an `[Options] variable:` row is the opposite (it
    /// overwrites the caller), so this is the flip that makes one request serve
    /// both a person and a report.
    #[test]
    fn a_caller_binding_beats_the_declared_default() {
        let entry = param_entry("FILE", "./samples/invoice.pdf");
        let vars = HashMap::from([("FILE".to_string(), "./inbox/real.pdf".to_string())]);

        let effective = effective_vars_reporting(&entry, &vars).0;

        assert_eq!(effective.get("FILE"), Some(&"./inbox/real.pdf".to_string()));
    }

    /// The bound row is removed from the entry that is handed to Hurl, so the
    /// request cannot re-assert its own value in the parts PaperBoy does not
    /// substitute itself (`[Captures]`/`[Asserts]`).
    #[test]
    fn the_run_entry_never_carries_a_variable_option_to_hurl() {
        let entry = param_entry("FILE", "./samples/invoice.pdf");
        let vars = HashMap::from([("FILE".to_string(), "./inbox/real.pdf".to_string())]);

        let mut run_entry = to_run_entry(&entry, resolve_entry(&entry, &vars));
        strip_variable_options(&mut run_entry);

        assert!(
            run_entry.options.iter().all(|r| r.key != "variable"),
            "a variable: row would override the caller inside Hurl",
        );
        assert!(
            run_entry.options.iter().any(|r| r.key == "retry"),
            "behavioural options are untouched",
        );
    }

    /// A whole-collection run hands the file to Hurl unresolved, so only the
    /// *bound* defaults are removed — the rest stay in for Hurl to apply, which
    /// is what a default should do.
    #[test]
    fn a_whole_file_run_strips_only_the_defaults_the_caller_bound() {
        let mut entries = vec![param_entry("FILE", "./samples/invoice.pdf")];
        entries[0]
            .options
            .push(KvRow::new("variable", "MODE=draft"));
        let vars = HashMap::from([("FILE".to_string(), "./inbox/real.pdf".to_string())]);

        assert!(strip_bound_variable_options(&mut entries, &vars));

        let rows: Vec<&str> = entries[0]
            .options
            .iter()
            .map(|r| r.value.as_str())
            .collect();
        assert_eq!(
            rows,
            vec!["3", "MODE=draft"],
            "the bound FILE default is gone; the unbound MODE default survives",
        );
    }

    /// Nothing to strip means nothing to re-serialize: the CLI runs a plain
    /// `.hurl` file verbatim, and round-tripping one nobody parameterised would
    /// be churn for its own sake.
    #[test]
    fn a_whole_file_run_reports_when_it_changed_nothing() {
        let mut entries = vec![param_entry("FILE", "./samples/invoice.pdf")];

        assert!(!strip_bound_variable_options(&mut entries, &HashMap::new()));
        assert_eq!(entries[0].options.len(), 2);
    }

    /// One parameter may be written in terms of another, and in terms of a
    /// caller-supplied variable — defaults are applied in written order so the
    /// composition is predictable rather than hash-order luck.
    #[test]
    fn a_default_may_reference_another_variable() {
        let mut entry = param_entry("SAMPLES", "{{ROOT}}/samples");
        entry
            .options
            .push(KvRow::new("variable", "DOC={{SAMPLES}}/invoice.pdf"));
        let vars = HashMap::from([("ROOT".to_string(), "/srv".to_string())]);

        let effective = effective_vars_reporting(&entry, &vars).0;

        assert_eq!(effective.get("SAMPLES"), Some(&"/srv/samples".to_string()));
        assert_eq!(
            effective.get("DOC"),
            Some(&"/srv/samples/invoice.pdf".to_string()),
        );
    }

    /// `[Options]` is a free-text grid the user may be mid-way through typing.
    /// A half-written row is ignored, never an error that refuses the send.
    #[test]
    fn malformed_and_disabled_parameter_rows_are_ignored() {
        let mut entry = param_entry("FILE", "./samples/invoice.pdf");
        entry.options = vec![
            KvRow::new("variable", "no-equals-sign"),
            KvRow::new("variable", "=novalue"),
            KvRow::new("variable", "SPACED NAME=x"),
            KvRow::toggled("variable", "OFF=x", false),
            KvRow::new("VARIABLE", "SHOUTED=y"),
        ];

        assert_eq!(
            entry.variable_defaults(),
            vec![("SHOUTED".to_string(), "y".to_string())],
            "only the well-formed enabled row counts, and the option name is \
             matched case-insensitively",
        );
    }

    /// A request that declares nothing is handed the caller's own map, not a
    /// clone of it — a send is hot enough that the common case should allocate
    /// nothing.
    #[test]
    fn a_request_without_parameters_borrows_the_callers_variables() {
        let entry = me_entry();
        let vars = HashMap::from([("TOKEN".to_string(), "abc".to_string())]);

        assert!(matches!(
            effective_vars_reporting(&entry, &vars).0,
            std::borrow::Cow::Borrowed(_)
        ));
    }

    /// A request declaring `FILE` for a `[Multipart]` file field — the case the
    /// feature exists for.
    fn param_entry(name: &str, default: &str) -> HurlEntry {
        HurlEntry {
            title: "upload_document".into(),
            method: "POST".into(),
            url: "https://example.test/documents".into(),
            options: vec![
                KvRow::new("retry", "3"),
                KvRow::new("variable", format!("{name}={default}")),
            ],
            is_multipart: true,
            form_fields: vec![FormField {
                key: "file".into(),
                value: format!("{{{{{name}}}}}"),
                kind: FormFieldKind::File,
                enabled: true,
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    /// A failed `[Gen]` row leaves its name unbound, so running on would send
    /// `{{sig}}` where a signature belongs. The interactive send refuses; the
    /// report runner used to throw the errors away and send it anyway, which
    /// is the one path with nobody watching.
    #[test]
    fn a_request_whose_computed_value_failed_is_not_sent() {
        let entry = HurlEntry {
            method: "GET".to_string(),
            url: "http://127.0.0.1:9/{{sig}}".to_string(),
            generators: vec![("sig".to_string(), "hmac_sha526(k, m)".to_string())],
            ..Default::default()
        };
        let out = run_resolved_entry(&entry, &HashMap::new(), None, &[]);
        assert!(out.entries.is_empty(), "nothing was sent");
        let error = out.error.unwrap_or_default();
        assert!(
            error.contains("hmac_sha526"),
            "and the reason names the row's fault: {error}"
        );
    }
}
