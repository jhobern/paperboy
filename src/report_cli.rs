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
use std::sync::Mutex;

use crate::environment::{looks_like_env, parse_vars};
use crate::postman::{looks_like_postman, parse_collection};
use crate::report::flow::Header;
use crate::report::model::{OutputColumn, Partial, ReportRow};
use crate::report::params::{ParamValues, undeclared};
use crate::report::producers::resolve_path;
use crate::report::report::{expand_output_tokens, name_has_output_token};
use crate::report::run::{
    Cancel, DryRunner, LiveRunner, RowEvent, RowSink, RunContext, finalize, run_flow_raw,
};
use crate::report::validate::{Context, Severity, validate};
use crate::report::writer::{OUTPUT_EXTENSIONS, writer_for_extension};
use crate::report::{CsvWriter, Report, ReportResult, ReportWriter};
use crate::shared_utils::sanitize_file_stem;

/// A seed for a bare `--shuffle`, from the clock.
///
/// Not cryptographic and not meant to be: it only has to differ between runs so
/// that repeated runs explore different legal orders, and it is printed, which
/// is what makes a failure reproducible.
fn random_seed() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1)
}

/// Run a report headlessly. Returns an OS exit code: 0 when the report was
/// produced and every row ran cleanly, 1 on a fatal setup/validation error
/// *or* when the run collected per-row errors. The output is still written in
/// that second case — the non-zero code is there so a CI pipeline can't pass
/// green on a report in which every request failed.
///
/// `collection` names the collection to run against (re-pointable without
/// editing the report); when `None`, the report's own `# collection:` header is
/// used, resolved relative to the report's folder. `env_paths` are zero or more
/// environments used as the base variable layer and (when repeated) the
/// environments an `ENVS` loop can select by name; when empty, the report's
/// `# environment:` header (if any) is used, likewise resolved relative to the
/// report. `--dry-run` expands the flow without sending any request, and `-o`
/// chooses the output (`-` = stdout; a path whose extension selects the format;
/// omitted = the `# output:` format written to a `# name:`-derived file next to
/// the report, honouring the `{time}` token). `outputs` is repeatable: one run
/// renders the same result once per requested format. `params` are the `--param
/// NAME=VALUE` values for the report's `PARAM` declarations; anything not
/// supplied falls back to the default written in the report. `progress_json`
/// replaces the human `done/total` counter with a newline-delimited JSON event
/// stream on stderr (see [`Progress`]).
///
/// A run can be **stopped**: on a signal, or — with `stop_on_stdin` — on a
/// `stop` line (or EOF) on stdin, which is the channel a parent process can
/// use on any platform. A stopped run starts no further rows, lets the ones in
/// flight finish, runs `CLEANUP`, writes the outputs it was asked for marked
/// as partial, and exits [`EXIT_INTERRUPTED`]; `grace_secs` bounds how long it
/// will wait for that (see [`Control`]).
pub fn run(
    collection_path: Option<String>,
    env_paths: Vec<String>,
    report_path: String,
    outputs: Vec<String>,
    dry_run: bool,
    targets: Vec<String>,
    shuffle: Option<Option<u64>>,
    params: ParamValues,
    progress_json: bool,
    grace_secs: Option<u64>,
    stop_on_stdin: bool,
) -> i32 {
    let control = Control::new(grace_secs);
    control.listen(stop_on_stdin);
    run_with_progress(
        collection_path,
        env_paths,
        report_path,
        outputs,
        dry_run,
        targets,
        shuffle,
        params,
        Progress::new(progress_json),
        control,
    )
}

/// The body of [`run`], with the event stream's destination passed in.
///
/// Split out so the stream itself can be tested: the events are a published
/// contract that other programs parse, and asserting on it by spawning the
/// binary and scraping its real stderr would test the shell as much as the
/// contract. A test hands in a capturing [`Progress`] and reads the events the
/// run actually produced, output files and all.
#[allow(clippy::too_many_arguments)]
fn run_with_progress(
    collection_path: Option<String>,
    env_paths: Vec<String>,
    report_path: String,
    outputs: Vec<String>,
    dry_run: bool,
    targets: Vec<String>,
    shuffle: Option<Option<u64>>,
    params: ParamValues,
    progress: Progress,
    control: Control,
) -> i32 {
    let progress_json = progress.on();
    // stdout stays clean for a piped CSV (`-o -`); everything human goes to the
    // "decorative" stream, which is stderr in that case and stdout otherwise.
    let to_stdout = outputs.iter().any(|o| o == "-");

    // --- report ----------------------------------------------------------
    let report = match Report::load_local(&report_path) {
        Ok(r) => r,
        Err(e) => {
            return progress.setup_failed(vec![format!("cannot read report file: {e}")]);
        }
    };
    let mut flow = match report.flow() {
        Ok(f) => f,
        Err(e) => {
            return progress.setup_failed(vec![format!(
                "report '{report_path}' has a syntax error: {e}"
            )]);
        }
    };
    // The report's folder anchors every relative reference it makes: the
    // `# collection:`/`# environment:` header fallbacks below, and (later) the
    // `# root:` producer/baseline base directory.
    let report_dir = report.path.as_deref().and_then(Path::parent);

    // --- parameters ------------------------------------------------------
    // Checked here, before anything is loaded or sent: a `--param` naming a
    // parameter this report doesn't declare is almost always a caller whose
    // command line has drifted from the script, and letting it through would
    // run the whole report against the default it thought it had replaced.
    let undeclared_params = undeclared(&flow.params(), &params);
    if !undeclared_params.is_empty() {
        let declared = flow.params();
        let known = if declared.is_empty() {
            "it declares none".to_string()
        } else {
            format!(
                "it declares: {}",
                declared
                    .iter()
                    .map(|p| p.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        return progress.setup_failed(vec![format!(
            "report '{report_path}' has no parameter named {} ({known})",
            undeclared_params
                .iter()
                .map(|n| format!("'{n}'"))
                .collect::<Vec<_>>()
                .join(", ")
        )]);
    }

    // --- outputs ---------------------------------------------------------
    // Judged before anything is loaded or sent, for the same reason the
    // parameters above are: a mistyped format is a setup error, and finding it
    // only once the report has been rendered would mean paying for a whole run
    // of live requests to be told where it couldn't be written.
    if let Err(e) = check_outputs(&outputs, &flow.header) {
        return progress.setup_failed(vec![e]);
    }

    // --- collection ------------------------------------------------------
    // `-c` re-points the report at any collection; when omitted, fall back to
    // the report's own `# collection:` header (resolved relative to the report's
    // folder) so a workspace report "just runs" without repeating the path.
    // A flow that embeds its own requests needs no collection at all: the
    // `REQUESTS` section *is* the collection, which is the whole point of a
    // monitor that ships as one file.
    let embedded = flow.embedded_entries();
    let collection_path = match collection_path {
        Some(c) => Some(c),
        None => match report.collection_ref() {
            Some(c) => Some(resolve_path(report_dir, &c).to_string_lossy().into_owned()),
            // The *declaration* is what excuses the missing collection, not
            // how many requests it yielded. A section that parsed to nothing
            // must reach validation, which knows why it did.
            None if flow.requests.is_some() => None,
            None => {
                return progress.setup_failed(vec![format!(
                    "no collection to run against — pass -c/--collection, add a '# collection:' header to '{report_path}', or embed the requests in a REQUESTS section"
                )]);
            }
        },
    };
    let col_content = match &collection_path {
        Some(path) => match fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                return progress
                    .setup_failed(vec![format!("cannot read collection file '{path}': {e}")]);
            }
        },
        None => String::new(),
    };
    let mut entries = match &collection_path {
        Some(_) => parse_collection(&col_content),
        None => Vec::new(),
    };
    // Appended, so an external collection's requests keep the names they had
    // and a collision is visible to validation rather than resolved silently
    // by whichever list was searched first.
    let embedded_count = embedded.len();
    entries.extend(embedded);
    let collection_path = collection_path.unwrap_or_else(|| {
        let s = if embedded_count == 1 { "" } else { "s" };
        format!("(embedded: {embedded_count} request{s})")
    });
    if entries.is_empty() {
        // As in `cli.rs`: prefer the concrete Hurl parse reason (line + what's
        // wrong) when the source is Hurl — one malformed line rejects the whole
        // file, so "no requests found" alone hides the real cause.
        // A `REQUESTS` section that yielded nothing is the likelier cause when
        // the flow has one, and it knows its own line numbers within the file.
        let embedded_why = flow
            .requests
            .as_deref()
            .and_then(|t| crate::hurl::parse_hurl_error_from(t, flow.requests_line.max(1)));
        let message = match embedded_why.or_else(|| {
            (!looks_like_postman(&col_content))
                .then(|| crate::hurl::parse_hurl_error(&col_content))
                .flatten()
        }) {
            Some(why) => format!("no requests found in '{collection_path}' — {why}"),
            None => format!("no requests found in '{collection_path}'"),
        };
        return progress.setup_failed(vec![message]);
    }

    // --- environment(s) --------------------------------------------------
    // Zero or more `-e` environments. Each is loaded, named by its file stem,
    // and made selectable by that name in an `ENVS` loop — so a
    // `FOR … IN ENVS BASELINE("prod"), COMPARISON("staging")` comparison runs
    // headlessly by passing `-e prod.vars -e staging.vars`. The first `-e`
    // doubles as the base variable layer for requests outside any `ENVS` loop.
    // Distinct stems are required so an `ENVS` clause names an environment
    // unambiguously. Backward compatible with a single `-e`.
    //
    // When no `-e` is given, fall back to the report's `# environment:` header
    // (resolved relative to the report's folder), mirroring the collection
    // fallback above. Explicit `-e` flags always win.
    let env_paths: Vec<String> = if env_paths.is_empty() {
        match report.environment_ref() {
            Some(e) => vec![resolve_path(report_dir, &e).to_string_lossy().into_owned()],
            None => Vec::new(),
        }
    } else {
        env_paths
    };
    let mut base_vars: HashMap<String, String> = HashMap::new();
    let mut named_envs: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut env_names_loaded: Vec<String> = Vec::new();
    for env_path in &env_paths {
        let env_content = match fs::read_to_string(env_path) {
            Ok(c) => c,
            Err(e) => {
                return progress.setup_failed(vec![format!(
                    "cannot read environment file '{env_path}': {e}"
                )]);
            }
        };
        if !looks_like_env(&env_content) {
            return progress.setup_failed(vec![format!(
                "'{env_path}' is not a valid environment file (expected KEY=value lines)"
            )]);
        }
        let name = crate::shared_utils::stem(env_path, "env");
        if named_envs.contains_key(&name) {
            return progress.setup_failed(vec![format!(
                "duplicate environment name '{name}' (from '{env_path}') — each -e file must have a distinct stem so an ENVS clause can name it unambiguously"
            )]);
        }
        let env = parse_vars(name.clone(), &env_content);
        let flat: HashMap<String, String> = env
            .vars
            .iter()
            .map(|v| (v.key.clone(), v.value.clone()))
            .collect();
        // The first environment is the base variable layer.
        if env_names_loaded.is_empty() {
            base_vars = flat.clone();
        }
        named_envs.insert(name.clone(), flat);
        env_names_loaded.push(name);
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
    // The headless runner has nothing open, so every helper collection is read
    // from disk relative to the report.
    let cli_strings = crate::i18n::Strings::for_language(&crate::i18n::Language::English);
    let (helpers, helper_errors) = crate::report::context::load_helpers(
        &[],
        &flow,
        Some(std::path::Path::new(&report_path)),
        &cli_strings,
    );
    // Relative producer paths (and the `# baseline:` snapshot) resolve against
    // `# root:` if set, else the report file's own directory (`report_dir`,
    // computed above). Computed here so validation's baseline-existence check
    // and the run context agree.
    let root: Option<PathBuf> = match flow.header.root() {
        Some(r) if !r.trim().is_empty() => Some(resolve_path(report_dir, r)),
        _ => report_dir.map(Path::to_path_buf),
    };
    // For the variable-availability check: the base env variables and the union
    // of all loaded env variables.
    let base_var_names_owned: Vec<String> = env_names_loaded
        .first()
        .and_then(|first_name| named_envs.get(first_name))
        .map(|m| {
            let mut keys: Vec<String> = m.keys().cloned().collect();
            keys.sort();
            keys
        })
        .unwrap_or_default();
    let mut all_env_var_names_owned: Vec<String> = named_envs
        .values()
        .flat_map(|m| m.keys().cloned())
        .collect();
    all_env_var_names_owned.sort();
    all_env_var_names_owned.dedup();

    let ctx = Context {
        request_titles: Some(&titles),
        env_names: Some(&env_names),
        request_fields: Some(&fields),
        root: root.as_deref(),
        base_var_names: Some(&base_var_names_owned),
        all_env_var_names: Some(&all_env_var_names_owned),
        request_entries: Some(&entries),
        helpers: &helpers,
        helper_errors: &helper_errors,
        // The headless runner has no language setting of its own — its output
        // is read by scripts and CI logs, which is English territory.
        strings: &cli_strings,
    };
    let diags = validate(&flow, &ctx);
    let has_error = diags.iter().any(|d| d.severity == Severity::Error);
    // Carried rather than printed when the stream is on: a validation warning
    // is a fact about the run a consumer wants, and prose on the stream's own
    // channel is a line it would have to throw away to keep parsing.
    let mut setup_warnings: Vec<String> = Vec::new();
    for d in &diags {
        let tag = match d.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        if progress_json {
            if d.severity == Severity::Warning {
                setup_warnings.push(d.message.clone());
            }
        } else {
            eprintln!("{tag}: {}", d.message);
        }
    }
    if has_error && !dry_run {
        // The diagnostics themselves were printed above in the human mode, so
        // only the stream needs them repeated here.
        let mut errors: Vec<String> = diags
            .iter()
            .filter(|d| progress_json && d.severity == Severity::Error)
            .map(|d| d.message.clone())
            .collect();
        errors.push(
            "the report has validation errors — fix them or use --dry-run to preview".to_string(),
        );
        return progress.setup_failed_with(errors, setup_warnings);
    }

    // --- targets ---------------------------------------------------------
    // Pruning happens after validation and before the run, on the flow itself,
    // so a dry run previews exactly the subset a live run would send. It is
    // refused outright on an unknown target: running a different set of steps
    // than the one asked for is a worse answer than running none.
    if !targets.is_empty()
        && let Err(errs) = crate::report::graph::prune_to_targets(
            &mut flow,
            &targets,
            &entries,
            &helpers,
            &cli_strings,
        )
    {
        return progress.setup_failed_with(errs, setup_warnings);
    }

    // --- run context -----------------------------------------------------
    // Live requests are rooted at the collection's directory so relative
    // form-file paths resolve as they would when sent by hand.
    let file_root = Path::new(&collection_path).parent().map(Path::to_path_buf);

    let live = LiveRunner {
        file_root: file_root.clone(),
    };
    let dry = DryRunner;

    // --- header block ----------------------------------------------------
    let mut decor = Decor::new(to_stdout, progress_json);
    decor.line(&format!("PaperBoy — report \"{}\"", report.name));
    decor.line(&format!("  Collection : {collection_path}"));
    if let [one] = env_names_loaded.as_slice() {
        decor.line(&format!("  Environment: {one}"));
    } else if !env_names_loaded.is_empty() {
        decor.line(&format!(
            "  Environments: {} (base: {})",
            env_names_loaded.join(", "),
            env_names_loaded[0]
        ));
    }
    if dry_run {
        decor.line("  Mode       : DRY RUN (no requests sent)");
    }
    if !targets.is_empty() {
        decor.line(&format!("  Targets    : {}", targets.join(", ")));
    }
    // Echoed for the same reason the seed below is: a report is a script's
    // output as much as a person's, and "which folder did last night's run
    // actually look at?" has to be answerable from the run's own log rather
    // than from the calling shell's history. Sorted, since the values arrive
    // as a map.
    if !params.is_empty() {
        let mut supplied: Vec<String> = params
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect();
        supplied.sort();
        decor.line(&format!("  Parameters : {}", supplied.join(", ")));
    }
    // Printed whether or not a seed was supplied, because a shuffled run that
    // fails is only useful if it can be repeated, and the seed is the whole of
    // what has to be carried from the failing run to the reproduction.
    let shuffle = shuffle.map(|s| s.unwrap_or_else(random_seed));
    if let Some(seed) = shuffle {
        decor.line(&format!(
            "  Shuffle    : seed {seed} (replay with --shuffle={seed})"
        ));
    }
    // The plan, printed as waves rather than a numbered sequence: a numbered
    // list would imply a total order that a graph does not have, and the reason
    // to print it at all is to show what the graph does and does not constrain.
    if dry_run {
        let lines = crate::report::graph::explain(&flow, &entries, &helpers, &cli_strings);
        if !lines.is_empty() {
            decor.line("");
            for l in lines {
                decor.line(&l);
            }
            decor.line("");
        }
    }

    // --- run -------------------------------------------------------------
    // A projection pass first: a no-HTTP expansion of the same flow, which
    // answers how many rows there will be and what the columns are called
    // *before* anything is sent. The human mode needs the count for the
    // `done/total` denominator; `--progress-json` needs the whole shape for its
    // `plan` event, so a front-end can draw the empty grid up front.
    //
    // A dry run is already that expansion, so it only pays for a second one
    // when the stream is on — and then only because `plan` has to precede the
    // rows it describes, which the pass producing them cannot do.
    let projection = (!dry_run || progress_json).then(|| {
        let ctx = RunContext {
            entries: &entries,
            helpers: &helpers,
            base_vars: base_vars.clone(),
            named_envs: named_envs.clone(),
            root: root.clone(),
            runner: &dry,
            strings: &cli_strings,
            params: params.clone(),
            sink: None,
            shuffle,
            // A projection sends nothing and takes no time worth stopping.
            cancel: None,
        };
        run_flow_raw(&flow, &ctx)
    });
    // Projected, not final: a column only a live response can name (and the
    // comparison columns `finalize` adds) isn't in this set, so a row's cells
    // are streamed against the planned grid and anything else is read from the
    // report file at the end. The alternative — growing the column set
    // mid-stream — would defeat the point of announcing it up front.
    let (streamed_columns, withheld_columns) = split_streamed_columns(
        projection
            .as_ref()
            .map(|p| p.resolved_columns(&flow.header))
            .unwrap_or_default(),
    );
    let total = projection.as_ref().map_or(0, |p| p.rows.len());
    let no_match = projection
        .as_ref()
        .map(|p| p.no_match_marker.clone())
        .unwrap_or_default();
    let slots = RowSlots::new(projection.as_ref().map_or(&[], |p| p.rows.as_slice()));
    // Kept rather than dropped, because it is the *shape* of the report —
    // column order, statistics, ground truths, the no-match marker — and a run
    // that is given up on never returns the assembled result those would
    // otherwise come from. An abandoned run's report is this shape with the
    // rows that actually finished poured into it.
    let shape = projection;

    let done = std::sync::atomic::AtomicUsize::new(0);
    // What a stopped run has to show for itself. The rows are copied out as
    // they land because a run that is given up on never hands its own result
    // back: the only rows reachable from outside the run thread are the ones
    // the sink has already seen. That costs a second copy of every row on a run
    // nobody interrupts — the price of not having a four-hour run leave nothing
    // behind when it is stopped in its fifth.
    let harvest = Mutex::new(Harvest::default());
    let sink = |ev: RowEvent| {
        if !dry_run && let RowEvent::Completed { row, errors } = &ev {
            let mut held = harvest.lock().unwrap_or_else(|e| e.into_inner());
            held.completed += 1;
            held.rows.push((*row).clone());
            held.errors
                .extend(errors.iter().map(|e| (row.path.clone(), e.clone())));
        }
        // The two progress modes are exclusive: interleaving a redrawn human
        // line with the NDJSON stream would corrupt both (the counter uses a
        // bare `\r` and no newline, so it would land *inside* an event line).
        if progress_json {
            match ev {
                RowEvent::Started(path) => progress.row_started(path, &slots),
                RowEvent::Completed { row, errors } => {
                    progress.row_completed(row, errors, &streamed_columns, &no_match, &slots)
                }
            }
            return;
        }
        // Count only completed rows for the progress readout (a row is also
        // announced when it starts, which we ignore here).
        if !matches!(ev, RowEvent::Completed { .. }) {
            return;
        }
        let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        // Progress is inherently ephemeral; keep it on stderr regardless of
        // where the CSV goes, redrawing one line in place.
        eprint!("\r  running {n}/{total}   ");
        let _ = std::io::stderr().flush();
    };
    // A dry run sends nothing, so there is no counter to draw — but it does
    // *produce* rows, and a consumer previewing a big run wants to watch the
    // grid fill exactly as a live one would. A live run always takes the hook:
    // it is also how a stop gets its rows.
    let sink: Option<&RowSink> = match (dry_run, progress_json) {
        (true, false) => None,
        _ => Some(&sink),
    };

    let result = if dry_run {
        progress.plan(
            &report.name,
            total,
            &streamed_columns,
            &withheld_columns,
            true,
        );
        let ctx = RunContext {
            entries: &entries,
            helpers: &helpers,
            base_vars,
            named_envs,
            root,
            runner: &dry,
            strings: &cli_strings,
            params: params.clone(),
            sink,
            shuffle,
            // Likewise a dry run: there is no request in flight to wind down,
            // and a stop that arrives during one is served by the process
            // ending, not by a partial report of requests never sent.
            cancel: None,
        };
        let mut r = run_flow_raw(&flow, &ctx);
        finalize(&mut r, &flow, &ctx);
        decor.line(&format!("  Rows       : {} projected", r.rows.len()));
        r
    } else {
        decor.line(&format!("  Rows       : {total}"));
        progress.plan(
            &report.name,
            total,
            &streamed_columns,
            &withheld_columns,
            false,
        );
        let ctx = RunContext {
            entries: &entries,
            helpers: &helpers,
            base_vars,
            named_envs,
            root,
            runner: &live,
            strings: &cli_strings,
            params,
            sink,
            shuffle,
            cancel: Some(control.cancel.as_ref()),
        };
        // The run gets a thread of its own so this one is free to watch for a
        // stop request while it happens. Nothing else here is concurrent: the
        // run is a single call that returns a whole report, and there is no
        // point in the middle of it at which it could ask "has anyone asked me
        // to stop?" on this thread's behalf.
        // Whether the stream has already been told the run is winding down. A
        // run can be stopped and finish before the watcher's next poll — two
        // fast rows and a stop between them — and a consumer that learns a run
        // was interrupted only from its terminal event has to work that out
        // backwards. Announced once, from whichever side notices first.
        let mut announced_stop = false;
        let ending = std::thread::scope(|s| {
            let (tx, rx) = std::sync::mpsc::channel();
            let (flow_ref, ctx_ref) = (&flow, &ctx);
            s.spawn(move || {
                let mut r = run_flow_raw(flow_ref, ctx_ref);
                finalize(&mut r, flow_ref, ctx_ref);
                let _ = tx.send(r);
            });
            let mut deadline: Option<std::time::Instant> = None;
            loop {
                match rx.recv_timeout(STOP_POLL) {
                    Ok(r) => return Ending::Ran(r),
                    // The run thread panicked, taking its sender with it.
                    // Stop waiting and let the scope's join re-raise that panic
                    // rather than inventing a result out of half a run.
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Ending::Lost,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                }
                if control.cancel.stopped() && deadline.is_none() {
                    deadline = Some(std::time::Instant::now() + control.grace);
                    announced_stop = true;
                    progress.run_stopping(control.source(), control.grace);
                    if !progress_json {
                        eprintln!(
                            "\r  stopping   : no new rows; up to {}s for the rows in flight and CLEANUP",
                            grace_label(control.grace)
                        );
                    }
                }
                if deadline.is_some_and(|d| std::time::Instant::now() >= d) {
                    // The grace period is up and rows are still in flight. What
                    // finished is all there will ever be, so write that and go:
                    // waiting any longer is the thing the caller just refused.
                    let mut r = abandoned_result(shape.as_ref(), &harvest, total, control.grace);
                    finalize(&mut r, &flow, &ctx);
                    let code = deliver(
                        &r,
                        &flow.header,
                        &report,
                        &outputs,
                        &mut decor,
                        &progress,
                        &setup_warnings,
                    );
                    (control.abandon)(code);
                    return Ending::Delivered(code);
                }
            }
        });
        match ending {
            Ending::Ran(mut r) => {
                if control.cancel.stopped() {
                    if !announced_stop {
                        progress.run_stopping(control.source(), control.grace);
                    }
                    // Stopped, but it wound down on its own terms: the rows in
                    // flight finished and `CLEANUP` ran. The report is short,
                    // and says so.
                    r.partial = Some(Partial {
                        rows_completed: harvest.lock().unwrap_or_else(|e| e.into_inner()).completed,
                        rows_planned: total,
                    });
                }
                if !progress_json {
                    match &r.partial {
                        Some(p) => eprintln!(
                            "\r  stopped    : {} of {} rows ran   ",
                            p.rows_completed, p.rows_planned
                        ),
                        None => eprintln!("\r  running {total}/{total}   done"),
                    }
                }
                r
            }
            // Already written and announced inside the scope above; in a real
            // run `Control::abandon` ended the process before this was reached.
            Ending::Delivered(code) => return code,
            Ending::Lost => unreachable!("the scope re-raises the run thread's panic"),
        }
    };

    deliver(
        &result,
        &flow.header,
        &report,
        &outputs,
        &mut decor,
        &progress,
        &setup_warnings,
    )
}

/// Say what happened and hand the report over: the human summary, one rendering
/// per requested `-o`, the exit code, and the stream's terminal event.
///
/// A function rather than the tail of [`run_with_progress`] because a run that
/// is given up on has to do all of this from *inside* the scope holding its
/// straggling threads (see [`Control::abandon`]) — and a partial report that
/// went out by a different path than a whole one would be exactly the kind of
/// second implementation that drifts.
#[allow(clippy::too_many_arguments)]
fn deliver(
    result: &ReportResult,
    header: &Header,
    report: &Report,
    outputs: &[String],
    decor: &mut Decor,
    progress: &Progress,
    setup_warnings: &[String],
) -> i32 {
    let progress_json = progress.on();
    // --- warnings, skips, errors -----------------------------------------
    if !result.warnings.is_empty() {
        decor.line(&format!("  Warnings   : {}", result.warnings.len()));
        for w in &result.warnings {
            decor.line(&format!("    ~ {w}"));
        }
    }
    if !result.skipped.is_empty() {
        decor.line(&format!(
            "  Skipped    : {} ({})",
            result.skipped.len(),
            result.skipped.join(", ")
        ));
    }
    if !result.errors.is_empty() {
        decor.line(&format!("  Errors     : {}", result.errors.len()));
        for e in &result.errors {
            decor.line(&format!("    ! {e}"));
        }
    }
    // Said in the summary as well as in the file, because the person who
    // pressed the button is looking at this and not at the report yet.
    if let Some(partial) = &result.partial {
        decor.line(&format!(
            "  Stopped    : PARTIAL — {} of {} rows ran",
            partial.rows_completed, partial.rows_planned
        ));
    }

    // --- output ----------------------------------------------------------
    // One run, one result, rendered once per requested format — never re-run.
    // Each file is announced as it lands, and a failure part-way through leaves
    // the ones already written where they are: they are faithful renderings of
    // a run that really happened, and removing them would destroy the only
    // record of it to tidy up after a disk that was full.
    let requested: Vec<Option<&str>> = if outputs.is_empty() {
        vec![None]
    } else {
        outputs.iter().map(|o| Some(o.as_str())).collect()
    };
    let mut write_failed = false;
    for target in requested {
        match write_output(&result, header, target, &report) {
            Ok(OutputTarget::Stdout(format)) => {
                // The report already went to stdout; nothing more to print there.
                progress.output_written("-", format, None);
            }
            Ok(OutputTarget::File(path)) => {
                decor.line(&format!("  Output     : {}", path.display()));
                let fmt = output_extension_of(&path.to_string_lossy());
                progress.output_written(&path.to_string_lossy(), &fmt, None);
            }
            Err(e) => {
                // Reported as a failed `output_written` rather than as prose,
                // so a `--progress-json` consumer hears about the file it asked
                // for on the same channel as the ones that landed.
                if progress_json {
                    let path = target.unwrap_or("");
                    progress.output_written(path, &output_extension_of(path), Some(&e));
                } else {
                    eprintln!("error: cannot write output: {e}");
                }
                write_failed = true;
            }
        }
    }

    // The report was produced either way, but a caller scripting this needs to
    // hear what happened in the exit code rather than by scraping the output.
    //
    // 3 beats 1 because it is the more informative of the two, and a skip only
    // ever arises *from* a failure — so exit 3 already implies exit 1's
    // condition while adding the fact that part of the run never happened at
    // all. (2 is left alone: clap uses it for argument errors, and a caller
    // must be able to tell "you invoked me wrongly" from "your API is broken".)
    //
    // 4 beats both, because it explains them: a stopped run's skips and errors
    // are as likely to be *of* the stop as of the API, and a caller that reads
    // 1 or 3 here would go looking for a fault that isn't there. A failed write
    // still wins, since then there is no report to have stopped short.
    let exit = if write_failed {
        1
    } else if result.partial.is_some() {
        EXIT_INTERRUPTED
    } else {
        match (result.skipped.is_empty(), result.errors.is_empty()) {
            (false, _) => EXIT_SKIPPED,
            (true, false) => 1,
            (true, true) => 0,
        }
    };
    progress.run_finished(result, exit, setup_warnings);
    exit
}

/// The run finished, but some steps never ran because something they depended
/// on failed. Documented in the README; changing it is a breaking change for
/// anyone scripting a release check.
pub const EXIT_SKIPPED: i32 = 3;

/// The run was stopped on request and wrote the rows it had.
///
/// Distinct from 1 and 3 because it answers a different question. 1 says the
/// API under test misbehaved; 3 says the report's own dependencies pruned part
/// of it; 4 says nothing was wrong at all — someone pressed stop. A caller that
/// retries on 1 must not retry on 4, and a dashboard that paints 1 red should
/// not paint this red.
pub const EXIT_INTERRUPTED: i32 = 4;

/// A second stop request, which gives up on the wind-down itself.
///
/// 128 + SIGINT, the shell's convention for "killed by an interrupt", and
/// deliberately *not* [`EXIT_INTERRUPTED`]: this path may have skipped
/// `CLEANUP` and may have written nothing, so a caller that treats 4 as "I have
/// a partial report" would be wrong to treat this the same way.
pub const EXIT_FORCED: i32 = 130;

/// How long a stopped run waits for the rows already in flight, unless
/// `--grace` says otherwise.
///
/// Thirty seconds because the wait is for work already paid for — an upload
/// half-sent, a slow report endpoint — and, after it, for `CLEANUP` to release
/// what the run took. Too short and a stop routinely abandons both; too long
/// and the stop button feels broken. A second stop is the escape hatch either
/// way, so this only has to be a sensible default rather than a bound anyone
/// has to live with.
pub const DEFAULT_GRACE_SECS: u64 = 30;

/// How often the waiting loop looks up from the run to see whether a stop has
/// been asked for. Short enough to feel immediate, long enough not to spin.
const STOP_POLL: std::time::Duration = std::time::Duration::from_millis(50);

/// The stop channel of a headless run: the switch the flow reads, how long a
/// stop will wait for the work already under way, and how the process ends if
/// that wait runs out.
///
/// Two things can flip the switch, and both are opt-in from the caller's side
/// rather than PaperBoy's business:
///
/// * a **signal** — `SIGINT` (Ctrl-C) or, on Unix, `SIGTERM`/`SIGHUP`, which is
///   what a CI job cancel sends;
/// * a **`stop` line on stdin**, with `--stop-on-stdin`. Signals are awkward to
///   send from a parent process on Windows (there is no `SIGTERM`; a parent can
///   only `TerminateProcess`, which is ungraceful, or arrange a console group
///   to send `CTRL_BREAK_EVENT`), so a program driving PaperBoy gets a channel
///   that works identically everywhere and needs no signal handling at all.
///   With `--progress-json` it completes the pair: events out on stderr,
///   control in on stdin. EOF counts as a stop, so a run also winds down when
///   the parent that started it goes away.
pub struct Control {
    cancel: std::sync::Arc<Cancel>,
    grace: std::time::Duration,
    /// What asked for the stop, for the `run_stopping` event: `"signal"`,
    /// `"stdin"` or `"eof"`. A consumer that started the run itself already
    /// knows it sent a signal; one watching a run it did not start does not,
    /// and "the parent went away" reads very differently from "an operator
    /// pressed Ctrl-C". Written once by whichever watcher fires first.
    source: std::sync::Arc<std::sync::Mutex<&'static str>>,
    /// Called when the grace period expires with rows still in flight, *after*
    /// the partial report has been written.
    ///
    /// In a real run this ends the process. The stragglers are inside a
    /// `std::thread::scope`, which joins on the way out, so there is no way to
    /// stop waiting for them and still return normally — "give up on the
    /// stragglers" and "exit" are the same act. A test passes a hook that
    /// returns instead, and simply waits them out.
    abandon: fn(i32),
}

impl Control {
    /// A control plane for a real run: `grace` seconds (or the default), ending
    /// the process if the wind-down overruns.
    pub fn new(grace_secs: Option<u64>) -> Self {
        Control {
            cancel: std::sync::Arc::new(Cancel::new()),
            grace: std::time::Duration::from_secs(grace_secs.unwrap_or(DEFAULT_GRACE_SECS)),
            source: std::sync::Arc::new(std::sync::Mutex::new("stop")),
            abandon: |code| std::process::exit(code),
        }
    }

    /// A control plane for a test: no signal handler and no stdin watcher
    /// (a test process must not have its Ctrl-C redefined, and its stdin is not
    /// a control channel), and a grace expiry that *returns* instead of ending
    /// the process — so a test of the abandoned path asserts on the report that
    /// was written and then simply waits the stragglers out.
    ///
    /// The switch itself is reachable through [`Control::switch`], which is how
    /// a test stops a run: from inside a fake runner, at a row of its choosing,
    /// with no timing to get wrong.
    #[cfg(test)]
    fn for_test() -> Self {
        Control {
            cancel: std::sync::Arc::new(Cancel::new()),
            grace: std::time::Duration::from_secs(DEFAULT_GRACE_SECS),
            source: std::sync::Arc::new(std::sync::Mutex::new("stop")),
            abandon: |_| {},
        }
    }

    /// The stop switch, for a caller that needs to flip it itself.
    #[cfg(test)]
    fn switch(&self) -> std::sync::Arc<Cancel> {
        self.cancel.clone()
    }

    /// How long this run will wait, once stopped, before giving up on the rows
    /// still in flight.
    #[cfg(test)]
    fn with_grace(mut self, grace: std::time::Duration) -> Self {
        self.grace = grace;
        self
    }

    /// What asked for the stop, as of now.
    fn source(&self) -> &'static str {
        *self.source.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Start listening for stop requests: always for signals, and for `stop`
    /// lines on stdin when the caller asked for that channel.
    ///
    /// The second request from either source is the hard kill: it exits at
    /// once, on the assumption that a caller repeating itself has decided the
    /// wind-down is not going to happen (a `CLEANUP` hanging on the very
    /// service that has stopped responding is the case this exists for).
    fn listen(&self, stop_on_stdin: bool) {
        let cancel = self.cancel.clone();
        let source = self.source.clone();
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let signal_hits = hits.clone();
        // Best effort: a handler can only be installed once per process, and a
        // failure here costs the graceful path, not the run. (It also keeps the
        // tests, which call the runner many times over, from tripping on the
        // second install.)
        let _ = ctrlc::set_handler(move || {
            if signal_hits.fetch_add(1, std::sync::atomic::Ordering::Relaxed) > 0 {
                std::process::exit(EXIT_FORCED);
            }
            *source.lock().unwrap_or_else(|e| e.into_inner()) = "signal";
            cancel.stop();
        });
        if !stop_on_stdin {
            return;
        }
        let cancel = self.cancel.clone();
        let source = self.source.clone();
        // Detached on purpose: a blocking read on stdin cannot be cancelled,
        // and there is nothing to join it for — the thread's only job is to
        // outlive its own `read_line` or the process, whichever comes first.
        std::thread::spawn(move || {
            let mut line = String::new();
            loop {
                line.clear();
                match std::io::stdin().read_line(&mut line) {
                    // EOF: the parent closed the pipe or died, and a run nobody
                    // is listening to any more should wind down rather than
                    // carry on sending requests for an hour. A `stop` line is
                    // usually followed by exactly this, though, and the reason
                    // the caller gave is worth more than the one it implies —
                    // so an already-stopped run keeps the source it has.
                    Ok(0) => {
                        if !cancel.stopped() {
                            *source.lock().unwrap_or_else(|e| e.into_inner()) = "eof";
                        }
                        cancel.stop();
                        return;
                    }
                    Ok(_) => {
                        if !line.trim().eq_ignore_ascii_case("stop") {
                            continue;
                        }
                        if hits.fetch_add(1, std::sync::atomic::Ordering::Relaxed) > 0 {
                            std::process::exit(EXIT_FORCED);
                        }
                        *source.lock().unwrap_or_else(|e| e.into_inner()) = "stdin";
                        cancel.stop();
                    }
                    Err(_) => return,
                }
            }
        });
    }
}

/// The one format stdout takes: a pipe is for text, and a binary `.xlsx` down
/// it would be useless. Named so the value written and the value *announced*
/// come from the same place.
const CSV_EXTENSION: &str = "csv";

/// Where the rendered report ended up (for the closing summary line).
enum OutputTarget {
    /// Written to stdout, in the format named — carried rather than assumed by
    /// the caller, so the announcement can't drift from what was written if
    /// stdout ever takes a second format.
    Stdout(&'static str),
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
            Ok(OutputTarget::Stdout(CSV_EXTENSION))
        }
        Some(path) => {
            let ext = output_extension_of(path);
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

/// Everything about `-o` that can be judged before the run, judged before it.
///
/// With nothing chosen the header decides, so the format it names has to exist;
/// with `-o` given, every path must carry a format PaperTrail can write.
fn check_outputs(outputs: &[String], header: &Header) -> Result<(), String> {
    if outputs.is_empty() {
        output_extension_from_header(header)?;
        return Ok(());
    }
    // Two formats written to one pipe would interleave into something that is
    // neither of them, and there is no second stdout to send the other to.
    if outputs.iter().filter(|o| o.as_str() == "-").count() > 1 {
        return Err("-o - was given more than once, but there is only one stdout".to_string());
    }
    let mut seen: Vec<&str> = Vec::new();
    for out in outputs {
        if out == "-" {
            continue;
        }
        // Writing the same path twice means the second write destroys the
        // first, so the run would quietly produce one file where two were
        // asked for. Far more likely a typo in one of them than an intent.
        if seen.contains(&out.as_str()) {
            return Err(format!("-o {out} was given more than once"));
        }
        seen.push(out);
        let ext = output_extension_of(out);
        if writer_for_extension(&ext).is_none() {
            return Err(unsupported_ext(&ext));
        }
    }
    Ok(())
}

/// The format an output path selects: its extension, lowercased, defaulting to
/// CSV for a path that has none.
fn output_extension_of(path: &str) -> String {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("csv")
        .to_ascii_lowercase()
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

/// Routes human-readable lines to the right stream: stderr when the CSV is going
/// to stdout (`-o -`, so stdout stays clean for piping), stdout otherwise.
///
/// Silenced entirely when those lines would land on a stderr that
/// `--progress-json` has claimed for the machine-readable stream: a consumer
/// reading NDJSON line by line must not have to sort prose out of it, and every
/// fact the header block carries is in the events anyway. Decoration bound for
/// *stdout* is left alone — nothing is competing for it there.
struct Decor {
    to_stderr: bool,
    silent: bool,
}

impl Decor {
    fn new(csv_to_stdout: bool, progress_json: bool) -> Self {
        Decor {
            to_stderr: csv_to_stdout,
            silent: csv_to_stdout && progress_json,
        }
    }
    fn line(&mut self, s: &str) {
        if self.silent {
        } else if self.to_stderr {
            eprintln!("{s}");
        } else {
            println!("{s}");
        }
    }
}

/// The version of the `--progress-json` event stream, carried on **every**
/// event as `"schema"`.
///
/// On every event rather than only on the first because a consumer may attach
/// to a stream already in progress (a tail, a restarted reader), and a version
/// it can only have learned from a line it missed is no version at all. It is
/// also the one thing that cannot be retrofitted: a reader written against
/// schema 1 has to be able to *refuse* a schema 2 stream rather than silently
/// mis-read it, which means the number has to be there from the first release.
const PROGRESS_SCHEMA: u32 = 1;

/// How long a cell value may be before `row_completed` stops carrying it.
///
/// Progress is meant to be cheap. A column holding a whole engine response
/// would be streamed through stderr on every row while the same bytes are
/// already going to the output file, so anything this long is announced as
/// withheld and the consumer reads the full value from the report at the end
/// (by `row_index`, which is where that row landed in it).
const PROGRESS_MAX_CELL: usize = 4096;

/// The `--progress-json` emitter: newline-delimited JSON on stderr, one object
/// per line, flushed as each event is produced.
///
/// **stderr**, because `-o -` has already promised stdout to the report itself
/// and the existing contract puts progress on stderr. **Line-delimited**,
/// because the point is to be read incrementally — `for line in proc.stderr` in
/// Python, with an ordinary `json.loads` per line, and no streaming parser.
///
/// Every row event carries `path`: the row's structural [`ReportRow::path`]
/// (`(loop index, iteration)` pairs flattened into a dotted string, `"0.3"`,
/// `"0.1.2.0"`, `""` for the single row of a loop-free report). It is the join
/// key, and it is what makes the stream usable under `PARALLEL`: it is assigned
/// before the run, stable, unique, and identifies a row no matter what order
/// the workers finish in. Beside it rides [`RowSlots`]'s `row_index`, which is
/// where that row lands in the written report — so a consumer can hold a cheap
/// live grid and fill in the full values from the file once the run is done.
struct Progress {
    /// Where a formatted event line goes, or `None` when `--progress-json` was
    /// not asked for.
    ///
    /// A sink rather than a bare `bool` so the stream can be asserted on in a
    /// test: the events are a published contract (a consumer parses them by
    /// key), and a contract that can only be observed by running the binary and
    /// scraping the real stderr is one that gets broken quietly.
    out: Option<Box<dyn Fn(&str) + Sync + Send>>,
    /// Set once `run_finished` has gone out; every later event is dropped.
    ///
    /// The stream's headline promise is a single terminal event — an event
    /// *after* it breaks the contract from the other end, and a consumer that
    /// finalises its state there will either throw or silently mis-record. The
    /// window is real: a run given up on writes its report and exits while
    /// straggler rows are still in flight, and one landing in the sink between
    /// the terminal event and `process::exit` would be announced into a stream
    /// that had already ended. A latch here rather than disarming the sink,
    /// because it closes that shape wherever it appears rather than in the one
    /// place we found it.
    finished: std::sync::atomic::AtomicBool,
}

impl Progress {
    fn new(on: bool) -> Self {
        Progress {
            finished: std::sync::atomic::AtomicBool::new(false),
            out: on.then(|| {
                Box::new(|line: &str| {
                    let mut err = std::io::stderr().lock();
                    let _ = err.write_all(line.as_bytes());
                    let _ = err.flush();
                }) as Box<dyn Fn(&str) + Sync + Send>
            }),
        }
    }

    /// Whether `--progress-json` was asked for.
    fn on(&self) -> bool {
        self.out.is_some()
    }

    /// A fatal setup failure — an unreadable collection, a `--param` the report
    /// doesn't declare, a validation error. Returns the exit code, so a call
    /// site reads `return progress.setup_failed(…)`.
    ///
    /// This still ends the stream with `run_finished`. The run never started,
    /// so there is no row to report on — but that is a distinction the *runner*
    /// cares about, not the program driving it, which asked one question: did
    /// it work, and if not, why? Leaving these outside the stream would have
    /// made `run_finished`'s promise ("one terminal event, carrying the code
    /// you will get") false for exactly the failures a caller hits most — a
    /// mistyped parameter, a folder that isn't there — and left a consumer
    /// that skips non-JSON lines with an empty stream, a bare exit 1, and
    /// nothing to show anyone.
    ///
    /// The prose is *replaced* rather than accompanied, because with `-o -` it
    /// would land on the stream's own channel: the message is in the event.
    fn setup_failed(&self, errors: Vec<String>) -> i32 {
        self.setup_failed_with(errors, Vec::new())
    }

    /// [`setup_failed`](Self::setup_failed), also carrying the validation
    /// warnings collected before the failure (which the human mode has already
    /// printed).
    fn setup_failed_with(&self, errors: Vec<String>, warnings: Vec<String>) -> i32 {
        if self.out.is_none() {
            for message in &errors {
                eprintln!("error: {message}");
            }
            return 1;
        }
        self.emit(
            "run_finished",
            serde_json::json!({
                "ok": false,
                "exit_code": 1,
                // No row ever ran, which is itself the fact a consumer needs to
                // tell a setup failure from a run in which everything failed.
                "rows": 0,
                // Carried even here, so every `run_finished` has one shape and
                // a consumer can read the same keys whatever ended the run.
                "interrupted": false,
                "partial": false,
                "rows_completed": serde_json::Value::Null,
                "rows_planned": serde_json::Value::Null,
                "warnings": warnings,
                "skipped": [],
                "errors": errors,
            }),
        );
        1
    }

    /// Write one event as a single line.
    ///
    /// Formatted into a `String` and written in one call rather than
    /// `eprintln!`'d piecewise, because `PARALLEL` rows are announced from
    /// several threads at once and a line assembled in pieces could interleave
    /// with another's — which would corrupt exactly the property (one object per
    /// line) the whole transport rests on.
    fn emit(&self, event: &str, fields: serde_json::Value) {
        let Some(out) = &self.out else {
            return;
        };
        if self.finished.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        let mut obj = serde_json::Map::new();
        obj.insert("event".into(), serde_json::Value::String(event.into()));
        obj.insert("schema".into(), serde_json::json!(PROGRESS_SCHEMA));
        if let serde_json::Value::Object(map) = fields {
            for (k, v) in map {
                obj.insert(k, v);
            }
        }
        out(&format!("{}\n", serde_json::Value::Object(obj)));
    }

    /// Once, after the projection pass and before anything is sent: how many
    /// rows there will be and what the columns are called, so a front-end can
    /// draw the empty grid up front instead of growing it a row at a time.
    fn plan(
        &self,
        report_name: &str,
        total: usize,
        streamed: &[OutputColumn],
        withheld: &[OutputColumn],
        dry_run: bool,
    ) {
        self.emit(
            "plan",
            serde_json::json!({
                "report": report_name,
                "dry_run": dry_run,
                "total": total,
                "columns": streamed.iter().map(|c| c.header.clone()).collect::<Vec<_>>(),
                // Named, not silently dropped: a consumer that sees a column
                // missing from `row_completed` has to be able to tell "too big
                // to stream, read it from the report" from "this report has no
                // such column".
                "withheld_columns": withheld.iter().map(|c| c.header.clone()).collect::<Vec<_>>(),
            }),
        );
    }

    fn row_started(&self, path: &[(usize, usize)], slots: &RowSlots) {
        self.emit(
            "row_started",
            serde_json::json!({ "path": path_key(path), "row_index": slots.index_of(path) }),
        );
    }

    /// One finished row: its slot, its scalar cells, and whether it went wrong.
    fn row_completed(
        &self,
        row: &ReportRow,
        errors: &[String],
        columns: &[OutputColumn],
        no_match: &str,
        slots: &RowSlots,
    ) {
        if self.out.is_none() {
            return;
        }
        let mut cells = serde_json::Map::new();
        // Withheld, not shortened: a consumer told a value was "truncated"
        // could reasonably render the stub it was given, and there is no stub —
        // the cell is absent, to be read from the report. Same word as `plan`'s
        // `withheld_columns`, because it is the same mechanism.
        let mut withheld: Vec<String> = Vec::new();
        for col in columns {
            let value = col.value(row, no_match);
            if value.len() > PROGRESS_MAX_CELL {
                withheld.push(col.header.clone());
                continue;
            }
            cells.insert(col.header.clone(), serde_json::Value::String(value));
        }
        self.emit(
            "row_completed",
            serde_json::json!({
                "path": path_key(&row.path),
                "row_index": slots.index_of(&row.path),
                "ok": errors.is_empty(),
                "target": row.target,
                "cells": serde_json::Value::Object(cells),
                "errors": errors,
                "withheld": withheld,
            }),
        );
    }

    /// One output file as it lands — announced individually, and including the
    /// ones that failed, because a part-written set is left in place on purpose:
    /// the files already written are faithful renderings of a run that really
    /// happened, and a consumer needs to know which of them exist.
    fn output_written(&self, path: &str, format: &str, error: Option<&str>) {
        self.emit(
            "output_written",
            serde_json::json!({
                "path": path,
                "format": format,
                "ok": error.is_none(),
                "error": error,
            }),
        );
    }

    /// The last line of the stream: the run's verdict and the process's exit
    /// code.
    ///
    /// Last, *after* the `output_written` events, because a file that fails to
    /// write changes the exit code — emitting this at the end of the run proper
    /// would mean publishing a number the process then contradicts. A consumer
    /// therefore has one terminal event to wait for, and the code on it is the
    /// code it will get.
    fn run_finished(&self, result: &ReportResult, exit_code: i32, setup_warnings: &[String]) {
        // The validation warnings are folded in with the run's own: a consumer
        // asked what it should be told about this run, and "which phase raised
        // it" is not a distinction it can act on. In the human mode they were
        // printed as they were found, which is why they are carried rather than
        // re-derived here.
        let warnings: Vec<&String> = setup_warnings
            .iter()
            .chain(result.warnings.iter())
            .collect();
        self.emit(
            "run_finished",
            serde_json::json!({
                "ok": exit_code == 0,
                "exit_code": exit_code,
                "rows": result.rows.len(),
                // A stop is not a silent EOF. `interrupted` says the run was
                // told to stop; `partial` says the report it produced is
                // therefore short, with the counts to say by how much.
                "interrupted": exit_code == EXIT_INTERRUPTED,
                "partial": result.partial.is_some(),
                "rows_completed": result.partial.map(|p| p.rows_completed),
                "rows_planned": result.partial.map(|p| p.rows_planned),
                "warnings": warnings,
                "skipped": result.skipped,
                "errors": result.errors,
            }),
        );
        // After the emit, not before — this is the one event the latch must
        // not swallow.
        self.finished
            .store(true, std::sync::atomic::Ordering::Release);
    }

    /// The stop was heard and the run is winding down: no new rows will start,
    /// and the rows in flight have `grace` to finish before they are given up
    /// on.
    ///
    /// Emitted because a dashboard that sent `stop` and then saw nothing for
    /// forty seconds has no way to tell "winding down" from "ignored me" —
    /// and the answer decides whether a user presses the button again (which
    /// is the hard kill).
    fn run_stopping(&self, source: &str, grace: std::time::Duration) {
        self.emit(
            "run_stopping",
            serde_json::json!({
                "source": source,
                "grace_seconds": grace.as_secs(),
            }),
        );
    }
}

/// A grace period as it is spoken about in prose: `30`, but `0.25` rather than
/// a truncated `0` for the sub-second periods only a test sets today.
///
/// `--grace` takes whole seconds, so this is belt and braces — but the two
/// sentences it feeds ("did not wind down within {}s") read as a lie at any
/// duration under one, and a number that renders as zero is the worst kind.
fn grace_label(grace: std::time::Duration) -> String {
    match grace.subsec_nanos() {
        0 => grace.as_secs().to_string(),
        _ => format!("{}", grace.as_secs_f64()),
    }
}

/// What a run has produced so far, copied out of the row stream as it goes.
///
/// The insurance policy behind a stopped run: the fully assembled result only
/// exists when the run *returns*, so a run abandoned mid-flight would otherwise
/// have nothing to show. See [`abandoned_result`].
#[derive(Default)]
struct Harvest {
    /// Every completed row, in the order the run finished them (not report
    /// order — the writer sorts nothing, so these are laid back into the
    /// projected shape as they came).
    rows: Vec<ReportRow>,
    /// The per-row errors those rows raised, each kept beside the path of the
    /// row that raised it — the errors arrive in the same completion order as
    /// the rows, and are laid back into report order the same way.
    errors: Vec<(Vec<(usize, usize)>, String)>,
    /// How many rows completed — counted rather than taken from `rows.len()`
    /// so it keeps meaning the same thing if the rows are ever pruned.
    completed: usize,
}

/// How the waiting loop around a live run ended.
enum Ending {
    /// The run returned — on its own, or because a stop request wound it down
    /// inside the grace period.
    Ran(ReportResult),
    /// The grace period expired, and the partial report has already been
    /// written and announced from inside the run's scope (which is the only
    /// place it can be, since the scope joins the stragglers on the way out).
    Delivered(i32),
    /// The run thread died without sending. Only reachable if it panicked, and
    /// the scope re-raises that panic as it joins.
    Lost,
}

/// The report an abandoned run leaves behind: the projection's *shape* —
/// columns, statistics, ground truths, the no-match marker — filled with the
/// rows that actually finished.
///
/// Rebuilt from the outside like this because the run that would have assembled
/// it is still running and will never be heard from again. What is lost with it
/// is everything computed at the end from the whole run: images, ground-truth
/// verdicts, and so the metrics over them. That is the real difference between
/// a wind-down inside the grace period and one that overran it, and it is why
/// the grace period is worth having rather than stopping dead on the first
/// request for it.
fn abandoned_result(
    shape: Option<&ReportResult>,
    harvest: &Mutex<Harvest>,
    planned: usize,
    grace: std::time::Duration,
) -> ReportResult {
    let held = harvest.lock().unwrap_or_else(|e| e.into_inner());
    let mut result = shape.cloned().unwrap_or_default();
    result.partial = Some(Partial {
        rows_completed: held.completed,
        rows_planned: planned,
    });
    // Back into report order. The rows were harvested as they *finished*, so
    // under PARALLEL they arrive scrambled, and nothing downstream sorts them:
    // the writers lay rows down as they are given. Leaving them would break the
    // one property that makes PARALLEL trustworthy — that a report is the same
    // at any degree — at exactly the moment someone is squinting at it, and
    // would make two abandoned runs of the same corpus undiffable.
    //
    // Sorting by path *is* the canonical order: it is the same ordering
    // `RowSlots` assigns slot numbers in, so a `row_index` streamed during the
    // run still points at the row it names here.
    let mut rows = held.rows.clone();
    rows.sort_by(|a, b| a.path.cmp(&b.path));
    result.rows = rows;
    // The errors inherit the same scramble, and the ordinary path merges them
    // in plan order; a stable sort by path keeps one row's errors in the order
    // that row raised them.
    let mut errors = held.errors.clone();
    errors.sort_by(|a, b| a.0.cmp(&b.0));
    result.errors = errors.into_iter().map(|(_, e)| e).collect();
    // A warning rather than an error: nothing about the API under test went
    // wrong. It is here because "CLEANUP may not have run" is the one
    // consequence of this path that outlives the run — a leaked session or an
    // unreleased lock is someone else's problem in ten minutes' time.
    result.warnings = vec![format!(
        "the run was stopped and did not wind down within {}s: rows still in flight were \
         abandoned, and CLEANUP may not have run (check for leftover sessions or locks)",
        grace_label(grace)
    )];
    result.skipped = Vec::new();
    // Ground truths and images are assembled at the end of a run, so this
    // report has none: leaving the *configuration* for them in place would
    // render a scoring column with nothing scored and metric cards reading 0%,
    // which is a lie about the rows that did run.
    result.column_truths.clear();
    result.column_images.clear();
    result.truths.clear();
    result.verdicts.clear();
    result.images.clear();
    result.pending.clear();
    result
}

/// The projected grid: every row's structural path in canonical (sorted) order,
/// so a streamed row can be announced with the slot number it occupies as well
/// as its path.
///
/// The number is what makes the live grid and the finished report *explicitly*
/// linkable: a `-o out.json` report is a list of rows in this same canonical
/// order, and it carries no path of its own (the path is a run-time coordinate,
/// deliberately outside the exported model). Without the index a consumer would
/// have to re-derive the ordering rule to match the two, which is exactly the
/// kind of implicit contract that breaks silently.
///
/// The one report where the two part company is a comparison: `ENVS` baseline
/// and candidate rows are streamed separately and then *collapsed* into one row
/// each by `finalize`, so the report has fewer rows than the grid. The index
/// still identifies the slot — it is the projection's, which is what `plan`'s
/// `total` counts too — and `path` remains the identity either way.
struct RowSlots {
    order: HashMap<Vec<(usize, usize)>, usize>,
}

impl RowSlots {
    fn new(projected: &[ReportRow]) -> Self {
        let mut paths: Vec<Vec<(usize, usize)>> =
            projected.iter().map(|r| r.path.clone()).collect();
        paths.sort();
        RowSlots {
            order: paths.into_iter().enumerate().map(|(i, p)| (p, i)).collect(),
        }
    }

    /// The slot this path occupies, or `None` for a row the projection did not
    /// foresee (a snapshot row injected by a `FILE(…)` role, say) — reported as
    /// `null` rather than guessed at.
    fn index_of(&self, path: &[(usize, usize)]) -> Option<usize> {
        self.order.get(path).copied()
    }
}

/// A row's structural path as a dotted string — `[(0, 3), (1, 2)]` → `"0.3.1.2"`,
/// the empty path (a report with no loop) → `""`.
///
/// A string rather than the nested array it is, because it exists to be used as
/// a dictionary key on the other side: `updates[ev["path"]] = ev` needs no
/// normalisation, whereas a list of pairs has to be tupled first in every
/// language that reads this.
fn path_key(path: &[(usize, usize)]) -> String {
    path.iter()
        .map(|(node, iter)| format!("{node}.{iter}"))
        .collect::<Vec<_>>()
        .join(".")
}

/// Split the planned columns into the ones `row_completed` carries and the ones
/// it withholds.
///
/// `DETAIL` columns and `IMAGE` columns are withheld by declaration: the author
/// has already said this column is a drill-down or a picture, which is exactly
/// the content that is too big to repeat on stderr once per row while the
/// report file is being written with it anyway. Everything else is a scalar the
/// grid can show.
fn split_streamed_columns(columns: Vec<OutputColumn>) -> (Vec<OutputColumn>, Vec<OutputColumn>) {
    columns
        .into_iter()
        .partition(|c| !c.detail && c.image.is_none())
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
        report.path = Some(PathBuf::from("/reports/nightly.trail"));
        assert_eq!(
            derived_output_path(&report, "csv"),
            PathBuf::from("/reports/nightly.csv")
        );
    }

    #[test]
    fn derived_output_path_expands_time_token_next_to_report() {
        let mut report = Report::from_text("run_{time}", "# name: run_{time}\n");
        report.path = Some(PathBuf::from("/reports/nightly.trail"));
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
        let report = dir.join("r.trail");
        fs::write(
            &report,
            "# name: r\n# collection: api.hurl\n# columns: Ping.HttpStatus as Status\nREPORT REQUEST Ping\n",
        )
        .unwrap();
        let out = dir.join("out.csv");

        let code = run(
            Some(coll.to_string_lossy().into_owned()),
            Vec::new(),
            report.to_string_lossy().into_owned(),
            vec![out.to_string_lossy().into_owned()],
            true, // dry-run: no HTTP
            Vec::new(),
            None,
            ParamValues::new(),
            false,
            None,
            false,
        );
        assert_eq!(code, 0, "dry run should succeed");

        let csv = fs::read_to_string(&out).unwrap();
        let mut lines = csv.lines();
        assert_eq!(lines.next(), Some("Status"), "header row");
        // One projected row exists (the dry cell value is a placeholder).
        assert!(lines.next().is_some(), "one projected row expected");

        fs::remove_dir_all(&dir).ok();
    }

    /// The point of `--param` for a caller shelling out to PaperBoy: one
    /// report, pointed at a different folder per run, without editing the
    /// `.trail` or writing a throwaway `.vars` file. The supplied value has to
    /// beat the declared default and reach the producer path, which is what
    /// decides how many rows there are.
    #[test]
    fn a_supplied_param_repoints_a_folders_loop() {
        let dir = temp_dir("param");
        let coll = dir.join("api.hurl");
        fs::write(&coll, "# Ping\nGET https://example.test/ping\nHTTP *\n").unwrap();

        // The batch this run is about, next to a decoy the default points at.
        let batch = dir.join("batch-07");
        fs::create_dir_all(batch.join("case-a")).unwrap();
        fs::create_dir_all(batch.join("case-b")).unwrap();
        fs::create_dir_all(dir.join("empty")).unwrap();

        let report = dir.join("r.trail");
        fs::write(
            &report,
            "# name: r\n# collection: api.hurl\nPARAM FOLDER CASES = \"./empty\"\n\
             FOR CASE IN FOLDERS \"{{CASES}}\"\n    REPORT CASE\n    REPORT REQUEST Ping\nEND\n",
        )
        .unwrap();

        let run_with = |params: ParamValues, out: &Path| {
            run(
                Some(coll.to_string_lossy().into_owned()),
                Vec::new(),
                report.to_string_lossy().into_owned(),
                vec![out.to_string_lossy().into_owned()],
                true, // dry-run: the loop still expands, no HTTP
                Vec::new(),
                None,
                params,
                false,
                None,
                false,
            )
        };

        // The default is honoured when nothing is supplied: an empty folder,
        // so nothing to iterate.
        let default_out = dir.join("default.csv");
        assert_eq!(run_with(ParamValues::new(), &default_out), 0);
        let csv = fs::read_to_string(&default_out).unwrap();
        assert!(
            !csv.contains("case-a"),
            "the declared default should still point at ./empty:\n{csv}"
        );

        // …and is beaten by the value this run was given.
        let chosen_out = dir.join("chosen.csv");
        let mut params = ParamValues::new();
        params.insert("CASES".into(), batch.to_string_lossy().into_owned());
        assert_eq!(run_with(params, &chosen_out), 0);
        let csv = fs::read_to_string(&chosen_out).unwrap();
        assert!(
            csv.contains("case-a") && csv.contains("case-b"),
            "both cases from the supplied folder expected:\n{csv}"
        );

        fs::remove_dir_all(&dir).ok();
    }

    /// A `--param` the report doesn't declare is a caller whose command line
    /// has drifted from the script. Running anyway would produce a full report
    /// built from the default it believed it had replaced, so it stops.
    #[test]
    fn a_param_the_report_does_not_declare_is_refused() {
        let dir = temp_dir("badparam");
        let coll = dir.join("api.hurl");
        fs::write(&coll, "# Ping\nGET https://example.test/ping\nHTTP *\n").unwrap();
        let report = dir.join("r.trail");
        fs::write(
            &report,
            "# name: r\n# collection: api.hurl\nPARAM FOLDER CASES = \"./empty\"\n\
             # columns: Ping.HttpStatus as Status\nREPORT REQUEST Ping\n",
        )
        .unwrap();
        let out = dir.join("out.csv");

        let mut params = ParamValues::new();
        params.insert("CASE_DIR".into(), "./whatever".into());
        let code = run(
            Some(coll.to_string_lossy().into_owned()),
            Vec::new(),
            report.to_string_lossy().into_owned(),
            vec![out.to_string_lossy().into_owned()],
            true,
            Vec::new(),
            None,
            params,
            false,
            None,
            false,
        );
        assert_eq!(code, 1, "an undeclared parameter is a setup error");
        assert!(!out.exists(), "nothing should be written for a refused run");

        fs::remove_dir_all(&dir).ok();
    }

    /// The point of a repeatable `-o` for an application embedding PaperBoy:
    /// one run of the requests yields a rendering to show a user *and* a
    /// structure to parse, instead of running the whole report twice and
    /// hoping the two runs agree.
    #[test]
    fn one_run_writes_every_requested_format() {
        let dir = temp_dir("multiout");
        let coll = dir.join("api.hurl");
        fs::write(&coll, "# Ping\nGET https://example.test/ping\nHTTP *\n").unwrap();
        let report = dir.join("r.trail");
        fs::write(
            &report,
            "# name: r\n# collection: api.hurl\n# columns: Ping.HttpStatus as Status\n\
             REPORT REQUEST Ping\n",
        )
        .unwrap();

        let html = dir.join("out.html");
        let json = dir.join("out.json");
        let csv = dir.join("out.csv");
        let code = run(
            Some(coll.to_string_lossy().into_owned()),
            Vec::new(),
            report.to_string_lossy().into_owned(),
            vec![
                html.to_string_lossy().into_owned(),
                json.to_string_lossy().into_owned(),
                csv.to_string_lossy().into_owned(),
            ],
            true, // dry-run: no HTTP
            Vec::new(),
            None,
            ParamValues::new(),
            false,
            None,
            false,
        );
        assert_eq!(code, 0, "a multi-output dry run should succeed");

        // Each file exists and is actually in its own format, so the extension
        // picked the writer rather than one format being written three times.
        let html_text = fs::read_to_string(&html).unwrap();
        assert!(html_text.contains("<table"), "not HTML:\n{html_text}");
        let json_text = fs::read_to_string(&json).unwrap();
        assert!(
            serde_json::from_str::<serde_json::Value>(&json_text).is_ok(),
            "not JSON:\n{json_text}"
        );
        let csv_text = fs::read_to_string(&csv).unwrap();
        assert!(csv_text.starts_with("Status"), "not CSV:\n{csv_text}");

        fs::remove_dir_all(&dir).ok();
    }

    /// The `-o` combinations that cannot mean what they say, refused before a
    /// single request goes out — a typo in a format should not cost a whole
    /// run of live traffic to discover.
    #[test]
    fn impossible_output_combinations_are_refused_before_the_run() {
        let dir = temp_dir("badout");
        let coll = dir.join("api.hurl");
        fs::write(&coll, "# Ping\nGET https://example.test/ping\nHTTP *\n").unwrap();
        let report = dir.join("r.trail");
        fs::write(
            &report,
            "# name: r\n# collection: api.hurl\n# columns: Ping.HttpStatus as Status\n\
             REPORT REQUEST Ping\n",
        )
        .unwrap();

        let go = |outs: Vec<String>| {
            run(
                Some(coll.to_string_lossy().into_owned()),
                Vec::new(),
                report.to_string_lossy().into_owned(),
                outs,
                true,
                Vec::new(),
                None,
                ParamValues::new(),
                false,
                None,
                false,
            )
        };

        // There is only one stdout, and two formats down it would interleave
        // into neither of them.
        assert_eq!(go(vec!["-".into(), "-".into()]), 1, "two stdouts");

        // The same path twice means the second write destroys the first.
        let dup = dir.join("out.json").to_string_lossy().into_owned();
        assert_eq!(go(vec![dup.clone(), dup.clone()]), 1, "duplicate path");
        assert!(
            !dir.join("out.json").exists(),
            "a refused run writes nothing"
        );

        // A format PaperTrail can't write, alongside one it can: the good one
        // must not be written either, or a caller gets a partial answer from a
        // command line that was rejected.
        let good = dir.join("fine.csv");
        assert_eq!(
            go(vec![
                good.to_string_lossy().into_owned(),
                dir.join("out.docx").to_string_lossy().into_owned(),
            ]),
            1,
            "unsupported extension"
        );
        assert!(!good.exists(), "nothing is written for a refused run");

        fs::remove_dir_all(&dir).ok();
    }

    /// `-o -` mixed with files is the shape an integrator actually uses: pipe
    /// one format onward while keeping another on disk.
    #[test]
    fn stdout_and_a_file_can_be_asked_for_together() {
        let dir = temp_dir("mixedout");
        let coll = dir.join("api.hurl");
        fs::write(&coll, "# Ping\nGET https://example.test/ping\nHTTP *\n").unwrap();
        let report = dir.join("r.trail");
        fs::write(
            &report,
            "# name: r\n# collection: api.hurl\n# columns: Ping.HttpStatus as Status\n\
             REPORT REQUEST Ping\n",
        )
        .unwrap();
        let json = dir.join("out.json");
        let code = run(
            Some(coll.to_string_lossy().into_owned()),
            Vec::new(),
            report.to_string_lossy().into_owned(),
            vec!["-".into(), json.to_string_lossy().into_owned()],
            true,
            Vec::new(),
            None,
            ParamValues::new(),
            false,
            None,
            false,
        );
        assert_eq!(code, 0);
        assert!(json.exists(), "the file output still lands");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn multi_env_loads_every_env_and_runs_the_envs_loop() {
        // Two `-e` files with distinct stems make a `FOR … IN ENVS` loop
        // resolvable headlessly: both environments load, are selectable by
        // stem, and the flow iterates once per environment.
        let dir = temp_dir("multienv");
        let coll = dir.join("api.hurl");
        fs::write(&coll, "# Ping\nGET https://example.test/ping\nHTTP *\n").unwrap();
        fs::write(dir.join("prod.vars"), "HOST=prod.test\n").unwrap();
        fs::write(dir.join("staging.vars"), "HOST=staging.test\n").unwrap();
        let report = dir.join("r.trail");
        fs::write(
            &report,
            "# name: r\n# collection: api.hurl\nFOR TARGET IN ENVS \"prod\", \"staging\"\n    REPORT TARGET\n    REPORT REQUEST Ping\nEND\n",
        )
        .unwrap();
        let out = dir.join("out.csv");

        let code = run(
            Some(coll.to_string_lossy().into_owned()),
            vec![
                dir.join("prod.vars").to_string_lossy().into_owned(),
                dir.join("staging.vars").to_string_lossy().into_owned(),
            ],
            report.to_string_lossy().into_owned(),
            vec![out.to_string_lossy().into_owned()],
            true, // dry-run: no HTTP, but the ENVS loop still expands per env
            Vec::new(),
            None,
            ParamValues::new(),
            false,
            None,
            false,
        );
        assert_eq!(code, 0, "a multi-env dry run should succeed");

        let csv = fs::read_to_string(&out).unwrap();
        // The ENVS loop iterated once per loaded environment (no "not loaded"
        // errors), so both env names appear in the reported TARGET column.
        assert!(csv.contains("prod"), "prod env row missing:\n{csv}");
        assert!(csv.contains("staging"), "staging env row missing:\n{csv}");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn duplicate_env_stem_is_rejected() {
        // Two `-e` files that share a stem are ambiguous for an ENVS clause, so
        // the second is a fatal setup error.
        let dir = temp_dir("dupenv");
        let coll = dir.join("api.hurl");
        fs::write(&coll, "# Ping\nGET https://example.test/ping\nHTTP *\n").unwrap();
        let a = dir.join("a");
        let b = dir.join("b");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(a.join("prod.vars"), "HOST=a.test\n").unwrap();
        fs::write(b.join("prod.vars"), "HOST=b.test\n").unwrap();
        let report = dir.join("r.trail");
        fs::write(
            &report,
            "# name: r\n# collection: api.hurl\nREPORT REQUEST Ping\n",
        )
        .unwrap();

        let code = run(
            Some(coll.to_string_lossy().into_owned()),
            vec![
                a.join("prod.vars").to_string_lossy().into_owned(),
                b.join("prod.vars").to_string_lossy().into_owned(),
            ],
            report.to_string_lossy().into_owned(),
            vec!["-".to_string()],
            true,
            Vec::new(),
            None,
            ParamValues::new(),
            false,
            None,
            false,
        );
        assert_eq!(code, 1, "a duplicate env stem is a fatal setup error");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_collection_is_a_setup_error() {
        let dir = temp_dir("nocoll");
        let report = dir.join("r.trail");
        fs::write(
            &report,
            "# name: r\n# collection: missing.hurl\nREPORT REQUEST Ping\n",
        )
        .unwrap();

        let code = run(
            Some(dir.join("missing.hurl").to_string_lossy().into_owned()),
            Vec::new(),
            report.to_string_lossy().into_owned(),
            vec!["-".to_string()],
            true,
            Vec::new(),
            None,
            ParamValues::new(),
            false,
            None,
            false,
        );
        assert_eq!(code, 1, "a missing collection is a fatal setup error");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unsupported_output_extension_is_rejected() {
        let dir = temp_dir("badext");
        let coll = dir.join("api.hurl");
        fs::write(&coll, "# Ping\nGET https://example.test/ping\nHTTP *\n").unwrap();
        let report = dir.join("r.trail");
        fs::write(
            &report,
            "# name: r\n# collection: api.hurl\nREPORT REQUEST Ping\n",
        )
        .unwrap();

        let code = run(
            Some(coll.to_string_lossy().into_owned()),
            Vec::new(),
            report.to_string_lossy().into_owned(),
            vec![dir.join("out.docx").to_string_lossy().into_owned()],
            true,
            Vec::new(),
            None,
            ParamValues::new(),
            false,
            None,
            false,
        );
        assert_eq!(code, 1, "an unsupported extension should fail");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn dry_run_writes_each_supported_output_format() {
        let dir = temp_dir("fmts");
        let coll = dir.join("api.hurl");
        fs::write(&coll, "# Ping\nGET https://example.test/ping\nHTTP *\n").unwrap();
        let report = dir.join("r.trail");
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
            (
                "pdf",
                &(|b: &[u8]| b.starts_with(b"%PDF-")) as &dyn Fn(&[u8]) -> bool,
            ),
        ] {
            let out = dir.join(format!("out.{ext}"));
            let code = run(
                Some(coll.to_string_lossy().into_owned()),
                Vec::new(),
                report.to_string_lossy().into_owned(),
                vec![out.to_string_lossy().into_owned()],
                true, // dry-run: no HTTP
                Vec::new(),
                None,
                ParamValues::new(),
                false,
                None,
                false,
            );
            assert_eq!(code, 0, ".{ext} output should succeed");
            let bytes = fs::read(&out).unwrap();
            assert!(!bytes.is_empty(), ".{ext} is non-empty");
            assert!(check(&bytes), ".{ext} has the expected magic/shape");
        }

        fs::remove_dir_all(&dir).ok();
    }

    /// With neither `-c` nor `-e`, the report's own `# collection:` and
    /// `# environment:` headers are honoured, resolved relative to the report's
    /// folder — so a workspace report "just runs" with `paperboy -r report`.
    #[test]
    fn headers_supply_collection_and_environment_when_flags_omitted() {
        let dir = temp_dir("hdrres");
        // Put the report in a sub-folder to prove the header paths resolve
        // relative to the report, not the process CWD.
        let sub = dir.join("reports");
        fs::create_dir_all(&sub).unwrap();
        fs::write(
            dir.join("api.hurl"),
            "# Ping\nGET https://example.test/ping\nHTTP *\n",
        )
        .unwrap();
        fs::write(dir.join("prod.vars"), "HOST=prod.test\n").unwrap();
        let report = sub.join("r.trail");
        fs::write(
            &report,
            "# name: r\n# collection: ../api.hurl\n# environment: ../prod.vars\nREPORT HOST\nREPORT REQUEST Ping\n",
        )
        .unwrap();
        let out = dir.join("out.csv");

        let code = run(
            None,       // no -c → header's `# collection:` is used
            Vec::new(), // no -e → header's `# environment:` is used
            report.to_string_lossy().into_owned(),
            vec![out.to_string_lossy().into_owned()],
            true, // dry-run: no HTTP
            Vec::new(),
            None,
            ParamValues::new(),
            false,
            None,
            false,
        );
        assert_eq!(code, 0, "header-resolved run should succeed");

        let csv = fs::read_to_string(&out).unwrap();
        // The environment loaded (HOST from prod.vars is in the projection).
        assert!(csv.contains("prod.test"), "env not applied:\n{csv}");

        fs::remove_dir_all(&dir).ok();
    }

    /// With no `-c` and no `# collection:` header there is nothing to run
    /// against — a clear, fatal setup error.
    #[test]
    fn missing_collection_and_no_header_is_a_setup_error() {
        let dir = temp_dir("nohdr");
        let report = dir.join("r.trail");
        fs::write(&report, "# name: r\nREPORT REQUEST Ping\n").unwrap();

        let code = run(
            None,
            Vec::new(),
            report.to_string_lossy().into_owned(),
            vec!["-".to_string()],
            true,
            Vec::new(),
            None,
            ParamValues::new(),
            false,
            None,
            false,
        );
        assert_eq!(
            code, 1,
            "no collection flag and no header is a fatal setup error"
        );

        fs::remove_dir_all(&dir).ok();
    }

    // --- --progress-json --------------------------------------------------

    /// Collect the stream into lines, as a consumer reading `proc.stderr`
    /// would.
    fn capturing() -> (Progress, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = std::sync::Arc::clone(&log);
        (
            Progress {
                finished: std::sync::atomic::AtomicBool::new(false),
                out: Some(Box::new(move |line: &str| {
                    sink.lock().unwrap().push(line.to_string())
                })),
            },
            log,
        )
    }

    fn col(header: &str) -> OutputColumn {
        OutputColumn {
            header: header.to_string(),
            sources: vec![header.to_string()],
            stats: Vec::new(),
            image: None,
            truth: None,
            detail: false,
        }
    }

    fn row(path: Vec<(usize, usize)>, cells: &[(&str, &str)]) -> ReportRow {
        ReportRow {
            cells: cells
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            path,
            ..ReportRow::default()
        }
    }

    /// The transport's whole contract in one test: every event is a complete
    /// JSON object on a line of its own, ending in a newline, and every one of
    /// them carries the schema version — including the ones a consumer that
    /// attached late is the first to see.
    #[test]
    fn every_progress_event_is_one_versioned_json_object_per_line() {
        let (progress, log) = capturing();
        progress.plan("nightly", 2, &[col("Status")], &[col("Body")], false);
        progress.row_started(&[(0, 1)], &RowSlots::new(&[]));
        progress.row_completed(
            &row(vec![(0, 1)], &[("Status", "200")]),
            &[],
            &[col("Status")],
            "",
            &RowSlots::new(&[]),
        );
        progress.output_written("out.json", "json", None);
        progress.run_finished(&ReportResult::default(), 0, &[]);

        let lines = log.lock().unwrap().clone();
        let events: Vec<String> = lines
            .iter()
            .map(|l| {
                assert!(l.ends_with('\n'), "each event is its own line: {l:?}");
                assert_eq!(l.matches('\n').count(), 1, "one line per event: {l:?}");
                let v: serde_json::Value = serde_json::from_str(l).expect("a line is one object");
                assert_eq!(v["schema"], serde_json::json!(PROGRESS_SCHEMA));
                v["event"].as_str().unwrap().to_string()
            })
            .collect();
        assert_eq!(
            events,
            [
                "plan",
                "row_started",
                "row_completed",
                "output_written",
                "run_finished"
            ]
        );
    }

    /// `path` is the join key, so it has to be the *same* key everywhere: the
    /// dotted flattening of the row's structural path, usable as a dictionary
    /// key without any normalisation on the consumer's side.
    #[test]
    fn a_rows_path_is_a_dotted_string_on_every_event() {
        assert_eq!(path_key(&[]), "");
        assert_eq!(path_key(&[(0, 3)]), "0.3");
        assert_eq!(path_key(&[(0, 3), (1, 2)]), "0.3.1.2");

        let (progress, log) = capturing();
        // Both events name the same row, so both must agree on its identity
        // *and* on the slot it occupies in the projected grid.
        let slots = RowSlots::new(&[
            row(vec![(0, 0)], &[]),
            row(vec![(0, 3), (1, 2)], &[]),
            row(vec![(9, 9)], &[]),
        ]);
        progress.row_started(&[(0, 3), (1, 2)], &slots);
        progress.row_completed(&row(vec![(0, 3), (1, 2)], &[]), &[], &[], "", &slots);
        for line in log.lock().unwrap().iter() {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(v["path"], serde_json::json!("0.3.1.2"), "{line}");
            assert_eq!(v["row_index"], serde_json::json!(1), "{line}");
        }
        // A row the projection never saw is reported as unplaced rather than
        // guessed at.
        assert_eq!(slots.index_of(&[(7, 7)]), None);
    }

    /// A row that failed has to be distinguishable from one that merely
    /// finished — the result-level error list says *that* something went wrong
    /// but not *which slot* to paint red.
    #[test]
    fn row_completed_carries_the_errors_that_row_raised() {
        let (progress, log) = capturing();
        progress.row_completed(
            &row(vec![(0, 0)], &[("Status", "500")]),
            &["Ping: connection refused".to_string()],
            &[col("Status")],
            "",
            &RowSlots::new(&[]),
        );
        let v: serde_json::Value = serde_json::from_str(&log.lock().unwrap()[0]).unwrap();
        assert_eq!(v["ok"], serde_json::json!(false));
        assert_eq!(v["errors"], serde_json::json!(["Ping: connection refused"]));
    }

    /// Progress must stay cheap. A `DETAIL` or `IMAGE` column is the author
    /// saying "this one is big", and the same bytes are already going to the
    /// report file — so they are named in `plan` as withheld and left out of
    /// every row, to be joined from the report by `path` at the end.
    #[test]
    fn detail_and_image_columns_are_withheld_from_the_stream() {
        let mut detail = col("Raw: Response");
        detail.detail = true;
        let mut picture = col("Shot");
        picture.image = Some(crate::report::flow::ImageSpec::default());
        let (streamed, withheld) = split_streamed_columns(vec![col("Status"), detail, picture]);
        assert_eq!(
            streamed
                .iter()
                .map(|c| c.header.as_str())
                .collect::<Vec<_>>(),
            ["Status"]
        );
        assert_eq!(
            withheld
                .iter()
                .map(|c| c.header.as_str())
                .collect::<Vec<_>>(),
            ["Raw: Response", "Shot"]
        );

        let (progress, log) = capturing();
        progress.plan("r", 1, &streamed, &withheld, false);
        let v: serde_json::Value = serde_json::from_str(&log.lock().unwrap()[0]).unwrap();
        assert_eq!(v["columns"], serde_json::json!(["Status"]));
        assert_eq!(
            v["withheld_columns"],
            serde_json::json!(["Raw: Response", "Shot"])
        );
    }

    /// The same protection for a column nobody flagged: a value big enough to
    /// be a response body is named rather than streamed, so a
    /// consumer knows to read that cell from the report instead of trusting a
    /// silently shortened one.
    #[test]
    fn an_oversized_cell_is_named_rather_than_streamed() {
        let big = "x".repeat(PROGRESS_MAX_CELL + 1);
        let (progress, log) = capturing();
        progress.row_completed(
            &row(vec![(0, 0)], &[("Status", "200"), ("Body", &big)]),
            &[],
            &[col("Status"), col("Body")],
            "",
            &RowSlots::new(&[]),
        );
        let v: serde_json::Value = serde_json::from_str(&log.lock().unwrap()[0]).unwrap();
        assert_eq!(v["cells"]["Status"], serde_json::json!("200"));
        assert!(
            v["cells"].get("Body").is_none(),
            "the giant cell is omitted"
        );
        assert_eq!(v["withheld"], serde_json::json!(["Body"]));
    }

    /// Nothing is emitted at all without the flag — the stream is opt-in, and a
    /// run that didn't ask for it must not find JSON on its stderr.
    #[test]
    fn without_the_flag_no_events_are_emitted() {
        let progress = Progress::new(false);
        assert!(progress.out.is_none());
        // Exercising the emitters must be a no-op rather than a panic.
        progress.plan("r", 1, &[col("Status")], &[], false);
        progress.row_started(&[(0, 0)], &RowSlots::new(&[]));
        progress.row_completed(
            &row(vec![(0, 0)], &[]),
            &[],
            &[col("Status")],
            "",
            &RowSlots::new(&[]),
        );
        progress.run_finished(&ReportResult::default(), 0, &[]);
    }

    /// The flag is wired all the way through and changes nothing about the
    /// report itself: the same run, with the same output, and the same exit
    /// code.
    #[test]
    fn progress_json_does_not_disturb_the_report() {
        let dir = temp_dir("progress");
        let coll = dir.join("api.hurl");
        fs::write(&coll, "# Ping\nGET https://example.test/ping\nHTTP *\n").unwrap();
        let report = dir.join("r.trail");
        fs::write(
            &report,
            "# name: r\n# collection: api.hurl\n# columns: Ping.HttpStatus as Status\nREPORT REQUEST Ping\n",
        )
        .unwrap();
        let out = dir.join("out.csv");

        let code = run(
            Some(coll.to_string_lossy().into_owned()),
            Vec::new(),
            report.to_string_lossy().into_owned(),
            vec![out.to_string_lossy().into_owned()],
            true, // dry-run: no HTTP
            Vec::new(),
            None,
            ParamValues::new(),
            true, // --progress-json
            None,
            false,
        );
        assert_eq!(code, 0);
        let csv = fs::read_to_string(&out).unwrap();
        assert_eq!(csv.lines().next(), Some("Status"), "header row");

        fs::remove_dir_all(&dir).ok();
    }

    /// Parse a captured stream into `(event name, object)` pairs.
    fn events(log: &std::sync::Arc<std::sync::Mutex<Vec<String>>>) -> Vec<serde_json::Value> {
        log.lock()
            .unwrap()
            .iter()
            .map(|l| serde_json::from_str(l).expect("every line is one JSON object"))
            .collect()
    }

    /// The load-bearing claim behind `row_index`: the slot a row is announced
    /// in is the row it occupies in the written report. Asserted against the
    /// real writer rather than against the ordering rule, because the rule is
    /// only worth anything if the two agree — and a `row_index` that quietly
    /// points at the wrong row is worse than none at all, since it looks like
    /// it works.
    #[test]
    fn a_streamed_rows_index_is_its_position_in_the_written_report() {
        let dir = temp_dir("slots");
        let coll = dir.join("api.hurl");
        fs::write(&coll, "# Ping\nGET https://example.test/ping\nHTTP *\n").unwrap();
        let report = dir.join("r.trail");
        // Nested loops, so the paths are two pairs deep and their order is a
        // real question rather than 0, 1, 2.
        fs::write(
            &report,
            "# name: r\n# collection: api.hurl\n# columns: X, Y\n\
             FOR X IN [\"a\", \"b\", \"c\"]\n    FOR Y IN [\"1\", \"2\"]\n        REPORT REQUEST Ping\n    END\nEND\n",
        )
        .unwrap();
        let out = dir.join("out.json");

        let (progress, log) = capturing();
        let code = run_with_progress(
            Some(coll.to_string_lossy().into_owned()),
            Vec::new(),
            report.to_string_lossy().into_owned(),
            vec![out.to_string_lossy().into_owned()],
            true, // dry-run: the expansion is what the ordering claim is about
            Vec::new(),
            None,
            ParamValues::new(),
            progress,
            Control::for_test(),
        );
        assert_eq!(code, 0);

        let written: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&out).unwrap()).unwrap();
        let rows = written["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 6, "three by two");

        let streamed: Vec<serde_json::Value> = events(&log)
            .into_iter()
            .filter(|e| e["event"] == "row_completed")
            .collect();
        assert_eq!(streamed.len(), rows.len(), "one event per written row");
        // `plan` promised this many, before any of them existed.
        let plan = events(&log)
            .into_iter()
            .find(|e| e["event"] == "plan")
            .unwrap();
        assert_eq!(plan["total"], serde_json::json!(6));

        let mut seen: Vec<usize> = Vec::new();
        for ev in &streamed {
            let index = ev["row_index"].as_u64().expect("a projected row is placed") as usize;
            seen.push(index);
            let written_row = &rows[index];
            for key in ["X", "Y"] {
                assert_eq!(
                    ev["cells"][key], written_row[key],
                    "row {index} disagrees on {key}: streamed {ev}, written {written_row}"
                );
            }
        }
        seen.sort();
        assert_eq!(
            seen,
            (0..rows.len()).collect::<Vec<_>>(),
            "every slot filled once"
        );

        fs::remove_dir_all(&dir).ok();
    }

    /// A setup failure is the most likely thing a caller will hit — a mistyped
    /// `--param`, a folder that isn't there — and it used to leave the stream
    /// empty: exit 1, no events, and nothing to show anyone. `run_finished` is
    /// the terminal event unconditionally, so a consumer has one thing to wait
    /// for and one place to read the reason.
    #[test]
    fn a_setup_failure_still_ends_the_stream_with_run_finished() {
        let dir = temp_dir("setupfail");
        let coll = dir.join("api.hurl");
        fs::write(&coll, "# Ping\nGET https://example.test/ping\nHTTP *\n").unwrap();
        let report = dir.join("r.trail");
        fs::write(
            &report,
            "# name: r\n# collection: api.hurl\nPARAM TEXT WHO = \"world\"\nREPORT REQUEST Ping\n",
        )
        .unwrap();
        let mut params = ParamValues::new();
        params.insert("WHOM".into(), "nobody".into());

        let (progress, log) = capturing();
        let code = run_with_progress(
            Some(coll.to_string_lossy().into_owned()),
            Vec::new(),
            report.to_string_lossy().into_owned(),
            vec![dir.join("out.csv").to_string_lossy().into_owned()],
            true,
            Vec::new(),
            None,
            params,
            progress,
            Control::for_test(),
        );
        assert_eq!(code, 1, "an undeclared parameter is a setup error");

        let events = events(&log);
        assert_eq!(
            events.len(),
            1,
            "nothing ran, so only the verdict: {events:?}"
        );
        let last = &events[0];
        assert_eq!(last["event"], "run_finished");
        assert_eq!(last["exit_code"], serde_json::json!(1));
        assert_eq!(last["ok"], serde_json::json!(false));
        // Zero rows is itself the fact that tells a setup failure from a run in
        // which every row failed.
        assert_eq!(last["rows"], serde_json::json!(0));
        assert!(
            last["errors"][0]
                .as_str()
                .unwrap()
                .contains("no parameter named 'WHOM'"),
            "the reason has to be *in* the stream: {last}"
        );

        fs::remove_dir_all(&dir).ok();
    }

    /// A validation error is the other common setup failure, and it carries
    /// more than one message: every diagnostic reaches the stream, not just the
    /// summary line the human mode ends on.
    #[test]
    fn a_validation_failure_puts_every_diagnostic_in_the_stream() {
        let dir = temp_dir("validfail");
        let coll = dir.join("api.hurl");
        fs::write(&coll, "# Ping\nGET https://example.test/ping\nHTTP *\n").unwrap();
        let report = dir.join("r.trail");
        fs::write(
            &report,
            "# name: r\n# collection: api.hurl\nREPORT REQUEST Ghost\n",
        )
        .unwrap();

        let (progress, log) = capturing();
        let code = run_with_progress(
            Some(coll.to_string_lossy().into_owned()),
            Vec::new(),
            report.to_string_lossy().into_owned(),
            vec![dir.join("out.csv").to_string_lossy().into_owned()],
            false, // a live run is what validation blocks
            Vec::new(),
            None,
            ParamValues::new(),
            progress,
            Control::for_test(),
        );
        assert_eq!(code, 1);

        let events = events(&log);
        assert_eq!(events.len(), 1, "blocked before the plan: {events:?}");
        let errors = events[0]["errors"].as_array().unwrap();
        assert!(
            errors.iter().any(|e| e.as_str().unwrap().contains("Ghost")),
            "the diagnostic itself, not just the summary: {errors:?}"
        );
        assert!(
            errors
                .iter()
                .any(|e| e.as_str().unwrap().contains("validation errors")),
            "{errors:?}"
        );

        fs::remove_dir_all(&dir).ok();
    }

    // --- stopping a run ---------------------------------------------------

    /// A local server that flips `cancel` once it is about to answer request
    /// number `stop_after`, and holds every later request for `straggler_ms`.
    ///
    /// The switch is thrown *before* the response goes out, which is what makes
    /// the sequential case exact: the run cannot finish row `stop_after` — let
    /// alone claim the next one — until it has read a response the server only
    /// writes after stopping it.
    /// `row_delays[i]` holds back the response to row `ri` by that many
    /// milliseconds, which is how a test decides the order rows *finish* in
    /// under `PARALLEL` — the property the report is then expected not to
    /// inherit. Empty leaves every row answered as fast as it can be.
    fn stopping_server(
        cancel: std::sync::Arc<Cancel>,
        stop_after: usize,
        straggler_ms: u64,
        row_delays: Vec<u64>,
    ) -> u16 {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        std::thread::spawn(move || {
            // One thread per connection: under PARALLEL there is more than one
            // in flight at a time, and serialising them here would hide exactly
            // the case these tests are about.
            while let Ok((mut sock, _)) = listener.accept() {
                let (cancel, seen, row_delays) = (cancel.clone(), seen.clone(), row_delays.clone());
                std::thread::spawn(move || {
                    let mut buf = [0u8; 2048];
                    let read = sock.read(&mut buf).unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..read]).into_owned();
                    let n = seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    if n >= stop_after {
                        cancel.stop();
                    }
                    // Which row asked is in the path (`GET /ping/r3`), so a
                    // delay can be pinned to a row rather than to arrival
                    // order — under PARALLEL the requests arrive together.
                    let row = req
                        .split("/ping/r")
                        .nth(1)
                        .and_then(|rest| rest.split_whitespace().next())
                        .and_then(|i| i.parse::<usize>().ok());
                    match row.and_then(|i| row_delays.get(i).copied()) {
                        Some(ms) => std::thread::sleep(std::time::Duration::from_millis(ms)),
                        None if n > stop_after && straggler_ms > 0 => {
                            std::thread::sleep(std::time::Duration::from_millis(straggler_ms))
                        }
                        None => {}
                    }
                    let body = "{\"ok\":true}";
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = sock.write_all(resp.as_bytes());
                    let _ = sock.flush();
                });
            }
        });
        port
    }

    /// Write a collection and a report that loops `rows` times over one request
    /// against `port`, and return the two paths.
    fn stop_fixture(
        dir: &Path,
        port: u16,
        rows: usize,
        parallel: Option<usize>,
    ) -> (PathBuf, PathBuf) {
        let coll = dir.join("api.hurl");
        fs::write(
            &coll,
            format!("# Ping\nGET http://127.0.0.1:{port}/ping/{{{{X}}}}\nHTTP *\n"),
        )
        .unwrap();
        let items: Vec<String> = (0..rows).map(|i| format!("\"r{i}\"")).collect();
        let head = match parallel {
            Some(n) => format!("PARALLEL({n}) FOR"),
            None => "FOR".to_string(),
        };
        let report = dir.join("r.trail");
        fs::write(
            &report,
            format!(
                "# name: r\n# collection: api.hurl\n# columns: X, Ping.HttpStatus as Status\n\
                 {head} X IN [{}]\n    REPORT REQUEST Ping\nEND\n",
                items.join(", ")
            ),
        )
        .unwrap();
        (coll, report)
    }

    /// The whole promise of a stop: the rows that finished are written, the
    /// report says it is short and by how much, and the exit code says the run
    /// was told to stop rather than that the API is broken.
    #[test]
    fn a_stopped_run_writes_the_rows_that_finished_and_says_it_is_partial() {
        let dir = temp_dir("stop");
        let control = Control::for_test();
        let port = stopping_server(control.switch(), 2, 0, Vec::new());
        let (coll, report) = stop_fixture(&dir, port, 6, None);
        let out = dir.join("out.json");

        let (progress, log) = capturing();
        let code = run_with_progress(
            Some(coll.to_string_lossy().into_owned()),
            Vec::new(),
            report.to_string_lossy().into_owned(),
            vec![out.to_string_lossy().into_owned()],
            false,
            Vec::new(),
            None,
            ParamValues::new(),
            progress,
            control,
        );
        assert_eq!(
            code, EXIT_INTERRUPTED,
            "\"I was told to stop\" has a code of its own"
        );

        let written: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&out).unwrap()).unwrap();
        assert_eq!(written["partial"], serde_json::json!(true));
        assert_eq!(written["rows_completed"], serde_json::json!(2));
        assert_eq!(written["rows_planned"], serde_json::json!(6));
        assert_eq!(
            written["rows"].as_array().unwrap().len(),
            2,
            "only the rows that really ran: {written}"
        );

        let events = events(&log);
        let stopping = events.iter().find(|e| e["event"] == "run_stopping");
        assert!(
            stopping.is_some(),
            "the stream says it is winding down: {events:?}"
        );
        let finished = events
            .iter()
            .find(|e| e["event"] == "run_finished")
            .expect("a terminal event, always");
        assert_eq!(finished["interrupted"], serde_json::json!(true));
        assert_eq!(finished["partial"], serde_json::json!(true));
        assert_eq!(finished["rows_completed"], serde_json::json!(2));
        assert_eq!(finished["rows_planned"], serde_json::json!(6));
        assert_eq!(finished["exit_code"], serde_json::json!(EXIT_INTERRUPTED));
        assert!(
            events.iter().any(|e| e["event"] == "output_written"),
            "and the output it announced was written before it: {events:?}"
        );

        fs::remove_dir_all(&dir).ok();
    }

    /// The grace period expiring is not an excuse to produce nothing: what the
    /// stream saw is written, marked partial, with the caveat that the rows
    /// still in flight — and `CLEANUP` — were left behind.
    #[test]
    fn a_run_given_up_on_still_writes_what_it_had() {
        let dir = temp_dir("abandon");
        // Zero grace: the first straggler is given up on immediately, which is
        // the same code path as a 30-second wait, minus the wait.
        let control = Control::for_test().with_grace(std::time::Duration::ZERO);
        let port = stopping_server(control.switch(), 1, 600, Vec::new());
        let (coll, report) = stop_fixture(&dir, port, 6, Some(2));
        let out = dir.join("out.json");

        let (progress, log) = capturing();
        let code = run_with_progress(
            Some(coll.to_string_lossy().into_owned()),
            Vec::new(),
            report.to_string_lossy().into_owned(),
            vec![out.to_string_lossy().into_owned()],
            false,
            Vec::new(),
            None,
            ParamValues::new(),
            progress,
            control,
        );
        assert_eq!(code, EXIT_INTERRUPTED);

        let written: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&out).unwrap()).unwrap();
        assert_eq!(written["partial"], serde_json::json!(true));
        assert_eq!(written["rows_planned"], serde_json::json!(6));
        assert!(
            written["rows"].as_array().unwrap().len() < 6,
            "the run was abandoned: {written}"
        );
        let events = events(&log);
        let finished = events
            .iter()
            .find(|e| e["event"] == "run_finished")
            .expect("still one terminal event");
        assert_eq!(finished["interrupted"], serde_json::json!(true));
        let warnings = finished["warnings"].as_array().unwrap();
        assert!(
            warnings
                .iter()
                .any(|w| w.as_str().unwrap_or_default().contains("CLEANUP")),
            "a report written over a run that never wound down has to say so: {warnings:?}"
        );
        assert_eq!(finished["exit_code"], serde_json::json!(EXIT_INTERRUPTED));

        fs::remove_dir_all(&dir).ok();
    }

    /// The two ways an abandoned run can lie about itself, both of them only
    /// on this path: rows in the order they *finished* rather than the order
    /// the report is defined to be in, and a row announced after the stream
    /// said it had ended.
    #[test]
    fn an_abandoned_parallel_report_is_still_in_report_order_and_ends_when_it_says_it_does() {
        let dir = temp_dir("abandon_order");
        let control = Control::for_test().with_grace(std::time::Duration::from_millis(250));
        // The rows are answered back-to-front, so a report that simply keeps
        // what the sink handed it comes out reversed. r0 is slow enough to
        // still be in flight when the grace period runs out — it is the
        // straggler whose late arrival must not reach the stream.
        let port = stopping_server(control.switch(), 1, 0, vec![700, 60, 40, 20]);
        let (coll, report) = stop_fixture(&dir, port, 4, Some(4));
        let out = dir.join("out.json");

        let (progress, log) = capturing();
        let code = run_with_progress(
            Some(coll.to_string_lossy().into_owned()),
            Vec::new(),
            report.to_string_lossy().into_owned(),
            vec![out.to_string_lossy().into_owned()],
            false,
            Vec::new(),
            None,
            ParamValues::new(),
            progress,
            control,
        );
        assert_eq!(code, EXIT_INTERRUPTED);

        let written: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&out).unwrap()).unwrap();
        let xs: Vec<String> = written["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["X"].as_str().unwrap_or_default().to_string())
            .collect();
        assert!(
            xs.len() > 1,
            "the ordering only means something with several rows: {written}"
        );
        let mut sorted = xs.clone();
        sorted.sort();
        assert_eq!(
            xs, sorted,
            "an abandoned report is still a report: the same rows in the same order a \
             sequential run would have written them"
        );

        let events = events(&log);
        assert_eq!(
            events.last().map(|e| e["event"].clone()),
            Some(serde_json::json!("run_finished")),
            "a straggler landing after the terminal event would break the one promise \
             the stream makes: {events:#?}"
        );
    }
}
