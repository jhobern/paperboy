//! Re-running some rows of a finished report and splicing the fresh answers in.
//!
//! A long run that hit one transient failure — a dropped connection, a 503
//! from an endpoint having a bad minute — leaves a row whose cells are wrong,
//! and in a comparison that row reads as a regression or an improvement that
//! never happened. Re-running the whole report to fix one row is slow and
//! re-rolls every other row's dice too, so this re-produces just the rows
//! asked for and replaces them in place.
//!
//! A row is re-produced the way it was produced: the flow is walked again with
//! only the loop iterations that lead to it enabled (see
//! [`RowSelection`]), everything outside those loops runs as before, and the
//! result goes through the same [`finalize`] as a whole run — so a comparison
//! row is collapsed against a freshly run baseline, and its `Result`,
//! `Correct` and `Trend` are worked out by the code that worked them out the
//! first time.

use std::collections::HashMap;

use super::flow::ReportFlow;
use super::model::{ReportResult, ReportRow, RowRole};
use super::run::{RowSelection, RunContext, finalize, run_flow_raw_selected};

/// Re-produce the rows of an earlier run of `flow` found at `paths`, returning
/// a finalized result holding them (and possibly other rows the same loop
/// iterations produced — [`splice`] picks out the ones it needs by path).
pub fn rerun_rows(
    flow: &ReportFlow,
    ctx: &RunContext,
    paths: &[Vec<(usize, usize)>],
) -> ReportResult {
    let selection = RowSelection::for_paths(flow, paths);
    let mut fresh = run_flow_raw_selected(flow, ctx, Some(&selection));
    finalize(&mut fresh, flow, ctx);
    fresh
}

/// What re-running rows of a report *still being run* produces: the fresh raw
/// rows, for the live grid (which holds raw rows), and the same rows finalized,
/// to be [`splice`]d into the finished result once the run delivers it —
/// without which the run's own result, made from the old rows, would quietly
/// put the old answer back.
pub struct LiveRerun {
    pub raw: ReportResult,
    pub finished: ReportResult,
}

/// Re-produce the rows at `paths` of a run of `flow` that is still going.
pub fn rerun_live(flow: &ReportFlow, ctx: &RunContext, paths: &[Vec<(usize, usize)>]) -> LiveRerun {
    let selection = RowSelection::for_paths(flow, paths);
    let raw = run_flow_raw_selected(flow, ctx, Some(&selection));
    let mut finished = raw.clone();
    finalize(&mut finished, flow, ctx);
    LiveRerun { raw, finished }
}

/// Resume a stopped run: produce the raw rows still pending in `prior` (the
/// grid the stop left), put them in their slots, and finalize the whole grid
/// as the run would have. Streams through `ctx.sink` like any run, so a live
/// grid fills in as the rows land. Rows the resumed run doesn't produce
/// either (stopped again) stay pending, and the result is left unfinalized.
pub fn resume(flow: &ReportFlow, ctx: &RunContext, prior: ReportResult) -> ReportResult {
    let paths: Vec<Vec<(usize, usize)>> = prior
        .pending
        .iter()
        .filter_map(|&r| prior.rows.get(r))
        .map(|row| row.path.clone())
        .collect();
    let fresh = run_flow_raw_selected(flow, ctx, Some(&RowSelection::exact(&paths)));
    let mut merged = prior;
    let slot: HashMap<Vec<(usize, usize)>, usize> = merged
        .rows
        .iter()
        .enumerate()
        .map(|(i, row)| (row.path.clone(), i))
        .collect();
    for row in fresh.rows {
        if let Some(&i) = slot.get(&row.path)
            && merged.pending.remove(&i)
        {
            merged.rows[i] = row;
        }
    }
    for c in &fresh.column_order {
        merged.note_column(c);
    }
    merged.timing_columns.extend(fresh.timing_columns);
    merged.duration_columns.extend(fresh.duration_columns);
    merged.errors.extend(fresh.errors);
    merged.warnings.extend(fresh.warnings);
    merged.skipped.extend(fresh.skipped);
    for (written, got) in fresh.role_targets {
        let seen = merged.role_targets.entry(written).or_default();
        for g in got {
            if !seen.contains(&g) {
                seen.push(g);
            }
        }
    }
    if merged.pending.is_empty() {
        merged.partial = None;
        finalize(&mut merged, flow, ctx);
    }
    merged
}

/// Replace rows of a live grid with their fresh raw counterparts, matched by
/// path and key — every side of a comparison the re-run ran, not only the row
/// that was picked. Returns the grid rows replaced. Rows still pending are
/// left alone: the run itself is about to fill them.
pub fn splice_live(live: &mut ReportResult, fresh: &ReportResult) -> Vec<usize> {
    let mut replaced = Vec::new();
    for row in &fresh.rows {
        let mut matches = live
            .rows
            .iter()
            .enumerate()
            .filter(|(i, r)| !live.pending.contains(i) && r.path == row.path && r.key == row.key)
            .map(|(i, _)| i);
        if let (Some(i), None) = (matches.next(), matches.next()) {
            live.rows[i] = row.clone();
            replaced.push(i);
        }
    }
    replaced
}

/// Replace rows `targets` of `result` with their counterparts in `fresh` (a
/// [`rerun_rows`] result), matched by structural path. Returns the rows that
/// were replaced; a target with no counterpart is left exactly as it was.
///
/// Everything keyed by row index moves with the row: images, verdicts, truths,
/// trends and the kept baseline. The run's flat error list has the replaced
/// row's errors (its baseline's included) swapped for the fresh row's — see
/// [`reconcile_errors`] for how that is done when the list's wording names a
/// step rather than a row.
pub fn splice(result: &mut ReportResult, fresh: &ReportResult, targets: &[usize]) -> Vec<usize> {
    let mut replaced = Vec::new();
    for &r in targets {
        let Some(old) = result.rows.get(r) else {
            continue;
        };
        // Path *and* key, and only an unambiguous match. Rows that never came
        // from a loop iteration — a saved snapshot's rows, shown as `no
        // candidate` — all carry an empty path, and taking whichever of them
        // came last replaced one row with another.
        let mut matches = fresh
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.path == old.path && row.key == old.key)
            .map(|(j, _)| j);
        let (Some(j), None) = (matches.next(), matches.next()) else {
            continue;
        };
        let old_claims = claims(result, r);
        result.rows[r] = fresh.rows[j].clone();
        rekey(&mut result.images, &fresh.images, r, j);
        rekey(&mut result.verdicts, &fresh.verdicts, r, j);
        rekey(&mut result.truths, &fresh.truths, r, j);
        rekey(&mut result.trends, &fresh.trends, r, j);
        move_entry(&mut result.baseline_rows, &fresh.baseline_rows, r, j);
        move_entry(&mut result.baseline_errors, &fresh.baseline_errors, r, j);
        reconcile_errors(result, r, &old_claims, &claims(result, r));
        replaced.push(r);
    }
    if !replaced.is_empty() {
        for c in &fresh.column_order {
            result.note_column(c);
        }
        result
            .timing_columns
            .extend(fresh.timing_columns.iter().cloned());
        result
            .duration_columns
            .extend(fresh.duration_columns.iter().cloned());
        for w in &fresh.warnings {
            if !result.warnings.contains(w) {
                result.warnings.push(w.clone());
            }
        }
    }
    replaced
}

/// Which row of a *live* grid a re-run was queued for, kept independent of
/// that grid: a running report's rows are the uncollapsed projection, and the
/// finished result that replaces them numbers its rows differently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowRef {
    path: Vec<(usize, usize)>,
    key: Vec<String>,
    role: RowRole,
    comparison: Option<String>,
}

impl RowRef {
    pub fn of(row: &ReportRow) -> Self {
        RowRef {
            path: row.path.clone(),
            key: row.key.clone(),
            role: row.role,
            comparison: row.comparison.clone(),
        }
    }

    /// Whether `row` is the row this was taken from.
    pub fn is(&self, row: &ReportRow) -> bool {
        self.path == row.path && self.key == row.key
    }
}

/// The rows of a `finished` result that re-run the live rows `queued`, in
/// order and without repeats.
///
/// A row that survived the collapse keeps its path. A baseline row doesn't —
/// it is folded into the comparison row(s) measured against it — so it maps to
/// those: re-running a comparison row runs its baseline again anyway, and that
/// is the only place the baseline's answer is still shown.
pub fn rows_after_finalize(queued: &[RowRef], finished: &ReportResult) -> Vec<usize> {
    let mut out: Vec<usize> = Vec::new();
    for q in queued {
        let survived = finished.rows.iter().position(|row| q.is(row));
        let found: Vec<usize> = match survived {
            Some(r) => vec![r],
            None if q.role == RowRole::Baseline => finished
                .rows
                .iter()
                .enumerate()
                .filter(|(_, row)| {
                    row.role == RowRole::Candidate
                        && row.key == q.key
                        && row.comparison == q.comparison
                })
                .map(|(r, _)| r)
                .collect(),
            None => Vec::new(),
        };
        for r in found {
            if !out.contains(&r) {
                out.push(r);
            }
        }
    }
    out
}

/// The errors row `r` answers for: its own, plus its baseline's.
fn claims(result: &ReportResult, r: usize) -> Vec<String> {
    let mut out = result
        .rows
        .get(r)
        .map(|row| row.errors.clone())
        .unwrap_or_default();
    if let Some(base) = result.baseline_errors.get(&r) {
        out.extend(base.iter().cloned());
    }
    out
}

/// Swap the replaced row's errors for the fresh row's in the run's flat list.
///
/// The flat list holds a per-row error once per row that raised it, but an
/// error raised *above* a loop only once, however many rows inherited it. The
/// two are told apart by counting: a message listed fewer times than the rows
/// claiming it is a shared one, and is kept (exactly once) while anyone still
/// claims it; anything else is a per-row message and is swapped one for one.
fn reconcile_errors(result: &mut ReportResult, r: usize, old: &[String], new: &[String]) {
    let count = |list: &[String], e: &String| list.iter().filter(|x| *x == e).count();
    let mut seen: Vec<&String> = Vec::new();
    for e in old.iter().chain(new) {
        if seen.contains(&e) {
            continue;
        }
        seen.push(e);
        let flat = count(&result.errors, e);
        let others: usize = (0..result.rows.len())
            .filter(|&o| o != r)
            .map(|o| count(&claims(result, o), e))
            .sum();
        let (was, now) = (count(old, e), count(new, e));
        if flat < others + was {
            if now > 0 && flat == 0 {
                result.errors.push(e.clone());
            } else if now == 0 && others == 0 {
                result.errors.retain(|x| x != e);
            }
        } else {
            for _ in 0..was {
                if let Some(at) = result.errors.iter().position(|x| x == e) {
                    result.errors.remove(at);
                }
            }
            for _ in 0..now {
                result.errors.push(e.clone());
            }
        }
    }
}

fn rekey<V: Clone>(
    into: &mut HashMap<(usize, String), V>,
    from: &HashMap<(usize, String), V>,
    r: usize,
    j: usize,
) {
    into.retain(|(row, _), _| *row != r);
    for ((row, col), v) in from {
        if *row == j {
            into.insert((r, col.clone()), v.clone());
        }
    }
}

fn move_entry<V: Clone>(
    into: &mut HashMap<usize, V>,
    from: &HashMap<usize, V>,
    r: usize,
    j: usize,
) {
    match from.get(&j) {
        Some(v) => {
            into.insert(r, v.clone());
        }
        None => {
            into.remove(&r);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::model::ReportRow;

    fn row(path: usize, errors: &[&str]) -> ReportRow {
        ReportRow {
            path: vec![(0, path)],
            errors: errors.iter().map(|e| e.to_string()).collect(),
            ..Default::default()
        }
    }

    fn errors(list: &[&str]) -> Vec<String> {
        list.iter().map(|e| e.to_string()).collect()
    }

    /// Several rows failing with the same per-row message: each re-run takes
    /// exactly its own copy away, so the list is right once they are all fixed.
    #[test]
    fn a_message_several_rows_raised_loses_one_copy_per_fixed_row() {
        let mut result = ReportResult {
            rows: vec![
                row(0, &["svc: reset"]),
                row(1, &["svc: reset"]),
                row(2, &["svc: reset"]),
            ],
            errors: errors(&["svc: reset", "svc: reset", "svc: reset"]),
            ..Default::default()
        };
        let fresh = ReportResult {
            rows: vec![row(0, &[]), row(1, &[]), row(2, &[])],
            ..Default::default()
        };
        splice(&mut result, &fresh, &[1]);
        assert_eq!(result.errors.len(), 2);
        splice(&mut result, &fresh, &[0]);
        splice(&mut result, &fresh, &[2]);
        assert!(result.errors.is_empty(), "{:?}", result.errors);
    }

    /// An error raised above the loop is listed once but carried by every row;
    /// re-running one row neither drops it nor lists it a second time.
    #[test]
    fn an_inherited_message_stays_listed_once() {
        let mut result = ReportResult {
            rows: vec![row(0, &["login: 401"]), row(1, &["login: 401"])],
            errors: errors(&["login: 401"]),
            ..Default::default()
        };
        let still_failing = ReportResult {
            rows: vec![row(1, &["login: 401"])],
            ..Default::default()
        };
        splice(&mut result, &still_failing, &[1]);
        assert_eq!(result.errors, errors(&["login: 401"]));
        let fixed = ReportResult {
            rows: vec![row(1, &[])],
            ..Default::default()
        };
        splice(&mut result, &fixed, &[1]);
        assert_eq!(result.errors, errors(&["login: 401"]), "row 0 still has it");
    }

    /// A saved snapshot's rows all carry an empty path, so the path alone
    /// can't say which of them is which: the key has to agree too, and a
    /// match that is still ambiguous replaces nothing.
    #[test]
    fn rows_sharing_a_path_are_told_apart_by_key_or_left_alone() {
        let snap = |key: &str, v: &str| ReportRow {
            key: vec![key.to_string()],
            cells: [("v".to_string(), v.to_string())].into_iter().collect(),
            ..Default::default()
        };
        let mut result = ReportResult {
            rows: vec![snap("4", "old"), snap("5", "old")],
            ..Default::default()
        };
        let fresh = ReportResult {
            rows: vec![snap("4", "new"), snap("5", "new")],
            ..Default::default()
        };
        assert_eq!(splice(&mut result, &fresh, &[0]), vec![0]);
        assert_eq!(result.rows[0].key, vec!["4".to_string()]);
        assert_eq!(result.rows[0].cells["v"], "new");
        assert_eq!(result.rows[1].cells["v"], "old");

        let twins = ReportResult {
            rows: vec![snap("5", "a"), snap("5", "b")],
            ..Default::default()
        };
        assert!(splice(&mut result, &twins, &[1]).is_empty());
        assert_eq!(result.rows[1].cells["v"], "old");
    }

    #[test]
    fn a_row_with_no_counterpart_is_left_alone() {
        let mut result = ReportResult {
            rows: vec![row(0, &["svc: reset"])],
            errors: errors(&["svc: reset"]),
            ..Default::default()
        };
        let fresh = ReportResult {
            rows: vec![row(9, &[])],
            ..Default::default()
        };
        assert!(splice(&mut result, &fresh, &[0]).is_empty());
        assert_eq!(result.errors.len(), 1);
    }
}
