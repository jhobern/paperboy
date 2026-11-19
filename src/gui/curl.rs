//! Export the current request without sending it. Hurl's curl formatter needs
//! private runner types, so this uses the shared run-entry preparation and
//! Hurl's parsed body types, refusing options that cannot be translated.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use hurl_core::ast::{Bytes, MultilineStringKind, Template, TemplateElement};
use hurl_core::types::ToSource;

use crate::environment::substitute;
use crate::hurl::{FormFieldKind, HurlEntry};
use crate::i18n::{Status, Strings, fill};
use crate::request;

use super::app::GuiApp;

pub(super) fn copy(app: &mut GuiApp, ctx: &eframe::egui::Context, ci: usize, sel: usize) {
    let result = (|| {
        if app.editor_section == super::app::EditorSection::Code && app.code_edit.error.is_some() {
            return Err(app.strings.gui_code_parse_error.to_string());
        }
        let env = app.session.effective_env(ci);
        let col = &app.session.collections[ci];
        let pending = request::pending_request_keys(col, env.as_ref());
        if !pending.is_empty() {
            return Err(Status::WaitingSecrets(pending).text(&app.strings));
        }
        let vars = request::collection_vars(env.as_ref(), &col.captures);
        command(
            &col.entries[sel],
            &vars,
            col.path.as_deref().and_then(Path::parent),
            &app.strings,
        )
    })();
    app.session.status = Some(match result {
        Ok(text) => {
            ctx.copy_text(text);
            Status::Copied
        }
        Err(reason) => Status::Error(reason),
    });
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn arg(args: &mut Vec<String>, flag: &str, value: &str) {
    args.push(format!("{flag} {}", quote(value)));
}

fn unsupported(s: &Strings, feature: &str) -> String {
    fill(s.gui_curl_unsupported, &[feature])
}

fn literal(template: &Template, s: &Strings) -> Result<String, String> {
    let mut text = String::new();
    for element in &template.elements {
        match element {
            TemplateElement::String { value, .. } => text.push_str(value),
            _ => return Err(unsupported(s, s.gui_curl_expression)),
        }
    }
    Ok(text)
}

fn file_path(path: &str, root: Option<&Path>, s: &Strings) -> Result<String, String> {
    let path = PathBuf::from(path);
    let path = if path.is_absolute() {
        path
    } else {
        root.unwrap_or(Path::new(".")).join(path)
    };
    let absolute = std::path::absolute(path).map_err(|e| e.to_string())?;
    absolute
        .to_str()
        .map(str::to_string)
        .ok_or_else(|| unsupported(s, s.gui_curl_path))
}

fn command(
    base: &HurlEntry,
    vars: &HashMap<String, String>,
    root: Option<&Path>,
    s: &Strings,
) -> Result<String, String> {
    if base.is_unreadable() || base.body_form_conflict() {
        return Err(s.gui_curl_invalid_request.to_string());
    }
    let (vars, errors) = request::effective_vars_reporting(base, vars);
    if !errors.is_empty() {
        return Err(Status::GeneratorErrors(errors).text(s));
    }
    if let Some((user, password)) = &base.basic_auth {
        let mut missing = crate::environment::referenced_keys(&substitute(user, &vars));
        missing.extend(crate::environment::referenced_keys(&substitute(
            password, &vars,
        )));
        missing.sort();
        missing.dedup();
        if !missing.is_empty() {
            return Err(fill(s.gui_curl_unresolved, &[&missing.join(", ")]));
        }
    }
    let mut entry = request::to_run_entry(base, request::resolve_entry(base, &vars));
    request::strip_variable_options(&mut entry);
    // curl sends a request; it does not evaluate Hurl response assertions or
    // captures. Those must not make an otherwise exportable request fail to parse.
    entry.expected_status = None;
    entry.response_version = None;
    entry.response_headers.clear();
    entry.response_body = None;
    entry.asserts.clear();
    entry.captures.clear();
    entry.reports.clear();
    for row in entry
        .queries
        .iter_mut()
        .chain(entry.options.iter_mut())
        .filter(|r| r.enabled)
    {
        row.key = substitute(&row.key, &vars);
        row.value = substitute(&row.value, &vars);
    }
    for row in &mut entry.headers {
        row.key = substitute(&row.key, &vars);
    }
    for row in entry
        .headers
        .iter_mut()
        .chain(entry.queries.iter_mut())
        .chain(entry.cookies.iter_mut())
        .chain(entry.options.iter_mut())
        .filter(|r| r.enabled)
    {
        if crate::hurl::key_problem(&row.key).is_some()
            || crate::hurl::value_problem(&row.value).is_some()
            || row.value.contains('\0')
        {
            return Err(s.gui_curl_invalid_request.to_string());
        }
    }
    crate::hurl::expand_base64_form_fields(std::slice::from_mut(&mut entry), root)
        .map_err(|e| e.to_string())?;
    for field in &entry.form_fields {
        if crate::hurl::key_problem(&field.key).is_some()
            || field.value.contains('\0')
            || (field.kind == FormFieldKind::Text
                && crate::hurl::value_problem(&field.value).is_some())
        {
            return Err(s.gui_curl_invalid_request.to_string());
        }
    }
    entry.ensure_run_content_length();
    let mut missing: Vec<_> = request::entry_referenced_keys(&entry).into_iter().collect();
    missing.sort();
    if !missing.is_empty() {
        return Err(fill(s.gui_curl_unresolved, &[&missing.join(", ")]));
    }
    let content = entry.to_hurl();
    let parsed = hurl_core::parser::parse_hurl_file(&content)
        .map_err(|_| s.gui_curl_invalid_request.to_string())?;
    if parsed.entries.len() != 1 || entry.url.contains('\0') {
        return Err(s.gui_curl_invalid_request.to_string());
    }
    let mut args = vec!["curl --disable --include --globoff".to_string()];
    let has_payload = !entry.form_fields.is_empty() || parsed.entries[0].request.body.is_some();
    let follows = entry.options.iter().any(|r| {
        r.enabled
            && matches!(r.key.as_str(), "location" | "location-trusted")
            && r.value.trim() == "true"
    });
    let empty_post = entry.method == "POST" && !has_payload && follows;
    if entry.method == "HEAD" {
        if has_payload {
            return Err(unsupported(s, s.gui_curl_head_body));
        }
        args.push("--head".into());
    } else if entry.method != "POST" || (!has_payload && !follows) {
        arg(&mut args, "--request", &entry.method);
    } else if empty_post {
        arg(&mut args, "--data-raw", "");
    }
    for row in &entry.headers {
        // curl's "Name:" removes a header; "Name;" sends an empty value.
        let header = if row.value.is_empty() {
            format!("{};", row.key)
        } else {
            format!("{}: {}", row.key, row.value)
        };
        arg(&mut args, "--header", &header);
    }
    let cookies = entry
        .cookies
        .iter()
        .filter(|r| r.enabled)
        .map(|r| format!("{}={}", r.key, r.value))
        .collect::<Vec<_>>();
    if !cookies.is_empty() {
        arg(&mut args, "--cookie", &cookies.join("; "));
    }
    options(&mut args, &entry, root, s)?;

    let mut encoder = ::curl::easy::Easy::new();
    let mut content_type = None;
    if !entry.form_fields.is_empty() {
        if entry.is_multipart {
            for field in &entry.form_fields {
                if field.key.contains(['=', ';', '"']) {
                    return Err(unsupported(s, s.gui_curl_form_name));
                }
                if field.kind == FormFieldKind::Text {
                    arg(
                        &mut args,
                        "--form-string",
                        &format!("{}={}", field.key, field.value),
                    );
                } else {
                    let path = file_path(&field.value, root, s)?;
                    // These quotes are curl's form grammar, inside the shell quoting.
                    let path = path.replace('\\', "\\\\").replace('"', "\\\"");
                    let mut value = format!("{}=@\"{path}\"", field.key);
                    if let Some(ct) = &field.content_type {
                        if ct.contains([';', '\r', '\n', '\0']) {
                            return Err(s.gui_curl_invalid_request.to_string());
                        }
                        value.push_str(&format!(";type={ct}"));
                    }
                    arg(&mut args, "--form", &value);
                }
            }
        } else {
            let form = entry
                .form_fields
                .iter()
                .map(|f| format!("{}={}", f.key, encoder.url_encode(f.value.as_bytes())))
                .collect::<Vec<_>>()
                .join("&");
            arg(&mut args, "--data-raw", &form);
            content_type = Some("application/x-www-form-urlencoded");
        }
    }
    let mut pipe = None;
    if let Some(body) = &parsed.entries[0].request.body {
        let text = match &body.value {
            Bytes::Json(value) => {
                content_type = Some("application/json");
                Some(value.to_source().to_string())
            }
            Bytes::Xml(value) => {
                content_type = Some("application/xml");
                Some(value.clone())
            }
            Bytes::OnelineString(value) => Some(literal(value, s)?),
            Bytes::MultilineString(value) => match &value.kind {
                MultilineStringKind::Text(t) => Some(literal(t, s)?),
                MultilineStringKind::Json(t) => {
                    content_type = Some("application/json");
                    Some(literal(t, s)?)
                }
                MultilineStringKind::Xml(t) => {
                    content_type = Some("application/xml");
                    Some(literal(t, s)?)
                }
                MultilineStringKind::Raw(t) => Some(t.to_source().to_string()),
                _ => return Err(unsupported(s, "GraphQL")),
            },
            Bytes::File(file) => {
                let path = file_path(&literal(&file.filename, s)?, root, s)?;
                arg(&mut args, "--data-binary", &format!("@{path}"));
                None
            }
            Bytes::Hex(_) | Bytes::Base64(_) => {
                let bytes = match &body.value {
                    Bytes::Hex(h) => &h.value,
                    Bytes::Base64(b) => &b.value,
                    _ => unreachable!(),
                };
                let octal: String = bytes.iter().map(|b| format!("\\0{b:03o}")).collect();
                pipe = Some(format!("printf '%b' {} | ", quote(&octal)));
                arg(&mut args, "--data-binary", "@-");
                None
            }
        };
        if let Some(text) = text {
            if text.contains('\0') {
                return Err(unsupported(s, s.gui_curl_nul));
            }
            arg(&mut args, "--data-raw", &text);
        }
    }
    let option_content_type = entry.options.iter().any(|r| {
        r.enabled
            && r.key == "header"
            && r.value
                .split_once(':')
                .is_some_and(|(name, _)| name.trim().eq_ignore_ascii_case("content-type"))
    });
    if !option_content_type
        && !entry
            .headers
            .iter()
            .any(|r| r.key.eq_ignore_ascii_case("content-type"))
    {
        if let Some(ct) = content_type {
            arg(&mut args, "--header", &format!("Content-Type: {ct}"));
        } else if parsed.entries[0].request.body.is_some() || empty_post {
            // curl otherwise labels raw bodies as URL-encoded forms.
            arg(&mut args, "--header", "Content-Type:");
        }
    }
    let query = entry
        .queries
        .iter()
        .filter(|r| r.enabled)
        .map(|r| format!("{}={}", r.key, encoder.url_encode(r.value.as_bytes())))
        .collect::<Vec<_>>()
        .join("&");
    let mut url = entry.url.clone();
    if !query.is_empty() {
        let split = url.find('#').unwrap_or(url.len());
        let suffix = url.split_off(split);
        url.push(if url.contains('?') { '&' } else { '?' });
        url.push_str(&query);
        url.push_str(&suffix);
    }
    arg(&mut args, "--url", &url);
    if args.iter().any(|arg| arg.contains('\0')) {
        return Err(s.gui_curl_invalid_request.to_string());
    }
    Ok(format!(
        "{}{}",
        pipe.unwrap_or_default(),
        args.join(" \\\n  ")
    ))
}

fn options(
    args: &mut Vec<String>,
    entry: &HurlEntry,
    root: Option<&Path>,
    s: &Strings,
) -> Result<(), String> {
    for row in entry.options.iter().filter(|r| r.enabled) {
        let name = row.key.as_str();
        let value = row.value.trim();
        match name {
            "insecure" | "location" | "location-trusted" | "compressed" => {
                if !matches!(value, "true" | "false") {
                    return Err(s.gui_curl_invalid_request.to_string());
                }
                args.push(format!(
                    "--{}{name}",
                    if value == "true" { "" } else { "no-" }
                ));
            }
            "connect-timeout" | "max-time" => {
                // Bare Hurl durations are milliseconds, curl's are seconds.
                let (number, factor) = [("ms", 1), ("s", 1000), ("m", 60_000), ("h", 3_600_000)]
                    .into_iter()
                    .find_map(|(unit, factor)| value.strip_suffix(unit).map(|n| (n, factor)))
                    .unwrap_or((value, 1));
                let ms = number
                    .trim()
                    .parse::<u64>()
                    .ok()
                    .and_then(|n| n.checked_mul(factor))
                    .ok_or_else(|| s.gui_curl_invalid_request.to_string())?;
                let seconds = format!("{}.{:03}", ms / 1000, ms % 1000);
                arg(args, &format!("--{name}"), &seconds);
            }
            "proxy" | "resolve" | "connect-to" | "user" | "max-redirs" => {
                arg(args, &format!("--{name}"), value);
            }
            "header" => {
                let (name, value) = value
                    .split_once(':')
                    .ok_or_else(|| s.gui_curl_invalid_request.to_string())?;
                let value = value.trim();
                let header = if value.is_empty() {
                    format!("{};", name.trim())
                } else {
                    format!("{}: {value}", name.trim())
                };
                arg(args, "--header", &header);
            }
            "cacert" | "cert" | "key" => {
                arg(args, &format!("--{name}"), &file_path(value, root, s)?);
            }
            "http1.0" | "http1.1" | "http2" | "http3" if value == "true" => {
                args.push(format!("--{name}"));
            }
            _ => return Err(unsupported(s, name)),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hurl::{FormField, KvRow};
    use crate::i18n::Language;

    fn strings() -> Strings {
        Strings::for_language(&Language::English)
    }

    fn entry() -> HurlEntry {
        HurlEntry {
            method: "POST".into(),
            url: "https://example.com/oauth2".into(),
            ..Default::default()
        }
    }

    #[test]
    fn oauth_headers_stay_headers_and_do_not_acquire_a_form_body() {
        let mut e = entry();
        e.url = "{{host}}/oauth2".into();
        e.headers = vec![
            KvRow::new("client_id", "{{client_id}}"),
            KvRow::new("client_secret", "{{secret}}"),
            KvRow::new("grant_type", "client_credentials"),
            KvRow::toggled("disabled", "do-not-copy", false),
        ];
        let vars = HashMap::from([
            ("host".into(), "https://example.com:8443".into()),
            ("client_id".into(), "test-client".into()),
            ("secret".into(), "test-secret".into()),
        ]);
        let cmd = command(&e, &vars, None, &strings()).unwrap();
        assert!(cmd.contains("--url 'https://example.com:8443/oauth2'"));
        assert!(cmd.contains("--request 'POST'"));
        assert!(cmd.contains("--header 'client_secret: test-secret'"));
        assert!(cmd.contains("--header 'Content-Length: 0'"));
        assert!(!cmd.contains("--data"));
        assert!(!cmd.contains("Content-Type"));
        assert!(!cmd.contains("do-not-copy"));
    }

    #[test]
    fn forms_queries_cookies_and_parameter_defaults_use_the_send_values() {
        let mut e = entry();
        e.options.push(KvRow::new("variable", "client=default"));
        e.url.push_str("?existing=yes#fragment");
        e.queries = vec![KvRow::new("q", "a b&é")];
        e.cookies = vec![KvRow::new("session", "{{session}}")];
        e.form_fields = vec![FormField {
            key: "client".into(),
            value: "{{client}}".into(),
            enabled: true,
            ..Default::default()
        }];
        let cmd = command(
            &e,
            &HashMap::from([
                ("client".into(), "a+b&c".into()),
                ("session".into(), "cookie".into()),
            ]),
            None,
            &strings(),
        )
        .unwrap();
        assert!(cmd.contains("--data-raw 'client=a%2Bb%26c'"));
        assert!(cmd.contains("Content-Type: application/x-www-form-urlencoded"));
        assert!(cmd.contains("?existing=yes&q=a%20b%26%C3%A9#fragment"));
        assert!(cmd.contains("--cookie 'session=cookie'"));
        assert!(!cmd.contains("variable"));
        assert!(
            !cmd.contains("--request"),
            "curl selects POST from the body"
        );
        let default = command(
            &e,
            &HashMap::from([("session".into(), "x".into())]),
            None,
            &strings(),
        )
        .unwrap();
        assert!(default.contains("--data-raw 'client=default'"));
    }

    #[test]
    fn body_syntax_is_unwrapped_and_content_type_is_inferred_or_overridden() {
        for (body, payload, ct) in [
            (
                r#"{"message":"{{value}}"}"#,
                r#"{"message":"hello"}"#,
                "application/json",
            ),
            ("`hello`", "hello", ""),
            (r#""hello""#, r#""hello""#, "application/json"),
            ("```json\n{\"n\":1}\n```", "{\"n\":1}\n", "application/json"),
            ("```raw\n@file\n```", "@file\n", ""),
            ("<root/>", "<root/>", "application/xml"),
        ] {
            let mut e = entry();
            e.body_src = Some(body.into());
            let vars = HashMap::from([("value".into(), "hello".into())]);
            let cmd = command(&e, &vars, None, &strings()).unwrap();
            assert!(
                cmd.contains(&format!("--data-raw {}", quote(payload))),
                "{cmd}"
            );
            let header = if ct.is_empty() {
                "Content-Type:".into()
            } else {
                format!("Content-Type: {ct}")
            };
            assert!(cmd.contains(&quote(&header)), "{cmd}");
            e.headers
                .push(KvRow::new("content-type", "application/custom"));
            let cmd = command(&e, &vars, None, &strings()).unwrap();
            assert!(cmd.contains("'content-type: application/custom'"));
            assert!(!cmd.contains("'Content-Type:"));
        }
    }

    #[test]
    fn multipart_and_body_file_paths_are_rooted_without_staging_temporary_files() {
        let root = std::env::temp_dir();
        let mut e = entry();
        e.form_fields = vec![
            FormField {
                key: "upload".into(),
                value: "a,b.txt".into(),
                kind: FormFieldKind::File,
                content_type: Some("text/plain".into()),
                enabled: true,
                ..Default::default()
            },
            FormField {
                key: "literal".into(),
                value: "@not-a-file".into(),
                enabled: true,
                ..Default::default()
            },
        ];
        let cmd = command(&e, &HashMap::new(), Some(&root), &strings()).unwrap();
        let path = root.join("a,b.txt").to_string_lossy().replace('\\', "\\\\");
        assert!(
            cmd.contains(&quote(&format!("upload=@\"{path}\";type=text/plain"))),
            "{cmd}"
        );
        assert!(cmd.contains("--form-string 'literal=@not-a-file'"));
        assert!(
            !cmd.contains("Content-Type: multipart"),
            "curl supplies the boundary"
        );
        e.form_fields.clear();
        e.body_src = Some("file,data.bin;".into());
        let cmd = command(&e, &HashMap::new(), Some(&root), &strings()).unwrap();
        assert!(cmd.contains(&quote(&format!("@{}", root.join("data.bin").display()))));
        assert!(cmd.contains("'Content-Type:'"));
    }

    #[test]
    fn base64_file_forms_embed_the_file_contents_and_report_missing_files() {
        struct TempFile(PathBuf);
        impl Drop for TempFile {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let file = TempFile(
            std::env::temp_dir().join(format!("paperboy-curl-{}.bin", uuid::Uuid::new_v4())),
        );
        std::fs::write(&file.0, [0, 255, 10]).unwrap();
        let mut e = entry();
        e.form_fields = vec![FormField {
            key: "encoded".into(),
            value: file.0.file_name().unwrap().to_str().unwrap().into(),
            kind: FormFieldKind::Base64File,
            base64_prefix: Some("data:;base64,".into()),
            enabled: true,
            ..Default::default()
        }];
        let cmd = command(&e, &HashMap::new(), file.0.parent(), &strings()).unwrap();
        assert!(
            cmd.contains("--form-string 'encoded=data:;base64,AP8K'"),
            "{cmd}"
        );
        std::fs::remove_file(&file.0).unwrap();
        assert!(command(&e, &HashMap::new(), file.0.parent(), &strings()).is_err());
    }

    #[test]
    fn options_use_curl_units_and_untranslatable_requests_fail_explicitly() {
        let mut e = entry();
        e.options = vec![
            KvRow::new("connect-timeout", "1500"),
            KvRow::new("max-time", "2m"),
            KvRow::new("insecure", "false"),
            KvRow::new("location", "true"),
        ];
        let cmd = command(&e, &HashMap::new(), None, &strings()).unwrap();
        assert!(cmd.contains("--connect-timeout '1.500'"));
        assert!(cmd.contains("--max-time '120.000'"));
        assert!(cmd.contains("--no-insecure"));
        assert!(cmd.contains("--location"));
        assert!(!cmd.contains("--request"));
        assert!(cmd.contains("--data-raw ''"));
        assert!(cmd.contains("'Content-Type:'"));
        e.options.push(KvRow::new("retry", "2"));
        assert!(
            command(&e, &HashMap::new(), None, &strings())
                .unwrap_err()
                .contains("retry")
        );
        e.options.clear();
        e.url = "{{missing}}/oauth2".into();
        assert!(
            command(&e, &HashMap::new(), None, &strings())
                .unwrap_err()
                .contains("missing")
        );
        e.url = "https://example.com".into();
        e.headers.push(KvRow::new("X-Bad", "a\nb"));
        assert!(command(&e, &HashMap::new(), None, &strings()).is_err());
        e.headers.clear();
        e.body_src = Some("{}".into());
        e.form_fields.push(FormField {
            key: "f".into(),
            value: "v".into(),
            enabled: true,
            ..Default::default()
        });
        assert!(command(&e, &HashMap::new(), None, &strings()).is_err());
    }

    #[test]
    fn auth_and_generated_values_are_resolved_without_mutating_the_entry() {
        let mut e = entry();
        e.basic_auth = Some(("{{user}}".into(), "{{password}}".into()));
        e.generators = vec![("value".into(), "\"generated\"".into())];
        e.headers.push(KvRow::new("X-Generated", "{{value}}"));
        let original = e.to_hurl();
        let cmd = command(
            &e,
            &HashMap::from([
                ("user".into(), "alice".into()),
                ("password".into(), "test".into()),
            ]),
            None,
            &strings(),
        )
        .unwrap();
        assert!(cmd.contains("'Authorization: Basic YWxpY2U6dGVzdA=='"));
        assert!(cmd.contains("'X-Generated: generated'"));
        assert_eq!(e.to_hurl(), original);
        assert!(
            command(&e, &HashMap::new(), None, &strings())
                .unwrap_err()
                .contains("password")
        );
    }

    #[cfg(unix)]
    #[test]
    fn shell_quoting_preserves_metacharacters_as_data() {
        let mut e = entry();
        let value = "O'Reilly $(printf INJECTED); `printf INJECTED` $HOME";
        e.headers.push(KvRow::new("X-Value", value));
        let cmd = command(&e, &HashMap::new(), None, &strings()).unwrap();
        // A local shell function captures argv instead of invoking curl.
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("curl() {{ printf '%s\\000' \"$@\"; }};\n{cmd}"))
            .output()
            .unwrap();
        assert!(output.status.success());
        let args: Vec<_> = output
            .stdout
            .split(|b| *b == 0)
            .map(|s| String::from_utf8(s.to_vec()).unwrap())
            .collect();
        assert!(args.contains(&format!("X-Value: {value}")));
    }

    #[test]
    fn option_headers_override_inference_and_keep_empty_headers() {
        let mut e = entry();
        e.body_src = Some("{}".into());
        e.options = vec![
            KvRow::new("header", "Content-Type: application/custom"),
            KvRow::new("header", "X-Empty:"),
        ];
        let cmd = command(&e, &HashMap::new(), None, &strings()).unwrap();
        assert!(cmd.contains("'Content-Type: application/custom'"));
        assert!(cmd.contains("'X-Empty;'"));
        assert!(!cmd.contains("application/json"));
    }

    #[test]
    fn invalid_code_does_not_copy_the_previous_valid_request() {
        let mut session = crate::session::Session::default();
        session.collections[0].entries = vec![entry()];
        let mut app = GuiApp::for_test(session);
        app.editor_section = super::super::app::EditorSection::Code;
        app.code_edit.error = Some("invalid code".into());
        let ctx = eframe::egui::Context::default();
        let output = ctx.run_ui(Default::default(), |ui| copy(&mut app, ui.ctx(), 0, 0));
        assert!(matches!(app.session.status, Some(Status::Error(_))));
        assert!(output.platform_output.commands.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn binary_bodies_survive_the_shell_pipeline() {
        for body in ["hex,00ff0a27;", "base64,AP8KJw==;"] {
            let mut e = entry();
            e.body_src = Some(body.into());
            let cmd = command(&e, &HashMap::new(), None, &strings()).unwrap();
            let output = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("curl() {{ cat; }};\n{cmd}"))
                .output()
                .unwrap();
            assert!(output.status.success());
            assert_eq!(output.stdout, [0, 255, 10, 39]);
        }
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "requires the curl command-line executable"]
    fn exported_commands_send_expected_local_http() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::time::{Duration, Instant};

        for (body, expected, content_type) in [
            (None, &b""[..], None),
            (
                Some("{\"n\":1}"),
                &b"{\"n\":1}"[..],
                Some("application/json"),
            ),
            (Some("`raw text`"), &b"raw text"[..], None),
            (Some("hex,00ff0a27;"), &b"\0\xff\n'"[..], None),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let mut e = entry();
            e.url = format!("http://{}/oauth2", listener.local_addr().unwrap());
            e.body_src = body.map(str::to_owned);
            e.headers = vec![KvRow::new("client_id", "test"), KvRow::new("X-Empty", "")];
            e.options = vec![KvRow::new("max-time", "5s")];
            let cmd = command(&e, &HashMap::new(), None, &strings()).unwrap();
            let server = std::thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(6);
                let mut socket = loop {
                    match listener.accept() {
                        Ok((socket, _)) => break socket,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < deadline, "curl never connected");
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(e) => panic!("{e}"),
                    }
                };
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                socket
                    .set_write_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut received = Vec::new();
                let (headers, body) = loop {
                    let mut buf = [0; 4096];
                    let n = socket.read(&mut buf).unwrap();
                    assert_ne!(n, 0, "incomplete HTTP request");
                    received.extend_from_slice(&buf[..n]);
                    if let Some(end) = received.windows(4).position(|s| s == b"\r\n\r\n") {
                        let headers = String::from_utf8(received[..end].to_vec()).unwrap();
                        let len: usize = headers
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse().unwrap())
                            })
                            .unwrap_or(0);
                        if received.len() >= end + 4 + len {
                            break (headers, received[end + 4..end + 4 + len].to_vec());
                        }
                    }
                };
                socket
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                    )
                    .unwrap();
                (headers, body)
            });
            let output = std::process::Command::new("sh")
                .arg("-c")
                .arg(cmd)
                .env("NO_PROXY", "*")
                .env("no_proxy", "*")
                .output()
                .unwrap();
            let (headers, body) = server.join().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).starts_with("HTTP/1.1 200 OK"));
            assert!(headers.starts_with("POST /oauth2 HTTP/1.1"));
            assert!(headers.to_lowercase().contains("\r\nclient_id: test"));
            assert!(headers.to_lowercase().contains("\r\nx-empty:"));
            let actual_type = headers.lines().find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-type")
                    .then_some(value.trim())
            });
            assert_eq!(actual_type, content_type);
            assert_eq!(body, expected);
        }
    }
}
