//! Static validation of a [`ReportFlow`] — run on open/edit and before a run.
//!
//! The parser already guarantees a structurally well-formed flow (balanced
//! `FOR`/`END`, valid syntax, reserved `JOIN`/`ON` rejected). This pass adds the
//! *semantic* checks that need the whole flow (and, when available, the bound
//! collection + loaded environments):
//!
//! - a `collection:` directive is present and (with context) every
//!   `REQUEST`/`REPORT REQUEST` name resolves to exactly one entry;
//! - destructuring arity matches the producer, where statically known;
//! - `LIST` names are unique and referenced only after declaration;
//! - `ENVS` role clauses obey the ≤1 `BASELINE` / ≥1 `COMPARISON` rule and
//!   (with context) name only loaded environments;
//! - `output:` is a supported format.
//!
//! Diagnostics never abort; the caller decides whether any `Error` blocks a run.

use std::collections::{HashMap, HashSet};

use super::flow::{
    EnvClause, FlowNode, OverrideTarget, ParamKind, Pattern, Producer, ReportFlow, ReportStmt,
    RoleRef, ShowField, UsingItem,
};
use crate::i18n::{Strings, fill};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
}

impl Diagnostic {
    fn error(msg: impl Into<String>) -> Self {
        Diagnostic {
            severity: Severity::Error,
            message: msg.into(),
        }
    }
    fn warning(msg: impl Into<String>) -> Self {
        Diagnostic {
            severity: Severity::Warning,
            message: msg.into(),
        }
    }
}

/// What's known about the environment a flow will run in. Both fields are
/// optional: `None` means "not bound yet", so name-resolution checks are
/// skipped (with a single reminder diagnostic) rather than producing noise.
pub struct Context<'a> {
    /// Full entry titles (incl. virtual-folder paths) of the bound collection.
    pub request_titles: Option<&'a [String]>,
    /// Names of environments currently loaded (for `ENVS` resolution).
    pub env_names: Option<&'a [String]>,
    /// Each bound-collection entry's full title paired with its `[Reports]`
    /// field names — used to validate a `SHOW(...)` selector's field list
    /// against the fields the request can actually produce. `None` when no
    /// collection is bound (the check is then skipped).
    pub request_fields: Option<&'a [(String, Vec<String>)]>,
    /// The directory relative paths resolve against (the report's folder or a
    /// `# root:` override). When present, a `# baseline:` snapshot that doesn't
    /// exist on disk is flagged so the user finds out before running rather
    /// than after. `None` skips the filesystem check (e.g. an unsaved report).
    pub root: Option<&'a std::path::Path>,
    /// Variable names that the report's effective base environment provides
    /// (global + pinned, or the `# environment:` override). `None` means the
    /// environment isn't known at validation time — the variable-availability
    /// check is skipped entirely to avoid false positives.
    pub base_var_names: Option<&'a [String]>,
    /// Union of every variable name defined across ALL loaded environments —
    /// used conservatively inside `FOR … IN ENVS` loop bodies, where any of
    /// the named environments may be active so any of their variables is
    /// potentially in scope. `None` skips the check inside ENVS bodies.
    pub all_env_var_names: Option<&'a [String]>,
    /// The bound collection's entries, used to scan each request's `{{VAR}}`
    /// references and to know which names its `[Captures]` block defines after
    /// it runs. `None` (unbound collection) skips the variable-availability
    /// check entirely.
    pub request_entries: Option<&'a [crate::hurl::HurlEntry]>,
    /// Aliased helper collections declared by extra `# collection: … AS x`
    /// directives, already loaded. Names resolve within one of these only when
    /// written `alias/name`, exactly as at run time — validation must agree with
    /// [`super::run::resolve_qualified`] or a report passes here and fails there.
    pub helpers: &'a [super::run::HelperCollection],
    /// Helper collections that were declared but couldn't be read, as
    /// `(reference, reason)`. Flagged on the directive so it's a validation
    /// error rather than a surprise mid-run.
    pub helper_errors: &'a [(String, String)],
    /// The language to phrase diagnostics in. They are user-facing text like
    /// any other, so they live in the `i18n` table rather than as literals
    /// here — a validation message is often the only thing standing between a
    /// user and a report that runs, which is exactly when it must be readable.
    pub strings: &'a Strings,
}

impl Default for Context<'_> {
    /// An empty context in English — every optional check skipped. Hand-written
    /// only because a `&Strings` has no `Default` of its own.
    fn default() -> Self {
        Self {
            request_titles: None,
            env_names: None,
            request_fields: None,
            root: None,
            base_var_names: None,
            all_env_var_names: None,
            request_entries: None,
            helpers: &[],
            helper_errors: &[],
            strings: Strings::english(),
        }
    }
}

/// Whether any step anywhere in the flow (loop bodies included) emits a column.
fn emits_a_column(nodes: &[FlowNode]) -> bool {
    nodes.iter().any(|n| match n {
        FlowNode::Report(_) => true,
        FlowNode::ForEach { body, .. } | FlowNode::ForEnvs { body, .. } => emits_a_column(body),
        _ => false,
    })
}

/// Validate `flow` against `ctx`, returning all diagnostics (errors + warnings).
pub fn validate(flow: &ReportFlow, ctx: &Context) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    let s = ctx.strings;

    // Header: collection binding + output format.
    match flow.header.collection() {
        None => diags.push(Diagnostic::error(s.diag_collection_unset)),
        Some(c) if c.trim().is_empty() => diags.push(Diagnostic::error(s.diag_collection_unset)),
        Some(_) => {}
    }
    check_collection_directives(flow, ctx, &mut diags);
    if let Some(out) = flow.header.output() {
        let out = out.trim();
        if !out.is_empty()
            && !super::writer::OUTPUT_EXTENSIONS
                .iter()
                .any(|e| out.eq_ignore_ascii_case(e))
        {
            diags.push(Diagnostic::error(fill(
                s.diag_output_unsupported,
                &[out, &super::writer::OUTPUT_EXTENSIONS.join(", ")],
            )));
        }
    }
    // Two resolved columns that share the same header collide when the report is
    // written as JSON (row objects are keyed by header, so the later column
    // silently overwrites the earlier one) — a data loss the other formats don't
    // have. Reject a duplicate header up front so every format stays faithful;
    // the fix is to give each column a distinct `AS <name>`.
    if let Some(spec) = flow.header.columns() {
        let cols = super::model::parse_columns(spec);
        let mut seen: Vec<&str> = Vec::new();
        let mut reported: Vec<&str> = Vec::new();
        for header in cols.iter().map(|c| c.header.as_str()) {
            if seen.contains(&header) {
                if !reported.contains(&header) {
                    diags.push(Diagnostic::error(fill(s.diag_duplicate_column, &[&header])));
                    reported.push(header);
                }
            } else {
                seen.push(header);
            }
        }
    }
    // Ground truth: the `# labels:` vocabulary and the `TRUTH` clauses that use
    // it. None of these block a run — a report that scores nothing still runs
    // and still produces its table — but each one is a silent no-op otherwise,
    // and a silently unscored column is exactly what ground truth exists to
    // stop happening.
    let labels = super::labels::LabelMap::parse(&flow.header.labels());
    for line in labels.malformed() {
        diags.push(Diagnostic::warning(fill(s.diag_labels_malformed, &[line])));
    }
    for (synonym, kept, asked) in labels.conflicts() {
        diags.push(Diagnostic::warning(fill(
            s.diag_labels_conflict,
            &[synonym, kept, asked, kept],
        )));
    }
    let images = flow.column_images();
    for (header, template) in flow.column_truths() {
        if template.trim().is_empty() {
            diags.push(Diagnostic::warning(fill(s.diag_truth_empty, &[&header])));
        }
        if images.contains_key(&header) {
            diags.push(Diagnostic::warning(fill(s.diag_truth_on_image, &[&header])));
        }
    }

    // An optional `# environment:` names a single already-loaded environment to
    // use as the report's base variable layer (the plain, no-comparison run).
    // Like an `ENVS` loop, the environment must be loaded — flag it when it
    // isn't (only once the loaded set is known).
    if let Some(env) = flow.header.environment() {
        let env = env.trim();
        if env.is_empty() {
            diags.push(Diagnostic::error(s.diag_environment_unset));
        } else if let Some(loaded) = ctx.env_names
            && !loaded.iter().any(|e| e == env)
        {
            diags.push(Diagnostic::error(fill(
                s.diag_environment_not_loaded,
                &[env],
            )));
        }
    }

    // A `# baseline:` snapshot diff and a live `ENVS BASELINE/COMPARISON`
    // clause both fill the `Result` column; the live comparison takes
    // precedence (see `run::run_flow`), so flag the directive as ignored rather
    // than let it silently do nothing.
    if flow.header.baseline().is_some_and(|b| !b.trim().is_empty())
        && super::compare::comparison_roles(flow).is_some()
    {
        diags.push(Diagnostic::warning(s.diag_baseline_ignored));
    } else if let Some(rel) = flow
        .header
        .baseline()
        .map(str::trim)
        .filter(|b| !b.is_empty())
        && let Some(root) = ctx.root
    {
        // The snapshot will be diffed against at finalize time; a missing file
        // there is only a non-fatal run error, so warn up front (once the
        // report is anchored) that the referenced snapshot can't be found.
        let path = super::producers::resolve_path(Some(root), rel);
        if !path.exists() {
            diags.push(Diagnostic::warning(fill(
                s.diag_baseline_missing,
                &[rel, &path.display().to_string()],
            )));
        }
    }

    // A report whose steps never emit a column runs perfectly and produces an
    // empty table. That is almost always a `REQUEST` that should have been a
    // `REPORT` — the distinction is the first thing a newcomer to the block
    // editor trips over, and nothing else in the UI mentions it.
    if !flow.nodes.is_empty() && !emits_a_column(&flow.nodes) {
        diags.push(Diagnostic::warning(s.diag_no_columns));
    }

    if ctx.request_titles.is_none() {
        diags.push(Diagnostic::warning(s.diag_collection_not_loaded));
    }

    // Parameters are read off the flow without executing it, so they have to
    // be findable: confined to the prelude, uniquely named, and consistent
    // with their own declared type.
    check_params(flow, ctx, &mut diags);

    // An `ENVS` clause may name its environments through parameters, which is
    // how one report compares two stacks this week and two others the next.
    // Those names are judged here rather than in `check_env_clause`, which sees
    // one clause at a time and not the declarations they refer to.
    check_env_refs(flow, ctx, &mut diags);

    // Walk the tree with a scope stack of declared LIST producers.
    let mut scopes: Vec<HashMap<String, Producer>> = vec![HashMap::new()];
    walk(&flow.nodes, ctx, &mut scopes, &mut diags);

    // Variable-availability analysis: walk the flow in execution order and
    // warn when a request references a `{{VAR}}` that is provably not defined
    // at that point. Only runs when both the base-env variable names AND the
    // bound collection's entries are known; if either is absent we can't
    // distinguish "definitely undefined" from "defined by an unknown source"
    // and must stay silent to avoid false positives.
    if ctx.request_entries.is_some() && ctx.base_var_names.is_some() {
        let mut defined = initial_defined_vars(ctx);
        check_var_availability(&flow.nodes, ctx, &mut defined, &mut diags);
    }

    diags
}

/// Whether a node is one of the "does nothing yet" statements a `PARAM` may
/// legally follow: another parameter, a plain assignment (`PRELUDE_*` settings
/// and constants), or a comment.
fn is_prelude_node(node: &FlowNode) -> bool {
    matches!(
        node,
        FlowNode::Param(_) | FlowNode::Assign { .. } | FlowNode::Comment(_)
    )
}

/// Check every `PARAM` in the flow: that it is in the prelude at all (a
/// parameter buried in a loop can never be offered before the run, so it would
/// silently be an ordinary assignment), that no two share a name, and that
/// each default agrees with its declared type.
fn check_params(flow: &ReportFlow, ctx: &Context, diags: &mut Vec<Diagnostic>) {
    let s = ctx.strings;

    // Anything after the first statement that acts can't be offered up front.
    let prelude_end = flow
        .nodes
        .iter()
        .position(|n| !is_prelude_node(n))
        .unwrap_or(flow.nodes.len());
    for p in nested_params(&flow.nodes[prelude_end..]) {
        diags.push(Diagnostic::error(fill(
            s.diag_param_not_in_prelude,
            &[&p.name],
        )));
    }

    let mut seen: HashSet<&str> = HashSet::new();
    // Two parameters that prompt with the same words make a form nobody can
    // fill in correctly, whether the clash is between two `LABEL`s or between
    // two names that read the same once made readable (`TICKET_REF` and
    // `ticket_ref`). It isn't an error — the report still runs, and the names
    // are distinct — but it is always a mistake worth saying out loud.
    let mut prompts: HashSet<String> = HashSet::new();
    for p in flow.params() {
        if !seen.insert(&p.name) {
            diags.push(Diagnostic::error(fill(s.diag_param_duplicate, &[&p.name])));
        }
        let prompt = p.prompt();
        if !prompts.insert(prompt.to_lowercase()) {
            diags.push(Diagnostic::warning(fill(
                s.diag_param_prompt_clash,
                &[&prompt],
            )));
        }
        check_param_default(p, ctx, diags);
    }
}

/// Every `PARAM` anywhere in `nodes`, loop bodies included — used to find the
/// ones that are past the prelude, wherever they are hiding.
fn nested_params<'a>(nodes: &'a [FlowNode]) -> Vec<&'a super::flow::ParamDecl> {
    let mut out = Vec::new();
    for node in nodes {
        match node {
            FlowNode::Param(p) => out.push(p),
            FlowNode::ForEach { body, .. } | FlowNode::ForEnvs { body, .. } => {
                out.extend(nested_params(body));
            }
            _ => {}
        }
    }
    out
}

/// Judge the `ENVS` names that are written as `{{PARAM}}` rather than spelled
/// out. Two things can be said about them without running anything: whether
/// they name a parameter at all (nothing else is resolvable this early — an
/// `ENVS` clause is read before the first request has run, so a capture or an
/// assignment would be a name whose meaning depends on where the run had got
/// to), and, once the parameters' defaults are filled in, whether what they
/// currently mean is loaded. The second is only a warning: changing it per run
/// is the entire point.
fn check_env_refs(flow: &ReportFlow, ctx: &Context, diags: &mut Vec<Diagnostic>) {
    let s = ctx.strings;
    let declared = flow.params();
    let defaults = super::params::effective(&declared, &Default::default());
    for name in env_ref_names(&flow.nodes) {
        for key in crate::environment::referenced_keys(name) {
            if !declared.iter().any(|p| p.name == key) {
                diags.push(Diagnostic::error(fill(
                    s.diag_env_ref_not_a_param,
                    &[&key, name],
                )));
            }
        }
        let resolved = crate::environment::substitute(name, &defaults);
        if resolved.contains("{{") {
            // Still unresolved: a required parameter with no default. What it
            // will be is decided in the run settings, so there is nothing to
            // check here.
            continue;
        }
        if let Some(loaded) = ctx.env_names
            && !loaded.iter().any(|e| *e == resolved)
        {
            diags.push(Diagnostic::warning(fill(
                s.diag_env_ref_default_not_loaded,
                &[name, &resolved],
            )));
        }
    }
}

/// Every live environment name in every `ENVS` clause in the tree that is
/// written through a `{{…}}` reference (a `FILE(…)` role is a path, not an
/// environment, and is left to the snapshot checks).
fn env_ref_names<'a>(nodes: &'a [FlowNode]) -> Vec<&'a String> {
    let mut out = Vec::new();
    for node in nodes {
        match node {
            FlowNode::ForEnvs { clause, body, .. } => {
                let names: Vec<&String> = match clause {
                    EnvClause::Plain(names) => names.iter().collect(),
                    EnvClause::Roles {
                        baseline,
                        comparisons,
                        ..
                    } => baseline
                        .iter()
                        .chain(comparisons.iter())
                        .filter_map(|r| match r {
                            RoleRef::Env(n) => Some(n),
                            RoleRef::File(_) => None,
                        })
                        .collect(),
                };
                out.extend(names.into_iter().filter(|n| n.contains("{{")));
                out.extend(env_ref_names(body));
            }
            FlowNode::ForEach { body, .. } => out.extend(env_ref_names(body)),
            _ => {}
        }
    }
    out
}

fn check_param_default(p: &super::flow::ParamDecl, ctx: &Context, diags: &mut Vec<Diagnostic>) {
    let s = ctx.strings;
    let default = p.default.as_deref().map(str::trim);
    match &p.kind {
        ParamKind::Choice(options) => {
            if options.is_empty() {
                diags.push(Diagnostic::error(fill(s.diag_param_no_choices, &[&p.name])));
            } else if let Some(d) = default.filter(|d| !d.is_empty())
                && !options.iter().any(|o| o == d)
            {
                diags.push(Diagnostic::error(fill(
                    s.diag_param_bad_choice,
                    &[&p.name, d, &options.join(", ")],
                )));
            }
        }
        // A default that interpolates something is checked at run time, when
        // there is something to interpolate; refusing `{{PORT}}` here would
        // reject a perfectly good report.
        ParamKind::Number => {
            if let Some(d) = default.filter(|d| !d.is_empty())
                && !d.contains("{{")
                && d.parse::<f64>().is_err()
            {
                diags.push(Diagnostic::error(fill(
                    s.diag_param_not_a_number,
                    &[&p.name, d],
                )));
            }
        }
        // An environment parameter is *meant* to be changed per run, so a
        // default naming an environment that isn't loaded right now is a
        // warning, not an error — unlike the `# environment:` directive, which
        // is the report's fixed choice.
        ParamKind::Env => {
            if let Some(d) = default.filter(|d| !d.is_empty())
                && !d.contains("{{")
                && let Some(loaded) = ctx.env_names
                && !loaded.iter().any(|e| e == d)
            {
                diags.push(Diagnostic::warning(fill(
                    s.diag_param_env_not_loaded,
                    &[&p.name, d],
                )));
            }
        }
        ParamKind::Text | ParamKind::Folder | ParamKind::File => {}
    }
}

fn walk(
    nodes: &[FlowNode],
    ctx: &Context,
    scopes: &mut Vec<HashMap<String, Producer>>,
    diags: &mut Vec<Diagnostic>,
) {
    for node in nodes {
        match node {
            // Nothing to check in a comment.
            FlowNode::Comment(_) => {}
            FlowNode::Assign { .. } => {}
            // Parameters are checked as a set, before this walk.
            FlowNode::Param(_) => {}
            FlowNode::ListDecl { name, producer } => {
                check_producer(producer, ctx, scopes, diags);
                if scopes.iter().any(|s| s.contains_key(name)) {
                    diags.push(Diagnostic::warning(fill(
                        ctx.strings.diag_list_shadowed,
                        &[name],
                    )));
                }
                scopes
                    .last_mut()
                    .unwrap()
                    .insert(name.clone(), producer.clone());
            }
            FlowNode::Request { name, using } => {
                check_request_name(name, ctx, diags);
                check_using(name, using, ctx, diags);
            }
            FlowNode::Report(stmt) => check_report(stmt, ctx, diags),
            FlowNode::ForEach {
                pattern,
                producer,
                body,
                ..
            } => {
                check_producer(producer, ctx, scopes, diags);
                check_arity(pattern, producer, scopes, ctx.strings, diags);
                scopes.push(HashMap::new());
                walk(body, ctx, scopes, diags);
                scopes.pop();
            }
            FlowNode::ForEnvs { clause, body, .. } => {
                check_env_clause(clause, ctx, diags);
                scopes.push(HashMap::new());
                walk(body, ctx, scopes, diags);
                scopes.pop();
            }
        }
    }
}

fn check_report(stmt: &ReportStmt, ctx: &Context, diags: &mut Vec<Diagnostic>) {
    if let ReportStmt::Request {
        name,
        using,
        show,
        hide,
        with,
        ..
    } = stmt
    {
        check_request_name(name, ctx, diags);
        check_using(name, using, ctx, diags);
        check_show_hide_overlap(show, hide, ctx.strings, diags);
        check_show_fields(name, show, with, ctx, diags);
        check_hide_fields(name, hide, with, ctx, diags);
    }
}

/// Check a `USING(…)` clause against the request it names.
///
/// This is the check the clause exists for. A flow binds a request's parameters
/// just by having them in scope, which reads well but means a flow copied onto
/// a collection whose `upload_document` was never parameterised still *runs* —
/// sending the request's own hardcoded value and reporting a healthy `200`.
/// `USING(FILE)` states the requirement, and this turns a mismatch into an
/// error the moment the report is opened, quoting what the request does
/// declare so the fix is obvious.
///
/// Every check is skipped when the request can't be resolved (its name is
/// already being reported) or the collection isn't bound — validation never
/// invents a complaint out of what it can't see.
fn check_using(name: &str, using: &[UsingItem], ctx: &Context, diags: &mut Vec<Diagnostic>) {
    let Some(entries) = ctx.request_entries else {
        return;
    };
    let Some(entry) = super::run::resolve_qualified(entries, ctx.helpers, name) else {
        return;
    };
    let s = ctx.strings;

    for item in using {
        match item {
            UsingItem::Require(param) => {
                if entry.declares_variable(param) {
                    continue;
                }
                let declared = entry.variable_defaults();
                let declared = if declared.is_empty() {
                    s.diag_using_none.to_string()
                } else {
                    declared
                        .iter()
                        .map(|(n, _)| n.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                diags.push(Diagnostic::error(fill(
                    s.diag_using_undeclared,
                    &[name, param, &declared, param],
                )));
            }
            // Only the payload-shaped targets are checked. A `header`/`query`/
            // `cookie`/`option` override upserts by design (adding a header the
            // request never had is a normal thing to want), so there is nothing
            // to be wrong about; a `form`/`multipart` field that doesn't exist
            // is a typo that would otherwise send a subtly wrong body.
            UsingItem::Override { target, .. } => match target {
                OverrideTarget::Form(k) | OverrideTarget::Multipart(k) => {
                    if entry.form_fields.iter().any(|f| &f.key == k) {
                        continue;
                    }
                    let known: Vec<&str> =
                        entry.form_fields.iter().map(|f| f.key.as_str()).collect();
                    let known = if known.is_empty() {
                        s.diag_using_no_fields.to_string()
                    } else {
                        fill(s.diag_using_has_fields, &[&known.join(", ")])
                    };
                    let section = match target {
                        OverrideTarget::Form(_) => "form",
                        _ => "multipart",
                    };
                    diags.push(Diagnostic::error(fill(
                        s.diag_using_no_such_field,
                        &[name, section, k, &known],
                    )));
                }
                // Hurl builds a body and a form onto the same libcurl handle,
                // so the pair is unsendable — see `BODY_FORM_CONFLICT_ERROR`.
                // Overriding the body of a form request creates exactly that
                // pair, silently, which is worth catching here rather than at
                // the send.
                OverrideTarget::Body if entry.form_fields.iter().any(|f| f.enabled) => {
                    diags.push(Diagnostic::error(fill(
                        s.diag_using_body_form_conflict,
                        &[name],
                    )));
                }
                _ => {}
            },
        }
    }

    // The nudge: a request that declares parameters is meant to be driven, and
    // a statement that drives one without saying so is the copy-paste hazard
    // this feature is about. A warning rather than an error — the flow is
    // correct *here*, it just won't complain when it stops being.
    let required: Vec<&str> = using
        .iter()
        .filter_map(|i| match i {
            UsingItem::Require(p) => Some(p.as_str()),
            _ => None,
        })
        .collect();
    if required.is_empty() {
        let declared: Vec<String> = entry
            .variable_defaults()
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        if !declared.is_empty() {
            diags.push(Diagnostic::warning(fill(
                s.diag_using_parameter_not_required,
                &[name, &declared.join(", ")],
            )));
        }
    }
}

/// Warn when a `SHOW(...)` field can't be produced by the request: it is
/// neither an intrinsic (`HttpStatus`/`Time`/`Asserts`/`Error`/`Response`), a
/// `WITH` field on this statement, nor a `[Reports]` field of the resolved
/// request.  Under the additive model such a field is silently ignored at
/// runtime (it will not appear in the output), so this is a warning rather than
/// an error — consistent with how `check_hide_fields` handles unknown fields.
/// Skipped when the collection isn't bound (the field set is unknown), so it
/// never false-warns on a real `[Reports]` field we can't see.
fn check_show_fields(
    name: &str,
    show: &[ShowField],
    with: &[super::flow::WithItem],
    ctx: &Context,
    diags: &mut Vec<Diagnostic>,
) {
    if show.is_empty() {
        return;
    }
    // An unresolved name is already reported by `check_request_name`, so bail
    // quietly here.
    let Some(report_fields) = declared_report_fields(name, ctx) else {
        return;
    };
    let report_fields = &report_fields;
    let with_fields: Vec<&str> = with
        .iter()
        .filter_map(|w| match w {
            super::flow::WithItem::Field { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    for field in show {
        let field = field.name();
        let known = super::run::INTRINSIC_FIELDS.contains(&field)
            || with_fields.contains(&field)
            || report_fields.iter().any(|f| f == field);
        if !known {
            diags.push(Diagnostic::warning(fill(
                ctx.strings.diag_show_unknown,
                &[field, name],
            )));
        }
    }
}

/// Error when the same field suffix appears in both SHOW and HIDE — the two
/// clauses are contradictory (SHOW keeps, HIDE removes) and no ordering of
/// evaluation resolves the conflict sensibly.
fn check_show_hide_overlap(
    show: &[ShowField],
    hide: &[String],
    s: &Strings,
    diags: &mut Vec<Diagnostic>,
) {
    for field in show {
        let field = field.name();
        if hide.iter().any(|h| h == field) {
            diags.push(Diagnostic::error(fill(s.diag_show_hide_conflict, &[field])));
        }
    }
}

/// Warn when a `HIDE(...)` field can't be produced by the request (mirrors
/// `check_show_fields`): it is neither an intrinsic, a WITH field, nor a
/// `[Reports]` field of the resolved request. Skipped when the collection isn't
/// bound (the field set is unknown).
fn check_hide_fields(
    name: &str,
    hide: &[String],
    with: &[super::flow::WithItem],
    ctx: &Context,
    diags: &mut Vec<Diagnostic>,
) {
    if hide.is_empty() {
        return;
    }
    let Some(report_fields) = declared_report_fields(name, ctx) else {
        return;
    };
    let resolved = Some((String::new(), report_fields));
    let Some((_, report_fields)) = resolved else {
        return;
    };
    let with_fields: Vec<&str> = with
        .iter()
        .filter_map(|w| match w {
            super::flow::WithItem::Field { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    for field in hide {
        let known = super::run::INTRINSIC_FIELDS.contains(&field.as_str())
            || with_fields.contains(&field.as_str())
            || report_fields.iter().any(|f| f == field);
        if !known {
            diags.push(Diagnostic::warning(fill(
                ctx.strings.diag_hide_unknown,
                &[field, name],
            )));
        }
    }
}

/// Resolve a request name against the bound collection's titles: exact
/// full-title → unique leaf name → error. Skipped when no collection is bound.
/// Split a `alias/name` reference when `alias` is a *declared* helper. Returns
/// the helper and the name within it. A `/` that isn't a declared alias is left
/// alone — it is far more likely a virtual-folder path.
fn split_helper<'a>(
    name: &'a str,
    ctx: &'a Context,
) -> Option<(&'a super::run::HelperCollection, &'a str)> {
    let (alias, rest) = name.split_once('/')?;
    ctx.helpers
        .iter()
        .find(|h| h.alias == alias)
        .map(|h| (h, rest))
}

/// The `# collection:` directives: exactly one primary (unaliased, first), every
/// helper aliased, aliases distinct identifiers that don't collide with a
/// top-level virtual folder, and every declared helper actually loadable.
fn check_collection_directives(flow: &ReportFlow, ctx: &Context, diags: &mut Vec<Diagnostic>) {
    let s = ctx.strings;
    let refs = flow.header.collections();
    let mut seen: Vec<&str> = Vec::new();
    for (i, c) in refs.iter().enumerate() {
        if i == 0 {
            // The primary is *the* collection under test, not a helper; an alias
            // on it would suggest its requests are addressed `alias/name`, which
            // they aren't.
            if c.alias.is_some() {
                diags.push(Diagnostic::error(s.diag_collection_primary_alias));
            }
            continue;
        }
        let Some(alias) = c.alias else {
            diags.push(Diagnostic::error(fill(
                s.diag_collection_alias_missing,
                &[c.reference],
            )));
            continue;
        };
        if !crate::report::parser::is_ident(alias) {
            diags.push(Diagnostic::error(fill(
                s.diag_collection_alias_invalid,
                &[alias],
            )));
            continue;
        }
        if seen.contains(&alias) {
            diags.push(Diagnostic::error(fill(
                s.diag_collection_alias_duplicate,
                &[alias],
            )));
            continue;
        }
        seen.push(alias);
        // `/` is also the virtual-folder separator, so an alias sharing a name
        // with a top-level folder makes `folder/request` ambiguous. PaperTrail
        // never picks silently between two readings of a name, so this is an
        // error rather than a precedence rule.
        if let Some(titles) = ctx.request_titles
            && titles
                .iter()
                .any(|t| t.split('/').next() == Some(alias) && t.contains('/'))
        {
            diags.push(Diagnostic::error(fill(
                s.diag_collection_alias_shadows_folder,
                &[alias],
            )));
        }
    }
    for (reference, reason) in ctx.helper_errors {
        diags.push(Diagnostic::error(fill(
            s.diag_collection_helper_unreadable,
            &[reference, reason],
        )));
    }
}

fn check_request_name(name: &str, ctx: &Context, diags: &mut Vec<Diagnostic>) {
    if let Some((helper, rest)) = split_helper(name, ctx) {
        let titles: Vec<String> = helper.entries.iter().map(|e| e.title.clone()).collect();
        check_name_in(rest, &titles, name, ctx, diags);
        return;
    }
    let Some(titles) = ctx.request_titles else {
        return;
    };
    check_name_in(name, titles, name, ctx, diags);
}

/// The shared exact-title → unique-leaf → error ladder. `shown` is the name as
/// the user wrote it (alias included), so a diagnostic quotes their own text.
fn check_name_in(
    name: &str,
    titles: &[String],
    shown: &str,
    ctx: &Context,
    diags: &mut Vec<Diagnostic>,
) {
    let exact = titles.iter().filter(|t| t.as_str() == name).count();
    if exact == 1 {
        return;
    }
    if exact > 1 {
        diags.push(Diagnostic::error(fill(
            ctx.strings.diag_request_ambiguous_title,
            &[shown, &exact.to_string()],
        )));
        return;
    }
    // No exact full-title match: try a unique leaf (last '/'-segment) match.
    let leaves: Vec<&String> = titles
        .iter()
        .filter(|t| t.rsplit('/').next() == Some(name))
        .collect();
    match leaves.len() {
        1 => {}
        0 => diags.push(Diagnostic::error(fill(
            ctx.strings.diag_request_not_found,
            &[shown],
        ))),
        n => diags.push(Diagnostic::error(fill(
            ctx.strings.diag_request_ambiguous_leaf,
            &[shown, &n.to_string()],
        ))),
    }
}

fn check_env_clause(clause: &EnvClause, ctx: &Context, diags: &mut Vec<Diagnostic>) {
    let names: Vec<&String> = match clause {
        EnvClause::Plain(names) => {
            if names.is_empty() {
                diags.push(Diagnostic::error(ctx.strings.diag_envs_empty));
            }
            names.iter().collect()
        }
        EnvClause::Roles {
            baseline,
            comparisons,
            ..
        } => {
            if baseline.len() > 1 {
                diags.push(Diagnostic::error(ctx.strings.diag_baseline_multiple));
            }
            if comparisons.is_empty() {
                diags.push(Diagnostic::error(ctx.strings.diag_comparison_missing));
            }
            // A `FILE(…)` snapshot stands in for a live run of its role, so a
            // path that isn't there means the role produces nothing — which
            // surfaces at run time only as an unmatched comparison, long after
            // the run has been paid for. Warn up front instead, exactly as the
            // `# baseline:` directive does, and for the same reason: it is only
            // a non-fatal error at run time, and the check needs the report to
            // be anchored (`ctx.root`) before a relative path means anything.
            if let Some(root) = ctx.root {
                for rel in baseline
                    .iter()
                    .chain(comparisons.iter())
                    .filter_map(|r| match r {
                        RoleRef::File(p) => Some(p),
                        RoleRef::Env(_) => None,
                    })
                {
                    let path = super::producers::resolve_path(Some(root), rel);
                    if !path.exists() {
                        diags.push(Diagnostic::warning(fill(
                            ctx.strings.diag_baseline_missing,
                            &[rel, &path.display().to_string()],
                        )));
                    }
                }
            }
            // Only live env-name refs are checked against the loaded set; a
            // `FILE(…)` snapshot is a path, not an environment.
            baseline
                .iter()
                .chain(comparisons.iter())
                .filter_map(|r| match r {
                    RoleRef::Env(n) => Some(n),
                    RoleRef::File(_) => None,
                })
                .collect()
        }
    };
    if let Some(loaded) = ctx.env_names {
        for n in names {
            // A name written through a parameter isn't an environment name yet
            // — `check_env_refs` judges those against what the parameter can
            // actually become.
            if n.contains("{{") {
                continue;
            }
            if !loaded.iter().any(|e| e == n) {
                diags.push(Diagnostic::error(fill(
                    ctx.strings.diag_environment_not_loaded,
                    &[n],
                )));
            }
        }
    }
}

/// The static element arity of a producer, if knowable without touching the
/// filesystem. `None` = runtime-determined (e.g. `TUPLES FROM` a CSV).
fn producer_arity(p: &Producer, scopes: &[HashMap<String, Producer>]) -> Option<usize> {
    match p {
        // A folder path (roles are accessed by name, not destructured).
        Producer::Files { .. } | Producer::Folders { .. } => Some(1),
        Producer::Zip(ps) => Some(ps.len()),
        Producer::Concat(ps) => {
            // CONCAT preserves arity: it appends items, it doesn't widen them.
            // The whole is knowable only when every input's arity is known and
            // they all agree (a disagreement is reported by check_producer).
            let arities: Vec<usize> = ps
                .iter()
                .filter_map(|p| producer_arity(p, scopes))
                .collect();
            if arities.len() != ps.len() {
                return None;
            }
            match arities.first() {
                Some(&first) if arities.iter().all(|&a| a == first) => Some(first),
                _ => None,
            }
        }
        Producer::Tuples { .. } => None,
        Producer::List(elems) => {
            let arities: Vec<usize> = elems
                .iter()
                .map(|e| match e {
                    super::flow::Element::Scalar(_) => 1,
                    super::flow::Element::Tuple(items) => items.len(),
                })
                .collect();
            match arities.first() {
                None => Some(1),
                Some(&first) if arities.iter().all(|&a| a == first) => Some(first),
                // Inconsistent — reported by check_arity via the mismatch below.
                _ => None,
            }
        }
        Producer::Named(name) => scopes
            .iter()
            .rev()
            .find_map(|s| s.get(name))
            .and_then(|inner| producer_arity(inner, scopes)),
    }
}

fn check_arity(
    pattern: &Pattern,
    producer: &Producer,
    scopes: &[HashMap<String, Producer>],
    s: &Strings,
    diags: &mut Vec<Diagnostic>,
) {
    let Some(arity) = producer_arity(producer, scopes) else {
        return; // Runtime-determined (or inconsistent — flagged by check_producer).
    };
    let binders = pattern.binders.len();
    if pattern.rest {
        if binders > arity {
            diags.push(Diagnostic::error(fill(
                s.diag_pattern_before_rest,
                &[&binders.to_string(), &arity.to_string()],
            )));
        }
    } else if binders != arity {
        diags.push(Diagnostic::error(fill(
            s.diag_pattern_arity,
            &[&binders.to_string(), &arity.to_string()],
        )));
    }
}

fn check_producer(
    producer: &Producer,
    ctx: &Context,
    scopes: &[HashMap<String, Producer>],
    diags: &mut Vec<Diagnostic>,
) {
    if let Producer::Named(name) = producer
        && !scopes.iter().rev().any(|s| s.contains_key(name))
    {
        diags.push(Diagnostic::error(fill(
            ctx.strings.diag_unknown_list,
            &[name, name],
        )));
    }
    // Inconsistent list-literal arity (a mix of scalars/tuples of different
    // sizes) is caught wherever the literal appears — a `LIST` declaration or an
    // inline `FOR … IN [ … ]` — so it surfaces at its definition site.
    if let Producer::List(elems) = producer {
        let arities: Vec<usize> = elems
            .iter()
            .map(|e| match e {
                super::flow::Element::Scalar(_) => 1,
                super::flow::Element::Tuple(items) => items.len(),
            })
            .collect();
        if let Some(&first) = arities.first()
            && !arities.iter().all(|&a| a == first)
        {
            diags.push(Diagnostic::error(ctx.strings.diag_list_arity));
        }
    }
    if let Producer::Zip(ps) = producer {
        for p in ps {
            check_producer(p, ctx, scopes, diags);
        }
    }
    if let Producer::Concat(ps) = producer {
        for p in ps {
            check_producer(p, ctx, scopes, diags);
        }
        // All inputs must yield items of the same arity, else the loop pattern
        // can't destructure them uniformly. Only flag when statically knowable.
        let arities: Vec<usize> = ps
            .iter()
            .filter_map(|p| producer_arity(p, scopes))
            .collect();
        if let Some(&first) = arities.first()
            && arities.len() == ps.len()
            && !arities.iter().all(|&a| a == first)
        {
            diags.push(Diagnostic::error(ctx.strings.diag_concat_arity));
        }
    }
}

// ---------------------------------------------------------------------------
// Variable-availability analysis
// ---------------------------------------------------------------------------

/// Build the initial set of variable names available before the first
/// statement executes: the base environment's keys plus the engine's
/// built-in `PRELUDE_*` names (which always have defaults, so a request
/// that references one is never provably undefined).
fn initial_defined_vars(ctx: &Context) -> HashSet<String> {
    let mut defined = HashSet::new();
    if let Some(names) = ctx.base_var_names {
        defined.extend(names.iter().cloned());
    }
    // Engine defaults — any flow can reference these without an explicit
    // assignment and they will always resolve.
    for name in [
        "PRELUDE_NO_MATCH_MARKER",
        "PRELUDE_RESPONSE_FORMAT",
        "PRELUDE_MAX_PARALLEL",
    ] {
        defined.insert(name.to_string());
    }
    defined
}

/// Resolve a request name against the bound entries — same leaf/exact logic
/// as [`check_request_name`] — returning the first matching entry, or `None`
/// for an ambiguous/missing name (those cases are already reported by the
/// structural walk; here we silently skip to avoid double-reporting).
/// The `[Reports]` field names a request can declare, alias-aware: exact full
/// title, then a unique leaf match, within the collection the name addresses.
fn declared_report_fields(name: &str, ctx: &Context) -> Option<Vec<String>> {
    if let Some((helper, rest)) = split_helper(name, ctx) {
        return resolve_entry_by_name(&helper.entries, rest)
            .map(|e| e.reports.iter().map(|(n, _)| n.clone()).collect());
    }
    let entries = ctx.request_fields?;
    let by_exact = entries.iter().find(|(t, _)| t == name);
    by_exact
        .or_else(|| {
            let mut leaves = entries
                .iter()
                .filter(|(t, _)| t.rsplit('/').next() == Some(name));
            match (leaves.next(), leaves.next()) {
                (Some(hit), None) => Some(hit),
                _ => None,
            }
        })
        .map(|(_, f)| f.clone())
}

/// The entry a name addresses, alias-aware — the validation-side twin of
/// [`super::run::resolve_qualified`].
fn resolve_entry_qualified<'a>(
    name: &'a str,
    ctx: &'a Context,
) -> Option<&'a crate::hurl::HurlEntry> {
    if let Some((helper, rest)) = split_helper(name, ctx) {
        return resolve_entry_by_name(&helper.entries, rest);
    }
    resolve_entry_by_name(ctx.request_entries?, name)
}

fn resolve_entry_by_name<'a>(
    entries: &'a [crate::hurl::HurlEntry],
    name: &str,
) -> Option<&'a crate::hurl::HurlEntry> {
    let exact: Vec<_> = entries.iter().filter(|e| e.title == name).collect();
    if exact.len() == 1 {
        return Some(exact[0]);
    }
    if exact.len() > 1 {
        return None; // ambiguous
    }
    let leaves: Vec<_> = entries
        .iter()
        .filter(|e| e.title.rsplit('/').next() == Some(name))
        .collect();
    if leaves.len() == 1 {
        Some(leaves[0])
    } else {
        None
    }
}

/// The named fields a producer binds by name (not position) — specifically
/// the role names in a `FOLDERS … WITH role="glob", …` producer. These bind
/// directly into the loop scope like `FOR (A, B) IN …` would bind `A` and `B`,
/// so they must be treated as defined inside the loop body.
fn producer_static_named_fields(producer: &Producer) -> Vec<String> {
    match producer {
        Producer::Folders { roles, .. } => roles.iter().map(|r| r.name.clone()).collect(),
        // ZIP/CONCAT: union the named fields from all sub-producers.
        Producer::Zip(ps) | Producer::Concat(ps) => {
            ps.iter().flat_map(producer_static_named_fields).collect()
        }
        _ => Vec::new(),
    }
}

/// Emit a warning for each `{{VAR}}` that `name`'s request references but
/// that isn't in `defined` at the call site. Silently skips unresolvable
/// request names (already reported by the structural walk).
fn warn_if_vars_undefined(
    name: &str,
    ctx: &Context,
    defined: &HashSet<String>,
    diags: &mut Vec<Diagnostic>,
) {
    if ctx.request_entries.is_none() {
        return;
    }
    let Some(entry) = resolve_entry_qualified(name, ctx) else {
        return; // unresolvable — structural check already warned
    };
    let refs = crate::request::entry_referenced_keys(entry);
    // Sorted, because `entry_referenced_keys` hands back a `HashSet` and its
    // iteration order differs from one instance to the next. Emitting warnings
    // straight out of it made the validation panel's contents reshuffle every
    // time it was rebuilt, so a request with several unset variables flickered.
    // Alphabetical is also simply the more useful order to read them in.
    let mut refs: Vec<&String> = refs.iter().collect();
    refs.sort();
    // A name the request declares in its own `[Options]` can never be unset:
    // the declared value is a default, and the whole point of a request
    // parameter is that the request still works when nobody binds it. Warning
    // about those made a properly parameterised collection noisier than an
    // unparameterised one, which is exactly backwards.
    let declared: HashSet<String> = entry
        .variable_defaults()
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    for var in refs {
        if !defined.contains(var.as_str()) && !declared.contains(var.as_str()) {
            diags.push(Diagnostic::warning(fill(
                ctx.strings.diag_var_maybe_undefined,
                &[name, &format!("{{{{{var}}}}}")],
            )));
        }
    }
}

/// Thread the capture names of a successfully-resolved request into `defined`
/// so that subsequent requests in the same block can use them.
fn add_entry_captures(name: &str, ctx: &Context, defined: &mut HashSet<String>) {
    if ctx.request_entries.is_none() {
        return;
    }
    let Some(entry) = resolve_entry_qualified(name, ctx) else {
        return;
    };
    for (cap_name, _) in &entry.captures {
        defined.insert(cap_name.clone());
    }
}

/// Walk `nodes` in execution order, maintaining `defined` (the set of
/// variable names provably in scope), and emit a warning for every `{{VAR}}`
/// in a request that isn't covered by any in-scope source.
///
/// Conservative design: when a scope source can't be statically enumerated
/// (e.g. `TUPLES FROM` column names, or a `FOR … IN ENVS` body when the
/// loaded env variable names aren't known), we skip that scope entirely and
/// produce no warnings — under-warning is far better than a false positive.
fn check_var_availability(
    nodes: &[FlowNode],
    ctx: &Context,
    defined: &mut HashSet<String>,
    diags: &mut Vec<Diagnostic>,
) {
    for node in nodes {
        match node {
            // A comment defines nothing and uses nothing.
            FlowNode::Comment(_) => {}
            // An assignment defines the key for all subsequent nodes.
            FlowNode::Assign { key, .. } => {
                defined.insert(key.clone());
            }
            // A parameter always has a value by the time anything runs —
            // its default, or whatever the run settings supplied instead.
            FlowNode::Param(p) => {
                defined.insert(p.name.clone());
            }
            FlowNode::ListDecl { .. } => {}
            // A bare REQUEST (no report output) — check its vars, then thread
            // its captures forward.
            FlowNode::Request { name, .. } => {
                warn_if_vars_undefined(name, ctx, defined, diags);
                add_entry_captures(name, ctx, defined);
            }
            // A REPORT statement — only the REQUEST form sends HTTP.
            FlowNode::Report(stmt) => {
                if let ReportStmt::Request { name, .. } = stmt {
                    warn_if_vars_undefined(name, ctx, defined, diags);
                    add_entry_captures(name, ctx, defined);
                }
            }
            // A FOR loop over a producer: pattern binders and any named fields
            // (FOLDERS roles, TUPLES headers when statically unknown are left
            // out — they're runtime-determined, so we err on the side of not
            // warning). The loop body runs with a snapshot of `defined` plus
            // those new names; changes inside the body don't leak outward.
            FlowNode::ForEach {
                pattern,
                producer,
                body,
                ..
            } => {
                let mut inner = defined.clone();
                for binder_name in pattern.named() {
                    inner.insert(binder_name.to_string());
                }
                // FOLDERS roles are known statically and bind by name.
                for fname in producer_static_named_fields(producer) {
                    inner.insert(fname);
                }
                // TUPLES FROM / ZIP / CONCAT may also yield named fields at
                // runtime (CSV headers, etc.) — we can't enumerate them here,
                // so we don't add them. This means we may miss some true
                // negatives inside TUPLES loops, but we'll never false-positive.
                check_var_availability(body, ctx, &mut inner, diags);
            }
            // A FOR … IN ENVS loop: the loop variable is in scope, and each
            // iteration's environment also makes its variables available.
            // We add the union of ALL loaded env vars so we don't false-warn
            // inside the body regardless of which env is active. If the loaded
            // env variable names are unknown (`all_env_var_names` is None) we
            // skip the body entirely to stay conservative.
            FlowNode::ForEnvs { var, body, .. } => {
                let mut inner = defined.clone();
                inner.insert(var.clone());
                if let Some(env_vars) = ctx.all_env_var_names {
                    inner.extend(env_vars.iter().cloned());
                    check_var_availability(body, ctx, &mut inner, diags);
                }
                // If all_env_var_names is None, skip the body — we can't know
                // what the environment will provide, so no warnings here.
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::parser::parse_flow;

    /// Validate a flow parsed from source under an optional collection/env
    /// context. Returns all diagnostics.
    fn diags_for(src: &str, titles: Option<&[String]>, envs: Option<&[String]>) -> Vec<Diagnostic> {
        let flow = parse_flow(src).expect("test source should parse");
        let ctx = Context {
            request_titles: titles,
            env_names: envs,
            ..Default::default()
        };
        validate(&flow, &ctx)
    }

    /// Validate a flow against real entries — needed for anything that checks a
    /// request's own shape (its declared parameters, its form fields).
    fn diags_with_entries(src: &str, entries: &[crate::hurl::HurlEntry]) -> Vec<Diagnostic> {
        let flow = parse_flow(src).expect("test source should parse");
        let titles: Vec<String> = entries.iter().map(|e| e.title.clone()).collect();
        let ctx = Context {
            request_titles: Some(&titles),
            request_entries: Some(entries),
            ..Default::default()
        };
        validate(&flow, &ctx)
    }

    /// A request declaring the parameter, for the `USING` checks.
    fn upload_entry(declares: &[&str], fields: &[&str]) -> crate::hurl::HurlEntry {
        crate::hurl::HurlEntry {
            title: "upload".into(),
            method: "POST".into(),
            url: "http://x".into(),
            options: declares
                .iter()
                .map(|n| crate::hurl::KvRow::new("variable", format!("{n}=./sample.pdf")))
                .collect(),
            form_fields: fields
                .iter()
                .map(|k| crate::hurl::FormField {
                    key: (*k).to_string(),
                    enabled: true,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    /// The whole point of the clause: the same flow text that works against a
    /// parameterised collection fails *loudly* against one that isn't, instead
    /// of silently sending the request's own hardcoded value.
    #[test]
    fn a_required_parameter_the_request_does_not_declare_is_an_error() {
        let src = "# collection: c\n\nREQUEST upload USING(FILE)\n";

        let ok = diags_with_entries(src, &[upload_entry(&["FILE"], &[])]);
        assert!(
            !ok.iter().any(|d| d.severity == Severity::Error),
            "declared: {ok:?}"
        );

        let bad = diags_with_entries(src, &[upload_entry(&[], &[])]);
        let msg = bad
            .iter()
            .find(|d| d.severity == Severity::Error)
            .map(|d| d.message.clone())
            .unwrap_or_default();
        assert!(
            msg.contains("does not declare a parameter 'FILE'") && msg.contains("declares: none"),
            "the error names the fix: {bad:?}"
        );
    }

    /// The nudge that moves people onto the clause: a request built to be
    /// driven, driven without saying so, is the copy-paste hazard. A warning,
    /// not an error — the flow is correct here, it just won't stay correct.
    #[test]
    fn driving_a_parameterised_request_without_using_is_a_warning() {
        let diags = diags_with_entries(
            "# collection: c\n\nREPORT REQUEST upload\n",
            &[upload_entry(&["FILE"], &[])],
        );

        assert!(
            diags
                .iter()
                .any(|d| d.severity == Severity::Warning && d.message.contains("USING")),
            "{diags:?}"
        );
    }

    /// A request that declares nothing is not nagged — the warning only fires
    /// where there is something to require.
    #[test]
    fn an_ordinary_request_is_not_nudged() {
        let diags = diags_with_entries(
            "# collection: c\n\nREQUEST upload\n",
            &[upload_entry(&[], &[])],
        );

        assert!(
            !diags.iter().any(|d| d.message.contains("USING")),
            "{diags:?}"
        );
    }

    /// A mistyped `multipart` field is caught before the run: applied, it would
    /// invent a field and send a body the request's author never designed.
    #[test]
    fn an_override_of_a_field_the_request_lacks_is_an_error() {
        let diags = diags_with_entries(
            "# collection: c\n\nREQUEST upload USING(multipart.fil = \"x\")\n",
            &[upload_entry(&[], &["file"])],
        );

        assert!(
            diags.iter().any(|d| d.severity == Severity::Error
                && d.message.contains("no multipart field 'fil'")
                && d.message.contains("it has: file")),
            "{diags:?}"
        );
    }

    /// Adding a header the request never had is normal and unremarkable, so an
    /// upserting target is never flagged.
    #[test]
    fn an_override_of_an_absent_header_is_fine() {
        let diags = diags_with_entries(
            "# collection: c\n\nREQUEST upload USING(header.X-Run = \"7\")\n",
            &[upload_entry(&[], &["file"])],
        );

        assert!(
            !diags.iter().any(|d| d.severity == Severity::Error),
            "{diags:?}"
        );
    }

    /// Overriding the body of a form request would produce the one shape Hurl
    /// cannot send (body + form on the same handle), so it is caught here
    /// rather than at the send.
    #[test]
    fn overriding_the_body_of_a_form_request_is_an_error() {
        let diags = diags_with_entries(
            "# collection: c\n\nREQUEST upload USING(body = \"{}\")\n",
            &[upload_entry(&[], &["file"])],
        );

        assert!(
            diags
                .iter()
                .any(|d| d.severity == Severity::Error
                    && d.message.contains("cannot be sent together")),
            "{diags:?}"
        );
    }

    /// The clause that motivated parameters in the first place: which two
    /// stacks to compare, chosen per run. The names are references, so the
    /// "not loaded" check has to look at what they mean rather than at what
    /// they say.
    #[test]
    fn an_environment_named_by_a_parameter_is_not_an_unloaded_environment() {
        let src = "# collection: c\nPARAM COMPARE_ENV = \"api_dev\"\nPARAM BASELINE_ENV = \"api_staging\"\n\
                   PARALLEL(2) FOR TARGET IN ENVS BASELINE(\"{{BASELINE_ENV}}\") SHOW(TimeWait), COMPARISON(\"{{COMPARE_ENV}}\")\n\
                       REPORT REQUEST r\nEND\n";
        let envs = ["api_dev".to_string(), "api_staging".to_string()];
        let msgs: Vec<String> = diags_for(src, None, Some(&envs))
            .into_iter()
            .map(|d| d.message)
            .collect();
        assert!(
            !msgs.iter().any(|m| m.contains("is not loaded")),
            "the parameters resolve to loaded environments: {msgs:?}"
        );
    }

    /// What the defaults currently mean is still checked — but only as a
    /// warning, because being changed per run is the entire point.
    #[test]
    fn a_parameter_pointing_at_an_unloaded_environment_is_only_a_warning() {
        let src = "# collection: c\nPARAM TARGET_ENV = \"gone\"\n\
                   FOR T IN ENVS \"{{TARGET_ENV}}\"\n    REPORT REQUEST r\nEND\n";
        let envs = ["here".to_string()];
        let diags = diags_for(src, None, Some(&envs));
        assert!(
            !diags
                .iter()
                .any(|d| d.severity == Severity::Error && d.message.contains("gone")),
            "not an error: {diags:?}"
        );
        assert!(
            diags
                .iter()
                .any(|d| d.severity == Severity::Warning && d.message.contains("gone")),
            "but it does say so: {diags:?}"
        );
    }

    /// An `ENVS` clause is read before anything has run, so a name it reaches
    /// for has to be a parameter — a capture or an assignment would mean
    /// something different depending on where the run had got to.
    #[test]
    fn an_environment_reference_that_isnt_a_parameter_is_refused() {
        let errs = errors_for(
            "# collection: c\nTARGET_ENV = \"staging\"\n\
             FOR T IN ENVS \"{{TARGET_ENV}}\"\n    REPORT REQUEST r\nEND\n",
        );
        assert!(
            errs.iter().any(|e| e.contains("TARGET_ENV")),
            "says which name and that it needs declaring: {errs:?}"
        );
    }

    /// A required parameter has nothing to check against yet: what it will name
    /// is decided in the run settings, so validation stays quiet rather than
    /// guessing.
    #[test]
    fn an_environment_named_by_an_unanswered_parameter_is_left_alone() {
        let src = "# collection: c\nPARAM ENV TARGET_ENV\n\
                   FOR T IN ENVS \"{{TARGET_ENV}}\"\n    REPORT REQUEST r\nEND\n";
        let envs = ["here".to_string()];
        let diags = diags_for(src, None, Some(&envs));
        assert!(
            !diags.iter().any(|d| d.message.contains("TARGET_ENV")),
            "nothing to say yet: {diags:?}"
        );
    }

    fn errors_for(src: &str) -> Vec<String> {
        diags_for(src, None, None)
            .into_iter()
            .filter(|d| d.severity == Severity::Error)
            .map(|d| d.message)
            .collect()
    }

    /// A parameter is only useful if it can be offered *before* the run, which
    /// means it has to be findable without executing the flow. One written
    /// after the first real step — or buried in a loop, where it would run
    /// many times — is an ordinary assignment wearing a parameter's clothes.
    #[test]
    fn a_parameter_past_the_prelude_is_refused() {
        let ok = errors_for(
            "# collection: c\n# a comment\nPRELUDE_MAX_PARALLEL=2\nPARAM ENV TARGET = \"s\"\nREPORT REQUEST r\n",
        );
        assert!(ok.is_empty(), "prelude parameter should be fine: {ok:?}");

        let late = errors_for("# collection: c\nREPORT REQUEST r\nPARAM ENV TARGET = \"s\"\n");
        assert_eq!(late.len(), 1, "{late:?}");
        assert!(late[0].contains("TARGET"), "{late:?}");

        let nested = errors_for(
            "# collection: c\nFOR F IN FILES \"x\"\n    PARAM TEXT NOTE = \"n\"\n    REPORT REQUEST r\nEND\n",
        );
        assert_eq!(nested.len(), 1, "{nested:?}");
        assert!(nested[0].contains("NOTE"), "{nested:?}");
    }

    #[test]
    fn a_parameter_must_agree_with_its_own_declared_type() {
        let choice = errors_for(
            "# collection: c\nPARAM CHOICE(\"a\", \"b\") PICK = \"c\"\nREPORT REQUEST r\n",
        );
        assert_eq!(choice.len(), 1, "{choice:?}");
        assert!(
            choice[0].contains("PICK") && choice[0].contains("a, b"),
            "{choice:?}"
        );

        let empty = errors_for("# collection: c\nPARAM CHOICE() PICK\nREPORT REQUEST r\n");
        assert_eq!(empty.len(), 1, "{empty:?}");

        let number =
            errors_for("# collection: c\nPARAM NUMBER TRIES = \"many\"\nREPORT REQUEST r\n");
        assert_eq!(number.len(), 1, "{number:?}");
        assert!(number[0].contains("TRIES"), "{number:?}");

        // A default that has to be interpolated can't be judged until there is
        // something to interpolate, so it must not be rejected here.
        let deferred =
            errors_for("# collection: c\nPARAM NUMBER TRIES = \"{{RETRIES}}\"\nREPORT REQUEST r\n");
        assert!(deferred.is_empty(), "{deferred:?}");

        let dupes = errors_for(
            "# collection: c\nPARAM TEXT A = \"1\"\nPARAM NUMBER A = \"2\"\nREPORT REQUEST r\n",
        );
        assert_eq!(dupes.len(), 1, "{dupes:?}");
        assert!(dupes[0].contains('A'), "{dupes:?}");
    }

    /// Distinct names can still land on the same prompt once they are made
    /// readable, which produces a form with two identical fields.
    #[test]
    fn two_parameters_asking_the_same_question_are_flagged() {
        let diags = diags_for(
            "# collection: c\nPARAM TEXT TICKET_REF\nPARAM TEXT ticket_ref LABEL \"Ticket ref\"\nREPORT REQUEST r\n",
            None,
            None,
        );
        assert!(
            !diags.iter().any(|d| d.severity == Severity::Error),
            "distinct names, so the report still runs: {diags:?}"
        );
        assert!(
            diags
                .iter()
                .any(|d| d.severity == Severity::Warning && d.message.contains("Ticket ref")),
            "{diags:?}"
        );
    }

    /// An environment parameter exists precisely so the environment can be
    /// changed per run, so a default naming one that isn't loaded right now
    /// must not block the report the way the fixed `# environment:` directive
    /// does.
    #[test]
    fn an_unloaded_environment_default_is_only_a_warning() {
        let loaded = ["staging".to_string()];
        let diags = diags_for(
            "# collection: c\nPARAM ENV TARGET = \"prod\"\nREPORT REQUEST r\n",
            None,
            Some(&loaded),
        );
        assert!(
            !diags.iter().any(|d| d.severity == Severity::Error),
            "{diags:?}"
        );
        assert!(
            diags
                .iter()
                .any(|d| d.severity == Severity::Warning && d.message.contains("prod")),
            "{diags:?}"
        );
    }

    /// The variable-availability warnings come out of a `HashSet`, whose
    /// iteration order differs between instances. Emitting them in that order
    /// meant the validation panel — which is rebuilt whenever its inputs change
    /// — reshuffled its contents each time, so a request with several unset
    /// variables flickered. They must come out sorted, every time.
    #[test]
    fn variable_warnings_come_out_in_a_stable_order() {
        use crate::hurl::HurlEntry;
        let entry = HurlEntry {
            title: "req".into(),
            method: "GET".into(),
            url: "http://x/{{alpha}}/{{bravo}}?q={{charlie}}".into(),
            body: Some("{\"d\":\"{{delta}}\",\"e\":\"{{echo}}\",\"f\":\"{{foxtrot}}\"}".into()),
            ..Default::default()
        };
        let entries = [entry];
        let titles = vec!["req".to_string()];
        let flow = parse_flow("# collection: c\nREQUEST req\n").expect("parses");
        let run = || {
            let ctx = Context {
                request_titles: Some(&titles),
                request_entries: Some(&entries),
                base_var_names: Some(&[]),
                ..Default::default()
            };
            validate(&flow, &ctx)
                .into_iter()
                .filter(|d| d.severity == Severity::Warning)
                .map(|d| d.message)
                // The flow emits no columns, which earns a warning of its own -
                // not what this test is about.
                .filter(|m| m.contains("{{"))
                .collect::<Vec<_>>()
        };
        let first = run();
        assert_eq!(first.len(), 6, "one warning per variable: {first:?}");
        // The order is not merely repeatable, it is alphabetical - so it also
        // stays put as unrelated variables are added and removed.
        let order: Vec<&str> = ["alpha", "bravo", "charlie", "delta", "echo", "foxtrot"].into();
        for (msg, name) in first.iter().zip(&order) {
            assert!(msg.contains(name), "expected {name} in {msg}");
        }
        // A fresh `HashSet` is built on every call, so repeating the walk is
        // what would shake out an order that depends on it.
        for i in 0..50 {
            assert_eq!(first, run(), "run {i} produced a different order");
        }
    }

    fn errors(src: &str, titles: Option<&[String]>, envs: Option<&[String]>) -> Vec<String> {
        diags_for(src, titles, envs)
            .into_iter()
            .filter(|d| d.severity == Severity::Error)
            .map(|d| d.message)
            .collect()
    }

    fn has_err(
        src: &str,
        titles: Option<&[String]>,
        envs: Option<&[String]>,
        needle: &str,
    ) -> bool {
        errors(src, titles, envs)
            .iter()
            .any(|m| m.to_lowercase().contains(&needle.to_lowercase()))
    }

    fn titles() -> Vec<String> {
        vec![
            "Oauth".into(),
            "CreateSession".into(),
            "upload/process_file".into(),
            "finalise_session".into(),
        ]
    }

    /// Warnings from validating `src` with a bound collection whose entries
    /// expose the given `[Reports]` field names (title → fields).
    fn warnings_with_fields(src: &str, fields: &[(&str, &[&str])]) -> Vec<String> {
        let flow = parse_flow(src).expect("test source should parse");
        let titles: Vec<String> = fields.iter().map(|(t, _)| t.to_string()).collect();
        let field_map: Vec<(String, Vec<String>)> = fields
            .iter()
            .map(|(t, fs)| (t.to_string(), fs.iter().map(|s| s.to_string()).collect()))
            .collect();
        let ctx = Context {
            request_titles: Some(&titles),
            request_fields: Some(&field_map),
            ..Default::default()
        };
        validate(&flow, &ctx)
            .into_iter()
            .filter(|d| d.severity == Severity::Warning)
            .map(|d| d.message)
            .collect()
    }

    #[test]
    fn duplicate_column_headers_are_rejected() {
        let titles = titles();
        // `FILE AS X` and `Oauth.status AS X` both resolve to header `X`, which
        // would collide in JSON output — one error, reported once.
        let errs = errors(
            "# collection: c\n# columns: FILE AS X, Oauth.status AS X, Oauth AS X\nREPORT REQUEST Oauth\n",
            Some(&titles),
            None,
        );
        let dup: Vec<_> = errs.iter().filter(|m| m.contains("Two columns")).collect();
        assert_eq!(dup.len(), 1, "one duplicate-header error: {errs:?}");
        assert!(dup[0].contains('X'));

        // Distinct headers are fine.
        assert!(!has_err(
            "# collection: c\n# columns: FILE AS Name, Oauth.status AS Status\nREPORT REQUEST Oauth\n",
            Some(&titles),
            None,
            "Two columns",
        ));
    }

    #[test]
    fn show_unknown_field_warns_but_known_fields_do_not() {
        // `Response`/`Time` are intrinsics; `status` is a [Reports] field —
        // all fine. `bogus` is none of those → one warning.
        let warns = warnings_with_fields(
            "REPORT REQUEST process SHOW(Response, Time, status, bogus)\n",
            &[("process", &["status", "overall"])],
        );
        assert_eq!(warns.len(), 1, "only 'bogus' should warn: {warns:?}");
        assert!(warns[0].contains("bogus"));
    }

    #[test]
    fn show_with_field_counts_as_known() {
        // A field provided only by this statement's WITH block is known.
        let warns = warnings_with_fields(
            "REPORT REQUEST process SHOW(extra) WITH\n    extra: jsonpath \"$.x\"\nEND\n",
            &[("process", &[])],
        );
        assert!(
            warns.iter().all(|w| !w.contains("extra")),
            "WITH field should not warn: {warns:?}"
        );
    }

    #[test]
    fn show_is_not_validated_without_a_bound_collection() {
        // No request_fields context → the field set is unknown, so no warning
        // (never false-warn on a real [Reports] field we can't see).
        let flow = parse_flow("REPORT REQUEST process SHOW(bogus)\n").unwrap();
        let ctx = Context::default();
        let warns: Vec<_> = validate(&flow, &ctx)
            .into_iter()
            .filter(|d| d.severity == Severity::Warning)
            .filter(|d| d.message.contains("SHOW"))
            .collect();
        assert!(warns.is_empty(), "unbound flow shouldn't warn: {warns:?}");
    }

    /// The header-directive diagnostics, grouped: each line is one "this source
    /// does / does not raise that message" case, which is all these families
    /// ever were. Grouped rather than one test apiece so the cases can be read
    /// against each other; a failure names the source and the message it wanted.
    #[test]
    fn collection_and_output_header_diagnostics() {
        let t = titles();
        // No collection at all, and a directive present but empty, are the same
        // error: nothing to resolve request names against.
        assert!(has_err(
            "REQUEST Oauth\n",
            None,
            None,
            "No collection chosen"
        ));
        let empty_directive = "# collection:\nREQUEST Oauth\n";
        assert!(has_err(empty_directive, None, None, "No collection chosen"));

        let docx = "# collection: ./c.hurl\n# output: docx\nREQUEST Oauth\n";
        assert!(has_err(docx, Some(&t), None, "unsupported output format"));
        let csv = "# collection: ./c.hurl\n# output: csv\nREQUEST Oauth\n";
        assert!(!has_err(csv, Some(&t), None, "unsupported"));
    }
    #[test]
    fn valid_header_with_bound_collection_has_no_errors() {
        let t = titles();
        let errs = errors("# collection: ./c.hurl\nREQUEST Oauth\n", Some(&t), None);
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
    }

    #[test]
    fn xlsx_json_html_outputs_are_accepted() {
        let t = titles();
        for fmt in ["xlsx", "json", "html"] {
            assert!(
                !has_err(
                    &format!("# collection: ./c.hurl\n# output: {fmt}\nREQUEST Oauth\n"),
                    Some(&t),
                    None,
                    "unsupported"
                ),
                "format {fmt} should be accepted"
            );
        }
    }

    /// Ground truth never blocks a run — a report that scores nothing still
    /// produces its table — but every silent no-op is pointed out.
    #[test]
    fn ground_truth_mistakes_warn_without_blocking_the_run() {
        let t = titles();
        let warn = |src: &str| -> Vec<String> {
            diags_for(src, Some(&t), None)
                .into_iter()
                .filter(|d| d.severity == Severity::Warning)
                .map(|d| d.message)
                .collect()
        };
        let said = |ws: &[String], needle: &str| {
            ws.iter()
                .any(|m| m.to_lowercase().contains(&needle.to_lowercase()))
        };

        let ws = warn("# collection: ./c.hurl\n# labels: nonsense\nREQUEST Oauth\n");
        assert!(said(&ws, "declares nothing"), "{ws:?}");

        let ws = warn(
            "# collection: ./c.hurl\n# labels: Pass = ok, maybe\n# labels: Fail = maybe\nREQUEST Oauth\n",
        );
        assert!(said(&ws, "claimed by both"), "{ws:?}");

        let ws = warn("# collection: ./c.hurl\nREPORT \"a\" AS V TRUTH \"\"\n");
        assert!(said(&ws, "empty ground truth"), "{ws:?}");

        let ws =
            warn("# collection: ./c.hurl\nREPORT \"a\" AS V IMAGE(HEIGHT 40) TRUTH \"{{ e }}\"\n");
        assert!(said(&ws, "shown as a picture"), "{ws:?}");

        // None of them is an error, and a well-formed one says nothing at all.
        let src = "# collection: ./c.hurl\n# labels: Pass = ok, real\nREPORT \"a\" AS V TRUTH \"{{ e }}\"\n";
        assert!(errors(src, Some(&t), None).is_empty());
        let ws = warn(src);
        assert!(
            !said(&ws, "ground truth") && !said(&ws, "label"),
            "a correct ground truth is silent: {ws:?}"
        );
    }

    /// The `# environment:` directive's diagnostics, grouped.
    #[test]
    fn environment_header_diagnostics() {
        let t = titles();
        let named_staging = "# collection: ./c.hurl\n# environment: staging\nREQUEST Oauth\n";
        let only_au = ["au".to_string()];
        let au_and_staging = ["au".to_string(), "staging".to_string()];

        let unloaded = "environment 'staging' is not loaded";
        assert!(has_err(named_staging, Some(&t), Some(&only_au), unloaded));
        let loaded = Some(&au_and_staging[..]);
        assert!(!has_err(named_staging, Some(&t), loaded, "is not loaded"));

        let empty = "# collection: ./c.hurl\n# environment:\nREQUEST Oauth\n";
        assert!(has_err(
            empty,
            Some(&t),
            None,
            "environment setting is empty"
        ));

        // With no loaded-env context, a named environment can't be verified —
        // it must not spuriously error (mirrors how ENVS names are skipped).
        assert!(!has_err(named_staging, Some(&t), None, "is not loaded"));
    }
    #[test]
    fn unbound_collection_warns_but_does_not_error_on_names() {
        let diags = diags_for("# collection: ./c.hurl\nREQUEST Whatever\n", None, None);
        assert!(
            diags.iter().any(|d| d.severity == Severity::Warning
                && d.message.contains("collection isn't loaded"))
        );
        // No name-resolution error while unbound.
        assert!(!diags.iter().any(|d| d.message.contains("not found")));
    }

    /// How a `REQUEST` name is matched against the bound collection's titles.
    #[test]
    fn request_name_resolution_diagnostics() {
        let t = titles();
        let full = "# collection: ./c.hurl\nREQUEST upload/process_file\n";
        assert!(!has_err(full, Some(&t), None, "not found"));

        // "process_file" is the leaf of "upload/process_file".
        let leaf = "# collection: ./c.hurl\nREPORT REQUEST process_file\n";
        assert!(!has_err(leaf, Some(&t), None, "not found"));

        let unknown = "# collection: ./c.hurl\nREQUEST nope\n";
        assert!(has_err(unknown, Some(&t), None, "not found"));

        // A leaf shared by two requests can't be resolved.
        let dups = vec!["a/dup".to_string(), "b/dup".to_string()];
        let ambiguous = "# collection: ./c.hurl\nREQUEST dup\n";
        assert!(has_err(ambiguous, Some(&dups), None, "ambiguous"));
    }
    /// The `ENVS` loop-source diagnostics: role clauses and unloaded names.
    #[test]
    fn envs_clause_diagnostics() {
        let t = titles();
        // An ENVS clause with no names can't be produced by the parser directly,
        // so drive check_env_clause via a role clause missing comparisons.
        let no_comparison =
            "# collection: ./c.hurl\nFOR T IN ENVS BASELINE(\"prod\")\n  REQUEST Oauth\nEND\n";
        assert!(has_err(
            no_comparison,
            Some(&t),
            None,
            "at least one COMPARISON"
        ));

        let two_baselines = "# collection: ./c.hurl\nFOR T IN ENVS BASELINE(\"a\", \"b\"), COMPARISON(\"c\")\n  REQUEST Oauth\nEND\n";
        assert!(has_err(
            two_baselines,
            Some(&t),
            None,
            "at most one BASELINE"
        ));

        let named = "# collection: ./c.hurl\nFOR T IN ENVS \"prod-au\", \"staging-au\"\n  REQUEST Oauth\nEND\n";
        let only_prod = vec!["prod-au".to_string()];
        let both = vec!["prod-au".to_string(), "staging-au".to_string()];
        let missing = "'staging-au' is not loaded";
        assert!(has_err(named, Some(&t), Some(&only_prod), missing));
        assert!(!has_err(named, Some(&t), Some(&both), "not loaded"));
    }
    #[test]
    fn baseline_directive_with_envs_comparison_warns_it_is_ignored() {
        // Both a `# baseline:` snapshot diff and a live ENVS comparison target
        // the `Result` column; the live comparison wins, so the directive is
        // flagged as ignored rather than silently doing nothing.
        let t = titles();
        let warns: Vec<String> = diags_for(
            "# collection: ./c.hurl\n# baseline: prev.baseline\nFOR T IN ENVS BASELINE(\"a\"), COMPARISON(\"b\")\n  REQUEST Oauth\nEND\n",
            Some(&t),
            None,
        )
        .into_iter()
        .filter(|d| d.severity == Severity::Warning)
        .map(|d| d.message)
        .collect();
        assert!(
            warns
                .iter()
                .any(|m| m.contains("baseline setting is ignored")),
            "expected the ignored-baseline warning: {warns:?}"
        );
    }

    #[test]
    fn baseline_directive_without_envs_comparison_does_not_warn() {
        // A plain snapshot diff (no ENVS roles) is the normal Source-B path — no
        // warning.
        let t = titles();
        let warns: Vec<String> = diags_for(
            "# collection: ./c.hurl\n# baseline: prev.baseline\nREPORT REQUEST Oauth\n",
            Some(&t),
            None,
        )
        .into_iter()
        .filter(|d| d.severity == Severity::Warning)
        .map(|d| d.message)
        .collect();
        assert!(
            !warns.iter().any(|m| m.contains("'# baseline:'")),
            "a plain baseline diff should not warn: {warns:?}"
        );
    }

    #[test]
    fn missing_baseline_snapshot_warns_when_anchored() {
        // With a known base directory, a `# baseline:` naming a file that isn't
        // there is surfaced as a warning up front (not silently at run time).
        let dir = std::env::temp_dir().join(format!("pb-vbl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let flow = parse_flow(
            "# collection: ./c.hurl\n# baseline: missing.baseline\nREPORT REQUEST Oauth\n",
        )
        .unwrap();
        let t = titles();
        let ctx = Context {
            request_titles: Some(&t),
            root: Some(dir.as_path()),
            ..Default::default()
        };
        let warns: Vec<String> = validate(&flow, &ctx)
            .into_iter()
            .filter(|d| d.severity == Severity::Warning)
            .map(|d| d.message)
            .collect();
        assert!(
            warns.iter().any(|m| m.contains("was not found")),
            "expected a missing-snapshot warning: {warns:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A `BASELINE(FILE(…))` role whose snapshot isn't on disk was the one
    /// baseline reference with no preflight at all: unlike the `# baseline:`
    /// directive it was skipped entirely, so a typo only showed up as an
    /// unmatched comparison after a full run had already been paid for.
    #[test]
    fn missing_baseline_file_role_warns_when_anchored() {
        let dir = std::env::temp_dir().join(format!("pb-vrole-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let flow = parse_flow(
            "# collection: ./c.hurl\nFOR T IN ENVS BASELINE(FILE(\"missing.baseline\")), COMPARISON(\"prod\")\n    REPORT REQUEST Oauth\nEND\n",
        )
        .unwrap();
        let t = titles();
        let envs = vec!["prod".to_string()];
        let ctx = Context {
            request_titles: Some(&t),
            env_names: Some(&envs),
            root: Some(dir.as_path()),
            ..Default::default()
        };
        let warns: Vec<String> = validate(&flow, &ctx)
            .into_iter()
            .filter(|d| d.severity == Severity::Warning)
            .map(|d| d.message)
            .collect();
        assert!(
            warns
                .iter()
                .any(|m| m.contains("was not found") && m.contains("missing.baseline")),
            "expected a missing-snapshot warning: {warns:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_comparison_file_role_is_checked_too_and_a_present_one_stays_quiet() {
        let dir = std::env::temp_dir().join(format!("pb-vrole-ok-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("prev.baseline"), "{}").unwrap();
        let flow = parse_flow(
            "# collection: ./c.hurl\nFOR T IN ENVS BASELINE(FILE(\"prev.baseline\")), COMPARISON(FILE(\"gone.baseline\"))\n    REPORT REQUEST Oauth\nEND\n",
        )
        .unwrap();
        let t = titles();
        let ctx = Context {
            request_titles: Some(&t),
            root: Some(dir.as_path()),
            ..Default::default()
        };
        let warns: Vec<String> = validate(&flow, &ctx)
            .into_iter()
            .filter(|d| d.severity == Severity::Warning)
            .map(|d| d.message)
            .collect();
        assert!(
            warns.iter().any(|m| m.contains("gone.baseline")),
            "a comparison role's snapshot is checked as well: {warns:?}"
        );
        assert!(
            !warns.iter().any(|m| m.contains("prev.baseline")),
            "an existing snapshot must stay quiet: {warns:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A `FILE(…)` role is a path, not an environment, so it must never be
    /// reported as "not loaded" — the check that protects live env names.
    #[test]
    fn a_file_role_is_never_mistaken_for_an_unloaded_environment() {
        let flow = parse_flow(
            "# collection: ./c.hurl\nFOR T IN ENVS BASELINE(FILE(\"snap.baseline\")), COMPARISON(\"prod\")\n    REPORT REQUEST Oauth\nEND\n",
        )
        .unwrap();
        let t = titles();
        let envs = vec!["prod".to_string()];
        // No `root`, so the filesystem check is skipped entirely: an unsaved
        // report has nothing to resolve a relative path against.
        let ctx = Context {
            request_titles: Some(&t),
            env_names: Some(&envs),
            ..Default::default()
        };
        let msgs: Vec<String> = validate(&flow, &ctx)
            .into_iter()
            .map(|d| d.message)
            .collect();
        assert!(
            !msgs.iter().any(|m| m.contains("snap.baseline")),
            "an unanchored report must not report on the snapshot at all: {msgs:?}"
        );
    }

    #[test]
    fn present_baseline_snapshot_does_not_warn() {
        let dir = std::env::temp_dir().join(format!("pb-vbl-ok-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("prev.baseline"), "{}").unwrap();
        let flow =
            parse_flow("# collection: ./c.hurl\n# baseline: prev.baseline\nREPORT REQUEST Oauth\n")
                .unwrap();
        let t = titles();
        let ctx = Context {
            request_titles: Some(&t),
            root: Some(dir.as_path()),
            ..Default::default()
        };
        let warns: Vec<String> = validate(&flow, &ctx)
            .into_iter()
            .filter(|d| d.severity == Severity::Warning)
            .map(|d| d.message)
            .collect();
        assert!(
            !warns.iter().any(|m| m.contains("was not found")),
            "an existing snapshot should not warn: {warns:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// How many names a loop pattern binds versus how wide its source is, and
    /// whether the list it names was ever declared.
    #[test]
    fn loop_arity_and_list_reference_diagnostics() {
        let t = titles();
        let c = "# collection: ./c.hurl\n";
        let check = |src: String, needle: &str| has_err(&src, Some(&t), None, needle);

        let two_from_one = format!("{c}FOR (A, B) IN FILES \"d\"\n  REQUEST Oauth\nEND\n");
        assert!(check(two_from_one, "binds 2 name"));
        let zip = format!("{c}FOR (A, B) IN ZIP(FILES \"x\", FILES \"y\")\n  REQUEST Oauth\nEND\n");
        assert!(!check(zip, "binds"));

        let concat_ok = format!(
            "{c}FOR F IN CONCAT(FILES \"x\", FILES \"y\", FOLDERS \"z\")\n  REQUEST Oauth\nEND\n"
        );
        assert!(!check(concat_ok, "arity"));
        let concat_bad = format!(
            "{c}FOR F IN CONCAT(FILES \"x\", ZIP(FILES \"a\", FILES \"b\"))\n  REQUEST Oauth\nEND\n"
        );
        assert!(check(concat_bad, "inconsistent arity"));

        let ragged =
            format!("{c}LIST L = [(\"a\", \"b\"), \"c\"]\nFOR (X, Y) IN L\n  REQUEST Oauth\nEND\n");
        assert!(check(ragged, "inconsistent arity"));
        // A rest pattern absorbs whatever extra positions a row carries.
        let rest = format!(
            "{c}LIST L = [(\"a\", \"b\", \"c\")]\nFOR (X, ...) IN L\n  REQUEST Oauth\nEND\n"
        );
        assert!(!check(rest, "binds"));

        let undeclared = format!("{c}FOR X IN MISSING\n  REQUEST Oauth\nEND\n");
        assert!(check(undeclared, "unknown list"));
        let declared = format!("{c}LIST DOCS = FILES \"d\"\nFOR X IN DOCS\n  REQUEST Oauth\nEND\n");
        assert!(!check(declared, "unknown list"));
    }
    #[test]
    fn show_and_hide_overlap_is_an_error() {
        // A field in both SHOW and HIDE is contradictory → validation error.
        let t = titles();
        let errs = errors(
            "# collection: c\nREPORT REQUEST Oauth SHOW(HttpStatus, Time) HIDE(Time)\n",
            Some(&t),
            None,
        );
        let overlap: Vec<_> = errs.iter().filter(|m| m.contains("Time")).collect();
        assert_eq!(overlap.len(), 1, "one overlap error for Time: {errs:?}");
        assert!(
            overlap[0].contains("conflict")
                || overlap[0].contains("SHOW")
                || overlap[0].contains("HIDE")
        );
    }

    #[test]
    fn hide_unknown_field_warns_but_known_fields_do_not() {
        // Same semantics as the SHOW unknown-field warning, but for HIDE.
        let warns = warnings_with_fields(
            "REPORT REQUEST process HIDE(Response, Time, status, ghost)\n",
            &[("process", &["status", "overall"])],
        );
        assert_eq!(warns.len(), 1, "only 'ghost' should warn: {warns:?}");
        assert!(warns[0].contains("ghost"));
    }

    #[test]
    fn hide_is_not_validated_without_a_bound_collection() {
        let flow = parse_flow("REPORT REQUEST process HIDE(bogus)\n").unwrap();
        let ctx = Context::default();
        let warns: Vec<_> = validate(&flow, &ctx)
            .into_iter()
            .filter(|d| d.severity == Severity::Warning)
            .filter(|d| d.message.contains("HIDE"))
            .collect();
        assert!(
            warns.is_empty(),
            "unbound flow shouldn't warn on HIDE: {warns:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Variable-availability analysis tests
    // -----------------------------------------------------------------------

    /// Make a minimal `HurlEntry` for testing: `title` as the name,
    /// `{{VAR}}` references baked into the URL, and named captures.
    fn test_entry(title: &str, url_vars: &[&str], captures: &[&str]) -> crate::hurl::HurlEntry {
        use crate::hurl::HurlEntry;
        let url: String = url_vars
            .iter()
            .map(|v| format!("{{{{{}}}}} ", v))
            .collect::<String>();
        HurlEntry {
            title: title.to_string(),
            method: "GET".to_string(),
            url: format!("http://example/{}x", url),
            captures: captures
                .iter()
                .map(|c| ((*c).to_string(), "jsonpath \"$.v\"".to_string()))
                .collect(),
            ..Default::default()
        }
    }

    /// Validate `src` with a given context and return only the variable-
    /// availability warning messages.
    fn var_warns(
        src: &str,
        base_vars: &[&str],
        all_env_vars: &[&str],
        entries: &[crate::hurl::HurlEntry],
    ) -> Vec<String> {
        let flow = parse_flow(src).expect("test source should parse");
        let base: Vec<String> = base_vars.iter().map(|s| s.to_string()).collect();
        let all_env: Vec<String> = all_env_vars.iter().map(|s| s.to_string()).collect();
        let titles: Vec<String> = entries.iter().map(|e| e.title.clone()).collect();
        let ctx = Context {
            request_titles: Some(&titles),
            base_var_names: Some(&base),
            all_env_var_names: Some(&all_env),
            request_entries: Some(entries),
            ..Default::default()
        };
        validate(&flow, &ctx)
            .into_iter()
            .filter(|d| d.severity == Severity::Warning && d.message.contains("may not be set"))
            .map(|d| d.message)
            .collect()
    }

    /// A request that declares its own parameter supplies a value for it, so
    /// the reference is never unset. Warning about it punished the collection
    /// for being properly parameterised — and buried the warnings that matter
    /// under one per parameter per call site.
    #[test]
    fn a_declared_parameter_is_not_reported_as_possibly_unset() {
        let mut entry = test_entry("upload", &["FILE"], &[]);
        entry.options.push(crate::hurl::KvRow::new(
            "variable".to_string(),
            "FILE=./samples/front.jpg".to_string(),
        ));
        let warns = var_warns("REQUEST upload\n", &[], &[], &[entry]);
        assert!(
            warns.is_empty(),
            "the request declares FILE itself: {warns:?}"
        );
    }

    /// The other side of it: an undeclared name is still reported, so removing
    /// the noise hasn't removed the check.
    #[test]
    fn an_undeclared_var_is_still_reported_beside_a_declared_one() {
        let mut entry = test_entry("upload", &["FILE", "SESSION"], &[]);
        entry.options.push(crate::hurl::KvRow::new(
            "variable".to_string(),
            "FILE=./samples/front.jpg".to_string(),
        ));
        let warns = var_warns("REQUEST upload\n", &[], &[], &[entry]);
        assert_eq!(warns.len(), 1, "only the undeclared one: {warns:?}");
        assert!(warns[0].contains("SESSION"), "{warns:?}");
    }

    #[test]
    fn missing_var_in_request_url_produces_a_warning() {
        // Oauth's URL references {{TOKEN}} which isn't in the env or in scope.
        let entries = vec![test_entry("Oauth", &["TOKEN"], &[])];
        let warns = var_warns(
            "# collection: c\nREPORT REQUEST Oauth\n",
            &[], // no env vars
            &[],
            &entries,
        );
        assert!(
            warns.iter().any(|w| w.contains("TOKEN")),
            "{{TOKEN}} should warn as undefined: {warns:?}"
        );
    }

    #[test]
    fn var_defined_by_base_env_does_not_warn() {
        // When BASE_URL is in the base environment, no warning.
        let entries = vec![test_entry("Oauth", &["BASE_URL"], &[])];
        let warns = var_warns(
            "# collection: c\nREPORT REQUEST Oauth\n",
            &["BASE_URL"], // provided by env
            &[],
            &entries,
        );
        assert!(
            warns.is_empty(),
            "BASE_URL is in the base env — no warning expected: {warns:?}"
        );
    }

    #[test]
    fn var_defined_by_explicit_assignment_does_not_warn() {
        // An explicit `KEY=value` assignment before the request defines it.
        let entries = vec![test_entry("Oauth", &["TOKEN"], &[])];
        let warns = var_warns(
            "# collection: c\nTOKEN=abc\nREPORT REQUEST Oauth\n",
            &[], // not in env
            &[],
            &entries,
        );
        assert!(
            warns.is_empty(),
            "TOKEN is assigned before the request — no warning: {warns:?}"
        );
    }

    #[test]
    fn var_defined_by_for_loop_binder_does_not_warn() {
        // TOKEN is the loop binder inside a FOR loop.
        let entries = vec![test_entry("Oauth", &["TOKEN"], &[])];
        let warns = var_warns(
            "# collection: c\nFOR TOKEN IN [\"x\", \"y\"]\n    REPORT REQUEST Oauth\nEND\n",
            &[],
            &[],
            &entries,
        );
        assert!(
            warns.is_empty(),
            "TOKEN is a FOR loop binder — no warning: {warns:?}"
        );
    }

    #[test]
    fn var_defined_by_prior_capture_does_not_warn() {
        // Auth request captures TOKEN; then Api uses it.
        let auth = test_entry("Auth", &[], &["TOKEN"]);
        let api = test_entry("Api", &["TOKEN"], &[]);
        let entries = vec![auth, api];
        let warns = var_warns(
            "# collection: c\nREQUEST Auth\nREPORT REQUEST Api\n",
            &[],
            &[],
            &entries,
        );
        assert!(
            warns.is_empty(),
            "TOKEN is captured by Auth before Api runs — no warning: {warns:?}"
        );
    }

    #[test]
    fn var_defined_by_envs_loop_does_not_warn() {
        // Inside a FOR … IN ENVS loop, any env variable is potentially in scope.
        let entries = vec![test_entry("Api", &["REGION"], &[])];
        // REGION is in one of the loaded envs (all_env_vars).
        let warns = var_warns(
            "# collection: c\nFOR ENV IN ENVS \"prod\", \"staging\"\n    REPORT REQUEST Api\nEND\n",
            &[],         // not in base env
            &["REGION"], // but one of the envs provides it
            &entries,
        );
        assert!(
            warns.is_empty(),
            "REGION comes from the ENVS loop env — no warning: {warns:?}"
        );
    }

    #[test]
    fn no_warning_without_base_var_names_context() {
        // When base_var_names is None the check is skipped entirely
        // (conservative: we can't know what the env provides).
        let entries = vec![test_entry("Oauth", &["MISSING"], &[])];
        let titles: Vec<String> = entries.iter().map(|e| e.title.clone()).collect();
        let flow = parse_flow("# collection: c\nREPORT REQUEST Oauth\n").unwrap();
        let ctx = Context {
            request_titles: Some(&titles),
            base_var_names: None, // unknown
            request_entries: Some(&entries),
            ..Default::default()
        };
        let warns: Vec<_> = validate(&flow, &ctx)
            .into_iter()
            .filter(|d| d.severity == Severity::Warning && d.message.contains("may not be defined"))
            .collect();
        assert!(
            warns.is_empty(),
            "without base_var_names the check must be skipped: {warns:?}"
        );
    }

    #[test]
    fn capture_is_only_available_after_the_capturing_request() {
        // TOKEN is captured by Auth, but if a request runs before Auth and uses
        // TOKEN, it should warn. After Auth the warning is gone.
        let auth = test_entry("Auth", &[], &["TOKEN"]);
        let before = test_entry("Before", &["TOKEN"], &[]);
        let after = test_entry("After", &["TOKEN"], &[]);
        let entries = vec![auth.clone(), before.clone(), after.clone()];
        // Flow: Before (uses TOKEN — not yet captured), then Auth (captures TOKEN),
        // then After (uses TOKEN — OK, captured by Auth).
        let warns_before = var_warns(
            "# collection: c\nREPORT REQUEST Before\nREQUEST Auth\nREPORT REQUEST After\n",
            &[],
            &[],
            &entries,
        );
        assert!(
            warns_before
                .iter()
                .any(|w| w.contains("TOKEN") && w.contains("Before")),
            "TOKEN is not yet captured when Before runs: {warns_before:?}"
        );
        assert!(
            !warns_before
                .iter()
                .any(|w| w.contains("TOKEN") && w.contains("After")),
            "TOKEN IS captured by the time After runs: {warns_before:?}"
        );
    }
}

#[cfg(test)]
mod helper_collection_validation_tests {
    use super::*;
    use crate::hurl::HurlEntry;
    use crate::report::parser::parse_flow;
    use crate::report::run::HelperCollection;

    fn entry(title: &str) -> HurlEntry {
        HurlEntry {
            title: title.to_string(),
            method: "GET".into(),
            url: "http://x".into(),
            ..Default::default()
        }
    }

    fn check(src: &str, titles: &[&str], helpers: &[HelperCollection]) -> Vec<String> {
        let flow = parse_flow(src).expect("parses");
        let titles: Vec<String> = titles.iter().map(|t| t.to_string()).collect();
        let ctx = Context {
            request_titles: Some(&titles),
            helpers,
            ..Default::default()
        };
        validate(&flow, &ctx)
            .into_iter()
            .filter(|d| d.severity == Severity::Error)
            .map(|d| d.message)
            .collect()
    }

    fn helper(alias: &str, titles: &[&str]) -> HelperCollection {
        HelperCollection {
            alias: alias.into(),
            entries: titles.iter().map(|t| entry(t)).collect(),
        }
    }

    /// The point of the feature: a request that deliberately isn't in the
    /// collection under test still validates when addressed through its alias.
    #[test]
    fn a_helper_request_validates_through_its_alias() {
        let errs = check(
            "# collection: ./api.hurl\n# collection: ./h.hurl AS h\n\nREQUEST h/fetch_frame\n",
            &["upload"],
            &[helper("h", &["fetch_frame"])],
        );
        assert!(errs.is_empty(), "{errs:?}");
    }

    /// …and validation agrees with `resolve_qualified`: a helper request named
    /// without its alias is an error here, because it would fail at run time.
    #[test]
    fn a_helper_request_without_its_alias_is_an_error() {
        let errs = check(
            "# collection: ./api.hurl\n# collection: ./h.hurl AS h\n\nREQUEST fetch_frame\n",
            &["upload"],
            &[helper("h", &["fetch_frame"])],
        );
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(errs[0].contains("fetch_frame"));
    }

    #[test]
    fn a_missing_name_inside_a_helper_is_reported_with_the_alias() {
        let errs = check(
            "# collection: ./api.hurl\n# collection: ./h.hurl AS h\n\nREQUEST h/nope\n",
            &["upload"],
            &[helper("h", &["fetch_frame"])],
        );
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(errs[0].contains("h/nope"), "{errs:?}");
    }

    #[test]
    fn the_primary_collection_takes_no_alias() {
        let errs = check(
            "# collection: ./api.hurl AS main\n\nREQUEST upload\n",
            &["upload"],
            &[],
        );
        assert_eq!(errs.len(), 1, "{errs:?}");
    }

    #[test]
    fn a_helper_must_be_aliased() {
        let errs = check(
            "# collection: ./api.hurl\n# collection: ./h.hurl\n\nREQUEST upload\n",
            &["upload"],
            &[],
        );
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(errs[0].contains("./h.hurl"), "{errs:?}");
    }

    #[test]
    fn two_helpers_cannot_share_an_alias() {
        let errs = check(
            "# collection: ./api.hurl\n# collection: ./a.hurl AS h\n# collection: ./b.hurl AS h\n\nREQUEST upload\n",
            &["upload"],
            &[helper("h", &["x"])],
        );
        assert_eq!(errs.len(), 1, "{errs:?}");
    }

    /// An alias that is also a top-level folder makes `folder/request`
    /// ambiguous, and PaperTrail never picks silently between two readings.
    #[test]
    fn an_alias_may_not_shadow_a_virtual_folder() {
        let errs = check(
            "# collection: ./api.hurl\n# collection: ./h.hurl AS auth\n\nREQUEST upload\n",
            &["auth/login", "upload"],
            &[helper("auth", &["x"])],
        );
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(errs[0].contains("auth"), "{errs:?}");
    }

    #[test]
    fn an_unreadable_helper_is_an_error_on_the_directive() {
        let flow = parse_flow(
            "# collection: ./api.hurl\n# collection: ./gone.hurl AS h\n\nREQUEST upload\n",
        )
        .expect("parses");
        let titles = vec!["upload".to_string()];
        let errors = vec![("./gone.hurl".to_string(), "no such file".to_string())];
        let ctx = Context {
            request_titles: Some(&titles),
            helper_errors: &errors,
            ..Default::default()
        };
        let errs: Vec<String> = validate(&flow, &ctx)
            .into_iter()
            .filter(|d| d.severity == Severity::Error)
            .map(|d| d.message)
            .collect();
        assert!(
            errs.iter()
                .any(|e| e.contains("./gone.hurl") && e.contains("no such file")),
            "{errs:?}"
        );
    }
}
