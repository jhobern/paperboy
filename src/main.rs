//! PaperBoy — a Rust-native API client (Postman alternative). Front-ends over
//! one core: a terminal UI (default), a headless CLI runner
//! (`-c collection.hurl [-e environment.vars]`), and — behind the `gui` Cargo
//! feature — a native graphical UI.

mod cli;
mod collection;
mod env_panel;
mod environment;
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
mod remote_flow;
mod report;
mod report_cli;
mod request;
mod save_flow;
mod session;
mod shared_utils;
mod theme;
mod tree;
mod tui;
mod workspace;

use clap::Parser;

/// PaperBoy — a Rust API client with a terminal UI and a headless runner.
#[derive(Parser)]
#[command(
    name = "paperboy",
    version,
    about = "PaperBoy — a Rust-native API client (a Postman alternative).",
    long_about = "PaperBoy — a Rust-native API client (a Postman alternative).\n\n\
Runs in one of four modes:\n\
\x20 TUI  (default)          a terminal user interface\n\
\x20 CLI  (-c/--collection)  run a Hurl or Postman collection headlessly, then exit\n\
\x20 Report (-r/--report)    run a PaperTrail report against a collection, then exit\n\
\x20 Import (--postman-import)  download a Postman workspace over the API, then exit",
    after_help = "Examples:\n\
\x20 paperboy                            Launch the terminal UI (default)\n\
\x20 paperboy -c collection.hurl         Run a collection headlessly\n\
\x20 paperboy -c collection.hurl -e environment.vars   Run a collection with an environment\n\
\x20 paperboy -c collection.hurl --batch    Run as one batch (preserves cookies across requests)\n\
\x20 paperboy -c collection.hurl -e environment.vars -r report.trail   Run a report\n\
\x20 paperboy -r report.trail   Run a report, taking its collection/environment from the report's own headers\n\
\x20 paperboy -c collection.hurl -e prod.vars -e staging.vars -r report.trail   Run a baseline/comparison report\n\
\x20 paperboy -c collection.hurl -r report.trail --dry-run   Preview a report without sending anything\n\
\x20 paperboy -c collection.hurl -r report.trail -o out.csv   Write the report to a file (- = stdout)\n\
\x20 paperboy --postman-import                          List the Postman workspaces your API key can see\n\
\x20 paperboy --postman-import --postman-workspace ID -o ./API   Download a whole Postman workspace\n\n\
Environment (.vars) entries are KEY=value, where the value is a literal or a\n\
{{ ... }} provider reference resolved when the environment is loaded:\n\
\x20 Literal value       USERNAME=demo\n\
\x20 Process env var     BASE_URL={{ env:DEMO_BASE_URL }}\n\
\x20 1Password (op CLI)  API_TOKEN={{ op://Vault/Item/field }}\n\
\x20 AWS SSM parameter   DB_PASSWORD={{ ssm:/path/to/param }}\n\n\
Collections are Hurl files (.hurl) or Postman collection exports (.json);\n\
Postman JSON is imported automatically."
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
    #[arg(long)]
    dry_run: bool,

    /// With `-r`: where to write the report output. `-` writes CSV to stdout
    /// (for piping); a path's extension selects the format (`.csv`, `.json`,
    /// `.html` or `.xlsx`); omitted derives the file from the report's
    /// `# output:`/`# name:` headers (next to the report file, honouring the
    /// `{time}` token).
    #[arg(short = 'o', long, value_name = "FILE")]
    output: Option<String>,

    /// Launch the native graphical UI (eframe/egui) instead of the terminal UI.
    /// Ignored in the headless modes (`-c`/`-r`). Only available when built
    /// with the `gui` feature (`cargo install paperboy --features gui`).
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

    // Headless Postman import (`--postman-import`): fetch a workspace over the
    // Postman API and exit. Checked before `-c`/`-r` because it produces the
    // collections those modes run, rather than running anything itself.
    if cli.postman_import {
        std::process::exit(postman_cli::run(postman_cli::Args {
            key: cli.postman_key,
            workspace: cli.postman_workspace,
            out: cli.output,
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
            cli.output,
            cli.dry_run,
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
    if let Err(e) = tui::run() {
        eprintln!("tui error: {e}");
        std::process::exit(1);
    }
    std::process::exit(0);
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
         \x20   cargo install paperboy --features gui"
    );
    1
}
