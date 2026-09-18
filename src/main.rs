//! PaperBoy — a Rust-native API client (Postman alternative). Front-ends over
//! one core: a terminal UI (default, behind the `tui` Cargo feature), a
//! headless CLI runner (`-c collection.hurl [-e environment.vars]`, always
//! built), and — behind the `gui` Cargo feature — a native graphical UI.

// Dead-code analysis is authoritative in the configurations that include the
// terminal UI — the default build and `--features gui`. A build without it
// (`--no-default-features`, with or without `gui`) is a partial configuration:
// it drops several hundred core items that exist to serve a front-end — wizard
// state machines, draw helpers, the `Strings` table's rows, and the re-exports
// that feed them — and reporting each one would be noise, not a finding.
//
// Keying the suppression on `tui` works because the terminal UI is the superset
// front-end: everything shared is used by it, and the items that belong to the
// GUI alone already carry the mirror-image `not(feature = "gui")` annotation at
// their definitions. So nothing escapes analysis — an item dead in every
// configuration is still reported by the two builds above, which CI runs
// alongside the partial ones.
#![cfg_attr(not(feature = "tui"), allow(dead_code, unused_imports))]

mod cli;
mod collection;
mod env_panel;
mod environment;
mod generators;
mod git_remote;
// The GUI pulls in eframe/winit/wgpu, which dominate build time, so it is
// opt-in (`--features gui`). Everything it needs lives under `src/gui`; the
// rest of the tree never refers to it, so the gate is this one line plus the
// `--gui` dispatch below.
#[cfg(feature = "gui")]
mod gui;
mod http;
mod hurl;
mod i18n;
mod persistence;
mod postman;
mod postman_api;
mod postman_cli;
mod postman_flow;
mod postman_import;
mod probe;
mod remote_flow;
mod report;
mod report_cli;
// The `.trail` syntax highlighter. It lives at the top level, not under `tui`,
// because both front-ends draw from it: it emits ratatui spans, which the GUI
// converts to egui text sections rather than reimplementing the rules and
// letting the two drift. Keeping it here means a GUI-only build does not have
// to pull in the terminal UI just to reach it. It is still drawing code, so a
// headless build leaves it out entirely.
#[cfg(any(feature = "tui", feature = "gui"))]
mod report_highlight;
mod request;
mod save_flow;
mod session;
mod shared_utils;
mod theme;
mod tree;
#[cfg(feature = "tui")]
mod tui;
mod vars_view;
mod workspace;

use clap::Parser;

/// How this build describes the mode it runs in when given no `-c`/`-r`.
///
/// A build without the TUI must not advertise one: the help is the only thing
/// telling a container user what their binary can actually do, and "TUI
/// (default)" on a binary that has none is a bug report waiting to happen.
#[cfg(feature = "tui")]
const DEFAULT_MODE: &str = "\x20 TUI  (default)          a terminal user interface\n";
#[cfg(all(not(feature = "tui"), feature = "gui"))]
const DEFAULT_MODE: &str =
    "\x20 GUI  (-g/--gui)         a native graphical interface (this build has no TUI)\n";
#[cfg(all(not(feature = "tui"), not(feature = "gui")))]
const DEFAULT_MODE: &str = "";

#[cfg(feature = "tui")]
const DEFAULT_EXAMPLE: &str =
    "\x20 paperboy                            Launch the terminal UI (default)\n";
#[cfg(all(not(feature = "tui"), feature = "gui"))]
const DEFAULT_EXAMPLE: &str = "\x20 paperboy --gui                      Launch the graphical UI\n";
#[cfg(all(not(feature = "tui"), not(feature = "gui")))]
const DEFAULT_EXAMPLE: &str = "";

/// Built at first use rather than written as a literal so the mode list and the
/// examples can differ by feature — see [`DEFAULT_MODE`].
static LONG_ABOUT: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    format!(
        "PaperBoy — a Rust-native API client (a Postman alternative).\n\n\
Runs in one of these modes:\n\
{DEFAULT_MODE}\
\x20 CLI  (-c/--collection)  run a Hurl or Postman collection headlessly, then exit\n\
\x20 Report (-r/--report)    run a PaperTrail report against a collection, then exit\n\
\x20 Import (--postman-import)  download a Postman workspace over the API, then exit"
    )
});

static AFTER_HELP: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    format!(
        "Examples:\n\
{DEFAULT_EXAMPLE}\
\x20 paperboy -c collection.hurl         Run a collection headlessly\n\
\x20 paperboy -c collection.hurl -e environment.vars   Run a collection with an environment\n\
\x20 paperboy -c collection.hurl --batch    Run as one batch (preserves cookies across requests)\n\
\x20 paperboy -c collection.hurl -e environment.vars -r report.trail   Run a report\n\
\x20 paperboy -r report.trail   Run a report, taking its collection/environment from the report's own headers\n\
\x20 paperboy -c collection.hurl -e prod.vars -e staging.vars -r report.trail   Run a baseline/comparison report\n\
\x20 paperboy -c collection.hurl -r report.trail --dry-run   Preview a report without sending anything\n\
\x20 paperboy -c collection.hurl -r report.trail -o out.csv   Write the report to a file (- = stdout)\n\
\x20 paperboy -r report.trail -o out.html -o out.json   One run, several formats\n\
\x20 paperboy -r report.trail --param CASES_DIR=./batch-07   Run a report, setting a PARAM it declares\n\
\x20 paperboy --postman-import                          List the Postman workspaces your API key can see\n\
\x20 paperboy --postman-import --postman-workspace ID -o ./API   Download a whole Postman workspace\n\
\x20 paperboy --postman-import --postman-all -o ./API           Download every workspace the key can see\n\n\
Environment (.vars) entries are KEY=value, where the value is a literal or a\n\
{{ ... }} provider reference resolved when the environment is loaded:\n\
\x20 Literal value       USERNAME=demo\n\
\x20 Process env var     BASE_URL={{ env:DEMO_BASE_URL }}\n\
\x20 1Password (op CLI)  API_TOKEN={{ op://Vault/Item/field }}\n\
\x20 AWS SSM parameter   DB_PASSWORD={{ ssm:/path/to/param }}\n\n\
Collections are Hurl files (.hurl) or Postman collection exports (.json);\n\
Postman JSON is imported automatically."
    )
});

/// PaperBoy — a Rust API client with a terminal UI and a headless runner.
#[derive(Parser)]
#[command(
    name = "paperboy",
    version,
    about = "PaperBoy — a Rust-native API client (a Postman alternative).",
    long_about = LONG_ABOUT.as_str(),
    after_help = AFTER_HELP.as_str()
)]
struct Cli {
    /// Run the given collection (Hurl `.hurl` or Postman `.json`) headlessly and print the results.
    #[arg(short = 'c', long, value_name = "FILE")]
    collection: Option<String>,

    /// Environment (.vars) file supplying `{{ VAR }}` values. Repeatable: pass
    /// `-e` more than once to load several environments for a report (`-r`) —
    /// each is named by its file stem and becomes selectable in an `ENVS` loop
    /// (e.g. `-e prod.vars -e staging.vars` satisfies
    /// `FOR … IN ENVS BASELINE("prod"), COMPARISON("staging")`). The first `-e`
    /// is the base variable layer. A plain collection run (`-c` only) uses just
    /// the first.
    #[arg(short = 'e', long, value_name = "FILE")]
    env: Vec<String>,

    /// Run every request as a single batch instead of streaming each result
    /// as soon as it finishes. Slower to show any output, but preserves
    /// Hurl's automatic cookie jar (cookies remembered from `Set-Cookie`
    /// response headers) across every request in the collection — the
    /// default streaming mode does not carry cookies between requests (an
    /// explicit `[Cookies]` section on a request is unaffected either way).
    #[arg(short = 'b', long)]
    batch: bool,

    /// Run a PaperTrail report (`.trail`) and exit. The collection to run
    /// against comes from `-c`, or (when `-c` is omitted) the report's own
    /// `# collection:` header resolved relative to the report's folder. `-e`
    /// supplies the base variable layer and (when repeated) the environments an
    /// `ENVS` loop can name; with no `-e`, the report's `# environment:` header
    /// (if any) is used instead.
    #[arg(short = 'r', long, value_name = "FILE")]
    report: Option<String>,

    /// With `-r`: expand the report and show what it would do without sending
    /// any request (no HTTP). Handy before a large run.
    #[arg(long, requires = "report")]
    dry_run: bool,

    /// With `-r`: run only these steps and whatever they depend on
    /// (comma-separated step names). Every named step must be inside a `GRAPH`
    /// region — only a region declares the complete graph that a closure needs,
    /// so a target anywhere else is an error rather than a silent full run.
    #[arg(long, value_name = "STEPS", value_delimiter = ',', requires = "report")]
    targets: Vec<String>,

    /// With `-r`: vary the order steps are taken within a `GRAPH` region, and
    /// print the seed used. A region is a *claim* that its dependency edges are
    /// the complete set, and that claim cannot be verified — but it can be
    /// falsified. Without this the earliest-written tie-break silently supplies
    /// the ordering a missing edge forgot, and the gap is never found. Pass
    /// `--shuffle=SEED` to replay a failure exactly.
    #[arg(long, value_name = "SEED", num_args = 0..=1, requires = "report")]
    shuffle: Option<Option<u64>>,

    /// Where to write the output. With `-r`, **repeatable**: give it once per
    /// format and one run writes them all from the same result — `-o
    /// report.html -o report.json` yields a rendering to show and a structure
    /// to parse without running the requests twice. `-` writes CSV to stdout
    /// (for piping) and may be given at most once; a path's extension selects
    /// the format (`.csv`, `.json`, `.html`, `.xlsx` or `.pdf`); omitted
    /// derives a single file from the report's `# output:`/`# name:` headers
    /// (next to the report file, honouring the `{time}` token). With
    /// `--postman-import` it is instead the download directory, and takes one
    /// value.
    #[arg(short = 'o', long = "output", value_name = "FILE")]
    outputs: Vec<String>,

    /// With `-r`: set a `PARAM` declared by the report, as `NAME=VALUE`.
    /// Repeatable, and the value wins over the default written in the `.trail`
    /// file (which is never rewritten). This is what lets one report serve many
    /// runs — `--param CASES_DIR=./batch-07` points a `FOR … IN FOLDERS
    /// "{{CASES_DIR}}"` loop somewhere new without editing the script. A name
    /// the report doesn't declare is an error rather than a value that silently
    /// does nothing.
    #[arg(
        long = "param",
        value_name = "NAME=VALUE",
        value_parser = report::params::parse_assignment,
        requires = "report"
    )]
    params: Vec<(String, String)>,

    /// Launch the native graphical UI (eframe/egui) instead of the terminal UI.
    /// Ignored in the headless modes (`-c`/`-r`). Only available when built
    /// with the `gui` feature (`cargo install paperboy --locked --features gui`).
    #[arg(short = 'g', long)]
    gui: bool,

    /// Import a Postman workspace over the Postman API and exit. With
    /// `--postman-workspace` it downloads that workspace's collections and
    /// environments into `-o`; without one it lists the workspaces the key can
    /// see, so you can pick an id.
    #[arg(long)]
    postman_import: bool,

    /// With `--postman-import`: the workspace to download, as its id or as the
    /// address of the workspace in Postman (both are accepted, so the browser
    /// address bar can simply be pasted).
    #[arg(long, value_name = "ID|URL")]
    postman_workspace: Option<String>,

    /// With `--postman-import`: download every workspace the key can see
    /// rather than one, each into its own folder inside `-o`. This is the
    /// migration case; it costs two API calls per workspace to list them, so
    /// it has to be asked for.
    #[arg(long, conflicts_with = "postman_workspace")]
    postman_all: bool,

    /// With `--postman-import`: the Postman API key. Defaults to
    /// `$POSTMAN_API_KEY`. Accepts the same `{{ … }}` provider references as a
    /// `.vars` file (e.g. `{{ op://Private/Postman/credential }}`), so the key
    /// need not appear in your shell history.
    #[arg(long, value_name = "KEY")]
    postman_key: Option<String>,

    /// With `--postman-import`: what to download — `all` (default),
    /// `collections` or `environments`.
    #[arg(long, value_name = "WHAT")]
    postman_what: Option<String>,

    /// With `--postman-import`: the API host, for tenants that are not on
    /// `api.postman.com` (EU Enterprise uses `https://api.eu.postman.com`).
    #[arg(long, value_name = "URL")]
    postman_base_url: Option<String>,

    /// With `--postman-import`: the on-disk format — `postman` (default) keeps
    /// Postman's own JSON exactly as sent, `hurl` converts collections to
    /// `.hurl` and environments to `.vars`. Converting is lossy (Hurl has no
    /// pre-request scripts, for one), so anything dropped is listed in
    /// `CONVERSION-NOTES.md` in the imported folder.
    #[arg(long, value_name = "FORMAT")]
    postman_format: Option<String>,

    /// With `--postman-import`: replace the destination folder if it already
    /// exists. Without this, a destination that exists and is not empty is
    /// refused.
    #[arg(long)]
    overwrite: bool,
}

fn main() {
    let cli = Cli::parse();

    // `-o` belongs to a headless mode: `-r` writes the report there and
    // `--postman-import` downloads into it. clap has no attribute for "requires
    // one of these two", and the `requires = "report"` this carried instead
    // rejected the documented `--postman-import --postman-workspace ID -o
    // ./API` outright. Exit 2, the code clap itself uses for "you invoked me
    // wrongly", so a caller can still tell a bad command line from a bad API.
    if !cli.outputs.is_empty() && cli.report.is_none() && !cli.postman_import {
        eprintln!("error: -o/--output requires -r/--report or --postman-import");
        std::process::exit(2);
    }

    // Headless Postman import (`--postman-import`): fetch a workspace over the
    // Postman API and exit. Checked before `-c`/`-r` because it produces the
    // collections those modes run, rather than running anything itself.
    if cli.postman_import {
        // One download, one destination. Repeating `-o` is meaningful for a
        // report (one result, several formats) and meaningless here, so it is
        // refused rather than silently resolved to whichever came last.
        if cli.outputs.len() > 1 {
            eprintln!(
                "error: --postman-import downloads into one directory, but -o was given {} times",
                cli.outputs.len()
            );
            std::process::exit(2);
        }
        std::process::exit(postman_cli::run(postman_cli::Args {
            key: cli.postman_key,
            workspace: cli.postman_workspace,
            all: cli.postman_all,
            out: cli.outputs.into_iter().next(),
            what: cli.postman_what,
            base_url: cli.postman_base_url,
            format: cli.postman_format,
            overwrite: cli.overwrite,
        }));
    }

    // Headless report mode (`-r`): run a PaperTrail report. `-c` may be omitted
    // — the report's `# collection:` header (resolved relative to the report's
    // folder) is used instead; `report_cli::run` raises a clear error if neither
    // is available.
    if let Some(report) = cli.report {
        std::process::exit(report_cli::run(
            cli.collection,
            cli.env,
            report,
            cli.outputs,
            cli.dry_run,
            cli.targets,
            cli.shuffle,
            cli.params.into_iter().collect(),
        ));
    }

    // Headless CLI mode (explicit "run and exit").
    if let Some(collection) = cli.collection {
        if cli.env.len() > 1 {
            eprintln!(
                "warning: multiple -e environments are only used by reports (-r); running the collection with the first one"
            );
        }
        std::process::exit(cli::run(collection, cli.env.into_iter().next(), cli.batch));
    }

    // Native GUI mode (`-g/--gui`): a graphical front-end over the same core.
    if cli.gui {
        std::process::exit(run_gui());
    }

    // Terminal UI (the default).
    std::process::exit(run_tui());
}

/// Launch the terminal UI, or explain why this build can't.
///
/// A `--no-default-features` build is headless on purpose (CI images, Docker),
/// so reaching here means the user ran `paperboy` with no `-c`/`-r` and wanted
/// the interactive front-end. Tell them how to get it rather than failing
/// silently or, worse, exiting 0 as though something had run.
#[cfg(feature = "tui")]
fn run_tui() -> i32 {
    if let Err(e) = tui::run() {
        eprintln!("tui error: {e}");
        return 1;
    }
    0
}

/// A build with the GUI but not the TUI has a front-end — it just isn't this
/// one. Calling itself headless and telling the user to reinstall would be
/// advice to rebuild something they already have.
#[cfg(all(not(feature = "tui"), feature = "gui"))]
fn run_tui() -> i32 {
    eprintln!(
        "This build of PaperBoy has no terminal UI.\n\
         Pass `-g/--gui` for the graphical one, or `-c <collection.hurl>` or \
         `-r <report.trail>` to run headlessly."
    );
    1
}

#[cfg(all(not(feature = "tui"), not(feature = "gui")))]
fn run_tui() -> i32 {
    eprintln!(
        "This build of PaperBoy is headless: it runs collections and reports, \
         but has no user interface.\n\
         Pass `-c <collection.hurl>` or `-r <report.trail>`, or reinstall with \
         the terminal UI:\n\
         \x20   cargo install paperboy --locked"
    );
    1
}

/// Launch the GUI, or explain why this build can't.
///
/// The flag is always accepted so that a user who copies a `--gui` command from
/// the README gets told how to get it, rather than an unhelpful "unexpected
/// argument" from the argument parser.
#[cfg(feature = "gui")]
fn run_gui() -> i32 {
    if let Err(e) = gui::run() {
        eprintln!("gui error: {e}");
        return 1;
    }
    0
}

#[cfg(not(feature = "gui"))]
fn run_gui() -> i32 {
    eprintln!(
        "This build of PaperBoy has no GUI. Reinstall it with the `gui` feature:\n\
         \x20   cargo install paperboy --locked --features gui"
    );
    1
}
