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

use crate::report::run::HelperCollection;
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
    /// Aliased helper collections, loaded and ready for `alias/name` lookups.
    pub helpers: Vec<HelperCollection>,
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

/// One selectable request across the primary collection and every declared
/// helper — what every picker, completion list and known/unknown tint is built
/// from, so all of them agree on what exists and how it must be written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestChoice {
    /// What the PaperTrail source must contain: `Upload document` for the
    /// primary collection, `helpers/fetch_frame` for a helper.
    pub qualified: String,
    /// The request's own title, without the alias.
    pub title: String,
    /// The helper alias, or `None` for the primary collection.
    pub alias: Option<String>,
}

/// Every request a report can call, primary collection first (in collection
/// order), then each helper in directive order.
///
/// The ordering is load-bearing, not incidental: pickers put the common case at
/// the top, and a newly dropped `REQUEST` block seeds itself from the first
/// choice — which must never be a helper.
pub fn request_choices(entries: &[HurlEntry], helpers: &[HelperCollection]) -> Vec<RequestChoice> {
    let mut out: Vec<RequestChoice> = entries
        .iter()
        .map(|e| RequestChoice {
            qualified: e.title.clone(),
            title: e.title.clone(),
            alias: None,
        })
        .collect();
    for h in helpers {
        out.extend(h.entries.iter().map(|e| RequestChoice {
            qualified: format!("{}/{}", h.alias, e.title),
            title: e.title.clone(),
            alias: Some(h.alias.clone()),
        }));
    }
    out
}

/// Load every helper collection a report declares (`# collection: … AS alias`),
/// in directive order. Returns the loaded helpers and, for each one that
/// couldn't be read, `(reference, reason)` for validation to flag.
///
/// An already-open collection matching the reference wins, so a helper that
/// *is* open in a tab is seen with its unsaved edits, exactly as the primary is.
/// Otherwise the file is read from disk — which is the normal case, since the
/// entire point of a helper collection is that it stays out of the collection
/// under test and so is usually not open at all.
///
/// The primary (first) directive is skipped, as is any helper missing an alias:
/// both are reported by validation, which can say something useful about them,
/// and neither can contribute addressable requests.
pub fn load_helpers(
    collections: &[Collection],
    flow: &ReportFlow,
    report_path: Option<&Path>,
    strings: &crate::i18n::Strings,
) -> (Vec<HelperCollection>, Vec<(String, String)>) {
    let mut loaded = Vec::new();
    let mut errors = Vec::new();
    for c in flow.header.collections().into_iter().skip(1) {
        let Some(alias) = c.alias else { continue };
        let reference = c.reference.trim();
        if reference.is_empty() {
            continue;
        }
        let open = collections.iter().find(|col| {
            col.name == reference
                || (!reference.starts_with("git:")
                    && col
                        .path
                        .as_ref()
                        .is_some_and(|p| paths_equal(p, &resolve_ref_path(report_path, reference))))
        });
        if let Some(col) = open {
            loaded.push(HelperCollection {
                alias: alias.to_string(),
                entries: col.entries.clone(),
            });
            continue;
        }
        if reference.starts_with("git:") {
            // Fetching a git ref means network I/O, which validation (and the
            // GUI, which revalidates as you type) must never do. Opening the
            // remote collection once makes it available by name above.
            errors.push((
                reference.to_string(),
                strings.diag_collection_helper_not_open.to_string(),
            ));
            continue;
        }
        let path = resolve_ref_path(report_path, reference);
        match std::fs::read_to_string(&path) {
            Ok(text) => match crate::hurl::parse_hurl_error(&text) {
                Some(err) => errors.push((reference.to_string(), err)),
                None => loaded.push(HelperCollection {
                    alias: alias.to_string(),
                    entries: crate::hurl::parse_hurl(&text),
                }),
            },
            Err(e) => errors.push((reference.to_string(), e.to_string())),
        }
    }
    (loaded, errors)
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

    let (helpers, helper_errors) = load_helpers(collections, flow, report_path, strings);
    let ctx = Context {
        request_titles: titles.as_deref(),
        env_names: Some(&env_names),
        request_fields: fields.as_deref(),
        root: anchored.then_some(base_dir.as_path()),
        base_var_names: base_var_names.as_deref(),
        all_env_var_names: Some(&all_env_var_names),
        request_entries: request_entries_owned.as_deref(),
        helpers: &helpers,
        helper_errors: &helper_errors,
        strings,
    };
    validate::validate(flow, &ctx)
}

/// A hash of everything [`report_diagnostics`] reads, so a caller can tell
/// whether re-running it could possibly produce a different answer.
///
/// Validation is not cheap — it deep-clones every request in the bound
/// collection and walks the whole flow — and a GUI redraws on every mouse move,
/// so a front-end that recomputed per frame would burn that cost dozens of times
/// a second to produce byte-identical output. Worse, it *isn't* byte-identical:
/// parts of the walk are driven by `HashSet`s whose iteration order differs
/// between instances, so the panel's contents were reshuffled on every frame and
/// a report with several warnings visibly flickered whenever the pointer moved.
///
/// **The contract:** this must hash every input `report_diagnostics` consults.
/// Anything new that function starts reading has to be added here too, or the
/// panel will go stale. It is deliberately the function immediately below it for
/// that reason.
///
/// Requests are hashed field by field rather than through a serializer because
/// this runs on every frame: hashing their raw strings is around seven times
/// cheaper than the validation pass it guards, whereas serializing them to JSON
/// first was *slower* than simply revalidating, which would have made the cache
/// worse than no cache at all. The fields listed are every one that can hold a
/// `{{variable}}`, a request name or a report/capture field name — the only
/// things validation looks at. Environments contribute their variable *names*
/// only: validation asks what is in scope, never what it is set to, and hashing
/// values would invalidate the cache on every keystroke in the environment
/// editor.
///
/// **One known exception to the contract:** a helper collection that is *not*
/// open as a tab is read from disk by `report_diagnostics`, and its file
/// contents are not hashed here — only the `# collection:` line naming it,
/// which comes in via `flow.to_text()`. Editing a helper file behind
/// PaperBoy's back therefore leaves the panel stale until something else in
/// the report changes. Hashing it properly would mean reading every helper
/// from disk on every frame, which is the exact cost this cache exists to
/// avoid; a helper opened as a tab (the normal way to edit one) is a
/// `Collection` and *is* hashed field by field below.
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
pub fn diagnostics_fingerprint(
    collections: &[Collection],
    global_envs: &[Environment],
    active_env_id: Option<u64>,
    flow: &ReportFlow,
    report_path: Option<&Path>,
    strings: &crate::i18n::Strings,
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    flow.to_text().hash(&mut h);
    report_path.hash(&mut h);
    active_env_id.hash(&mut h);
    // The language decides the wording of every message, so a diagnostic set
    // computed under one is not reusable under another.
    (strings.diag_var_maybe_undefined.as_ptr() as usize).hash(&mut h);
    for c in collections {
        c.name.hash(&mut h);
        c.path.hash(&mut h);
        c.linked_env_id.hash(&mut h);
        c.entries.len().hash(&mut h);
        for e in &c.entries {
            e.title.hash(&mut h);
            e.method.hash(&mut h);
            e.url.hash(&mut h);
            e.body.hash(&mut h);
            e.basic_auth.hash(&mut h);
            for kv in e
                .headers
                .iter()
                .chain(&e.queries)
                .chain(&e.cookies)
                .chain(&e.options)
            {
                kv.key.hash(&mut h);
                kv.value.hash(&mut h);
                kv.enabled.hash(&mut h);
            }
            for f in &e.form_fields {
                f.key.hash(&mut h);
                f.value.hash(&mut h);
            }
            for (k, v) in e.captures.iter().chain(&e.reports) {
                k.hash(&mut h);
                v.hash(&mut h);
            }
        }
    }
    for e in global_envs {
        e.id.hash(&mut h);
        e.name.hash(&mut h);
        for v in &e.vars {
            v.key.hash(&mut h);
        }
    }
    h.finish()
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

    // Load failures are already validation errors on the directive; a run that
    // gets here regardless resolves the missing helper's requests as unknown
    // names, which is reported per row exactly like any other unresolved name.
    let (helpers, _) = load_helpers(
        collections,
        flow,
        report_path,
        crate::i18n::Strings::english(),
    );
    Ok(ReportRunInputs {
        flow: flow.clone(),
        entries: collections[ci].entries.clone(),
        helpers,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hurl::{HurlEntry, KvRow};

    fn strings() -> crate::i18n::Strings {
        crate::i18n::Strings::for_language(&crate::i18n::Language::English)
    }

    fn entry() -> HurlEntry {
        HurlEntry {
            title: "Oauth".into(),
            method: "POST".into(),
            url: "https://api.example.com/token".into(),
            ..Default::default()
        }
    }

    fn fingerprint(cols: &[Collection], envs: &[Environment], flow: &ReportFlow) -> u64 {
        diagnostics_fingerprint(cols, envs, None, flow, None, &strings())
    }

    /// The cache is only sound if the fingerprint moves whenever anything
    /// validation reads moves. Each case here changes exactly one input and
    /// asserts the key notices — miss one and the panel silently goes stale.
    #[test]
    fn fingerprint_notices_every_input_it_guards() {
        let flow = crate::report::parse_flow("# collection: c\nREPORT REQUEST Oauth\n").unwrap();
        let cols = vec![Collection::new("c".into(), vec![entry()])];
        let envs: Vec<Environment> = Vec::new();
        let base = fingerprint(&cols, &envs, &flow);

        // Same inputs, same key — otherwise nothing is ever cached.
        assert_eq!(base, fingerprint(&cols, &envs, &flow), "must be stable");

        let mut cases: Vec<(&str, Collection)> = Vec::new();
        let mut c = cols[0].clone();
        c.entries[0].title = "Renamed".into();
        cases.push(("title", c));
        let mut c = cols[0].clone();
        c.entries[0].url = "https://api.example.com/other".into();
        cases.push(("url", c));
        let mut c = cols[0].clone();
        c.entries[0].method = "GET".into();
        cases.push(("method", c));
        let mut c = cols[0].clone();
        c.entries[0].body = Some("{{tok}}".into());
        cases.push(("body", c));
        let mut c = cols[0].clone();
        c.entries[0].headers.push(KvRow::new("A", "{{v}}"));
        cases.push(("header", c));
        let mut c = cols[0].clone();
        c.entries[0]
            .captures
            .push(("cap".into(), "jsonpath \"$.a\"".into()));
        cases.push(("capture", c));
        let mut c = cols[0].clone();
        c.entries[0]
            .reports
            .push(("F".into(), "jsonpath \"$.a\"".into()));
        cases.push(("report field", c));
        let mut c = cols[0].clone();
        c.entries.push(entry());
        cases.push(("entry count", c));
        let mut c = cols[0].clone();
        c.name = "other".into();
        cases.push(("collection name", c));

        for (what, c) in cases {
            assert_ne!(
                base,
                fingerprint(&[c], &envs, &flow),
                "changing the {what} must change the fingerprint"
            );
        }

        // A different report is a different key.
        let other = crate::report::parse_flow("# collection: c\nREPORT REQUEST Renamed\n").unwrap();
        assert_ne!(base, fingerprint(&cols, &envs, &other), "flow");

        // And so is an environment gaining a variable *name* …
        let mut env = Environment {
            id: 1,
            name: "e".into(),
            vars: Vec::new(),
            path: None,
            git_origin: None,
        };
        env.vars
            .push(crate::environment::EnvVar::user("HOST".into(), "a".into()));
        let with_env = vec![env.clone()];
        let env_key = fingerprint(&cols, &with_env, &flow);
        assert_ne!(base, env_key, "an env variable name must count");

        // … but not that variable merely changing value: validation asks what is
        // in scope, not what it is set to, so re-keying on every keystroke in the
        // environment editor would throw the cache away for nothing.
        let mut quiet = env;
        quiet.vars[0] = crate::environment::EnvVar::user("HOST".into(), "b".into());
        assert_eq!(
            env_key,
            fingerprint(&cols, &[quiet], &flow),
            "a value change must not re-key"
        );
    }
}

#[cfg(test)]
mod helper_loading_tests {
    use super::*;
    use crate::report::parser::parse_flow;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "paperboy_helpers_{tag}_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    const HELPER_HURL: &str = "# fetch_frame\nGET http://example.test/frame\n\n";

    /// The whole point of the feature: the helper is a plain `.hurl` file that
    /// is *not* open anywhere, and it still resolves.
    #[test]
    fn a_helper_is_read_from_disk_relative_to_the_report() {
        let dir = tmpdir("disk");
        std::fs::write(dir.join("h.hurl"), HELPER_HURL).unwrap();
        let report = dir.join("r.trail");
        let flow = parse_flow(
            "# collection: ./api.hurl\n# collection: ./h.hurl AS h\n\nREQUEST h/fetch_frame\n",
        )
        .expect("parses");
        let (helpers, errors) = load_helpers(
            &[],
            &flow,
            Some(report.as_path()),
            crate::i18n::Strings::english(),
        );
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(helpers.len(), 1);
        assert_eq!(helpers[0].alias, "h");
        assert_eq!(helpers[0].entries.len(), 1);
        assert!(crate::report::run::resolve_qualified(&[], &helpers, "h/fetch_frame").is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_helper_file_is_reported_not_silently_empty() {
        let dir = tmpdir("missing");
        let report = dir.join("r.trail");
        let flow =
            parse_flow("# collection: ./api.hurl\n# collection: ./gone.hurl AS h\n\nREQUEST h/x\n")
                .expect("parses");
        let (helpers, errors) = load_helpers(
            &[],
            &flow,
            Some(report.as_path()),
            crate::i18n::Strings::english(),
        );
        assert!(helpers.is_empty());
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].0, "./gone.hurl");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Choices are ordered primary-first, which the request pickers and the
    /// "seed a new REQUEST block" default both rely on.
    #[test]
    fn choices_put_the_primary_collection_first() {
        let primary = [crate::hurl::HurlEntry {
            title: "upload".into(),
            ..Default::default()
        }];
        let helpers = [HelperCollection {
            alias: "h".into(),
            entries: vec![crate::hurl::HurlEntry {
                title: "fetch_frame".into(),
                ..Default::default()
            }],
        }];
        let choices = request_choices(&primary, &helpers);
        assert_eq!(choices[0].qualified, "upload");
        assert_eq!(choices[0].alias, None);
        assert_eq!(choices[1].qualified, "h/fetch_frame");
        assert_eq!(choices[1].title, "fetch_frame");
    }
}
