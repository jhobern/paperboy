//! Headless CLI runner: `paperboy -c collection.hurl [-e env.vars] [--batch]`.
//!
//! Parsing, HTTP and `[Captures]`/`[Asserts]` evaluation are all delegated to
//! the Hurl runner (via [`crate::hurl::run_hurl`] / [`crate::hurl::run_hurl_streaming`]);
//! this module only handles file I/O, environment loading and formatting the
//! results.

use std::collections::HashMap;
use std::fs;
use std::io::IsTerminal;

use ratatui::crossterm::style::Stylize;

use crate::environment::{looks_like_env, parse_vars};
use crate::generators::SystemSource;
use crate::hurl::{
    EntryOutcome, FormFieldKind, collection_to_hurl, expand_base64_form_fields, parse_hurl_error,
    run_hurl, run_hurl_streaming_with,
};
use crate::postman::{looks_like_postman, parse_collection};
use crate::request;
use crate::shared_utils::stem;

/// Run all requests in the collection. Returns an OS exit code (0 = all passed).
/// By default each request's result is printed as soon as it finishes
/// (`batch: false`); `batch: true` waits for the whole collection to run
/// (the original behaviour), which is the only mode that preserves Hurl's
/// automatic cookie jar across every request — see [`run_hurl_streaming`]'s
/// docs for why streaming mode can't do that too.
pub fn run(collection_path: String, env_path: Option<String>, batch: bool) -> i32 {
    let col_content = match fs::read_to_string(&collection_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: cannot read collection file '{collection_path}': {e}");
            return 1;
        }
    };

    let col_name = stem(&collection_path, "collection");
    // Parsed once for the display metadata (titles); the runner re-parses + runs.
    // A Postman JSON export is imported to `HurlEntry`s and then serialized to
    // Hurl text so the runner (which only speaks Hurl) can execute it.
    let entries = parse_collection(&col_content);
    if entries.is_empty() {
        // Surface the concrete Hurl parse reason (line + what's wrong) when the
        // source is Hurl: a single malformed line rejects the whole file, so
        // "no requests found" alone is unhelpful. (Skip for Postman JSON, where
        // a Hurl-parse reason would be meaningless.)
        match (!looks_like_postman(&col_content))
            .then(|| parse_hurl_error(&col_content))
            .flatten()
        {
            Some(why) => eprintln!("error: no requests found in '{collection_path}' — {why}"),
            None => eprintln!("error: no requests found in '{collection_path}'"),
        }
        // Nothing ran, so nothing passed. Exiting 0 here let a typo'd path, an
        // empty file or one malformed line pass a CI pipeline green, which is
        // the opposite of what `-c` is for.
        return 1;
    }
    // A Base64File field is a PaperBoy concept Hurl can't run directly: expand
    // each into the plain `key: prefix+base64` text it's actually sent as
    // (resolving files against the collection's directory) and always run from
    // the re-serialized text so the on-disk marker never reaches the server.
    let has_base64 = entries.iter().any(|e| {
        e.form_fields
            .iter()
            .any(|f| f.kind == FormFieldKind::Base64File)
    });
    let mut entries = entries;
    // A request the file could not be read at is text, not something that can
    // be sent. Skipping it runs the rest of the collection, which is the whole
    // point of recovering the file at all — running the raw text instead would
    // fail every request in it to parse.
    let unreadable: Vec<String> = entries
        .iter()
        .filter(|e| e.is_unreadable())
        .map(|e| {
            if e.title.is_empty() {
                "<unnamed>".to_string()
            } else {
                e.title.clone()
            }
        })
        .collect();
    let skipped = !unreadable.is_empty();
    if skipped {
        eprintln!(
            "warning: {} request(s) in '{collection_path}' could not be read and were skipped: {}",
            unreadable.len(),
            unreadable.join(", ")
        );
        entries.retain(|e| !e.is_unreadable());
        if entries.is_empty() {
            eprintln!("error: nothing in '{collection_path}' could be read as a request");
            return 1;
        }
    }
    if has_base64 {
        let root = std::path::Path::new(&collection_path).parent();
        if let Err(e) = expand_base64_form_fields(&mut entries, root) {
            eprintln!("error: cannot read a Base64 File form field: {e}");
            return 1;
        }
    }
    let run_content = if looks_like_postman(&col_content) || has_base64 || skipped {
        // These paths re-serialize from the entry model, so normalize each
        // entry the same way the TUI runner does: a bodyless POST/PUT/PATCH/
        // DELETE gets an explicit `Content-Length: 0` (Postman/browsers send
        // it; libcurl omits it over HTTP/2 and some servers 400 without it).
        for e in &mut entries {
            e.ensure_run_content_length();
        }
        collection_to_hurl(&entries)
    } else {
        col_content
    };

    // Substitution variables come from the environment file (if any); captures
    // are chained by the runner itself. `BASE_URL` must be provided by the env
    // file, so `-e` is required for collections that use `{{ BASE_URL }}`.
    let mut vars: HashMap<String, String> = HashMap::new();
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
        let name = stem(env_path, "env");
        env_name = Some(name.clone());
        for v in &parse_vars(name, &env_content).vars {
            vars.insert(v.key.clone(), v.value.clone());
        }
    }

    // A request may declare parameters as `[Options] variable: NAME=value`.
    // PaperBoy reads those as *defaults*, so a name the environment file binds
    // must win — Hurl would let the request's own row overwrite it. Dropping
    // just the bound rows leaves the rest for Hurl to apply as written. The
    // re-serialize is conditional because a plain `.hurl` is otherwise run
    // verbatim, and round-tripping a file nobody parameterised buys nothing.
    let mut run_content = run_content;
    if request::strip_bound_variable_options(&mut entries, &vars) {
        for e in &mut entries {
            e.ensure_run_content_length();
        }
        run_content = collection_to_hurl(&entries);
    }

    let color = color_enabled();
    println!();
    println!(
        "{}",
        paint(
            color,
            Hue::Bold,
            &format!("🦀 PaperBoy — running collection \"{col_name}\"")
        )
    );
    if let Some(name) = &env_name {
        println!("   Environment : {name}");
    }
    println!("   Requests    : {}", entries.len());
    if !batch {
        println!(
            "{}",
            paint(
                color,
                Hue::Dim,
                "   (streaming results as they finish; cookies set by one request are not \
                 automatically carried to the next — pass --batch if your collection relies on \
                 that. An explicit [Cookies] section on a request is unaffected either way.)"
            )
        );
    }
    println!("{}", "─".repeat(60));

    let total = entries.len();
    let titles: Vec<&str> = entries.iter().map(|e| e.title.as_str()).collect();
    let file_root = std::path::Path::new(&collection_path).parent();

    // A request carrying `[Options] repeat`/`retry` reports more than once, so
    // results can't be handed out one request at a time: each outcome says
    // which request it came from (`entry_index`), and that is what decides the
    // title, the `[n/total]` label and which request the result counts
    // towards. Counting per *outcome* instead used to credit a repeat's later
    // runs to the requests after it and print a position past the end.
    //
    // A request is passed only if every one of its runs passed; one that never
    // reported at all (the run stopped early) stays `None` and is counted
    // neither way, so the totals never claim more than actually happened.
    let mut per_request: Vec<Option<bool>> = vec![None; total];
    let mut record = |eo: &EntryOutcome| credit(&mut per_request, eo);

    // `# [Gen]` blocks. A block belongs to one request and is evaluated per
    // send, so it cannot simply be folded into the run's variables up front:
    // two requests that each compute a `nonce` must each get their own.
    // Streaming already runs one entry at a time, so each block is evaluated
    // in its window — which also lets a generator read a value an earlier
    // request captured. Batch is a single Hurl call over the whole file and
    // has no such window, so there every block is evaluated once, before the
    // run, against the environment alone; a name computed by two requests
    // takes the first request's value for both.
    let gen_entries = entries.clone();
    let mut gen_reported: Vec<bool> = vec![false; gen_entries.len()];
    let strings = crate::i18n::Strings::for_language(&crate::i18n::Language::English);
    let report_gen = |title: &str, errors: &[crate::generators::GenError]| {
        for detail in crate::i18n::describe_gen_errors(&strings, errors) {
            eprintln!(
                "{}",
                paint(color, Hue::Red, &format!("  ! {title}: {detail}"))
            );
        }
    };

    let out = if batch {
        let mut vars = vars.clone();
        for e in &gen_entries {
            if e.generators.is_empty() {
                continue;
            }
            let mut merged = vars.clone();
            let errors =
                crate::generators::expand(&e.generators, &mut merged, &SystemSource::new());
            report_gen(&e.title, &errors);
            for (name, _) in &e.generators {
                if let Some(v) = merged.get(name) {
                    vars.entry(name.clone()).or_insert_with(|| v.clone());
                }
            }
        }
        let out = run_hurl(&run_content, &vars, file_root);
        for eo in out.entries.iter() {
            print_entry(
                color,
                eo.entry_index,
                total,
                titles.get(eo.entry_index).copied(),
                eo,
            );
            record(eo);
        }
        out
    } else {
        run_hurl_streaming_with(
            &run_content,
            &vars,
            file_root,
            |i, known| {
                let Some(entry) = gen_entries.get(i) else {
                    return Vec::new();
                };
                if entry.generators.is_empty() {
                    return Vec::new();
                }
                let mut merged = known.clone();
                let errors =
                    crate::generators::expand(&entry.generators, &mut merged, &SystemSource::new());
                // Reported once per request however often it repeats, so a
                // `[Options] retry` does not print the same typo five times.
                if !std::mem::replace(&mut gen_reported[i], true) {
                    report_gen(&entry.title, &errors);
                }
                entry
                    .generators
                    .iter()
                    .filter_map(|(name, _)| merged.get(name).map(|v| (name.clone(), v.clone())))
                    .collect()
            },
            |eo| {
                print_entry(
                    color,
                    eo.entry_index,
                    total,
                    titles.get(eo.entry_index).copied(),
                    eo,
                );
                record(eo);
            },
        )
    };
    let passed = per_request.iter().filter(|r| **r == Some(true)).count();
    let failed = per_request.iter().filter(|r| **r == Some(false)).count();

    if out.entries.is_empty() {
        if let Some(e) = &out.error {
            eprintln!("\nerror: {e}");
        }
        return 1;
    }

    println!();
    println!("{}", "─".repeat(60));
    let summary = format!("  Passed: {passed}  Failed: {failed}  Total: {total}");
    println!(
        "{}",
        paint(
            color,
            if failed > 0 { Hue::Red } else { Hue::Green },
            &summary
        )
    );
    println!();

    if failed > 0 { 1 } else { 0 }
}

/// Record one outcome against the request it came from: passed only if every
/// run of that request passed. Out-of-range indices are ignored rather than
/// panicking — the runner is the authority on how many entries there were.
fn credit(per_request: &mut [Option<bool>], eo: &EntryOutcome) {
    if let Some(slot) = per_request.get_mut(eo.entry_index) {
        *slot = Some(slot.unwrap_or(true) && eo.ok);
    }
}

/// Print one request's result to stdout, coloured if `color` is enabled.
fn print_entry(color: bool, idx: usize, total: usize, title: Option<&str>, eo: &EntryOutcome) {
    println!();
    println!(
        "{}",
        paint(
            color,
            Hue::Cyan,
            &format!("[{}/{}] {} {}", idx + 1, total, eo.method, eo.url)
        )
    );
    if let Some(title) = title.filter(|t| !t.is_empty()) {
        println!("         {title}");
    }

    let status_hue = if eo.ok { Hue::Green } else { Hue::Red };
    let icon = if eo.ok { "✓" } else { "✗" };
    println!(
        "  {}",
        paint(
            color,
            status_hue,
            &format!("{icon} {} {}", eo.status, eo.status_text)
        )
    );
    if let Some(err) = &eo.error {
        println!("  {}", paint(color, Hue::Red, &format!("! {err}")));
    }

    if !eo.body.is_empty() {
        for line in eo.body.lines().take(40) {
            println!("    {line}");
        }
        let n = eo.body.lines().count();
        if n > 40 {
            println!(
                "{}",
                paint(color, Hue::Dim, &format!("    … ({} more lines)", n - 40))
            );
        }
    }

    if !eo.asserts.is_empty() {
        let n_ok = eo.asserts.iter().filter(|a| a.passed).count();
        let all_ok = n_ok == eo.asserts.len();
        let badge = if all_ok { "✓" } else { "✗" };
        println!(
            "  {}",
            paint(
                color,
                if all_ok { Hue::Green } else { Hue::Red },
                &format!("[Asserts] {badge} {n_ok}/{}", eo.asserts.len())
            )
        );
        for a in &eo.asserts {
            if a.passed {
                println!(
                    "      {}",
                    paint(color, Hue::Green, &format!("✓ {}", a.expr))
                );
            } else if a.detail.is_empty() {
                println!("      {}", paint(color, Hue::Red, &format!("✗ {}", a.expr)));
            } else {
                println!(
                    "      {}",
                    paint(color, Hue::Red, &format!("✗ {}  ({})", a.expr, a.detail))
                );
            }
        }
    }

    for (name, value) in &eo.captures {
        println!(
            "  {}",
            paint(color, Hue::Yellow, &format!("→ captured {name} = {value}"))
        );
    }
}

/// Whether ANSI colour should be emitted: only when stdout is a real terminal
/// (not piped/redirected) and the user hasn't opted out via `NO_COLOR`
/// (https://no-color.org), the conventional way to disable colour output.
fn color_enabled() -> bool {
    std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none()
}

/// A small, fixed palette for CLI output; kept separate from the TUI's
/// `Theme` since this is plain ANSI text, not a `ratatui` widget.
#[derive(Clone, Copy)]
enum Hue {
    Green,
    Red,
    Yellow,
    Cyan,
    Dim,
    Bold,
}

/// Style `s` with `hue` when `on`, otherwise return it unchanged.
fn paint(on: bool, hue: Hue, s: &str) -> String {
    if !on {
        return s.to_string();
    }
    match hue {
        Hue::Green => s.green().to_string(),
        Hue::Red => s.red().to_string(),
        Hue::Yellow => s.yellow().to_string(),
        Hue::Cyan => s.cyan().to_string(),
        Hue::Dim => s.dark_grey().to_string(),
        Hue::Bold => s.bold().to_string(),
    }
}

#[cfg(test)]
mod tests {
    /// Regression: nothing ran, so nothing passed. Exiting 0 here let a
    /// mistyped path, an empty file, or a collection whose every request was
    /// unreadable pass a CI pipeline green — the opposite of what `-c` is for.
    #[test]
    fn a_collection_with_nothing_to_run_exits_non_zero() {
        let dir = std::env::temp_dir().join(format!("paperboy_cli_empty_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("empty.hurl");
        std::fs::write(&path, "\n\n").unwrap();

        let code = super::run(path.to_string_lossy().into_owned(), None, false);
        assert_eq!(code, 1, "an empty collection is a failure, not a pass");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Regression: a request carrying `[Options] repeat` reports more than
    /// once. Counting per *outcome* credited the extra runs to the requests
    /// after it, printed a position past the end (`[3/2]`), and could claim
    /// more passes than there were requests. Results are keyed by the request
    /// they came from, and a request passes only if all of its runs did.
    #[test]
    fn a_repeated_request_counts_once_and_keeps_its_own_result() {
        use crate::hurl::run::EntryOutcome;
        let outcome = |entry_index: usize, ok: bool| EntryOutcome {
            entry_index,
            ok,
            ..EntryOutcome::default()
        };
        let mut per_request = vec![None; 2];
        // Request 0 runs twice (the second run fails), then request 1 runs.
        for eo in [outcome(0, true), outcome(0, false), outcome(1, true)] {
            super::credit(&mut per_request, &eo);
        }
        assert_eq!(
            per_request,
            vec![Some(false), Some(true)],
            "the repeat's failure sticks to request 0, and request 1 keeps its own pass"
        );
        let passed = per_request.iter().filter(|r| **r == Some(true)).count();
        assert_eq!(passed, 1, "never more passes than there are requests");
    }

    /// An outcome for a request the collection doesn't have is ignored rather
    /// than panicking: the runner decides how many entries ran.
    #[test]
    fn an_out_of_range_result_is_ignored() {
        use crate::hurl::run::EntryOutcome;
        let mut per_request = vec![None; 1];
        super::credit(
            &mut per_request,
            &EntryOutcome {
                entry_index: 9,
                ok: true,
                ..EntryOutcome::default()
            },
        );
        assert_eq!(per_request, vec![None]);
    }
}
