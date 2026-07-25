//! Env-to-env comparison — the `Result` column (build-breakdown phase 11).
//!
//! When a flow loops over environments with a role clause
//! (`FOR TARGET IN ENVS BASELINE("prod"), COMPARISON("staging", …)`), the rows
//! for one logical case (same [row key](super::model::ReportRow::key), which
//! *excludes* the ENVS `TARGET` axis) align across environments. This module is
//! a pure post-processing pass over the produced [`ReportResult`]: it collapses
//! each candidate (comparison) row against its baseline sibling into a single
//! output row that carries a computed `Result` cell describing the diff.
//!
//! It runs entirely off the row model, so both the CSV writer and the TUI grid
//! pick up the `Result` column for free (they both render the same rows).
//!
//! Only *reported fields* are compared — the `[Reports]`/`WITH` fields a request
//! declares (`alias.<field>`). A request with no such fields falls back to its
//! whole `Response`; the noisy intrinsics (`Time`, `HttpStatus`, `Asserts`,
//! `Error`) are surfaced as their own columns and left out of the diff.

use std::collections::{BTreeSet, HashMap, HashSet};

use super::flow::{EnvClause, FlowNode, ReportFlow};
use super::model::{ReportResult, ReportRow};

/// The reserved output column that carries the comparison outcome. Added to the
/// default column order (at the front) only when the flow configures a
/// comparison run; rename it in `columns:` like any column (`Result AS …`).
pub const RESULT_COLUMN: &str = "Result";

/// `Result` value when the candidate matches the baseline on every compared
/// field.
pub const MATCH: &str = "OK";
/// `Result` value for a candidate row whose row key has no baseline sibling.
pub const NO_BASELINE: &str = "no baseline";
/// `Result` value for a baseline row whose row key produced no candidate.
pub const NO_CANDIDATE: &str = "no candidate";

/// Per-request intrinsic column suffixes. These are excluded from the compared
/// "reported fields": `Time` always differs, and `HttpStatus`/`Asserts`/`Error`/
/// `Response` already have their own columns. A request with *no* reported
/// fields falls back to comparing its `Response` (see [`comparable_keys`]).
const INTRINSICS: [&str; 5] = ["HttpStatus", "Time", "Asserts", "Error", "Response"];

/// The baseline/comparison environment roles that drive a comparison run.
pub struct Roles {
    /// Environment names that act as the reference (usually one).
    baseline: HashSet<String>,
    /// Candidate environment names, in clause order (each compared to the
    /// baseline; duplicates removed).
    comparisons: Vec<String>,
}

/// Extract the comparison roles a flow configures, or `None` when it has no
/// `ENVS` role clause with a baseline (a plain `ENVS` list, or no `ENVS` loop at
/// all, produces per-env rows with no diff — unchanged behaviour).
pub fn comparison_roles(flow: &ReportFlow) -> Option<Roles> {
    let mut baseline = HashSet::new();
    let mut comparisons = Vec::new();
    collect_roles(&flow.nodes, &mut baseline, &mut comparisons);
    if baseline.is_empty() {
        return None;
    }
    Some(Roles {
        baseline,
        comparisons,
    })
}

/// Walk the flow tree, unioning every `ENVS BASELINE/COMPARISON` clause's names.
fn collect_roles(
    nodes: &[FlowNode],
    baseline: &mut HashSet<String>,
    comparisons: &mut Vec<String>,
) {
    for node in nodes {
        match node {
            FlowNode::ForEnvs { clause, body, .. } => {
                if let EnvClause::Roles {
                    baseline: b,
                    comparisons: c,
                } = clause
                {
                    for name in b {
                        baseline.insert(name.clone());
                    }
                    for name in c {
                        if !comparisons.contains(name) {
                            comparisons.push(name.clone());
                        }
                    }
                }
                collect_roles(body, baseline, comparisons);
            }
            FlowNode::ForEach { body, .. } => collect_roles(body, baseline, comparisons),
            _ => {}
        }
    }
}

/// Collapse baseline/candidate rows into one candidate-per-comparison row
/// carrying a `Result` cell, in place. Rows whose target is neither a baseline
/// nor a comparison env (e.g. a top-level `REPORT` outside the ENVS loop) pass
/// through unchanged, ahead of the compared rows.
pub fn apply(result: &mut ReportResult, roles: &Roles) {
    // Surface the reserved column (prominently, at the front) so a directive-free
    // run still shows the diff.
    if !result.column_order.iter().any(|c| c == RESULT_COLUMN) {
        result.column_order.insert(0, RESULT_COLUMN.to_string());
    }

    let rows = std::mem::take(&mut result.rows);

    let mut baseline_by_key: HashMap<Vec<String>, ReportRow> = HashMap::new();
    let mut candidate_by_key_target: HashMap<(Vec<String>, String), ReportRow> = HashMap::new();
    let mut key_order: Vec<Vec<String>> = Vec::new();
    let mut seen_key: HashSet<Vec<String>> = HashSet::new();
    let mut passthrough: Vec<ReportRow> = Vec::new();

    for row in rows {
        let target = row.target.clone();
        let is_baseline = target
            .as_deref()
            .is_some_and(|t| roles.baseline.contains(t));
        let is_candidate = target
            .as_deref()
            .is_some_and(|t| roles.comparisons.iter().any(|c| c == t));

        if (is_baseline || is_candidate) && seen_key.insert(row.key.clone()) {
            key_order.push(row.key.clone());
        }

        if is_baseline {
            baseline_by_key.entry(row.key.clone()).or_insert(row);
        } else if is_candidate {
            let t = target.unwrap_or_default();
            candidate_by_key_target
                .entry((row.key.clone(), t))
                .or_insert(row);
        } else {
            passthrough.push(row);
        }
    }

    let mut out: Vec<ReportRow> = Vec::new();
    out.append(&mut passthrough);

    for key in &key_order {
        let baseline = baseline_by_key.get(key);
        let mut emitted = false;
        for comp in &roles.comparisons {
            if let Some(mut cand) = candidate_by_key_target.remove(&(key.clone(), comp.clone())) {
                let verdict = compute_result(baseline, &cand);
                cand.cells.insert(RESULT_COLUMN.to_string(), verdict);
                out.push(cand);
                emitted = true;
            }
        }
        // A baseline with no candidate still emits a row (its own values), flagged.
        if !emitted && let Some(base) = baseline {
            let mut base = base.clone();
            base.cells
                .insert(RESULT_COLUMN.to_string(), NO_CANDIDATE.to_string());
            out.push(base);
        }
    }

    result.rows = out;
}

/// Compare `cand` against its `baseline`, returning the `Result` cell: a
/// `field: base→cand` summary of every differing compared field (joined by
/// `; `), [`MATCH`] when all agree, or [`NO_BASELINE`] when there is no baseline.
pub(super) fn compute_result(baseline: Option<&ReportRow>, cand: &ReportRow) -> String {
    let Some(base) = baseline else {
        return NO_BASELINE.to_string();
    };
    let mut diffs = Vec::new();
    for key in comparable_keys(cand, base) {
        let b = base.cells.get(&key).map(String::as_str).unwrap_or("");
        let c = cand.cells.get(&key).map(String::as_str).unwrap_or("");
        if b != c {
            let label = key.rsplit('.').next().unwrap_or(&key);
            diffs.push(format!("{label}: {b}→{c}"));
        }
    }
    if diffs.is_empty() {
        MATCH.to_string()
    } else {
        diffs.join("; ")
    }
}

/// The set of cell keys to compare across two rows: every reported field
/// (`alias.<field>` where `<field>` is not an intrinsic), plus the `Response` of
/// any reported request that declared *no* fields (so a field-less `REPORT
/// REQUEST` still contributes its body to the diff). Returned in a stable order
/// so the `Result` summary is deterministic.
pub(super) fn comparable_keys(a: &ReportRow, b: &ReportRow) -> Vec<String> {
    let mut fields: BTreeSet<String> = BTreeSet::new();
    let mut aliases_with_fields: HashSet<String> = HashSet::new();
    let mut responses: BTreeSet<String> = BTreeSet::new();

    for key in a.cells.keys().chain(b.cells.keys()) {
        let Some((alias, field)) = key.split_once('.') else {
            continue; // bare vars (FILE, TARGET, Result) are not compared
        };
        if INTRINSICS.contains(&field) {
            if field == "Response" {
                responses.insert(key.clone());
            }
        } else {
            fields.insert(key.clone());
            aliases_with_fields.insert(alias.to_string());
        }
    }

    let mut out: Vec<String> = fields.into_iter().collect();
    for resp in responses {
        let alias = resp.split_once('.').map(|(a, _)| a).unwrap_or("");
        if !aliases_with_fields.contains(alias) {
            out.push(resp);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::parse_flow;

    fn row(key: &[&str], target: &str, cells: &[(&str, &str)]) -> ReportRow {
        ReportRow {
            cells: cells
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            vars: HashMap::new(),
            key: key.iter().map(|k| k.to_string()).collect(),
            path: Vec::new(),
            target: Some(target.to_string()),
        }
    }

    fn roles(baseline: &[&str], comparisons: &[&str]) -> Roles {
        Roles {
            baseline: baseline.iter().map(|s| s.to_string()).collect(),
            comparisons: comparisons.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn roles_extracted_only_when_baseline_present() {
        let plain = parse_flow("FOR T IN ENVS \"au\", \"eu\"\n  REQUEST r\nEND\n").unwrap();
        assert!(comparison_roles(&plain).is_none());

        let cmp = parse_flow(
            "FOR T IN ENVS BASELINE(\"prod\"), COMPARISON(\"staging\", \"staging2\")\n  REQUEST r\nEND\n",
        )
        .unwrap();
        let r = comparison_roles(&cmp).expect("roles");
        assert!(r.baseline.contains("prod"));
        assert_eq!(r.comparisons, vec!["staging", "staging2"]);
    }

    #[test]
    fn roles_extracted_from_nested_loops() {
        let flow = parse_flow(
            "FOR FILE IN [\"a\"]\n  FOR T IN ENVS BASELINE(\"prod\"), COMPARISON(\"staging\")\n    REQUEST r\n  END\nEND\n",
        )
        .unwrap();
        let r = comparison_roles(&flow).expect("roles");
        assert!(r.baseline.contains("prod"));
        assert_eq!(r.comparisons, vec!["staging"]);
    }

    #[test]
    fn candidate_merges_with_baseline_and_diffs_fields() {
        let mut result = ReportResult {
            rows: vec![
                row(
                    &["a"],
                    "prod",
                    &[("proc.overall", "CLEAR"), ("proc.HttpStatus", "200")],
                ),
                row(
                    &["a"],
                    "staging",
                    &[("proc.overall", "REVIEW"), ("proc.HttpStatus", "200")],
                ),
            ],
            ..Default::default()
        };
        apply(&mut result, &roles(&["prod"], &["staging"]));

        assert_eq!(result.rows.len(), 1, "baseline consumed, one candidate row");
        let r = &result.rows[0];
        assert_eq!(r.target.as_deref(), Some("staging"));
        assert_eq!(r.cells.get("proc.overall"), Some(&"REVIEW".to_string()));
        // HttpStatus is an intrinsic — not part of the diff, and it matched anyway.
        assert_eq!(
            r.cells.get(RESULT_COLUMN),
            Some(&"overall: CLEAR→REVIEW".to_string())
        );
        assert_eq!(
            result.column_order.first(),
            Some(&RESULT_COLUMN.to_string())
        );
    }

    #[test]
    fn matching_fields_report_ok() {
        let mut result = ReportResult {
            rows: vec![
                row(&["a"], "prod", &[("proc.overall", "CLEAR")]),
                row(&["a"], "staging", &[("proc.overall", "CLEAR")]),
            ],
            ..Default::default()
        };
        apply(&mut result, &roles(&["prod"], &["staging"]));
        assert_eq!(
            result.rows[0].cells.get(RESULT_COLUMN),
            Some(&MATCH.to_string())
        );
    }

    #[test]
    fn candidate_without_baseline_is_flagged() {
        let mut result = ReportResult {
            rows: vec![row(&["a"], "staging", &[("proc.overall", "CLEAR")])],
            ..Default::default()
        };
        apply(&mut result, &roles(&["prod"], &["staging"]));
        assert_eq!(result.rows.len(), 1);
        assert_eq!(
            result.rows[0].cells.get(RESULT_COLUMN),
            Some(&NO_BASELINE.to_string())
        );
    }

    #[test]
    fn baseline_without_candidate_still_emits() {
        let mut result = ReportResult {
            rows: vec![row(&["a"], "prod", &[("proc.overall", "CLEAR")])],
            ..Default::default()
        };
        apply(&mut result, &roles(&["prod"], &["staging"]));
        assert_eq!(result.rows.len(), 1);
        assert_eq!(
            result.rows[0].cells.get(RESULT_COLUMN),
            Some(&NO_CANDIDATE.to_string())
        );
    }

    #[test]
    fn fieldless_request_falls_back_to_response() {
        let mut result = ReportResult {
            rows: vec![
                row(
                    &["a"],
                    "prod",
                    &[("proc.HttpStatus", "200"), ("proc.Response", "{\"x\":1}")],
                ),
                row(
                    &["a"],
                    "staging",
                    &[("proc.HttpStatus", "200"), ("proc.Response", "{\"x\":2}")],
                ),
            ],
            ..Default::default()
        };
        apply(&mut result, &roles(&["prod"], &["staging"]));
        assert_eq!(
            result.rows[0].cells.get(RESULT_COLUMN),
            Some(&"Response: {\"x\":1}→{\"x\":2}".to_string())
        );
    }

    #[test]
    fn multiple_comparisons_group_by_key() {
        let mut result = ReportResult {
            rows: vec![
                row(&["a"], "prod", &[("proc.v", "1")]),
                row(&["b"], "prod", &[("proc.v", "1")]),
                row(&["a"], "au", &[("proc.v", "1")]),
                row(&["b"], "au", &[("proc.v", "9")]),
                row(&["a"], "eu", &[("proc.v", "2")]),
                row(&["b"], "eu", &[("proc.v", "1")]),
            ],
            ..Default::default()
        };
        apply(&mut result, &roles(&["prod"], &["au", "eu"]));
        // Grouped by key (a then b), comparisons in clause order (au then eu).
        let got: Vec<(&str, Option<&String>)> = result
            .rows
            .iter()
            .map(|r| (r.target.as_deref().unwrap(), r.cells.get(RESULT_COLUMN)))
            .collect();
        assert_eq!(
            got,
            vec![
                ("au", Some(&MATCH.to_string())),
                ("eu", Some(&"v: 1→2".to_string())),
                ("au", Some(&"v: 1→9".to_string())),
                ("eu", Some(&MATCH.to_string())),
            ]
        );
    }
}
