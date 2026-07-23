//! Execution + evaluation boundary for the Hurl runner.
//!
//! Internally the app keeps its own editable [`HurlEntry`](super::HurlEntry)
//! model; here we serialize it to Hurl text and hand it to the `hurl` crate's
//! runner, which performs the HTTP request(s) and evaluates every `[Captures]`
//! and `[Asserts]` (the full Hurl feature set). The result is mapped back into
//! the app's response/capture/assert model.
//!
//! All runner stdout/stderr is captured in buffered terminals so nothing leaks
//! to the real terminal (which would corrupt the TUI); any error text is
//! returned via [`RunOutput::error`] for display on the status bar instead.

use std::collections::HashMap;
use std::path::Path;

use hurl::runner::{
    self, AssertResult, EntryResult, RunnerError, RunnerOptionsBuilder, Value, VariableSet,
};
use hurl::util::logger::{Logger, LoggerOptionsBuilder};
use hurl::util::path::ContextDir;
use hurl::util::term::{Stderr, Stdout, WriteMode};
use hurl_core::error::DisplaySourceError;
use hurl_core::parser::parse_hurl_file;
use serde_json::Value as JsonValue;

/// Outcome of evaluating one explicit `[Asserts]` expression, for the response
/// panel. The `hurl` runner does the actual evaluation.
#[derive(Debug, Clone)]
pub struct AssertOutcome {
    /// The assert expression text (e.g. `jsonpath "$.status" == "ok"`).
    pub expr: String,
    pub passed: bool,
    /// A short reason shown on failure; empty on success.
    pub detail: String,
}

/// The mapped result of running one Hurl entry.
pub struct EntryOutcome {
    /// The method/URL actually sent (fully substituted, incl. chained captures).
    pub method: String,
    pub url: String,
    pub status: u16,
    pub status_text: String,
    pub headers: Vec<(String, String)>,
    /// Response body, pretty-printed when it parses as JSON.
    pub body: String,
    /// The response body exactly as received (never reformatted), so a report
    /// can render `RESPONSE RAW` without losing the server's original bytes
    /// (whitespace, key order, non-JSON payloads). `body` is the pretty view of
    /// this same content.
    pub raw_body: String,
    pub asserts: Vec<AssertOutcome>,
    pub captures: Vec<(String, String)>,
    /// Effective duration of the HTTP transfer(s) for this entry, in
    /// milliseconds (excludes assert/capture processing). Reports surface this
    /// as the per-request "Time" column.
    pub duration_ms: u64,
    /// `true` when the runner reported no errors for this entry (status
    /// expectation, asserts and transport all satisfied).
    pub ok: bool,
    /// The first runner error for this entry (transport / failed assert / status
    /// mismatch), rendered concisely; `None` when the entry passed.
    pub error: Option<String>,
}

/// The mapped result of a whole run (one or more entries).
pub struct RunOutput {
    pub entries: Vec<EntryOutcome>,
    /// A concise message for the status bar: a parse error, transport failure,
    /// or the first failed assertion. `None` when everything succeeded.
    pub error: Option<String>,
}

/// Builds the [`ContextDir`] that gates local file access for `[Form]`/
/// `[Multipart]` file fields (and `[Options] output`), matching the real
/// `hurl` CLI's own default: `file_root` is the directory containing the
/// `.hurl`/collection file (so a relative form file path like `avatar.png`
/// resolves next to it, exactly as the user expects), falling back to the
/// process's current directory when no source file is known (e.g. an
/// unsaved/remote collection). `current_dir` is always the process's actual
/// working directory, so absolute paths (as produced by the file picker)
/// resolve correctly too.
fn context_dir(file_root: Option<&Path>) -> ContextDir {
    let current_dir = std::env::current_dir().unwrap_or_default();
    let file_root = file_root.unwrap_or(&current_dir);
    ContextDir::new(&current_dir, file_root)
}

/// Parse and run `content` (Hurl text) with `vars` as substitution variables,
/// mapping the runner's result into the app model. Captures from earlier entries
/// flow into later ones automatically within this call. `file_root` should be
/// the collection's source directory, if known, so relative `[Form]`/
/// `[Multipart]` file paths resolve against it (see [`context_dir`]).
pub fn run_hurl(
    content: &str,
    vars: &HashMap<String, String>,
    file_root: Option<&Path>,
) -> RunOutput {
    let hurl_file = match parse_hurl_file(content) {
        Ok(h) => h,
        Err(e) => {
            return RunOutput {
                entries: vec![],
                error: Some(format!("Parse error (line {}): {:?}", e.pos.line, e.kind)),
            };
        }
    };

    let runner_opts = RunnerOptionsBuilder::new()
        .continue_on_error(true)
        .context_dir(&context_dir(file_root))
        .build();
    let logger_opts = LoggerOptionsBuilder::new().build();
    let mut variables = VariableSet::new();
    for (k, v) in vars {
        variables.insert(k.clone(), Value::String(v.clone()));
    }
    let secrets = variables.secrets();
    // Buffered terminals: the runner's output/errors are captured, not written
    // to the real terminal.
    let mut stdout = Stdout::new(WriteMode::Buffered);
    let mut logger = Logger::new(&logger_opts, Stderr::new(WriteMode::Buffered), &secrets);

    let result = runner::run_entries(
        &hurl_file.entries,
        content,
        None,
        &runner_opts,
        &variables,
        &mut stdout,
        None,
        &mut logger,
    );

    let lines: Vec<&str> = content.lines().collect();
    let mut error: Option<String> = None;
    let mut entries = Vec::new();

    for e in &result.entries {
        let (outcome, entry_error) = map_entry_result(e, &lines);
        if error.is_none() {
            error = entry_error;
        }
        entries.push(outcome);
    }

    RunOutput { entries, error }
}

/// Like [`run_hurl`], but invokes `on_entry` immediately after each request
/// finishes, instead of only returning once the whole collection has run —
/// so a caller (the CLI) can stream results out as they happen.
///
/// Each entry runs via its own [`runner::run_entries`] call, windowed to just
/// that one entry (`from_entry`/`to_entry`); `[Captures]` still flow from one
/// entry to the next exactly as in a single full run, by threading the same
/// `VariableSet` returned by each call into the next one. The one behavioural
/// difference from `run_hurl`: Hurl's automatic cookie jar (cookies
/// remembered from `Set-Cookie` response headers) does *not* carry across
/// entries in this mode, since each call starts a fresh HTTP client — an
/// explicit `[Cookies]` section on a request is unaffected either way.
pub fn run_hurl_streaming(
    content: &str,
    vars: &HashMap<String, String>,
    file_root: Option<&Path>,
    mut on_entry: impl FnMut(&EntryOutcome),
) -> RunOutput {
    let hurl_file = match parse_hurl_file(content) {
        Ok(h) => h,
        Err(e) => {
            return RunOutput {
                entries: vec![],
                error: Some(format!("Parse error (line {}): {:?}", e.pos.line, e.kind)),
            };
        }
    };

    let ctx_dir = context_dir(file_root);
    let logger_opts = LoggerOptionsBuilder::new().build();
    let mut variables = VariableSet::new();
    for (k, v) in vars {
        variables.insert(k.clone(), Value::String(v.clone()));
    }

    let lines: Vec<&str> = content.lines().collect();
    let mut error: Option<String> = None;
    let mut entries = Vec::new();
    let total = hurl_file.entries.len();

    for i in 1..=total {
        let runner_opts = RunnerOptionsBuilder::new()
            .continue_on_error(true)
            .from_entry(Some(i))
            .to_entry(Some(i))
            .context_dir(&ctx_dir)
            .build();
        let secrets = variables.secrets();
        let mut stdout = Stdout::new(WriteMode::Buffered);
        let mut logger = Logger::new(&logger_opts, Stderr::new(WriteMode::Buffered), &secrets);

        let result = runner::run_entries(
            &hurl_file.entries,
            content,
            None,
            &runner_opts,
            &variables,
            &mut stdout,
            None,
            &mut logger,
        );
        // Carry captures forward into the next entry's window.
        variables = result.variables;

        for e in &result.entries {
            let (outcome, entry_error) = map_entry_result(e, &lines);
            if error.is_none() {
                error = entry_error;
            }
            on_entry(&outcome);
            entries.push(outcome);
        }
    }

    RunOutput { entries, error }
}

/// Map one runner [`EntryResult`] to the app's [`EntryOutcome`], returning it
/// alongside its own concise error (if any) for the caller to fold into the
/// whole run's status. Shared by [`run_hurl`] and [`run_hurl_streaming`] so
/// both stay in lockstep on exactly what gets surfaced from a Hurl result.
fn map_entry_result(e: &EntryResult, lines: &[&str]) -> (EntryOutcome, Option<String>) {
    let (method, url) = e
        .calls
        .last()
        .map(|c| (c.request.method.clone(), c.request.url.to_string()))
        .unwrap_or_default();
    let (status, headers, body, raw_body) = match e.calls.last() {
        Some(call) => {
            let r = &call.response;
            let hdrs = r
                .headers
                .iter()
                .map(|h| (h.name.clone(), h.value.clone()))
                .collect();
            let raw = String::from_utf8_lossy(&r.body).to_string();
            let body = serde_json::from_str::<JsonValue>(&raw)
                .map(|v| serde_json::to_string_pretty(&v).unwrap_or_else(|_| raw.clone()))
                .unwrap_or_else(|_| raw.clone());
            (r.status as u16, hdrs, body, raw)
        }
        None => (0, Vec::new(), String::new(), String::new()),
    };

    // Only surface EXPLICIT [Asserts] plus the implicit status assertion; the
    // implicit HTTP-version assert stays folded into the status/version line.
    let mut asserts = Vec::new();
    // Surface the implicit HTTP status assertion (the `HTTP <code>` response
    // line) as a leading `status == <code>` row, so the response's [Asserts]
    // view shows the status check alongside the explicit asserts — Hurl treats
    // the status line as an assertion too. `HTTP *` / no status line produces
    // no `ImplicitStatus`, so nothing is shown in that case.
    for a in &e.asserts {
        if let AssertResult::ImplicitStatus {
            actual, expected, ..
        } = a
        {
            let failed = a.to_runner_error().is_some();
            asserts.push(AssertOutcome {
                expr: format!("status == {expected}"),
                passed: !failed,
                detail: if failed {
                    format!("got {actual}")
                } else {
                    String::new()
                },
            });
        }
    }
    for a in &e.asserts {
        if !matches!(a, AssertResult::Explicit { .. }) {
            continue;
        }
        let err = a.to_runner_error();
        let expr = lines
            .get(a.line().saturating_sub(1))
            .map(|l| l.trim().to_string())
            .unwrap_or_default();
        let detail = err.as_ref().map(assert_detail).unwrap_or_default();
        asserts.push(AssertOutcome {
            expr,
            passed: err.is_none(),
            detail,
        });
    }

    let captures = e
        .captures
        .iter()
        .map(|c| (c.name.clone(), c.value.to_string()))
        .collect();

    // A failed status assertion gets a clear "expected X but got Y" message
    // (with the request that produced it) rather than the runner's terse
    // "Assert status code: HTTP 200", which hides both the expected and the
    // actual status. Other errors (transport, failed explicit asserts) keep
    // their concise per-line rendering.
    let status_mismatch = e.asserts.iter().find_map(|a| match a {
        AssertResult::ImplicitStatus {
            actual, expected, ..
        } if a.to_runner_error().is_some() => Some((*actual, *expected)),
        _ => None,
    });
    let entry_error = if let Some((actual, expected)) = status_mismatch {
        let reason = reason(actual as u16);
        let actual_txt = if reason.is_empty() {
            format!("{actual}")
        } else {
            format!("{actual} {reason}")
        };
        Some(format!(
            "Expected status {expected} but got {actual_txt} ({method} {url})"
        ))
    } else {
        // The first error (transport failure or failed assert) is surfaced
        // per-entry and, for the whole run, on the status bar.
        e.errors.first().map(|er| render_error(er, lines))
    };

    (
        EntryOutcome {
            method,
            url,
            status,
            status_text: reason(status).to_string(),
            headers,
            body,
            raw_body,
            asserts,
            captures,
            duration_ms: e.transfer_duration.as_millis() as u64,
            ok: e.errors.is_empty(),
            error: entry_error.clone(),
        },
        entry_error,
    )
}

/// A short "actual vs expected"-style detail for a failed assert.
fn assert_detail(e: &RunnerError) -> String {
    e.description()
}

/// A concise, single-line rendering of a runner error for the status bar:
/// its description plus the offending source line when available.
fn render_error(e: &RunnerError, lines: &[&str]) -> String {
    let desc = e.description();
    let line = e.source_info().start.line;
    match lines.get(line.saturating_sub(1)) {
        Some(l) if !l.trim().is_empty() => format!("{desc}: {}", l.trim()),
        _ => desc,
    }
}

/// Canonical reason phrase for common status codes (the runner exposes only the
/// numeric status).
fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        422 => "Unprocessable Entity",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spawn a one-shot HTTP/1.1 server on an ephemeral port that answers the
    /// first connection with `status`/`reason` and a tiny JSON body, then
    /// closes. Returns the bound port. Used to exercise the status-assertion
    /// mapping against a real (local) response without any network access.
    fn one_shot_server(status: u16, reason: &str) -> u16 {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let reason = reason.to_string();
        std::thread::spawn(move || {
            if let Ok((mut sock, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf);
                let body = "{\"ok\":true}";
                let resp = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes());
                let _ = sock.flush();
            }
        });
        port
    }

    /// Feature: the implicit `HTTP <code>` status line surfaces in the mapped
    /// asserts as a `status == <code>` row (so the response's [Asserts] view
    /// shows the status check), and passes when the status matches.
    #[test]
    fn status_line_appears_as_a_passing_assert() {
        let port = one_shot_server(200, "OK");
        let content = format!("GET http://127.0.0.1:{port}/\nHTTP 200\n");
        let out = run_hurl(&content, &HashMap::new(), None);
        let e = out.entries.first().expect("one entry");
        assert!(e.ok, "entry should pass, error: {:?}", e.error);
        let status_assert = e
            .asserts
            .iter()
            .find(|a| a.expr == "status == 200")
            .expect("a `status == 200` assert row");
        assert!(status_assert.passed);
    }

    /// Feature: a failed status assertion is both surfaced as a failed
    /// `status == <expected>` assert row (with the actual status in its
    /// detail) and rendered as a clear "expected X but got Y" error message
    /// naming the request — not the runner's terse "Assert status code".
    #[test]
    fn failed_status_assertion_has_a_clear_message() {
        let port = one_shot_server(404, "Not Found");
        let content = format!("GET http://127.0.0.1:{port}/\nHTTP 200\n");
        let out = run_hurl(&content, &HashMap::new(), None);
        let e = out.entries.first().expect("one entry");
        assert!(!e.ok);
        let status_assert = e
            .asserts
            .iter()
            .find(|a| a.expr == "status == 200")
            .expect("a `status == 200` assert row");
        assert!(!status_assert.passed);
        assert!(
            status_assert.detail.contains("404"),
            "detail should show the actual status, got: {}",
            status_assert.detail
        );
        let msg = e.error.as_deref().unwrap_or_default();
        assert!(
            msg.contains("Expected status 200") && msg.contains("got 404"),
            "message should state expected vs actual, got: {msg}"
        );
        assert!(
            !msg.contains("Assert status code"),
            "message should not be the terse runner default, got: {msg}"
        );
    }

    /// A `HTTP *` wildcard status line asserts nothing about the status, so no
    /// synthetic `status == …` row is produced.
    #[test]
    fn wildcard_status_line_produces_no_status_assert() {
        let port = one_shot_server(200, "OK");
        let content = format!("GET http://127.0.0.1:{port}/\nHTTP *\n");
        let out = run_hurl(&content, &HashMap::new(), None);
        let e = out.entries.first().expect("one entry");
        assert!(
            !e.asserts.iter().any(|a| a.expr.starts_with("status ==")),
            "HTTP * should not synthesize a status assert"
        );
    }

    /// A `[Multipart]` file field referenced by a path relative to the
    /// collection's own directory must be authorized when `file_root` is
    /// passed through — this is the regression this test guards: before
    /// `context_dir(...)` was wired into the runner options, every relative
    /// (and most absolute) form-file path was rejected with "Unauthorized
    /// file access", regardless of where the `.hurl` file actually lived.
    #[test]
    fn relative_form_file_path_is_authorized_against_the_collection_directory() {
        let dir = std::env::temp_dir().join(format!("paperboy_run_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("avatar.png"), b"fake-png").unwrap();

        // The URL is unroutable (TEST-NET-1, RFC 5737) with a tiny implicit
        // timeout via an unreachable address; either way, the file-access
        // check happens before any network I/O, so the result is
        // deterministic regardless of network availability.
        let content = "POST http://192.0.2.1/upload\n[Multipart]\navatar: file,avatar.png;\n";
        let out = run_hurl(content, &HashMap::new(), Some(dir.as_path()));

        let msg = out
            .entries
            .first()
            .and_then(|e| e.error.as_deref())
            .unwrap_or_default();
        assert!(
            !msg.to_ascii_lowercase().contains("unauthorized"),
            "a form file relative to the collection directory must be authorized, got: {msg}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Without a `file_root`, the runner falls back to the process's current
    /// directory (matching the real `hurl` CLI's own behaviour when no
    /// `.hurl` source path or explicit `--file-root` is known).
    #[test]
    fn missing_file_root_falls_back_to_the_process_current_directory() {
        let cwd = std::env::current_dir().unwrap();
        let unique = format!("paperboy_run_test_cwd_{}.bin", uuid::Uuid::new_v4());
        let file_path = cwd.join(&unique);
        std::fs::write(&file_path, b"fake").unwrap();

        let content = format!("POST http://192.0.2.1/upload\n[Multipart]\nf: file,{unique};\n");
        let out = run_hurl(&content, &HashMap::new(), None);

        let msg = out
            .entries
            .first()
            .and_then(|e| e.error.as_deref())
            .unwrap_or_default();
        assert!(
            !msg.to_ascii_lowercase().contains("unauthorized"),
            "a file in the process's current directory must be authorized when no file_root is given, got: {msg}"
        );

        std::fs::remove_file(&file_path).ok();
    }

    /// A form file path that resolves outside the given `file_root` must
    /// still be rejected — the fix must not disable the sandbox entirely.
    #[test]
    fn form_file_path_outside_the_file_root_is_still_rejected() {
        let root =
            std::env::temp_dir().join(format!("paperboy_run_test_root_{}", uuid::Uuid::new_v4()));
        let outside = std::env::temp_dir().join(format!(
            "paperboy_run_test_outside_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.bin"), b"fake").unwrap();

        let content = "POST http://192.0.2.1/upload\n[Multipart]\nf: file,../secret.bin;\n";
        // "../secret.bin" relative to `root` escapes it entirely (it doesn't
        // even land inside `outside`), so this must be rejected.
        let out = run_hurl(content, &HashMap::new(), Some(root.as_path()));

        let msg = out
            .entries
            .first()
            .and_then(|e| e.error.as_deref())
            .unwrap_or_default();
        assert!(
            msg.to_ascii_lowercase().contains("unauthorized"),
            "a file outside file_root must still be rejected, got: {msg}"
        );

        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&outside).ok();
    }

    /// End-to-end proof that staging fixes the exact scenario the previous
    /// test guards against: an out-of-scope form file, once staged via
    /// `stage_out_of_scope_form_files`, is authorized by `run_hurl` even
    /// though it was rejected before staging.
    #[test]
    fn staging_authorizes_a_form_file_that_would_otherwise_be_rejected() {
        use crate::hurl::entry::{FormField, FormFieldKind, HurlEntry};
        use crate::hurl::stage_out_of_scope_form_files;

        let root =
            std::env::temp_dir().join(format!("paperboy_stage_run_root_{}", uuid::Uuid::new_v4()));
        let outside = std::env::temp_dir().join(format!(
            "paperboy_stage_run_outside_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let outside_file = outside.join("secret.bin");
        std::fs::write(&outside_file, b"fake").unwrap();

        let mut entries = vec![HurlEntry {
            method: "POST".into(),
            url: "http://192.0.2.1/upload".into(),
            form_fields: vec![FormField {
                key: "f".into(),
                value: outside_file.to_string_lossy().into_owned(),
                kind: FormFieldKind::File,
                ..Default::default()
            }],
            ..Default::default()
        }];

        let staged = stage_out_of_scope_form_files(&mut entries, Some(root.as_path())).unwrap();
        assert!(
            staged.is_some(),
            "an out-of-scope file must trigger staging"
        );
        let staged_dir = staged.unwrap();

        let content = entries[0].to_hurl();
        let out = run_hurl(&content, &HashMap::new(), Some(staged_dir.as_path()));

        let msg = out
            .entries
            .first()
            .and_then(|e| e.error.as_deref())
            .unwrap_or_default();
        assert!(
            !msg.to_ascii_lowercase().contains("unauthorized"),
            "the staged file must be authorized against the staging directory, got: {msg}"
        );

        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&outside).ok();
        std::fs::remove_dir_all(&staged_dir).ok();
    }
}
