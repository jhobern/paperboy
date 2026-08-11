//! Headless Postman import:
//! `paperboy --postman-import [--postman-workspace ID|URL] [-o DIR]`.
//!
//! A thin shell over [`crate::postman_import`], which does the actual work.
//! Two shapes, because a workspace id is not something anyone has memorised:
//! without `--postman-workspace` it lists the workspaces the key can see and
//! exits, and with one it downloads that workspace into a folder PaperBoy can
//! open.
//!
//! Following [`crate::report_cli`], progress goes to **stderr** and the result
//! to **stdout**, so `--postman-import` on its own can be piped into `grep` or
//! `awk` to pick out a workspace id without the progress chatter mixed in.

use std::io::IsTerminal;
use std::path::PathBuf;
use std::time::Duration;

use crate::postman_api::{ApiError, PostmanClient, WorkspaceKind};
use crate::postman_import::{
    ImportError, ImportFormat, ImportOptions, ImportPlan, Importer, ItemKind, parse_workspace_ref,
};

/// What the import should fetch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum What {
    All,
    Collections,
    Environments,
}

impl What {
    fn parse(s: &str) -> Option<What> {
        match s.trim().to_ascii_lowercase().as_str() {
            "all" | "both" => Some(What::All),
            "collections" | "collection" => Some(What::Collections),
            "environments" | "environment" | "envs" => Some(What::Environments),
            _ => None,
        }
    }
}

/// Arguments for the headless import, already parsed by clap.
pub struct Args {
    pub key: Option<String>,
    pub workspace: Option<String>,
    pub out: Option<String>,
    pub what: Option<String>,
    pub base_url: Option<String>,
    pub format: Option<String>,
    pub overwrite: bool,
}

/// Run the headless import. Returns an OS exit code.
pub fn run(args: Args) -> i32 {
    let key = match resolve_key(args.key.as_deref()) {
        Ok(k) => k,
        Err(msg) => {
            eprintln!("error: {msg}");
            return 2;
        }
    };

    let what = match args.what.as_deref().map(What::parse) {
        Some(None) => {
            eprintln!("error: --postman-what must be one of: all, collections, environments");
            return 2;
        }
        Some(Some(w)) => w,
        None => What::All,
    };

    let format = match args.format.as_deref().map(parse_format) {
        Some(None) => {
            eprintln!("error: --postman-format must be one of: postman, hurl");
            return 2;
        }
        Some(Some(f)) => f,
        None => ImportFormat::default(),
    };

    let client = PostmanClient::new(key, args.base_url.clone());

    // No workspace given: list what this key can see and stop. Downloading
    // "everything" by default could be hundreds of collections and a large
    // slice of the account's monthly API budget, so it has to be asked for.
    let Some(raw_workspace) = args.workspace.as_deref() else {
        return list_workspaces(&client);
    };

    let Some(workspace_id) = parse_workspace_ref(raw_workspace) else {
        eprintln!(
            "error: {raw_workspace:?} is not a workspace id or URL.\n\
             Expected a UUID, or the address of the workspace in Postman.\n\
             Run `paperboy --postman-import` with no --postman-workspace to list them."
        );
        return 2;
    };

    let Some(out) = args.out.as_deref() else {
        eprintln!("error: --postman-import needs -o/--output to say where to write the workspace");
        return 2;
    };
    let dest = PathBuf::from(out);

    let options = ImportOptions {
        include_collections: matches!(what, What::All | What::Collections),
        include_environments: matches!(what, What::All | What::Environments),
        format,
        overwrite: args.overwrite,
    };

    download(&client, &workspace_id, &dest, &options)
}

/// `--postman-format`. `postman` keeps the JSON exactly as Postman sends it;
/// `hurl` converts, which is lossy and says so in `CONVERSION-NOTES.md`.
fn parse_format(v: &str) -> Option<ImportFormat> {
    match v.trim().to_ascii_lowercase().as_str() {
        "postman" | "raw" | "json" => Some(ImportFormat::Raw),
        "hurl" => Some(ImportFormat::Hurl),
        _ => None,
    }
}

/// Where the API key comes from, in order of preference.
///
/// `--postman-key` accepts the same `{{ … }}` provider references as a `.vars`
/// file, so the key can be stored in 1Password or SSM and never appear in a
/// shell history or a process listing. `$POSTMAN_API_KEY` is the fallback
/// because that is what the surrounding ecosystem (including Postman's own
/// tooling) uses.
fn resolve_key(flag: Option<&str>) -> Result<String, String> {
    let raw = match flag {
        Some(v) if !v.trim().is_empty() => v.trim().to_string(),
        _ => match std::env::var("POSTMAN_API_KEY") {
            Ok(v) if !v.trim().is_empty() => v.trim().to_string(),
            _ => {
                return Err(
                    "no Postman API key. Pass --postman-key, or set $POSTMAN_API_KEY.\n\
                     The key may be a provider reference, e.g.\n\
                     \x20   --postman-key '{{ op://Private/Postman/credential }}'\n\
                     Create a key at https://go.postman.co/settings/me/api-keys"
                        .to_string(),
                );
            }
        },
    };

    // The app's own resolver, so a reference behaves exactly as it would in a
    // `.vars` file — including 1Password prompting only once.
    crate::environment::resolve_reference(&raw).ok_or_else(|| {
        format!(
            "could not resolve the API key reference {raw}.\n\
             Check the reference, and that the provider's CLI (`op` or `aws`) is installed and signed in."
        )
    })
}

/// Print every workspace the key can see, so the user can pick an id.
fn list_workspaces(client: &PostmanClient) -> i32 {
    eprintln!("Listing workspaces…");
    let kinds = WorkspaceKind::default_selection();
    let (mut workspaces, _rate) = match client.list_workspaces(&kinds) {
        Ok(v) => v,
        Err(e) => return fail_api(e),
    };

    if workspaces.is_empty() {
        eprintln!(
            "No workspaces found for this key.\n\
             A Postman API key carries its owner's own access, so a workspace you\n\
             cannot see here is one your Postman account is not a member of."
        );
        return 1;
    }

    workspaces.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    let width = workspaces.iter().map(|w| w.name.len()).max().unwrap_or(0);
    for w in &workspaces {
        println!("{:<width$}  {:<8}  {}", w.name, w.kind.as_str(), w.id);
    }
    eprintln!(
        "\n{} workspace(s). Import one with:\n\
         \x20   paperboy --postman-import --postman-workspace <ID> -o <FOLDER>",
        workspaces.len()
    );
    0
}

/// Plan, then download, reporting progress as it goes.
fn download(
    client: &PostmanClient,
    workspace_id: &str,
    dest: &PathBuf,
    options: &ImportOptions,
) -> i32 {
    // One importer for both phases: the pacer learns the account's real rate
    // budget from the listing calls, and a fresh importer for the download
    // would throw that away and burst straight into a 429.
    let (tx, rx) = std::sync::mpsc::channel();
    let mut importer = Importer::new(client).with_progress(tx);

    eprintln!("Listing the workspace…");
    let plan = match importer.plan(workspace_id, "", options) {
        Ok(p) => p,
        Err(e) => return fail_import(e),
    };

    announce_plan(&plan);

    // Progress is pumped to stderr on its own thread so a slow terminal never
    // holds up the download. Messages already buffered by the planning phase
    // are ignored by the printer.
    let total = plan.item_count();
    let printer = std::thread::spawn(move || pump_progress(rx, total));

    let result = importer.download(&plan, dest, options);
    // Release the sender before joining. The engine reports every outcome, but
    // a printer thread blocked on a channel nobody will write to again is a
    // hang rather than an error, so the CLI does not rely on that alone.
    drop(importer);
    let _ = printer.join();

    match result {
        Ok(summary) => {
            println!(
                "Imported {} collection(s) and {} environment(s) into {}",
                summary.collections,
                summary.environments,
                dest.display()
            );
            if !summary.failures.is_empty() {
                eprintln!("\n{} item(s) could not be fetched:", summary.failures.len());
                for (name, err) in &summary.failures {
                    eprintln!("  {name}: {err}");
                }
                // The folder is usable, but it is not what was asked for, so
                // a script must be able to tell.
                return 1;
            }
            if summary.converted_with_notes {
                eprintln!(
                    "\nSome things could not be converted to Hurl. See {}/{}",
                    dest.display(),
                    crate::postman_import::NOTES_FILE
                );
            }
            eprintln!("Open it with: paperboy   (then Load ▸ Workspace)");
            0
        }
        Err(e) => fail_import(e),
    }
}

fn announce_plan(plan: &ImportPlan) {
    eprintln!(
        "{} collection(s), {} environment(s) — about {}",
        plan.collections.len(),
        plan.environments.len(),
        human_duration(plan.estimated_duration())
    );
    // Postman's rate limits are the reason this is not instant; saying so up
    // front stops a slow import looking like a hang.
    eprintln!("(Postman limits how fast its API may be called, so this is paced deliberately.)");
    if plan.strains_monthly_budget() {
        if let Some(rem) = plan.remaining_month {
            eprintln!(
                "warning: this will use {} of the {} API calls left on your plan this month",
                plan.api_calls(),
                rem
            );
        }
    }
}

/// Render progress to stderr, in place on a terminal and line by line when
/// redirected to a file, so a log doesn't fill up with carriage returns.
fn pump_progress(rx: std::sync::mpsc::Receiver<crate::postman_import::ImportMsg>, total: usize) {
    use crate::postman_import::ImportMsg;
    let tty = std::io::stderr().is_terminal();
    let mut last_len = 0usize;
    for msg in rx {
        match msg {
            ImportMsg::Item {
                index, kind, name, ..
            } => {
                let kind = match kind {
                    ItemKind::Collection => "collection",
                    ItemKind::Environment => "environment",
                };
                let line = format!("[{index}/{total}] {kind}: {name}");
                if tty {
                    eprint!("\r{line:<last_len$}");
                    last_len = line.len().max(last_len);
                } else {
                    eprintln!("{line}");
                }
            }
            ImportMsg::Waiting { reason, secs } => {
                let why = match reason {
                    crate::postman_import::WaitReason::RateLimited => "rate limited",
                    crate::postman_import::WaitReason::Pacing => "pacing",
                };
                let line = format!("  … waiting {secs}s ({why})");
                if tty {
                    eprint!("\r{line:<last_len$}");
                    last_len = line.len().max(last_len);
                } else {
                    eprintln!("{line}");
                }
            }
            ImportMsg::ItemFailed { name, error } => {
                if tty {
                    eprintln!("\r{:<last_len$}", "");
                    last_len = 0;
                }
                eprintln!("  ! {name}: {error}");
            }
            ImportMsg::Done(_) | ImportMsg::Failed(_) => {
                if tty && last_len > 0 {
                    eprintln!("\r{:<last_len$}", "");
                }
                break;
            }
            _ => {}
        }
    }
}

fn fail_api(e: ApiError) -> i32 {
    eprintln!("error: {e}");
    if matches!(e, ApiError::Unauthorized) {
        eprintln!("Check the key at https://go.postman.co/settings/me/api-keys");
    }
    1
}

fn fail_import(e: ImportError) -> i32 {
    match e {
        ImportError::Api(api) => fail_api(api),
        ImportError::Cancelled => {
            eprintln!("Cancelled; nothing was written.");
            130
        }
        other => {
            eprintln!("error: {other}");
            1
        }
    }
}

/// A duration a person can read, rounded honestly — an estimate that says
/// "1m 3s" pretends to a precision it doesn't have.
fn human_duration(d: Duration) -> String {
    let secs = d.as_secs();
    if secs < 5 {
        "a few seconds".to_string()
    } else if secs < 60 {
        format!("{secs} seconds")
    } else {
        let mins = (secs + 30) / 60;
        format!("{mins} minute{}", if mins == 1 { "" } else { "s" })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_accepts_the_obvious_spellings() {
        assert_eq!(What::parse("all"), Some(What::All));
        assert_eq!(What::parse("Collections"), Some(What::Collections));
        assert_eq!(What::parse(" envs "), Some(What::Environments));
        assert_eq!(What::parse("environment"), Some(What::Environments));
        assert_eq!(What::parse("nonsense"), None);
    }

    #[test]
    fn a_literal_key_is_used_as_given() {
        assert_eq!(resolve_key(Some("PMAK-abc")).unwrap(), "PMAK-abc");
    }

    #[test]
    fn a_key_is_trimmed() {
        // Copy-pasting a key out of a browser very often brings a newline.
        assert_eq!(resolve_key(Some("  PMAK-abc\n")).unwrap(), "PMAK-abc");
    }

    #[test]
    fn a_missing_key_explains_where_to_get_one() {
        // The env var must not leak in from the developer's own shell.
        unsafe { std::env::remove_var("POSTMAN_API_KEY") };
        let err = resolve_key(None).unwrap_err();
        assert!(err.contains("POSTMAN_API_KEY"));
        assert!(err.contains("api-keys"));
    }

    #[test]
    fn an_env_reference_resolves_like_it_would_in_a_vars_file() {
        unsafe { std::env::set_var("PB_TEST_POSTMAN_KEY", "PMAK-from-env") };
        let got = resolve_key(Some("{{ env:PB_TEST_POSTMAN_KEY }}")).unwrap();
        assert_eq!(got, "PMAK-from-env");
        unsafe { std::env::remove_var("PB_TEST_POSTMAN_KEY") };
    }

    #[test]
    fn an_unresolvable_reference_is_an_error_not_a_literal() {
        // Sending the literal text "{{ op://… }}" as a key would produce a
        // baffling 401 instead of naming the real problem.
        let err = resolve_key(Some("{{ env:PB_TEST_DEFINITELY_UNSET }}")).unwrap_err();
        assert!(err.contains("could not resolve"));
    }

    #[test]
    fn durations_are_rounded_to_something_readable() {
        assert_eq!(human_duration(Duration::from_secs(2)), "a few seconds");
        assert_eq!(human_duration(Duration::from_secs(13)), "13 seconds");
        assert_eq!(human_duration(Duration::from_secs(60)), "1 minute");
        assert_eq!(human_duration(Duration::from_secs(150)), "3 minutes");
    }
}
