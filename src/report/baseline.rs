//! Snapshot comparison — the `Result` column's *Source B* (build-breakdown
//! phase 11).
//!
//! Where [`super::compare`] diffs environments *within one run* (an `ENVS
//! BASELINE/COMPARISON` clause), this module diffs a run against a **saved
//! snapshot** of an earlier run — the "compare a new release against the last
//! accepted results" workflow. A snapshot is a `.baseline` JSON file written
//! from the results grid; a report references one with a `# baseline:` header
//! directive, and every subsequent run diffs its reported fields against it to
//! fill the same `Result` column.
//!
//! Like `compare`, this is a pure post-processing pass over a produced
//! [`ReportResult`] and reuses that module's field-selection
//! ([`super::compare::comparable_keys`]) and diff
//! ([`super::compare::compute_result`]) so the snapshot and env-to-env verdicts
//! read identically.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::compare::{self, NO_CANDIDATE, RESULT_COLUMN};
use super::model::{ReportResult, ReportRow};

/// The on-disk schema version. Bumped only if the stored shape changes
/// incompatibly; `load` rejects versions it doesn't understand so a stale file
/// fails loudly rather than diffing against garbage.
const BASELINE_VERSION: u32 = 1;

/// A saved run snapshot: the rows a previous run produced, in a stable JSON
/// schema decoupled from the in-memory [`ReportRow`] so model changes don't
/// silently invalidate stored baselines.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Baseline {
    pub version: u32,
    pub rows: Vec<BaselineRow>,
}

/// One snapshotted row: the same `key`/`cells`/`vars`/`target` a [`ReportRow`]
/// carries, so it can be diffed against a fresh row or reconstructed into one
/// when it has no current candidate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaselineRow {
    pub key: Vec<String>,
    pub cells: HashMap<String, String>,
    #[serde(default)]
    pub vars: HashMap<String, String>,
    #[serde(default)]
    pub target: Option<String>,
}

impl BaselineRow {
    /// Reconstruct an in-memory [`ReportRow`] from the snapshot — used to feed
    /// the diff, to emit a `no candidate` row for a snapshot key the current run
    /// didn't produce, and to inject a `FILE(…)` role's rows in [`super::run`].
    pub(crate) fn to_row(&self) -> ReportRow {
        ReportRow {
            cells: self.cells.clone(),
            vars: self.vars.clone(),
            key: self.key.clone(),
            path: Vec::new(),
            target: self.target.clone(),
        }
    }
}

impl Baseline {
    /// Snapshot a completed run's rows for later comparison.
    pub fn from_result(result: &ReportResult) -> Self {
        Baseline {
            version: BASELINE_VERSION,
            rows: result
                .rows
                .iter()
                .map(|r| BaselineRow {
                    key: r.key.clone(),
                    cells: r.cells.clone(),
                    vars: r.vars.clone(),
                    target: r.target.clone(),
                })
                .collect(),
        }
    }

    /// Serialize to pretty JSON (stable key order — the crate builds
    /// `serde_json` with `preserve_order`).
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    /// Write the snapshot to `path` as JSON.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        std::fs::write(path, self.to_json())
    }

    /// Load a snapshot from `path`, rejecting an unrecognised schema version.
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let baseline: Baseline = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        if baseline.version != BASELINE_VERSION {
            return Err(format!(
                "unsupported baseline version {} (expected {BASELINE_VERSION})",
                baseline.version
            ));
        }
        Ok(baseline)
    }
}

/// Diff the current run against a saved `baseline`, filling the `Result` column
/// in place — the snapshot analogue of [`super::compare::apply`]. Each current
/// row is aligned to a snapshot row by [row key](ReportRow::key); the verdict is
/// the same `field: base→cand` summary. Current rows with no snapshot sibling
/// read `no baseline`; snapshot rows with no current candidate are appended as
/// `no candidate` rows reconstructed from the snapshot.
pub fn apply(result: &mut ReportResult, baseline: &Baseline) {
    // Surface the reserved column at the front so a `columns:`-free run still
    // shows the diff (mirrors `compare::apply`).
    if !result.column_order.iter().any(|c| c == RESULT_COLUMN) {
        result.column_order.insert(0, RESULT_COLUMN.to_string());
    }

    // Index the snapshot by key (first row wins on a duplicate key — the same
    // first-match rule `compare::apply` uses for baseline rows).
    let mut base_by_key: HashMap<&[String], ReportRow> = HashMap::new();
    for br in &baseline.rows {
        base_by_key
            .entry(br.key.as_slice())
            .or_insert_with(|| br.to_row());
    }

    // Diff each produced row against its snapshot sibling.
    let mut matched: HashSet<Vec<String>> = HashSet::new();
    for row in &mut result.rows {
        let base = base_by_key.get(row.key.as_slice());
        if base.is_some() {
            matched.insert(row.key.clone());
        }
        let verdict = compare::compute_result(base, row);
        row.cells.insert(RESULT_COLUMN.to_string(), verdict);
    }

    // A snapshot key the current run never produced still emits a row (its
    // stored values), flagged — in snapshot order, deduped by key.
    let mut emitted: HashSet<&[String]> = HashSet::new();
    for br in &baseline.rows {
        if matched.contains(&br.key) || !emitted.insert(br.key.as_slice()) {
            continue;
        }
        let mut row = br.to_row();
        row.cells
            .insert(RESULT_COLUMN.to_string(), NO_CANDIDATE.to_string());
        result.rows.push(row);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::compare::{MATCH, NO_BASELINE};

    fn row(key: &[&str], cells: &[(&str, &str)]) -> ReportRow {
        ReportRow {
            cells: cells
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            vars: HashMap::new(),
            key: key.iter().map(|k| k.to_string()).collect(),
            path: Vec::new(),
            target: None,
        }
    }

    fn result(rows: Vec<ReportRow>) -> ReportResult {
        ReportResult {
            rows,
            ..Default::default()
        }
    }

    #[test]
    fn snapshot_round_trips_through_json() {
        let res = result(vec![
            row(&["a"], &[("proc.overall", "CLEAR")]),
            row(&["b"], &[("proc.overall", "REVIEW")]),
        ]);
        let snap = Baseline::from_result(&res);
        let json = snap.to_json();
        let back: Baseline = serde_json::from_str(&json).unwrap();
        assert_eq!(back.version, BASELINE_VERSION);
        assert_eq!(back.rows.len(), 2);
        assert_eq!(back.rows[0].key, vec!["a".to_string()]);
        assert_eq!(
            back.rows[1].cells.get("proc.overall"),
            Some(&"REVIEW".to_string())
        );
    }

    #[test]
    fn load_rejects_unknown_version() {
        let json = r#"{"version":999,"rows":[]}"#;
        let dir = std::env::temp_dir();
        let path = dir.join(format!("pb_baseline_ver_{}.baseline", std::process::id()));
        std::fs::write(&path, json).unwrap();
        let err = Baseline::load(&path).unwrap_err();
        assert!(err.contains("unsupported baseline version 999"), "{err}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn matching_row_reports_ok_and_adds_result_column() {
        let snap = Baseline::from_result(&result(vec![row(&["a"], &[("proc.overall", "CLEAR")])]));
        let mut res = result(vec![row(&["a"], &[("proc.overall", "CLEAR")])]);
        apply(&mut res, &snap);
        assert_eq!(res.column_order.first(), Some(&RESULT_COLUMN.to_string()));
        assert_eq!(res.rows.len(), 1);
        assert_eq!(
            res.rows[0].cells.get(RESULT_COLUMN),
            Some(&MATCH.to_string())
        );
    }

    #[test]
    fn differing_field_is_summarised() {
        let snap = Baseline::from_result(&result(vec![row(&["a"], &[("proc.overall", "CLEAR")])]));
        let mut res = result(vec![row(&["a"], &[("proc.overall", "REVIEW")])]);
        apply(&mut res, &snap);
        let result_cell = res.rows[0].cells.get(RESULT_COLUMN).expect("Result column");
        // Parse the JSON to verify structure.
        let parsed: serde_json::Value = serde_json::from_str(result_cell).expect("valid JSON");
        let obj = parsed.as_object().expect("object");
        assert!(obj.contains_key("baseline (baseline)"));
        assert!(obj.contains_key("comparison"));
        assert_eq!(obj["baseline (baseline)"]["overall"], "CLEAR");
        assert_eq!(obj["comparison"]["overall"], "REVIEW");
    }

    #[test]
    fn current_row_without_snapshot_reads_no_baseline() {
        let snap = Baseline::from_result(&result(vec![]));
        let mut res = result(vec![row(&["a"], &[("proc.overall", "CLEAR")])]);
        apply(&mut res, &snap);
        assert_eq!(
            res.rows[0].cells.get(RESULT_COLUMN),
            Some(&NO_BASELINE.to_string())
        );
    }

    #[test]
    fn snapshot_row_without_candidate_is_appended() {
        let snap = Baseline::from_result(&result(vec![
            row(&["a"], &[("proc.overall", "CLEAR")]),
            row(&["b"], &[("proc.overall", "CLEAR")]),
        ]));
        let mut res = result(vec![row(&["a"], &[("proc.overall", "CLEAR")])]);
        apply(&mut res, &snap);
        assert_eq!(res.rows.len(), 2, "matched row + appended no-candidate row");
        assert_eq!(
            res.rows[0].cells.get(RESULT_COLUMN),
            Some(&MATCH.to_string())
        );
        let appended = &res.rows[1];
        assert_eq!(appended.key, vec!["b".to_string()]);
        assert_eq!(
            appended.cells.get(RESULT_COLUMN),
            Some(&NO_CANDIDATE.to_string())
        );
        // The snapshot's stored cells survive so the appended row still renders.
        assert_eq!(
            appended.cells.get("proc.overall"),
            Some(&"CLEAR".to_string())
        );
    }

    #[test]
    fn fieldless_request_falls_back_to_response() {
        let snap = Baseline::from_result(&result(vec![row(
            &["a"],
            &[("proc.HttpStatus", "200"), ("proc.Response", "{\"x\":1}")],
        )]));
        let mut res = result(vec![row(
            &["a"],
            &[("proc.HttpStatus", "200"), ("proc.Response", "{\"x\":2}")],
        )]);
        apply(&mut res, &snap);
        let result_cell = res.rows[0].cells.get(RESULT_COLUMN).expect("Result");
        // Parse the JSON to verify structure: Response values should be embedded as
        // parsed objects (not escaped strings).
        let parsed: serde_json::Value = serde_json::from_str(result_cell).expect("valid JSON");
        let obj = parsed.as_object().expect("object");
        assert_eq!(obj["baseline (baseline)"]["Response"]["x"], 1);
        assert_eq!(obj["comparison"]["Response"]["x"], 2);
    }
}
