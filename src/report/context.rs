//! Front-end-agnostic assembly of a report's **run** and **validation**
//! contexts from the app's loaded collections and environments.
//!
//! Both the terminal UI and the GUI need to answer the same questions about a
//! `.trail` report before they can validate or run it: which loaded collection
//! is it bound to, what request titles / `[Reports]` fields / environment names
//! and variables are in scope, and where do relative producer paths resolve?
//! That logic used to live only in `tui/reports.rs`; it lives here so the two
//! front-ends share one implementation (each just passes its own
//! `&[Collection]` / `&[Environment]`) and can never drift apart.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::collection::Collection;
use crate::environment::Environment;
use crate::hurl::HurlEntry;
use crate::session::effective_env;

use super::flow::ReportFlow;
use super::validate::{self, Context, Diagnostic};

/// The fully-owned inputs a report run needs, so it can be handed to a
/// background thread with no borrow of the app: the flow, a clone of the bound
/// collection's entries, the resolved base/named environment layers, the
/// producer-path root and the runner's file root.
pub struct ReportRunInputs {
    pub flow: ReportFlow,
    pub entries: Vec<HurlEntry>,
    pub base_vars: HashMap<String, String>,
    pub named_envs: HashMap<String, HashMap<String, String>>,
    pub root: Option<PathBuf>,
    pub file_root: Option<PathBuf>,
}

/// Flatten an [`Environment`] into a plain `KEY → value` map for the
/// interpreter's variable layers.
pub(crate) fn flatten_env(env: &Environment) -> HashMap<String, String> {
    env.vars
        .iter()
        .map(|v| (v.key.clone(), v.value.clone()))
        .collect()
}

/// Resolve a collection/producer reference `cref` (relative when possible, else
/// absolute) against the report file's own directory.
pub(crate) fn resolve_ref_path(report_path: Option<&Path>, cref: &str) -> PathBuf {
    let p = Path::new(cref);
    if p.is_absolute() {
        return p.to_path_buf();
    }
    if let Some(dir) = report_path.and_then(|rp| rp.parent()) {
        return dir.join(p);
    }
    p.to_path_buf()
}

/// Compare two paths, canonicalising when possible (so `./a.hurl` and an
/// absolute form of the same file match) but falling back to a plain equality
/// check for paths that don't yet exist on disk.
pub(crate) fn paths_equal(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// The directory relative paths (and the `# baseline:` snapshot existence
/// check) resolve against, and whether the report is *anchored* (saved, or
/// carrying a `# root:` override) — a scratch report with no directory is
/// unanchored so filesystem checks skip rather than warn against the CWD.
pub fn report_base_dir(flow: &ReportFlow, report_path: Option<&Path>) -> (PathBuf, bool) {
    if let Some(r) = flow.header.root()
        && !r.trim().is_empty()
    {
        return (resolve_ref_path(report_path, r), true);
    }
    if let Some(dir) = report_path.and_then(|p| p.parent()) {
        return (dir.to_path_buf(), true);
    }
    (std::env::current_dir().unwrap_or_default(), false)
}

/// The index of the loaded collection a report's flow is bound to (via its
/// `# collection:` directive), resolved by path (relative to the report's own
/// directory when possible, else absolute) and falling back to a name match so
/// a report bound before its collection was ever saved still resolves. Git refs
/// (`git:…`) aren't auto-resolved. `None` when nothing matches.
pub fn resolve_bound_collection(
    collections: &[Collection],
    flow: &ReportFlow,
    report_path: Option<&Path>,
) -> Option<usize> {
    let cref = flow.header.collection()?;
    if cref.starts_with("git:") {
        return None;
    }
    let target = resolve_ref_path(report_path, cref);
    collections
        .iter()
        .position(|c| c.path.as_ref().is_some_and(|p| paths_equal(p, &target)) || c.name == cref)
}

/// The base variable *names* in scope for a report: a `# environment:` directive
/// names a single loaded env; otherwise the bound collection's effective
/// (active global + pinned) merge. `None` when the collection is unbound and no
/// `# environment:` is set, so the variable-availability check is skipped.
fn base_var_names(
    collections: &[Collection],
    global_envs: &[Environment],
    active_env_id: Option<u64>,
    flow: &ReportFlow,
    bound: Option<usize>,
) -> Option<Vec<String>> {
    match (bound, flow.header.environment()) {
        (_, Some(name)) => {
            let name = name.trim();
            global_envs
                .iter()
                .find(|e| e.name == name)
                .map(|env| env.vars.iter().map(|v| v.key.clone()).collect())
        }
        (Some(ci), None) => Some(
            effective_env(collections, global_envs, ci, active_env_id)
                .map(|env| env.vars.iter().map(|v| v.key.clone()).collect())
                .unwrap_or_default(),
        ),
        (None, _) => None,
    }
}

/// Validate `flow` against the loaded collections/environments, returning all
/// diagnostics. Assembles the [`validate::Context`] from the app state exactly
/// as the terminal UI's `revalidate_report` did, so both front-ends agree.
pub fn report_diagnostics(
    collections: &[Collection],
    global_envs: &[Environment],
    active_env_id: Option<u64>,
    flow: &ReportFlow,
    report_path: Option<&Path>,
    strings: &crate::i18n::Strings,
) -> Vec<Diagnostic> {
    let bound = resolve_bound_collection(collections, flow, report_path);
    let titles: Option<Vec<String>> = bound.map(|ci| {
        collections[ci]
            .entries
            .iter()
            .map(|e| e.title.clone())
            .collect()
    });
    // Each entry's [Reports] field names, so a SHOW(...) selector can be
    // validated against what the request can produce.
    let fields: Option<Vec<(String, Vec<String>)>> = bound.map(|ci| {
        collections[ci]
            .entries
            .iter()
            .map(|e| {
                (
                    e.title.clone(),
                    e.reports.iter().map(|(n, _)| n.clone()).collect(),
                )
            })
            .collect()
    });
    let env_names: Vec<String> = global_envs.iter().map(|e| e.name.clone()).collect();
    let (base_dir, anchored) = report_base_dir(flow, report_path);
    let base_var_names = base_var_names(collections, global_envs, active_env_id, flow, bound);
    // Union of ALL loaded env variable names — used conservatively inside
    // `FOR … IN ENVS` bodies so we don't false-warn when any of the named envs
    // might supply a var.
    let mut all_env_var_names: Vec<String> = global_envs
        .iter()
        .flat_map(|e| e.vars.iter().map(|v| v.key.clone()))
        .collect();
    all_env_var_names.sort();
    all_env_var_names.dedup();
    let request_entries_owned: Option<Vec<HurlEntry>> =
        bound.map(|ci| collections[ci].entries.clone());

    let ctx = Context {
        request_titles: titles.as_deref(),
        env_names: Some(&env_names),
        request_fields: fields.as_deref(),
        root: anchored.then_some(base_dir.as_path()),
        base_var_names: base_var_names.as_deref(),
        all_env_var_names: Some(&all_env_var_names),
        request_entries: request_entries_owned.as_deref(),
        strings,
    };
    validate::validate(flow, &ctx)
}

/// Assemble the fully-owned [`ReportRunInputs`] for `flow` (bound to a loaded
/// collection). `Err` with a user-facing key when the flow isn't bound to a
/// loaded collection. Mirrors the terminal UI's `build_report_run_inputs`.
pub fn report_run_inputs(
    collections: &[Collection],
    global_envs: &[Environment],
    active_env_id: Option<u64>,
    flow: &ReportFlow,
    report_path: Option<&Path>,
) -> Result<ReportRunInputs, RunInputError> {
    let ci =
        resolve_bound_collection(collections, flow, report_path).ok_or(RunInputError::Unbound)?;

    // Base variable layer. A `# environment:` directive names a single loaded
    // environment for a plain, no-comparison run; otherwise fall back to the
    // bound collection's effective (active + pinned) environment.
    let base_vars = match flow
        .header
        .environment()
        .map(str::trim)
        .filter(|e| !e.is_empty())
    {
        Some(name) => global_envs
            .iter()
            .find(|e| e.name == name)
            .map(flatten_env)
            .unwrap_or_default(),
        None => effective_env(collections, global_envs, ci, active_env_id)
            .map(|env| flatten_env(&env))
            .unwrap_or_default(),
    };
    // Every loaded global environment is selectable by name in a `FOR … IN
    // ENVS` loop.
    let named_envs = global_envs
        .iter()
        .map(|e| (e.name.clone(), flatten_env(e)))
        .collect();
    // Relative producer paths resolve against `# root:` if set, else the report
    // file's own directory.
    let report_dir = report_path.and_then(|p| p.parent()).map(Path::to_path_buf);
    let root = match flow.header.root() {
        Some(r) if !r.trim().is_empty() => Some(resolve_ref_path(report_path, r)),
        _ => report_dir,
    };
    // The live runner is rooted at the bound collection's directory so relative
    // form-file paths in its requests resolve as they would by hand.
    let file_root = collections[ci]
        .path
        .as_deref()
        .and_then(|p| p.parent())
        .map(Path::to_path_buf);

    Ok(ReportRunInputs {
        flow: flow.clone(),
        entries: collections[ci].entries.clone(),
        base_vars,
        named_envs,
        root,
        file_root,
    })
}

/// Why [`report_run_inputs`] couldn't assemble a runnable context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunInputError {
    /// The flow isn't bound to a currently-loaded collection.
    Unbound,
}
