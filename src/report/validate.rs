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

use std::collections::HashMap;

use super::flow::{EnvClause, FlowNode, Pattern, Producer, ReportFlow, ReportStmt};

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
#[derive(Default)]
pub struct Context<'a> {
    /// Full entry titles (incl. virtual-folder paths) of the bound collection.
    pub request_titles: Option<&'a [String]>,
    /// Names of environments currently loaded (for `ENVS` resolution).
    pub env_names: Option<&'a [String]>,
}

/// Validate `flow` against `ctx`, returning all diagnostics (errors + warnings).
pub fn validate(flow: &ReportFlow, ctx: &Context) -> Vec<Diagnostic> {
    let mut diags = Vec::new();

    // Header: collection binding + output format.
    match flow.header.collection() {
        None => diags.push(Diagnostic::error(
            "missing '# collection:' header — the report isn't bound to a collection",
        )),
        Some(c) if c.trim().is_empty() => diags.push(Diagnostic::error("'# collection:' is empty")),
        Some(_) => {}
    }
    if let Some(out) = flow.header.output()
        && !out.trim().eq_ignore_ascii_case("csv")
    {
        diags.push(Diagnostic::error(format!(
            "unsupported output format '{out}' (only 'csv' is supported in v1)"
        )));
    }

    if ctx.request_titles.is_none() {
        diags.push(Diagnostic::warning(
            "collection not loaded — request names can't be validated until it's bound",
        ));
    }

    // Walk the tree with a scope stack of declared LIST producers.
    let mut scopes: Vec<HashMap<String, Producer>> = vec![HashMap::new()];
    walk(&flow.nodes, ctx, &mut scopes, &mut diags);

    diags
}

fn walk(
    nodes: &[FlowNode],
    ctx: &Context,
    scopes: &mut Vec<HashMap<String, Producer>>,
    diags: &mut Vec<Diagnostic>,
) {
    for node in nodes {
        match node {
            FlowNode::Assign { .. } => {}
            FlowNode::ListDecl { name, producer } => {
                check_producer(producer, ctx, scopes, diags);
                if scopes.iter().any(|s| s.contains_key(name)) {
                    diags.push(Diagnostic::warning(format!(
                        "LIST '{name}' shadows an earlier declaration of the same name"
                    )));
                }
                scopes
                    .last_mut()
                    .unwrap()
                    .insert(name.clone(), producer.clone());
            }
            FlowNode::Request { name } => check_request_name(name, ctx, diags),
            FlowNode::Report(stmt) => check_report(stmt, ctx, diags),
            FlowNode::ForEach {
                pattern,
                producer,
                body,
                ..
            } => {
                check_producer(producer, ctx, scopes, diags);
                check_arity(pattern, producer, scopes, diags);
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
    if let ReportStmt::Request { name, .. } = stmt {
        check_request_name(name, ctx, diags);
    }
}

/// Resolve a request name against the bound collection's titles: exact
/// full-title → unique leaf name → error. Skipped when no collection is bound.
fn check_request_name(name: &str, ctx: &Context, diags: &mut Vec<Diagnostic>) {
    let Some(titles) = ctx.request_titles else {
        return;
    };
    let exact = titles.iter().filter(|t| t.as_str() == name).count();
    if exact == 1 {
        return;
    }
    if exact > 1 {
        diags.push(Diagnostic::error(format!(
            "request '{name}' is ambiguous ({exact} entries share that title)"
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
        0 => diags.push(Diagnostic::error(format!(
            "request '{name}' not found in the bound collection"
        ))),
        n => diags.push(Diagnostic::error(format!(
            "request '{name}' is ambiguous ({n} entries end with that name — qualify it with its folder path)"
        ))),
    }
}

fn check_env_clause(clause: &EnvClause, ctx: &Context, diags: &mut Vec<Diagnostic>) {
    let names: Vec<&String> = match clause {
        EnvClause::Plain(names) => {
            if names.is_empty() {
                diags.push(Diagnostic::error("ENVS loop has no environments"));
            }
            names.iter().collect()
        }
        EnvClause::Roles {
            baseline,
            comparisons,
        } => {
            if baseline.len() > 1 {
                diags.push(Diagnostic::error(
                    "at most one BASELINE environment is allowed",
                ));
            }
            if comparisons.is_empty() {
                diags.push(Diagnostic::error(
                    "a role clause needs at least one COMPARISON environment",
                ));
            }
            baseline.iter().chain(comparisons.iter()).collect()
        }
    };
    if let Some(loaded) = ctx.env_names {
        for n in names {
            if !loaded.iter().any(|e| e == n) {
                diags.push(Diagnostic::error(format!(
                    "environment '{n}' is not loaded"
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
    diags: &mut Vec<Diagnostic>,
) {
    let Some(arity) = producer_arity(producer, scopes) else {
        return; // Runtime-determined (or inconsistent — flagged by check_producer).
    };
    let binders = pattern.binders.len();
    if pattern.rest {
        if binders > arity {
            diags.push(Diagnostic::error(format!(
                "pattern binds {binders} names before '...' but the producer yields only {arity}"
            )));
        }
    } else if binders != arity {
        diags.push(Diagnostic::error(format!(
            "pattern binds {binders} name(s) but the producer yields {arity} per item \
             (use '_' to discard or '...' to absorb extras)"
        )));
    }
}

fn check_producer(
    producer: &Producer,
    _ctx: &Context,
    scopes: &[HashMap<String, Producer>],
    diags: &mut Vec<Diagnostic>,
) {
    if let Producer::Named(name) = producer
        && !scopes.iter().rev().any(|s| s.contains_key(name))
    {
        diags.push(Diagnostic::error(format!(
            "unknown list '{name}' (declare it with 'LIST {name} = …' before use)"
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
            diags.push(Diagnostic::error(
                "list elements have inconsistent arity (mix of scalars/tuples of different sizes)",
            ));
        }
    }
    if let Producer::Zip(ps) = producer {
        for p in ps {
            check_producer(p, _ctx, scopes, diags);
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
        };
        validate(&flow, &ctx)
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

    #[test]
    fn missing_collection_is_an_error() {
        assert!(has_err(
            "REQUEST Oauth\n",
            None,
            None,
            "missing '# collection:'"
        ));
    }

    #[test]
    fn empty_collection_directive_is_an_error() {
        assert!(has_err(
            "# collection:\nREQUEST Oauth\n",
            None,
            None,
            "empty"
        ));
    }

    #[test]
    fn valid_header_with_bound_collection_has_no_errors() {
        let t = titles();
        let errs = errors("# collection: ./c.hurl\nREQUEST Oauth\n", Some(&t), None);
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
    }

    #[test]
    fn unsupported_output_format_is_an_error() {
        let t = titles();
        assert!(has_err(
            "# collection: ./c.hurl\n# output: xlsx\nREQUEST Oauth\n",
            Some(&t),
            None,
            "unsupported output format"
        ));
    }

    #[test]
    fn csv_output_is_accepted() {
        let t = titles();
        assert!(!has_err(
            "# collection: ./c.hurl\n# output: csv\nREQUEST Oauth\n",
            Some(&t),
            None,
            "unsupported"
        ));
    }

    #[test]
    fn unbound_collection_warns_but_does_not_error_on_names() {
        let diags = diags_for("# collection: ./c.hurl\nREQUEST Whatever\n", None, None);
        assert!(diags.iter().any(
            |d| d.severity == Severity::Warning && d.message.contains("collection not loaded")
        ));
        // No name-resolution error while unbound.
        assert!(!diags.iter().any(|d| d.message.contains("not found")));
    }

    #[test]
    fn request_name_resolves_by_exact_full_title() {
        let t = titles();
        assert!(!has_err(
            "# collection: ./c.hurl\nREQUEST upload/process_file\n",
            Some(&t),
            None,
            "not found"
        ));
    }

    #[test]
    fn request_name_resolves_by_unique_leaf() {
        let t = titles();
        // "process_file" is the leaf of "upload/process_file".
        assert!(!has_err(
            "# collection: ./c.hurl\nREPORT REQUEST process_file\n",
            Some(&t),
            None,
            "not found"
        ));
    }

    #[test]
    fn unknown_request_name_is_an_error() {
        let t = titles();
        assert!(has_err(
            "# collection: ./c.hurl\nREQUEST nope\n",
            Some(&t),
            None,
            "not found"
        ));
    }

    #[test]
    fn ambiguous_leaf_is_an_error() {
        let t = vec!["a/dup".to_string(), "b/dup".to_string()];
        assert!(has_err(
            "# collection: ./c.hurl\nREQUEST dup\n",
            Some(&t),
            None,
            "ambiguous"
        ));
    }

    #[test]
    fn envs_plain_empty_is_an_error() {
        // An ENVS clause with no names can't be produced by the parser directly,
        // so drive check_env_clause via a role clause missing comparisons.
        let t = titles();
        assert!(has_err(
            "# collection: ./c.hurl\nFOR T IN ENVS BASELINE(\"prod\")\n  REQUEST Oauth\nEND\n",
            Some(&t),
            None,
            "at least one COMPARISON"
        ));
    }

    #[test]
    fn envs_multiple_baseline_is_an_error() {
        let t = titles();
        assert!(has_err(
            "# collection: ./c.hurl\nFOR T IN ENVS BASELINE(\"a\", \"b\"), COMPARISON(\"c\")\n  REQUEST Oauth\nEND\n",
            Some(&t),
            None,
            "at most one BASELINE"
        ));
    }

    #[test]
    fn envs_unloaded_environment_is_an_error() {
        let t = titles();
        let envs = vec!["prod-au".to_string()];
        assert!(has_err(
            "# collection: ./c.hurl\nFOR T IN ENVS \"prod-au\", \"staging-au\"\n  REQUEST Oauth\nEND\n",
            Some(&t),
            Some(&envs),
            "'staging-au' is not loaded"
        ));
    }

    #[test]
    fn envs_all_loaded_has_no_env_error() {
        let t = titles();
        let envs = vec!["prod-au".to_string(), "staging-au".to_string()];
        assert!(!has_err(
            "# collection: ./c.hurl\nFOR T IN ENVS \"prod-au\", \"staging-au\"\n  REQUEST Oauth\nEND\n",
            Some(&t),
            Some(&envs),
            "not loaded"
        ));
    }

    #[test]
    fn arity_mismatch_is_an_error() {
        let t = titles();
        assert!(has_err(
            "# collection: ./c.hurl\nFOR (A, B) IN FILES \"d\"\n  REQUEST Oauth\nEND\n",
            Some(&t),
            None,
            "binds 2 name"
        ));
    }

    #[test]
    fn arity_match_on_zip_is_ok() {
        let t = titles();
        assert!(!has_err(
            "# collection: ./c.hurl\nFOR (A, B) IN ZIP(FILES \"x\", FILES \"y\")\n  REQUEST Oauth\nEND\n",
            Some(&t),
            None,
            "binds"
        ));
    }

    #[test]
    fn inconsistent_list_literal_arity_is_an_error() {
        let t = titles();
        assert!(has_err(
            "# collection: ./c.hurl\nLIST L = [(\"a\", \"b\"), \"c\"]\nFOR (X, Y) IN L\n  REQUEST Oauth\nEND\n",
            Some(&t),
            None,
            "inconsistent arity"
        ));
    }

    #[test]
    fn rest_pattern_absorbs_extra_positions() {
        let t = titles();
        assert!(!has_err(
            "# collection: ./c.hurl\nLIST L = [(\"a\", \"b\", \"c\")]\nFOR (X, ...) IN L\n  REQUEST Oauth\nEND\n",
            Some(&t),
            None,
            "binds",
        ));
    }

    #[test]
    fn unknown_list_reference_is_an_error() {
        let t = titles();
        assert!(has_err(
            "# collection: ./c.hurl\nFOR X IN MISSING\n  REQUEST Oauth\nEND\n",
            Some(&t),
            None,
            "unknown list"
        ));
    }

    #[test]
    fn declared_list_reference_is_ok() {
        let t = titles();
        assert!(!has_err(
            "# collection: ./c.hurl\nLIST DOCS = FILES \"d\"\nFOR X IN DOCS\n  REQUEST Oauth\nEND\n",
            Some(&t),
            None,
            "unknown list"
        ));
    }
}
