//! Headless CLI report runner:
//! `paperboy -c collection -e env -r report [--dry-run] [-o out.csv|-]`.
//!
//! The report engine ([`crate::report`]) is front-end agnostic, so this module
//! is a thin CLI shell around it: it loads the report / collection / environment
//! files, assembles a [`RunContext`], runs the flow (live, or a no-HTTP dry
//! expansion under `--dry-run`), streams a `done/total` progress line to stderr,
//! and writes the tabular result (CSV in v1) to a file or stdout.
//!
//! Decorative/progress output goes to **stderr** so that `-o -` can emit clean
//! CSV to stdout for piping; a file/derived output prints its human summary to
//! stdout instead.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::environment::{looks_like_env, parse_vars};
use crate::postman::parse_collection;
use crate::report::flow::Header;
use crate::report::producers::resolve_path;
use crate::report::report::{expand_output_tokens, name_has_output_token};
use crate::report::run::{DryRunner, LiveRunner, RunContext, finalize, run_flow_raw};
use crate::report::validate::{Context, Severity, validate};
use crate::report::writer::{OUTPUT_EXTENSIONS, writer_for_extension};
use crate::report::{CsvWriter, Report, ReportResult, ReportRow, ReportWriter};

/// Run a report headlessly. Returns an OS exit code (0 = success, 1 = a fatal
/// setup/validation error; a run that merely collected per-row errors still
/// exits 0 — its errors are reported but every row was produced).
///
/// `-c` names the collection to run against (re-pointable without editing the
/// report), `-e` an optional environment used as the base variable layer,
/// `--dry-run` expands the flow without sending any request, and `-o` chooses
/// the output (`-` = stdout; a path whose extension selects the format; omitted
/// = the `# output:` format written to a `# name:`-derived file next to the
/// report, honouring the `{time}` token).
pub fn run(
    collection_path: String,
    env_path: Option<String>,
    report_path: String,
    output: Option<String>,
    dry_run: bool,
) -> i32 {
    // stdout stays clean for a piped CSV (`-o -`); everything human goes to the
    // "decorative" stream, which is stderr in that case and stdout otherwise.
    let to_stdout = output.as_deref() == Some("-");

    // --- report ----------------------------------------------------------
    let report = match Report::load_local(&report_path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: cannot read report file: {e}");
            return 1;
        }
    };
    let flow = match report.flow() {
        Ok(f) => f,
        Err(e) => {
            eprintln!("error: report '{report_path}' has a syntax error: {e}");
            return 1;
        }
    };

    // --- collection ------------------------------------------------------
    let col_content = match fs::read_to_string(&collection_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: cannot read collection file '{collection_path}': {e}");
            return 1;
        }
    };
    let entries = parse_collection(&col_content);
    if entries.is_empty() {
        eprintln!("error: no requests found in '{collection_path}'");
        return 1;
    }

    // --- environment -----------------------------------------------------
    // The single `-e` environment is the report's base variable layer, and is
    // also the one environment selectable by name in an `ENVS` loop (multiple
    // environments aren't supported in the CLI in v1 — an `ENVS` loop naming an
    // unloaded environment fails validation).
    let mut base_vars: HashMap<String, String> = HashMap::new();
    let mut named_envs: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut env_name: Option<String> = None;
    if let Some(env_path) = &env_path {
        let env_content = match fs::read_to_string(env_path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("error: cannot read environment file '{env_path}': {e}");
                return 1;
            }
        };
        if !looks_like_env(&env_content) {
            eprintln!(
                "error: '{env_path}' is not a valid environment file (expected KEY=value lines)"
            );
            return 1;
        }
        let name = crate::shared_utils::stem(env_path, "env");
        let env = parse_vars(name.clone(), &env_content);
        let flat: HashMap<String, String> = env
            .vars
            .iter()
            .map(|v| (v.key.clone(), v.value.clone()))
            .collect();
        base_vars = flat.clone();
        named_envs.insert(name.clone(), flat);
        env_name = Some(name);
    }

    // --- validation ------------------------------------------------------
    // Same checks the TUI runs. A hard error blocks a live run (as it does in
    // the TUI); a dry run proceeds regardless so the projected expansion — and
    // any unresolved names as per-row errors — can still be inspected.
    let titles: Vec<String> = entries.iter().map(|e| e.title.clone()).collect();
    let fields: Vec<(String, Vec<String>)> = entries
        .iter()
        .map(|e| {
            (
                e.title.clone(),
                e.reports.iter().map(|(n, _)| n.clone()).collect(),
            )
        })
        .collect();
    let env_names: Vec<String> = named_envs.keys().cloned().collect();
    let ctx = Context {
        request_titles: Some(&titles),
        env_names: Some(&env_names),
        request_fields: Some(&fields),
    };
    let diags = validate(&flow, &ctx);
    let has_error = diags.iter().any(|d| d.severity == Severity::Error);
    for d in &diags {
        let tag = match d.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        eprintln!("{tag}: {}", d.message);
    }
    if has_error && !dry_run {
        eprintln!("error: the report has validation errors — fix them or use --dry-run to preview");
        return 1;
    }

    // --- run context -----------------------------------------------------
    let report_dir = report.path.as_deref().and_then(Path::parent);
    // Relative producer paths resolve against `# root:` if set, else the report
    // file's own directory.
    let root: Option<PathBuf> = match flow.header.root() {
        Some(r) if !r.trim().is_empty() => Some(resolve_path(report_dir, r)),
        _ => report_dir.map(Path::to_path_buf),
    };
    // Live requests are rooted at the collection's directory so relative
    // form-file paths resolve as they would when sent by hand.
    let file_root = Path::new(&collection_path).parent().map(Path::to_path_buf);

    let live = LiveRunner {
        file_root: file_root.clone(),
    };
    let dry = DryRunner;

    // --- header block ----------------------------------------------------
    let mut decor = Decor::new(to_stdout);
    decor.line(&format!("PaperBoy — report \"{}\"", report.name));
    decor.line(&format!("  Collection : {collection_path}"));
    if let Some(name) = &env_name {
        decor.line(&format!("  Environment: {name}"));
    }
    if dry_run {
        decor.line("  Mode       : DRY RUN (no requests sent)");
    }

    // --- run -------------------------------------------------------------
    let result = if dry_run {
        let ctx = RunContext {
            entries: &entries,
            base_vars,
            named_envs,
            root,
            runner: &dry,
            sink: None,
        };
        let mut r = run_flow_raw(&flow, &ctx);
        finalize(&mut r, &flow, &ctx);
        decor.line(&format!("  Rows       : {} projected", r.rows.len()));
        r
    } else {
        // Count the projected rows up front (a cheap no-HTTP expansion) so the
        // progress line has a denominator, then run for real, streaming a
        // `done/total` counter to stderr as each row completes.
        let total = {
            let ctx = RunContext {
                entries: &entries,
                base_vars: base_vars.clone(),
                named_envs: named_envs.clone(),
                root: root.clone(),
                runner: &dry,
                sink: None,
            };
            run_flow_raw(&flow, &ctx).rows.len()
        };
        decor.line(&format!("  Rows       : {total}"));
        let done = std::sync::atomic::AtomicUsize::new(0);
        let sink = |_row: &ReportRow| {
            let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            // Progress is inherently ephemeral; keep it on stderr regardless of
            // where the CSV goes, redrawing one line in place.
            eprint!("\r  running {n}/{total}   ");
            let _ = std::io::stderr().flush();
        };
        let ctx = RunContext {
            entries: &entries,
            base_vars,
            named_envs,
            root,
            runner: &live,
            sink: Some(&sink),
        };
        let mut r = run_flow_raw(&flow, &ctx);
        finalize(&mut r, &flow, &ctx);
        eprintln!("\r  running {total}/{total}   done");
        r
    };

    // --- errors ----------------------------------------------------------
    if !result.errors.is_empty() {
        decor.line(&format!("  Errors     : {}", result.errors.len()));
        for e in &result.errors {
            decor.line(&format!("    ! {e}"));
        }
    }

    // --- output ----------------------------------------------------------
    match write_output(&result, &flow.header, output.as_deref(), &report) {
        Ok(OutputTarget::Stdout) => {
            // The CSV already went to stdout; nothing more to print there.
        }
        Ok(OutputTarget::File(path)) => {
            decor.line(&format!("  Output     : {}", path.display()));
        }
        Err(e) => {
            eprintln!("error: cannot write output: {e}");
            return 1;
        }
    }

    0
}

/// Where the rendered report ended up (for the closing summary line).
enum OutputTarget {
    Stdout,
    File(PathBuf),
}

/// Serialize `result` and write it to the chosen destination:
/// - `Some("-")`  → stdout (clean CSV, for piping);
/// - `Some(path)` → that file (its extension selects the format: csv/json/xlsx);
/// - `None`       → a file derived from the header (`# output:` format,
///   `# name:`-derived stem honouring `{time}`, next to the report file).
///
/// An unrecognised extension/format is an error naming the supported set.
fn write_output(
    result: &ReportResult,
    header: &Header,
    output: Option<&str>,
    report: &Report,
) -> Result<OutputTarget, String> {
    match output {
        Some("-") => {
            // stdout is for piping text, so it always emits CSV (a binary xlsx
            // to a terminal would be useless); write to a named file for other
            // formats.
            let bytes = CsvWriter.write(result, header)?;
            std::io::stdout()
                .write_all(&bytes)
                .map_err(|e| e.to_string())?;
            Ok(OutputTarget::Stdout)
        }
        Some(path) => {
            let ext = Path::new(path)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("csv")
                .to_ascii_lowercase();
            let writer = writer_for_extension(&ext).ok_or_else(|| unsupported_ext(&ext))?;
            let bytes = writer.write(result, header)?;
            fs::write(path, bytes).map_err(|e| format!("{path}: {e}"))?;
            Ok(OutputTarget::File(PathBuf::from(path)))
        }
        None => {
            // The format comes from a `# output:` directive (default csv).
            let ext = output_extension_from_header(header)?;
            let writer = writer_for_extension(&ext).ok_or_else(|| unsupported_ext(&ext))?;
            let path = derived_output_path(report, &ext);
            let bytes = writer.write(result, header)?;
            fs::write(&path, bytes).map_err(|e| format!("{}: {e}", path.display()))?;
            Ok(OutputTarget::File(path))
        }
    }
}

/// The output extension implied by a `# output:` directive: its value lowercased
/// and trimmed (empty ⇒ `csv`). Errors when the named format isn't supported.
fn output_extension_from_header(header: &Header) -> Result<String, String> {
    let ext = header
        .output()
        .map(|f| f.trim().to_ascii_lowercase())
        .filter(|f| !f.is_empty())
        .unwrap_or_else(|| "csv".to_string());
    if writer_for_extension(&ext).is_none() {
        return Err(format!(
            "unsupported '# output:' format '{ext}' (supported: {})",
            OUTPUT_EXTENSIONS.join(", ")
        ));
    }
    Ok(ext)
}

/// The error for an output extension PaperTrail can't write.
fn unsupported_ext(ext: &str) -> String {
    format!(
        "unsupported output extension '.{ext}' (supported: {})",
        OUTPUT_EXTENSIONS.join(", ")
    )
}

/// The default output path when `-o` is omitted: alongside the report file with
/// the `ext` extension, unless the report *name* carries the `{time}` token, in
/// which case the token-expanded, sanitised name wins (a distinct file per run)
/// — placed in the report's own folder. Mirrors the TUI's `csv_export_path`.
fn derived_output_path(report: &Report, ext: &str) -> PathBuf {
    if name_has_output_token(&report.name) {
        let stem = sanitize_file_stem(&expand_output_tokens(&report.name));
        let file = format!("{stem}.{ext}");
        return match report.path.as_deref().and_then(Path::parent) {
            Some(dir) => dir.join(file),
            None => PathBuf::from(file),
        };
    }
    if let Some(path) = &report.path {
        return path.with_extension(ext);
    }
    PathBuf::from(format!("{}.{ext}", sanitize_file_stem(&report.name)))
}

/// Turn a display name into a safe single-segment file stem (path separators and
/// awkward characters → `_`), so a name can't escape the target directory.
/// Mirrors the TUI helper of the same name.
fn sanitize_file_stem(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "report".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Routes human-readable lines to the right stream: stderr when the CSV is going
/// to stdout (`-o -`, so stdout stays clean for piping), stdout otherwise.
struct Decor {
    to_stderr: bool,
}

impl Decor {
    fn new(csv_to_stdout: bool) -> Self {
        Decor {
            to_stderr: csv_to_stdout,
        }
    }
    fn line(&mut self, s: &str) {
        if self.to_stderr {
            eprintln!("{s}");
        } else {
            println!("{s}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique scratch directory for a test, cleaned up by the caller.
    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "paperboy_report_cli_{tag}_{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn sanitize_file_stem_replaces_path_and_awkward_chars() {
        assert_eq!(sanitize_file_stem("a/b:c"), "a_b_c");
        assert_eq!(sanitize_file_stem("../escape"), "___escape");
        assert_eq!(sanitize_file_stem("  keep me-1_2  "), "keep me-1_2");
        // Empty / all-punctuation names fall back to a safe default.
        assert_eq!(sanitize_file_stem("   "), "report");
    }

    #[test]
    fn derived_output_path_uses_report_path_for_plain_name() {
        let mut report = Report::from_text("nightly", "# name: nightly\n");
        report.path = Some(PathBuf::from("/reports/nightly.report"));
        assert_eq!(
            derived_output_path(&report, "csv"),
            PathBuf::from("/reports/nightly.csv")
        );
    }

    #[test]
    fn derived_output_path_expands_time_token_next_to_report() {
        let mut report = Report::from_text("run_{time}", "# name: run_{time}\n");
        report.path = Some(PathBuf::from("/reports/nightly.report"));
        let out = derived_output_path(&report, "csv");
        let name = out.file_name().unwrap().to_string_lossy();
        // Token expanded (no literal "{time}") and placed in the report's dir.
        assert!(name.starts_with("run_"), "unexpected name: {name}");
        assert!(name.ends_with(".csv"), "unexpected name: {name}");
        assert!(!name.contains("{time}"), "token not expanded: {name}");
        assert_eq!(out.parent(), Some(Path::new("/reports")));
    }

    #[test]
    fn derived_output_path_pathless_report_sanitizes_name() {
        let report = Report::from_text("weird/name", "# name: weird/name\n");
        assert_eq!(
            derived_output_path(&report, "csv"),
            PathBuf::from("weird_name.csv")
        );
    }

    #[test]
    fn dry_run_writes_projected_csv_to_file() {
        let dir = temp_dir("dry");
        let coll = dir.join("api.hurl");
        fs::write(&coll, "# Ping\nGET https://example.test/ping\nHTTP *\n").unwrap();
        let report = dir.join("r.report");
        fs::write(
            &report,
            "# name: r\n# collection: api.hurl\n# columns: Ping.HttpStatus as Status\nREPORT REQUEST Ping\n",
        )
        .unwrap();
        let out = dir.join("out.csv");

        let code = run(
            coll.to_string_lossy().into_owned(),
            None,
            report.to_string_lossy().into_owned(),
            Some(out.to_string_lossy().into_owned()),
            true, // dry-run: no HTTP
        );
        assert_eq!(code, 0, "dry run should succeed");

        let csv = fs::read_to_string(&out).unwrap();
        let mut lines = csv.lines();
        assert_eq!(lines.next(), Some("Status"), "header row");
        // One projected row exists (the dry cell value is a placeholder).
        assert!(lines.next().is_some(), "one projected row expected");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_collection_is_a_setup_error() {
        let dir = temp_dir("nocoll");
        let report = dir.join("r.report");
        fs::write(
            &report,
            "# name: r\n# collection: missing.hurl\nREPORT REQUEST Ping\n",
        )
        .unwrap();

        let code = run(
            dir.join("missing.hurl").to_string_lossy().into_owned(),
            None,
            report.to_string_lossy().into_owned(),
            Some("-".to_string()),
            true,
        );
        assert_eq!(code, 1, "a missing collection is a fatal setup error");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unsupported_output_extension_is_rejected() {
        let dir = temp_dir("badext");
        let coll = dir.join("api.hurl");
        fs::write(&coll, "# Ping\nGET https://example.test/ping\nHTTP *\n").unwrap();
        let report = dir.join("r.report");
        fs::write(
            &report,
            "# name: r\n# collection: api.hurl\nREPORT REQUEST Ping\n",
        )
        .unwrap();

        let code = run(
            coll.to_string_lossy().into_owned(),
            None,
            report.to_string_lossy().into_owned(),
            Some(dir.join("out.pdf").to_string_lossy().into_owned()),
            true,
        );
        assert_eq!(code, 1, "an unsupported extension should fail");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn dry_run_writes_each_supported_output_format() {
        let dir = temp_dir("fmts");
        let coll = dir.join("api.hurl");
        fs::write(&coll, "# Ping\nGET https://example.test/ping\nHTTP *\n").unwrap();
        let report = dir.join("r.report");
        fs::write(
            &report,
            "# name: r\n# collection: api.hurl\n# columns: Ping.HttpStatus as Status\nREPORT REQUEST Ping\n",
        )
        .unwrap();

        for (ext, check) in [
            (
                "json",
                &(|b: &[u8]| b.starts_with(b"{")) as &dyn Fn(&[u8]) -> bool,
            ),
            (
                "html",
                &(|b: &[u8]| b.starts_with(b"<!DOCTYPE html>")) as &dyn Fn(&[u8]) -> bool,
            ),
            (
                "xlsx",
                &(|b: &[u8]| b.starts_with(b"PK")) as &dyn Fn(&[u8]) -> bool,
            ),
        ] {
            let out = dir.join(format!("out.{ext}"));
            let code = run(
                coll.to_string_lossy().into_owned(),
                None,
                report.to_string_lossy().into_owned(),
                Some(out.to_string_lossy().into_owned()),
                true, // dry-run: no HTTP
            );
            assert_eq!(code, 0, ".{ext} output should succeed");
            let bytes = fs::read(&out).unwrap();
            assert!(!bytes.is_empty(), ".{ext} is non-empty");
            assert!(check(&bytes), ".{ext} has the expected magic/shape");
        }

        fs::remove_dir_all(&dir).ok();
    }
}
