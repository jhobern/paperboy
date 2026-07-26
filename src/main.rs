//! PaperBoy — a Rust-native API client (Postman alternative). Two front-ends
//! over one core: a terminal UI (default), and a headless CLI runner
//! (`-c collection.hurl [-e environment.vars]`).

mod cli;
mod collection;
mod environment;
mod git_remote;
mod http;
mod hurl;
mod i18n;
mod persistence;
mod postman;
mod report;
mod report_cli;
mod request;
mod shared_utils;
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
Runs in one of three modes:\n\
\x20 TUI  (default)          a terminal user interface\n\
\x20 CLI  (-c/--collection)  run a Hurl or Postman collection headlessly, then exit\n\
\x20 Report (-r/--report)    run a PaperTrail report against a collection, then exit",
    after_help = "Examples:\n\
\x20 paperboy                            Launch the terminal UI (default)\n\
\x20 paperboy -c collection.hurl         Run a collection headlessly\n\
\x20 paperboy -c collection.hurl -e environment.vars   Run a collection with an environment\n\
\x20 paperboy -c collection.hurl --batch    Run as one batch (preserves cookies across requests)\n\
\x20 paperboy -c collection.hurl -e environment.vars -r report.report   Run a report\n\
\x20 paperboy -c collection.hurl -e prod.vars -e staging.vars -r report.report   Run a baseline/comparison report\n\
\x20 paperboy -c collection.hurl -r report.report --dry-run   Preview a report without sending anything\n\
\x20 paperboy -c collection.hurl -r report.report -o out.csv   Write the report to a file (- = stdout)\n\n\
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

    /// Run a PaperTrail report (`.report`) against the collection given by
    /// `-c` and exit. Requires `-c`; `-e` supplies the base variable layer and
    /// (when repeated) the environments an `ENVS` loop can name.
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
}

fn main() {
    let cli = Cli::parse();

    // Headless report mode (`-r`): run a PaperTrail report against `-c`.
    if let Some(report) = cli.report {
        let Some(collection) = cli.collection else {
            eprintln!(
                "error: -r/--report requires -c/--collection (the collection to run against)"
            );
            std::process::exit(2);
        };
        std::process::exit(report_cli::run(
            collection,
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

    // Terminal UI (the default).
    if let Err(e) = tui::run() {
        eprintln!("tui error: {e}");
        std::process::exit(1);
    }
    std::process::exit(0);
}
