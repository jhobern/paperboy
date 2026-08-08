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
//!
//! The `Result` cell for differing rows is a compact single-line JSON object
//! keyed by environment name, showing only fields that differ: the baseline
//! entry has `(baseline)` suffix, the candidate entry carries just its environment
//! name. Field values that parse as JSON are embedded structurally; other values
//! are embedded as JSON strings.

use std::collections::{BTreeSet, HashMap, HashSet};

use super::flow::{EnvClause, FlowNode, ReportFlow};
use super::model::{ReportResult, ReportRow};

/// The reserved output column that carries the comparison outcome. Added to the
/// default column order (at the front) only when the flow configures a
/// comparison run; rename it in `columns:` like any column (`Result AS …`).
pub const RESULT_COLUMN: &str = "Result";

/// `Result` value when the candidate matches the baseline on every compared
/// field. A descriptive phrase (rather than a bare "OK") so a matching row is
/// self-explanatory in an exported CSV/JSON. Kept a plain data constant like
/// its `NO_BASELINE`/`NO_CANDIDATE` siblings — these are report *data* written
/// to shared files, not UI chrome, so they are not translated per UI language.
pub const MATCH: &str = "Comparison matched baseline";
/// `Result` value for a candidate row whose row key has no baseline sibling.
pub const NO_BASELINE: &str = "no baseline";
/// `Result` value for a baseline row whose row key produced no candidate.
pub const NO_CANDIDATE: &str = "no candidate";

/// Per-request intrinsic column suffixes. These are excluded from the compared
/// "reported fields": `Time` (and its `TimeSetup`/`TimeWait`/`TimeDownload`
/// parts) always differs, and `HttpStatus`/`Asserts`/`Error`/`Response` already
/// have their own columns. A request with *no* reported fields falls back to
/// comparing its `Response` (see [`comparable_keys`]).
const INTRINSICS: [&str; 8] = [
    "HttpStatus",
    "Time",
    "TimeSetup",
    "TimeWait",
    "TimeDownload",
    "Asserts",
    "Error",
    "Response",
];

/// The baseline/comparison environment roles that drive a comparison run.
pub struct Roles {
    /// Environment names that act as the reference (usually one).
    baseline: HashSet<String>,
    /// Candidate environment names, in clause order (each compared to the
    /// baseline; duplicates removed).
    comparisons: Vec<String>,
    /// Field suffixes from the `BASELINE(…) SHOW(…)` clause.  For each such
    /// field, matching baseline cells (`<alias>.<field>`) are copied into the
    /// candidate row as `baseline.<alias>.<field>` — but only for aliases where
    /// the candidate itself emits that field.
    baseline_show: Vec<String>,
}

/// Extract the comparison roles a flow configures, or `None` when it has no
/// `ENVS` role clause with a baseline (a plain `ENVS` list, or no `ENVS` loop at
/// all, produces per-env rows with no diff — unchanged behaviour).
pub fn comparison_roles(flow: &ReportFlow) -> Option<Roles> {
    let mut baseline = HashSet::new();
    let mut comparisons = Vec::new();
    let mut baseline_show = Vec::new();
    collect_roles(
        &flow.nodes,
        &mut baseline,
        &mut comparisons,
        &mut baseline_show,
    );
    if baseline.is_empty() {
        return None;
    }
    Some(Roles {
        baseline,
        comparisons,
        baseline_show,
    })
}

/// Walk the flow tree, unioning every `ENVS BASELINE/COMPARISON` clause's names.
fn collect_roles(
    nodes: &[FlowNode],
    baseline: &mut HashSet<String>,
    comparisons: &mut Vec<String>,
    baseline_show: &mut Vec<String>,
) {
    for node in nodes {
        match node {
            FlowNode::ForEnvs { clause, body, .. } => {
                if let EnvClause::Roles {
                    baseline: b,
                    comparisons: c,
                    baseline_show: s,
                } = clause
                {
                    // A role's comparison *target* is its name (a live env) or
                    // its snapshot path (a `FILE(…)`); either way the produced /
                    // injected rows carry that string as their target.
                    for r in b {
                        baseline.insert(r.target().to_string());
                    }
                    for r in c {
                        let name = r.target().to_string();
                        if !comparisons.contains(&name) {
                            comparisons.push(name);
                        }
                    }
                    for field in s {
                        if !baseline_show.contains(field) {
                            baseline_show.push(field.clone());
                        }
                    }
                }
                collect_roles(body, baseline, comparisons, baseline_show);
            }
            FlowNode::ForEach { body, .. } => {
                collect_roles(body, baseline, comparisons, baseline_show)
            }
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
                // Copy baseline cells for each SHOW field, but only for aliases
                // where the candidate row already emits that field.  This avoids
                // inventing columns for requests that don't report the field at all.
                if let Some(base) = baseline {
                    for field in &roles.baseline_show {
                        for cand_key in cand.cells.keys().cloned().collect::<Vec<_>>() {
                            if let Some((alias, f)) = cand_key.split_once('.')
                                && f == field
                            {
                                let base_col = format!("baseline.{alias}.{field}");
                                if let Some(val) = base.cells.get(&cand_key) {
                                    cand.cells
                                        .entry(base_col.clone())
                                        .or_insert_with(|| val.clone());
                                    result.note_column(&base_col);
                                }
                            }
                        }
                    }
                }
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
/// compact single-line JSON object keyed by environment name (baseline with
/// `(baseline)` suffix, candidate as-is) showing only differing fields. Field
/// values that parse as JSON are embedded structurally; others as JSON strings.
/// Returns [`MATCH`] when all agree, or [`NO_BASELINE`] when there is no baseline.
pub(super) fn compute_result(baseline: Option<&ReportRow>, cand: &ReportRow) -> String {
    let Some(base) = baseline else {
        return NO_BASELINE.to_string();
    };

    let mut base_diffs = serde_json::Map::new();
    let mut cand_diffs = serde_json::Map::new();

    for key in comparable_keys(cand, base) {
        let b = base.cells.get(&key).map(String::as_str).unwrap_or("");
        let c = cand.cells.get(&key).map(String::as_str).unwrap_or("");
        if b != c {
            let label = key.rsplit('.').next().unwrap_or(&key);
            // Try to parse as JSON; if successful, embed structurally, else as string.
            let b_value = serde_json::from_str::<serde_json::Value>(b)
                .unwrap_or_else(|_| serde_json::Value::String(b.to_string()));
            let c_value = serde_json::from_str::<serde_json::Value>(c)
                .unwrap_or_else(|_| serde_json::Value::String(c.to_string()));
            base_diffs.insert(label.to_string(), b_value);
            cand_diffs.insert(label.to_string(), c_value);
        }
    }

    if base_diffs.is_empty() {
        MATCH.to_string()
    } else {
        // Build the outer object with baseline and candidate entries.
        let base_env = base.target.as_deref().unwrap_or("baseline");
        let cand_env = cand.target.as_deref().unwrap_or("comparison");
        let mut result = serde_json::Map::new();
        result.insert(
            format!("{} (baseline)", base_env),
            serde_json::Value::Object(base_diffs),
        );
        result.insert(cand_env.to_string(), serde_json::Value::Object(cand_diffs));
        // Render as compact single-line JSON.
        serde_json::to_string(&serde_json::Value::Object(result))
            .unwrap_or_else(|_| MATCH.to_string())
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
            baseline_show: Vec::new(),
        }
    }

    fn roles_show(baseline: &[&str], comparisons: &[&str], show: &[&str]) -> Roles {
        Roles {
            baseline: baseline.iter().map(|s| s.to_string()).collect(),
            comparisons: comparisons.iter().map(|s| s.to_string()).collect(),
            baseline_show: show.iter().map(|s| s.to_string()).collect(),
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
        let result_cell = r.cells.get(RESULT_COLUMN).expect("Result column");
        // Parse the JSON to verify structure.
        let parsed: serde_json::Value = serde_json::from_str(result_cell).expect("valid JSON");
        let obj = parsed.as_object().expect("object");
        assert!(obj.contains_key("prod (baseline)"), "has baseline key");
        assert!(obj.contains_key("staging"), "has candidate key");
        assert_eq!(
            obj["prod (baseline)"]["overall"],
            serde_json::Value::String("CLEAR".to_string())
        );
        assert_eq!(
            obj["staging"]["overall"],
            serde_json::Value::String("REVIEW".to_string())
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
        let result_cell = result.rows[0].cells.get(RESULT_COLUMN).expect("Result");
        // Parse the JSON to verify structure: Response values should be embedded as
        // parsed objects (not escaped strings).
        let parsed: serde_json::Value = serde_json::from_str(result_cell).expect("valid JSON");
        let obj = parsed.as_object().expect("object");
        assert_eq!(obj["prod (baseline)"]["Response"]["x"], 1);
        assert_eq!(obj["staging"]["Response"]["x"], 2);
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
        assert_eq!(result.rows.len(), 4);

        // Row 0: key "a", target "au" - should match (both "1").
        assert_eq!(result.rows[0].target.as_deref(), Some("au"));
        assert_eq!(
            result.rows[0].cells.get(RESULT_COLUMN),
            Some(&MATCH.to_string())
        );

        // Row 1: key "a", target "eu" - should differ ("1" → "2").
        assert_eq!(result.rows[1].target.as_deref(), Some("eu"));
        let r1_cell = result.rows[1].cells.get(RESULT_COLUMN).expect("Result");
        let parsed: serde_json::Value = serde_json::from_str(r1_cell).expect("valid JSON");
        assert_eq!(parsed["prod (baseline)"]["v"], serde_json::json!(1));
        assert_eq!(parsed["eu"]["v"], serde_json::json!(2));

        // Row 2: key "b", target "au" - should differ ("1" → "9").
        assert_eq!(result.rows[2].target.as_deref(), Some("au"));
        let r2_cell = result.rows[2].cells.get(RESULT_COLUMN).expect("Result");
        let parsed: serde_json::Value = serde_json::from_str(r2_cell).expect("valid JSON");
        assert_eq!(parsed["prod (baseline)"]["v"], serde_json::json!(1));
        assert_eq!(parsed["au"]["v"], serde_json::json!(9));

        // Row 3: key "b", target "eu" - should match (both "1").
        assert_eq!(result.rows[3].target.as_deref(), Some("eu"));
        assert_eq!(
            result.rows[3].cells.get(RESULT_COLUMN),
            Some(&MATCH.to_string())
        );
    }

    #[test]
    fn result_embeds_json_values_structurally_and_is_single_line() {
        // Baseline and candidate have JSON array values that differ; the Result
        // should embed them as arrays (not escaped strings), and the whole result
        // must be a single line (no newlines).
        let mut result = ReportResult {
            rows: vec![
                row(
                    &["a"],
                    "staging",
                    &[(
                        "proc.Breakdown",
                        "[{\"key\":\"detail_check\",\"value\":\"Pass\"},{\"key\":\"photo_check\",\"value\":\"Fail\"}]",
                    )],
                ),
                row(
                    &["a"],
                    "dev",
                    &[(
                        "proc.Breakdown",
                        "[{\"key\":\"detail_check\",\"value\":\"Pass\"},{\"key\":\"photo_check\",\"value\":\"Pass\"}]",
                    )],
                ),
            ],
            ..Default::default()
        };
        apply(&mut result, &roles(&["staging"], &["dev"]));

        let result_cell = result.rows[0].cells.get(RESULT_COLUMN).expect("Result");

        // 1. Must be a single line (no newlines).
        assert!(!result_cell.contains('\n'), "Result must be single line");

        // 2. Must round-trip via serde_json.
        let parsed: serde_json::Value =
            serde_json::from_str(result_cell).expect("Result must be valid JSON");

        // 3. Values must be embedded structurally (arrays, not escaped strings).
        let obj = parsed.as_object().expect("Result is an object");
        let baseline_obj = obj["staging (baseline)"]
            .as_object()
            .expect("baseline is object");
        let dev_obj = obj["dev"].as_object().expect("dev is object");

        let baseline_breakdown = baseline_obj["Breakdown"]
            .as_array()
            .expect("baseline Breakdown is array");
        let dev_breakdown = dev_obj["Breakdown"]
            .as_array()
            .expect("dev Breakdown is array");

        assert_eq!(baseline_breakdown.len(), 2);
        assert_eq!(dev_breakdown.len(), 2);

        // Verify the nested structure of the arrays.
        assert_eq!(baseline_breakdown[0]["key"], "detail_check");
        assert_eq!(baseline_breakdown[0]["value"], "Pass");
        assert_eq!(baseline_breakdown[1]["key"], "photo_check");
        assert_eq!(baseline_breakdown[1]["value"], "Fail");

        assert_eq!(dev_breakdown[0]["key"], "detail_check");
        assert_eq!(dev_breakdown[0]["value"], "Pass");
        assert_eq!(dev_breakdown[1]["key"], "photo_check");
        assert_eq!(dev_breakdown[1]["value"], "Pass");

        // 4. Exact single-line string for this small case (verifies ordering).
        let expected = "{\"staging (baseline)\":{\"Breakdown\":[{\"key\":\"detail_check\",\"value\":\"Pass\"},{\"key\":\"photo_check\",\"value\":\"Fail\"}]},\"dev\":{\"Breakdown\":[{\"key\":\"detail_check\",\"value\":\"Pass\"},{\"key\":\"photo_check\",\"value\":\"Pass\"}]}}";
        assert_eq!(result_cell, expected);
    }

    #[test]
    fn baseline_show_copies_matching_alias_cells() {
        // proc.Time exists on both baseline and candidate → baseline.proc.Time added.
        // aux has no Time on the candidate → no baseline.aux.Time added.
        let mut result = ReportResult {
            rows: vec![
                row(
                    &["a"],
                    "prod",
                    &[
                        ("proc.Time", "100ms"),
                        ("aux.Time", "50ms"),
                        ("proc.overall", "CLEAR"),
                    ],
                ),
                row(
                    &["a"],
                    "staging",
                    // candidate has proc.Time but no aux.Time
                    &[("proc.Time", "120ms"), ("proc.overall", "CLEAR")],
                ),
            ],
            ..Default::default()
        };
        apply(&mut result, &roles_show(&["prod"], &["staging"], &["Time"]));

        assert_eq!(result.rows.len(), 1);
        let r = &result.rows[0];
        // baseline.proc.Time copied because candidate emits proc.Time
        assert_eq!(
            r.cells.get("baseline.proc.Time"),
            Some(&"100ms".to_string())
        );
        // baseline.aux.Time NOT added because candidate has no aux.Time
        assert!(!r.cells.contains_key("baseline.aux.Time"));
        // The new column is registered in column_order
        assert!(
            result
                .column_order
                .contains(&"baseline.proc.Time".to_string())
        );
    }

    #[test]
    fn baseline_show_extracted_from_flow() {
        let flow = parse_flow(
            "FOR T IN ENVS BASELINE(\"prod\") SHOW(Time), COMPARISON(\"staging\")\n  REQUEST r\nEND\n",
        )
        .unwrap();
        let r = comparison_roles(&flow).expect("roles");
        assert!(r.baseline.contains("prod"));
        assert_eq!(r.comparisons, vec!["staging"]);
        assert_eq!(r.baseline_show, vec!["Time"]);
    }
}
