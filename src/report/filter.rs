//! Row classes and filters — "show me only the rows that matter".
//!
//! Every reader of a comparison or a ground-truthed run asks the same three
//! questions within a minute of opening it: *what changed?*, *what is wrong?*,
//! and *what did we break?* This module answers them once, off the row model,
//! so the interactive HTML export and the two in-app grids filter to exactly
//! the same rows.
//!
//! It lives beside [`super::metrics`] for the same reason that module exists:
//! the moment a renderer decides for itself what "regressed" means, two views
//! of one run start disagreeing about which rows they are hiding — and a filter
//! that quietly drops a row is worse than no filter at all.

use super::labels::LabelMap;
use super::metrics::Metrics;
use super::model::{OutputColumn, ReportResult, Trend, Verdict};

/// What is true of one row, read off the reserved columns the run already
/// filled in rather than recomputed. Cheap enough to build per row per frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RowFacts {
    /// The comparison found a difference on this row. `false` for a report with
    /// no comparison, and for a row that matched its baseline.
    pub differs: bool,
    /// The row's ground-truth roll-up, if it has one.
    pub verdict: Option<Verdict>,
    /// Which way the row moved against the baseline, if both sides were scored.
    pub trend: Option<Trend>,
}

impl RowFacts {
    /// The facts of row `r`.
    pub fn of(result: &ReportResult, r: usize) -> RowFacts {
        let Some(row) = result.rows.get(r) else {
            return RowFacts::default();
        };
        let cell = |name: &str| row.cells.get(name).map(String::as_str);
        RowFacts {
            // Any non-empty `Result` other than the "matched" phrase is a diff
            // listing. `no baseline`/`no candidate` count as differences: a row
            // that exists on only one side is exactly what a reader scanning
            // for changes needs to see.
            // A live case still waiting on its other side isn't one either.
            differs: cell(super::compare::RESULT_COLUMN).is_some_and(|v| {
                !v.trim().is_empty()
                    && v != super::compare::MATCH
                    && v != super::compare::WAITING_BASELINE
            }),
            verdict: cell(super::compare::CORRECT_COLUMN).and_then(|v| match v {
                v if v == Verdict::Correct.as_str() => Some(Verdict::Correct),
                v if v == Verdict::Incorrect.as_str() => Some(Verdict::Incorrect),
                v if v == Verdict::Untested.as_str() => Some(Verdict::Untested),
                _ => None,
            }),
            // Taken from the scored cells, not the column's text: `unchanged`
            // is what both a still-right and a still-wrong row say, and a
            // filter that confused the two would hide rows it claimed to show.
            // Rows carried over from a source with no scoring (nothing puts one
            // there today) fall back to the word, which can still distinguish
            // the two that matter.
            trend: result.row_trend(r).or_else(|| {
                cell(super::compare::TREND_COLUMN).and_then(|v| match v {
                    v if v == Trend::Fixed.as_str() => Some(Trend::Fixed),
                    v if v == Trend::Regressed.as_str() => Some(Trend::Regressed),
                    _ => None,
                })
            }),
        }
    }
}

/// One way of narrowing the table.
///
/// Deliberately a small closed set rather than a query language: these are the
/// questions people actually ask, and a filter you have to *write* is one
/// nobody uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowFilter {
    /// Every row (the default).
    All,
    /// Rows something went wrong on — a request that didn't come back, an
    /// assertion that didn't hold, a step that couldn't be resolved.
    ///
    /// The one filter that isn't about what the run *found*: it is about the
    /// run itself, and it is what a network blip leaves behind. Gathering
    /// those rows in one place is what makes re-running them one action
    /// instead of hunting them down a sorted column.
    Failed,
    /// Rows the comparison found a difference on.
    Differ,
    /// Rows whose ground-truth roll-up is `correct` — the other half of the
    /// `Correct` column, so its count card has rows to show.
    Correct,
    /// Rows whose ground-truth roll-up is `incorrect`. Untested rows are *not*
    /// included: "wrong" and "unchecked" are different problems, and merging
    /// them is how an unlabelled corpus comes to look like a broken engine.
    Incorrect,
    /// Rows that were right before and are wrong now — the ones a release
    /// decision turns on.
    Regressed,
    /// Rows that were wrong before and are right now — the other half of the
    /// same question. A release is judged on both: "we fixed nine and broke
    /// two" is a different story from "we broke two", and only offering the
    /// regressions made the second the only one the grid could tell.
    Fixed,
    /// While a run is going: rows that have come back whose comparison pair
    /// hasn't — one side landed, the other still on its way. Only ever offered
    /// for a live grid (see [`waiting_rows`]); a finished report has nothing
    /// waiting, so it selects nothing there.
    Waiting,
    /// One cell of a confusion matrix: rows where `column`'s ground truth is
    /// the `truth` class and its answer is the `answer` class. This is what
    /// makes the matrix clickable — "which 7 rows are those?" is the first
    /// question anyone asks of an off-diagonal count.
    MatrixCell {
        column: String,
        truth: String,
        answer: String,
    },
}

impl RowFilter {
    /// The filter's button text.
    ///
    /// English, like the reserved column values it filters on: the interactive
    /// HTML export is a *document*, produced by writers that carry no `Strings`
    /// at all, and a shared exported file must read the same for everyone who
    /// receives it. The in-app views label their own controls.
    pub fn label(&self) -> String {
        match self {
            RowFilter::All => "All".to_string(),
            RowFilter::Failed => "Failed".to_string(),
            RowFilter::Differ => "Differences".to_string(),
            RowFilter::Correct => "Correct".to_string(),
            RowFilter::Incorrect => "Incorrect".to_string(),
            RowFilter::Regressed => "Regressions".to_string(),
            RowFilter::Fixed => "Improvements".to_string(),
            RowFilter::Waiting => "Waiting".to_string(),
            RowFilter::MatrixCell { truth, answer, .. } => format!("{truth} → {answer}"),
        }
    }

    /// The filters worth offering for `result`, in the order they should be
    /// shown.
    ///
    /// A filter is offered when the report *can* answer it — it carries the
    /// reserved column the filter reads (`Result`, `Correct`, `Trend`) — not
    /// only once a row does: buttons that turned up one by one as a run went
    /// on shuffled the bar under the pointer, and an empty Regressions button
    /// on a scored comparison says something true ("none"). A filter that
    /// could only ever select nothing is still left out: no Regressions button
    /// on a report with no ground truth, inviting the reader to conclude there
    /// are none. A row that answers a filter without the column (a hand-built
    /// result) still brings its button.
    pub fn available(result: &ReportResult) -> Vec<RowFilter> {
        let has = |c: &str| result.column_order.iter().any(|k| k == c);
        let mut out = vec![RowFilter::All];
        // Failed reads no column — nothing declares that a report might fail —
        // so it is offered by the rows themselves, as soon as one has failed.
        // It comes first because it is the one a reader reaches for when a run
        // has gone wrong, and that is a bad moment to go hunting.
        if (0..result.rows.len()).any(|r| row_failed(result, r)) {
            out.push(RowFilter::Failed);
        }
        let facts: Vec<RowFacts> = (0..result.rows.len())
            .map(|r| RowFacts::of(result, r))
            .collect();
        if has(super::compare::RESULT_COLUMN) || facts.iter().any(|f| f.differs) {
            out.push(RowFilter::Differ);
        }
        let scored = has(super::compare::CORRECT_COLUMN);
        if scored || facts.iter().any(|f| f.verdict == Some(Verdict::Correct)) {
            out.push(RowFilter::Correct);
        }
        if scored || facts.iter().any(|f| f.verdict == Some(Verdict::Incorrect)) {
            out.push(RowFilter::Incorrect);
        }
        let trended = has(super::compare::TREND_COLUMN);
        if trended || facts.iter().any(|f| f.trend == Some(Trend::Regressed)) {
            out.push(RowFilter::Regressed);
        }
        if trended || facts.iter().any(|f| f.trend == Some(Trend::Fixed)) {
            out.push(RowFilter::Fixed);
        }
        out
    }

    /// Whether row `r` passes this filter.
    pub fn matches(
        &self,
        result: &ReportResult,
        columns: &[OutputColumn],
        labels: &LabelMap,
        r: usize,
    ) -> bool {
        match self {
            RowFilter::All => true,
            RowFilter::Failed => row_failed(result, r),
            RowFilter::Differ => RowFacts::of(result, r).differs,
            RowFilter::Correct => RowFacts::of(result, r).verdict == Some(Verdict::Correct),
            RowFilter::Incorrect => RowFacts::of(result, r).verdict == Some(Verdict::Incorrect),
            RowFilter::Regressed => RowFacts::of(result, r).trend == Some(Trend::Regressed),
            RowFilter::Fixed => RowFacts::of(result, r).trend == Some(Trend::Fixed),
            RowFilter::Waiting => false,
            RowFilter::MatrixCell {
                column,
                truth,
                answer,
            } => {
                let key = (r, column.clone());
                // Only a scored cell can be in the matrix, and the comparison
                // is by *class*, exactly as the matrix counted it — otherwise
                // clicking a cell counted through `# labels:` would select rows
                // by raw text and come back with fewer rows than the count.
                let Some(expected) = result.truths.get(&key) else {
                    return false;
                };
                let Some(row) = result.rows.get(r) else {
                    return false;
                };
                let Some(col) = columns.iter().find(|c| &c.header == column) else {
                    return false;
                };
                labels.label_of(expected) == *truth
                    && labels.label_of(&col.value(row, &result.no_match_marker)) == *answer
            }
        }
    }
}

/// Whether anything went wrong on row `r` — its own errors, or those of the
/// baseline it was measured against.
///
/// The baseline counts because a collapsed comparison row *is* the pair: a
/// candidate that came back fine beside a baseline that didn't is a row whose
/// `Result` is wrong, and re-running it runs both sides again anyway.
pub fn row_failed(result: &ReportResult, r: usize) -> bool {
    result.rows.get(r).is_some_and(|row| !row.errors.is_empty())
        || result
            .baseline_errors
            .get(&r)
            .is_some_and(|e| !e.is_empty())
}

/// Every filter a report offers, and how many of them are toolbar buttons.
///
/// The buttons come first (see [`RowFilter::available`]), then one
/// [`RowFilter::MatrixCell`] per non-empty confusion-matrix cell — "which seven
/// rows are those?" is the first question anyone asks of an off-diagonal count,
/// so every count is a way into the rows it counted.
///
/// One list rather than two because the HTML export indexes into it: rows carry
/// the filter indices they pass, so the browser only ever compares numbers.
/// Built here rather than in either renderer so the in-app views and the export
/// cannot end up offering different sets.
pub fn all_filters(result: &ReportResult, metrics: Option<&Metrics>) -> (Vec<RowFilter>, usize) {
    let mut filters = RowFilter::available(result);
    let buttons = filters.len();
    if let Some(metrics) = metrics {
        for m in &metrics.columns {
            let Some(matrix) = &m.matrix else { continue };
            for (t, truth) in matrix.axis.iter().enumerate() {
                for (p, answer) in matrix.axis.iter().enumerate() {
                    if matrix.counts[t][p] > 0 {
                        filters.push(RowFilter::MatrixCell {
                            column: m.header.clone(),
                            truth: truth.clone(),
                            answer: answer.clone(),
                        });
                    }
                }
            }
        }
    }
    (filters, buttons)
}

/// The rows to show: those passing `filter` and containing `text` (a
/// case-insensitive substring of any shown column's value; empty matches
/// everything).
///
/// The text search runs over the columns the caller is *showing*, so it finds
/// what the reader can see. Pending rows are excluded — a live run must not
/// offer a filtered view that is mostly blank skeleton.
// Consumed by the in-app interactive views, which unlike the HTML export
// filter in Rust rather than in the browser; kept beside the predicates it
// applies so the two renderings can never disagree about what they hide. Only
// the GUI results view calls it so far, so the non-GUI build still needs the
// allow; drop it once the TUI results view filters too.
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
pub fn visible_rows(
    result: &ReportResult,
    columns: &[OutputColumn],
    labels: &LabelMap,
    filter: &RowFilter,
    text: &str,
) -> Vec<usize> {
    let needle = text.trim().to_lowercase();
    (0..result.rows.len())
        .filter(|r| !result.pending.contains(r))
        .filter(|&r| filter.matches(result, columns, labels, r))
        .filter(|&r| {
            needle.is_empty()
                || result.rows.get(r).is_some_and(|row| {
                    columns.iter().any(|c| {
                        c.value(row, &result.no_match_marker)
                            .to_lowercase()
                            .contains(&needle)
                    })
                })
        })
        .collect()
}

/// The rows of a *live* grid that `filter` and `text` select.
///
/// A running report's grid holds its raw rows — a comparison's baseline and
/// candidate each on a row of their own — while the filters describe the
/// report those rows will become (see [`crate::report::run::project_live`]).
/// So the filter is judged on `projected`, and each row it picks is shown as
/// the raw rows it was made from: the candidate and the baseline it was
/// measured against, side by side. `All` with no text shows every row,
/// pending ones included, exactly as an unfiltered live grid does.
/// The rows of a *live* grid that have come back while the other side of
/// their comparison pair hasn't — what [`RowFilter::Waiting`] shows.
pub fn waiting_rows(live: &ReportResult) -> Vec<usize> {
    use crate::report::model::RowRole;
    let pending: std::collections::HashSet<(&[String], Option<&str>)> = live
        .pending
        .iter()
        .filter_map(|&r| live.rows.get(r))
        .filter(|row| matches!(row.role, RowRole::Baseline | RowRole::Candidate))
        .map(|row| (row.key.as_slice(), row.comparison.as_deref()))
        .collect();
    (0..live.rows.len())
        .filter(|r| !live.pending.contains(r))
        .filter(|&r| {
            let row = &live.rows[r];
            matches!(row.role, RowRole::Baseline | RowRole::Candidate)
                && pending.contains(&(row.key.as_slice(), row.comparison.as_deref()))
        })
        .collect()
}

#[cfg_attr(not(feature = "gui"), allow(dead_code))]
pub fn live_visible_rows(
    live: &ReportResult,
    live_columns: &[OutputColumn],
    projected: &ReportResult,
    projected_columns: &[OutputColumn],
    labels: &LabelMap,
    filter: &RowFilter,
    text: &str,
) -> Vec<usize> {
    if *filter == RowFilter::All && text.trim().is_empty() {
        return (0..live.rows.len()).collect();
    }
    let by_text: std::collections::HashSet<usize> =
        visible_rows(live, live_columns, labels, &RowFilter::All, text)
            .into_iter()
            .collect();
    if *filter == RowFilter::All {
        return (0..live.rows.len())
            .filter(|r| by_text.contains(r))
            .collect();
    }
    if *filter == RowFilter::Waiting {
        return waiting_rows(live)
            .into_iter()
            .filter(|r| by_text.contains(r))
            .collect();
    }
    let picked = visible_rows(projected, projected_columns, labels, filter, "");
    let paths: std::collections::HashSet<&[(usize, usize)]> = picked
        .iter()
        .map(|&r| projected.rows[r].path.as_slice())
        .collect();
    let sides: std::collections::HashSet<(&[String], Option<&str>)> = picked
        .iter()
        .map(|&r| &projected.rows[r])
        .filter(|row| row.role == crate::report::model::RowRole::Candidate)
        .map(|row| (row.key.as_slice(), row.comparison.as_deref()))
        .collect();
    (0..live.rows.len())
        .filter(|r| by_text.contains(r))
        .filter(|&r| {
            let row = &live.rows[r];
            paths.contains(row.path.as_slice())
                || (row.role == crate::report::model::RowRole::Baseline
                    && sides.contains(&(row.key.as_slice(), row.comparison.as_deref())))
        })
        .collect()
}

/// Reorder `rows` (indices into `result.rows`) by `column`'s rendered value.
///
/// Numbers compare as numbers, so `9` sorts before `10`, and text compares
/// case-insensitively with any runs of digits inside it taken as numbers, so
/// `img2.jpg` sorts before `img10.jpg`. In a mixed column the numbers come
/// first. Empty cells and the no-match marker always sink to the bottom, in
/// either direction: a missing value is not the smallest or the largest one,
/// and bunching them at the top would hide the answers being sorted for. The
/// sort is stable, so ties keep the run's order.
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
pub fn sort_rows(
    result: &ReportResult,
    column: &OutputColumn,
    descending: bool,
    rows: &mut [usize],
) {
    use std::cmp::Ordering;
    enum Key {
        Number(f64),
        Text(String),
        Blank,
    }
    let key = |r: usize| -> Key {
        let Some(row) = result.rows.get(r) else {
            return Key::Blank;
        };
        // A row still waiting on its requests sorts by what it shows: the
        // values settled before the run (its file, its environment), and
        // otherwise nothing — not the dry pass's made-up `0`s, which would
        // float every unfinished row to the top of an ascending sort.
        let v = if result.pending.contains(&r) {
            column.pending_value(row, "", &result.planned_columns)
        } else {
            column.value(row, &result.no_match_marker)
        };
        if v.trim().is_empty() || v == result.no_match_marker {
            Key::Blank
        } else if let Some(n) = crate::report::writer::parse_report_number(&v) {
            Key::Number(n)
        } else {
            Key::Text(v.to_lowercase())
        }
    };
    let mut keyed: Vec<(Key, usize)> = rows.iter().map(|&r| (key(r), r)).collect();
    keyed.sort_by(|(a, _), (b, _)| {
        let ord = match (a, b) {
            (Key::Blank, Key::Blank) => return Ordering::Equal,
            (Key::Blank, _) => return Ordering::Greater,
            (_, Key::Blank) => return Ordering::Less,
            (Key::Number(x), Key::Number(y)) => x.total_cmp(y),
            (Key::Number(_), Key::Text(_)) => Ordering::Less,
            (Key::Text(_), Key::Number(_)) => Ordering::Greater,
            (Key::Text(x), Key::Text(y)) => natural_cmp(x, y),
        };
        if descending { ord.reverse() } else { ord }
    });
    for (slot, (_, r)) in rows.iter_mut().zip(keyed) {
        *slot = r;
    }
}

/// Compare two strings with each run of ASCII digits taken as a number.
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (mut a, mut b) = (a, b);
    loop {
        match (a.chars().next(), b.chars().next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let da = a.len() - a.trim_start_matches(|c: char| c.is_ascii_digit()).len();
                let db = b.len() - b.trim_start_matches(|c: char| c.is_ascii_digit()).len();
                let (na, nb) = (&a[..da], &b[..db]);
                let (ta, tb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
                let ord = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if ord != Ordering::Equal {
                    return ord;
                }
                a = &a[da..];
                b = &b[db..];
            }
            (Some(x), Some(y)) => {
                if x != y {
                    return x.cmp(&y);
                }
                a = &a[x.len_utf8()..];
                b = &b[y.len_utf8()..];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sort_rows_orders_numbers_numerically_and_sinks_blanks() {
        let result = ReportResult {
            rows: vec![
                row(&[("Time", "100")]),
                row(&[("Time", "")]),
                row(&[("Time", "9")]),
                row(&[("Time", "timeout")]),
                row(&[("Time", "1000")]),
            ],
            ..Default::default()
        };
        let mut rows: Vec<usize> = (0..5).collect();
        sort_rows(&result, &col("Time"), false, &mut rows);
        assert_eq!(rows, vec![2, 0, 4, 3, 1]);
        sort_rows(&result, &col("Time"), true, &mut rows);
        assert_eq!(rows, vec![3, 4, 0, 2, 1], "blank still last");
    }

    /// Mid-run, a row still on its way sorts by what it shows — nothing, here
    /// — rather than by the dry pass's placeholder `0`.
    #[test]
    fn sort_rows_sinks_rows_still_on_their_way() {
        let mut result = ReportResult {
            rows: vec![
                row(&[("Time", "0")]),
                row(&[("Time", "250")]),
                row(&[("Time", "120")]),
            ],
            ..Default::default()
        };
        result.pending.insert(0);
        let mut rows: Vec<usize> = (0..3).collect();
        sort_rows(&result, &col("Time"), false, &mut rows);
        assert_eq!(rows, vec![2, 1, 0]);
    }

    #[test]
    fn sort_rows_orders_paths_naturally_and_keeps_ties_stable() {
        let result = ReportResult {
            rows: vec![
                row(&[("FILE", "img/img10.jpg")]),
                row(&[("FILE", "IMG/img2.jpg")]),
                row(&[("FILE", "img/a.jpg")]),
                row(&[("FILE", "img/img2.jpg")]),
            ],
            ..Default::default()
        };
        let mut rows: Vec<usize> = (0..4).collect();
        sort_rows(&result, &col("FILE"), false, &mut rows);
        assert_eq!(rows, vec![2, 1, 3, 0]);
    }
    use crate::report::compare::{CORRECT_COLUMN, MATCH, RESULT_COLUMN, TREND_COLUMN};
    use crate::report::model::ReportRow;

    fn row(cells: &[(&str, &str)]) -> ReportRow {
        ReportRow {
            cells: cells
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ..Default::default()
        }
    }

    fn col(header: &str) -> OutputColumn {
        OutputColumn {
            header: header.to_string(),
            sources: vec![header.to_string()],
            stats: Vec::new(),
            image: None,
            truth: None,
            detail: false,
        }
    }

    /// A four-row fixture covering every class: matched-and-right,
    /// changed-and-fixed, changed-and-regressed, changed-and-still-wrong.
    fn fixture() -> ReportResult {
        ReportResult {
            column_order: vec![
                RESULT_COLUMN.into(),
                CORRECT_COLUMN.into(),
                TREND_COLUMN.into(),
                "Name".into(),
                "Verdict".into(),
            ],
            rows: vec![
                row(&[
                    (RESULT_COLUMN, MATCH),
                    (CORRECT_COLUMN, "correct"),
                    (TREND_COLUMN, "unchanged"),
                    ("Name", "alpha"),
                    ("Verdict", "Low Risk"),
                ]),
                row(&[
                    (RESULT_COLUMN, "Verdict: a≠b"),
                    (CORRECT_COLUMN, "correct"),
                    (TREND_COLUMN, "fixed"),
                    ("Name", "beta"),
                    ("Verdict", "Low Risk"),
                ]),
                row(&[
                    (RESULT_COLUMN, "Verdict: a≠b"),
                    (CORRECT_COLUMN, "incorrect"),
                    (TREND_COLUMN, "regressed"),
                    ("Name", "gamma"),
                    ("Verdict", "High Risk"),
                ]),
                row(&[
                    (RESULT_COLUMN, "Verdict: a≠b"),
                    (CORRECT_COLUMN, "incorrect"),
                    // A still-wrong row reads `unchanged` like a still-right
                    // one; what tells them apart is the scored cell below.
                    (TREND_COLUMN, "unchanged"),
                    ("Name", "delta"),
                    ("Verdict", "High Risk"),
                ]),
            ],
            trends: [
                ((0, "Verdict".to_string()), Trend::Unchanged),
                ((1, "Verdict".to_string()), Trend::Fixed),
                ((2, "Verdict".to_string()), Trend::Regressed),
                ((3, "Verdict".to_string()), Trend::StillWrong),
            ]
            .into_iter()
            .collect(),
            ..Default::default()
        }
    }

    /// The two rows that didn't move share a word but not a class: a filter
    /// reading the column's text alone would sweep the still-failing row in
    /// with the passing ones.
    #[test]
    fn a_still_wrong_row_is_told_apart_from_a_still_right_one() {
        let res = fixture();
        assert_eq!(RowFacts::of(&res, 0).trend, Some(Trend::Unchanged));
        assert_eq!(RowFacts::of(&res, 3).trend, Some(Trend::StillWrong));
        assert_eq!(
            res.rows[0].cells.get(TREND_COLUMN),
            res.rows[3].cells.get(TREND_COLUMN),
            "even though the column says the same thing about both"
        );
    }

    fn columns() -> Vec<OutputColumn> {
        vec![col("Name"), col("Verdict")]
    }

    #[test]
    fn each_filter_selects_exactly_its_class() {
        let res = fixture();
        let cols = columns();
        let labels = LabelMap::parse(&[]);
        let pick = |f: RowFilter| visible_rows(&res, &cols, &labels, &f, "");

        assert_eq!(pick(RowFilter::All), vec![0, 1, 2, 3]);
        assert_eq!(
            pick(RowFilter::Differ),
            vec![1, 2, 3],
            "the matched row is not a difference"
        );
        assert_eq!(pick(RowFilter::Correct), vec![0, 1], "the two right rows");
        assert_eq!(
            pick(RowFilter::Incorrect),
            vec![2, 3],
            "both wrong rows, whether or not the wrongness is new"
        );
        assert_eq!(
            pick(RowFilter::Regressed),
            vec![2],
            "`still wrong` is failing, but it is not a regression"
        );
        assert_eq!(
            pick(RowFilter::Fixed),
            vec![1],
            "and the row that came right is the improvement, on its own"
        );
    }

    /// An unlabelled row is unchecked, not wrong: merging the two is how a
    /// half-tagged corpus comes to look like a broken engine.
    #[test]
    fn an_untested_row_is_not_incorrect() {
        let mut res = fixture();
        res.rows.push(row(&[
            (RESULT_COLUMN, MATCH),
            (CORRECT_COLUMN, "untested"),
            ("Name", "epsilon"),
        ]));
        let cols = columns();
        let labels = LabelMap::parse(&[]);
        assert_eq!(
            visible_rows(&res, &cols, &labels, &RowFilter::Incorrect, ""),
            vec![2, 3]
        );
    }

    #[test]
    fn the_text_filter_searches_the_shown_columns_and_combines_with_the_class() {
        let res = fixture();
        let cols = columns();
        let labels = LabelMap::parse(&[]);
        assert_eq!(
            visible_rows(&res, &cols, &labels, &RowFilter::All, "GAM"),
            vec![2],
            "case-insensitive substring of a shown value"
        );
        assert_eq!(
            visible_rows(&res, &cols, &labels, &RowFilter::Incorrect, "delta"),
            vec![3],
            "the text and the class narrow together"
        );
        assert!(
            visible_rows(&res, &cols, &labels, &RowFilter::All, "unchanged").is_empty(),
            "a value only in a column the caller isn't showing is not searched"
        );
    }

    /// Clicking a matrix cell has to come back with exactly the rows it counted
    /// — including through the label classes, or a cell counted as `Fail` would
    /// select nothing.
    #[test]
    fn a_matrix_cell_selects_the_rows_it_counted_through_the_label_classes() {
        let mut res = fixture();
        let labels = LabelMap::parse(&[
            "Pass = pass, real, low risk",
            "Fail = fail, fake, high risk",
        ]);
        for (r, truth) in [(0, "real"), (1, "real"), (2, "real"), (3, "fake")] {
            res.truths.insert((r, "Verdict".to_string()), truth.into());
        }
        let cols = columns();
        let cell = |truth: &str, answer: &str| {
            visible_rows(
                &res,
                &cols,
                &labels,
                &RowFilter::MatrixCell {
                    column: "Verdict".into(),
                    truth: truth.into(),
                    answer: answer.into(),
                },
                "",
            )
        };
        assert_eq!(
            cell("Pass", "Pass"),
            vec![0, 1],
            "`real` and `Low Risk` are both the Pass class"
        );
        assert_eq!(cell("Pass", "Fail"), vec![2], "the off-diagonal cell");
        assert_eq!(cell("Fail", "Fail"), vec![3]);
        assert!(cell("Fail", "Pass").is_empty());
    }

    /// A filter that could only ever select nothing is not offered: an
    /// always-empty "Regressions" button reads as "there are none" — and the
    /// same is true of "Improvements", which is why the pair is offered
    /// independently rather than together.
    #[test]
    fn only_the_filters_a_report_can_answer_are_offered() {
        assert_eq!(
            RowFilter::available(&fixture()),
            vec![
                RowFilter::All,
                RowFilter::Differ,
                RowFilter::Correct,
                RowFilter::Incorrect,
                RowFilter::Regressed,
                RowFilter::Fixed
            ]
        );
        let plain = ReportResult {
            rows: vec![row(&[("Name", "a")])],
            ..Default::default()
        };
        assert_eq!(RowFilter::available(&plain), vec![RowFilter::All]);
    }

    /// A row that failed is one whose own request errored *or* whose baseline
    /// side did: in a comparison the baseline's errors are held apart from the
    /// row, and a reader looking for "what went wrong" means both.
    #[test]
    fn failed_selects_rows_that_errored_on_either_side() {
        let mut res = fixture();
        res.rows[1].errors.push("connection reset".into());
        res.baseline_errors.insert(3, vec!["timeout".into()]);
        assert!(!row_failed(&res, 0));
        assert!(row_failed(&res, 1));
        assert!(!row_failed(&res, 2));
        assert!(row_failed(&res, 3), "the baseline side failed");
        let cols = vec![col("Name")];
        let labels = LabelMap::default();
        assert_eq!(
            visible_rows(&res, &cols, &labels, &RowFilter::Failed, ""),
            vec![1, 3]
        );
    }

    /// Nothing declares that a report *might* fail, so the Failed filter is
    /// offered by the rows themselves — and first, because it is what a reader
    /// reaches for when a run has gone wrong.
    #[test]
    fn failed_is_offered_once_a_row_has_failed_and_comes_first() {
        let mut res = fixture();
        assert!(!RowFilter::available(&res).contains(&RowFilter::Failed));
        res.rows[2].errors.push("connection reset".into());
        assert_eq!(
            RowFilter::available(&res).first(),
            Some(&RowFilter::All),
            "All stays the first of them"
        );
        assert_eq!(RowFilter::available(&res).get(1), Some(&RowFilter::Failed));
    }

    /// A scored comparison offers both halves of "what moved?" whatever moved:
    /// a run that only fixed things still has a Regressions button, reading
    /// "none", rather than one that turns up the first time something breaks.
    #[test]
    fn a_run_that_only_fixed_things_is_offered_both_movement_filters() {
        let mut res = fixture();
        res.rows.remove(2);
        res.trends = [
            ((0, "Verdict".to_string()), Trend::Unchanged),
            ((1, "Verdict".to_string()), Trend::Fixed),
            ((2, "Verdict".to_string()), Trend::StillWrong),
        ]
        .into_iter()
        .collect();
        let offered = RowFilter::available(&res);
        assert!(
            offered.contains(&RowFilter::Fixed),
            "something came right, so the filter that shows it is there: {offered:?}"
        );
        assert!(
            offered.contains(&RowFilter::Regressed),
            "nothing broke, and the button says so by showing nothing: {offered:?}"
        );
    }

    /// A live run must not offer a filtered view that is mostly empty skeleton.
    #[test]
    fn rows_that_have_not_run_yet_are_never_shown() {
        let mut res = fixture();
        res.pending.insert(2);
        let cols = columns();
        let labels = LabelMap::parse(&[]);
        assert_eq!(
            visible_rows(&res, &cols, &labels, &RowFilter::All, ""),
            vec![0, 1, 3]
        );
    }
}
