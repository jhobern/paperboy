//! Ground-truth metrics — how well a run scored, and where it went wrong.
//!
//! Every renderer reads its figures from here: the footer rows CSV gets, the
//! metric cards and heatmap HTML draws, the `Metrics` worksheet in a workbook,
//! the `metrics` object in JSON, and the live grid in both front-ends. That is
//! the whole point of the module — the moment two renderers derive "accuracy"
//! separately they will eventually disagree, and a report that quotes two
//! different accuracies is worse than one that quotes none.
//!
//! Nothing here is computed unless the report declares a `TRUTH` column, so a
//! report without ground truth is byte-identical in every format.

use std::collections::HashMap;

use super::labels::LabelMap;
use super::model::{OutputColumn, ReportResult, StatValue, SummaryRow, Verdict};

/// The footer/card labels. Like the reserved `Result` and `Correct` columns
/// these are report *data* — a figure someone will paste into a ticket or sort
/// a spreadsheet by — so they are English in every language, and the writers
/// (which carry no `Strings` at all) can emit them unaided.
pub const COMPARED_LABEL: &str = "Compared";
pub const INCORRECT_LABEL: &str = "Incorrect";
pub const ACCURACY_LABEL: &str = "Accuracy";
pub const FIXED_LABEL: &str = "Fixed";
pub const REGRESSED_LABEL: &str = "Regressed";
pub const STILL_WRONG_LABEL: &str = "Still wrong";
pub const UNCHANGED_LABEL: &str = "Unchanged";
pub const MOVEMENT_LABEL: &str = "Movement";

/// How one ground-truthed column scored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnMetrics {
    /// The output column these figures are for.
    pub header: String,
    /// Rows in the table, scored or not.
    pub total: usize,
    /// Rows that had a ground truth to score against — the denominator of
    /// [`Self::accuracy`]. Deliberately *not* `total`: unlabelled rows are not
    /// evidence either way, and dividing by them would let anyone raise the
    /// accuracy of a run by adding rows nobody has checked.
    pub compared: usize,
    pub correct: usize,
    pub incorrect: usize,
    /// Truth × prediction counts, present only when `# labels:` declares the
    /// axis (without a declared vocabulary there is no meaningful order, and a
    /// matrix whose axes are whatever turned up is a table of noise).
    pub matrix: Option<ConfusionMatrix>,
}

impl ColumnMetrics {
    /// Correct as a fraction of compared, or `None` when nothing was scored —
    /// which reads as "not measured" rather than as 0%, a number that would
    /// look like a catastrophic failure.
    pub fn accuracy(&self) -> Option<f64> {
        (self.compared > 0).then(|| self.correct as f64 / self.compared as f64)
    }

    /// The accuracy as it is shown: one decimal place, e.g. `95.9%`.
    pub fn accuracy_text(&self) -> Option<String> {
        self.accuracy().map(|a| format!("{:.1}%", a * 100.0))
    }
}

/// Truth (down) against prediction (across).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfusionMatrix {
    /// The labels, in `# labels:` declaration order, followed by any value that
    /// turned up but was never declared (in first-seen order).
    pub axis: Vec<String>,
    /// `counts[truth][predicted]`, indexed into [`Self::axis`].
    pub counts: Vec<Vec<usize>>,
}

impl ConfusionMatrix {
    /// The largest count in the matrix — the denominator a heatmap shades
    /// against. Zero for an empty matrix.
    pub fn max(&self) -> usize {
        self.counts
            .iter()
            .flat_map(|r| r.iter().copied())
            .max()
            .unwrap_or(0)
    }

    /// Whether every scored row landed on the diagonal, so a renderer can say
    /// "everything matched" instead of leaving the reader to check a grid of
    /// zeroes.
    pub fn is_diagonal(&self) -> bool {
        self.counts
            .iter()
            .enumerate()
            .all(|(t, row)| row.iter().enumerate().all(|(p, &n)| t == p || n == 0))
    }

    /// The total number of scored rows the matrix accounts for.
    pub fn total(&self) -> usize {
        self.counts.iter().flat_map(|r| r.iter()).sum()
    }
}

/// The heatmap shade for a cell holding `n` of a maximum `max`: its `(r, g, b)`
/// and whether the text on it has to flip to white to stay readable.
///
/// A blue ramp, deliberately: colour-blind safe, and — more importantly — it
/// does not pre-judge which cells are the bad ones. The diagonal is not "good"
/// in every matrix; for a rare-event detector the interesting cells are off it,
/// and a green-to-red scheme would mislead on exactly those reports.
///
/// Shared by every renderer rather than being the HTML writer's private detail,
/// so the same matrix cannot come out shaded two different ways depending on
/// where it is being read.
pub fn heat_rgb(n: usize, max: usize) -> ([u8; 3], bool) {
    if n == 0 || max == 0 {
        return ([0xff, 0xff, 0xff], false);
    }
    let f = n as f64 / max as f64;
    // Interpolate from a very pale blue to a deep one.
    let lerp = |from: f64, to: f64| (from + (to - from) * f).round() as u8;
    (
        [lerp(234.0, 8.0), lerp(242.0, 48.0), lerp(252.0, 107.0)],
        f > 0.55,
    )
}

/// How a run moved against the baseline it was compared with: how many rows got
/// better, how many got worse, and how many were wrong on both sides.
///
/// The accuracy figures can't answer this. Two runs that score 98% are not the
/// same run if one of them fixed three rows and broke three others, and "did
/// anything move?" is the first thing anyone asks of a comparison — usually
/// before they have looked at a single row.
///
/// Only counts rows that were scored on *both* sides: without a truth there is
/// no direction to report (see [`super::model::Trend::of`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Movement {
    /// Wrong before, right now.
    pub fixed: usize,
    /// Right before, wrong now — the reason this summary exists.
    pub regressed: usize,
    /// Wrong on both sides: not new, but not fixed either.
    pub still_wrong: usize,
    /// Right on both sides. The expected case, counted so the others can be
    /// read as a proportion of the run rather than as bare numbers.
    pub unchanged: usize,
}

impl Movement {
    /// Whether anything moved at all. A run where every row landed where it did
    /// last time can say so in one line instead of four zeroes.
    pub fn is_still(&self) -> bool {
        self.fixed == 0 && self.regressed == 0
    }
}

/// The metrics of a whole run: one entry per ground-truthed column, plus the
/// row roll-up carried by the reserved `Correct` column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Metrics {
    pub columns: Vec<ColumnMetrics>,
    /// The per-row roll-up (the `Correct` column), present when the table shows
    /// that column. With a single truth-bearing column it repeats that column's
    /// figures, which is why it is separate rather than folded in: with several
    /// it is the only honest per-row count.
    pub overall: Option<ColumnMetrics>,
    /// How the run moved against its baseline, or `None` when there was nothing
    /// to compare it with (no baseline, or no row scored on both sides).
    pub movement: Option<Movement>,
}

impl Metrics {
    /// Compute the metrics of `result` over the columns it will actually show,
    /// or `None` when the report has no ground truth at all.
    ///
    /// `columns` is the *resolved* column set, so a `# columns:` directive that
    /// leaves a truth-bearing column out also leaves it out of the metrics: the
    /// figures describe the report in front of the reader, not a different one.
    pub fn compute(
        result: &ReportResult,
        columns: &[OutputColumn],
        labels: &LabelMap,
    ) -> Option<Metrics> {
        if result.verdicts.is_empty() {
            return None;
        }
        // A row still waiting on its request is not part of the table yet: a
        // live "compared 3 of 500" would spend the whole run reading as a
        // catastrophe in progress.
        let total = result.rows.len() - result.pending.len();
        let mut out = Vec::new();
        for col in columns.iter().filter(|c| c.truth.is_some()) {
            let mut m = ColumnMetrics {
                header: col.header.clone(),
                total,
                compared: 0,
                correct: 0,
                incorrect: 0,
                matrix: None,
            };
            // The axis starts as the declared vocabulary and grows with
            // anything unexpected, so a value nobody declared is visible as its
            // own row/column rather than silently dropped from the counts.
            let mut axis: Vec<String> = labels.classes().to_vec();
            let mut index: HashMap<String, usize> = axis
                .iter()
                .enumerate()
                .map(|(i, c)| (c.clone(), i))
                .collect();
            let mut pairs: Vec<(usize, usize)> = Vec::new();
            for (r, row) in result.rows.iter().enumerate() {
                if result.pending.contains(&r) {
                    continue;
                }
                let key = (r, col.header.clone());
                match result.verdicts.get(&key) {
                    Some(Verdict::Correct) => m.correct += 1,
                    Some(Verdict::Incorrect) => m.incorrect += 1,
                    _ => continue,
                }
                m.compared += 1;
                let Some(truth) = result.truths.get(&key) else {
                    continue;
                };
                let predicted = col.value(row, &result.no_match_marker);
                let mut slot = |value: &str, axis: &mut Vec<String>| -> usize {
                    let label = labels.label_of(value);
                    *index.entry(label.clone()).or_insert_with(|| {
                        axis.push(label);
                        axis.len() - 1
                    })
                };
                let t = slot(truth, &mut axis);
                let p = slot(&predicted, &mut axis);
                pairs.push((t, p));
            }
            if !labels.classes().is_empty() {
                let n = axis.len();
                let mut counts = vec![vec![0usize; n]; n];
                for (t, p) in pairs {
                    counts[t][p] += 1;
                }
                m.matrix = Some(ConfusionMatrix { axis, counts });
            }
            out.push(m);
        }
        if out.is_empty() {
            return None;
        }
        let overall = columns
            .iter()
            .any(|c| c.header == super::compare::CORRECT_COLUMN)
            .then(|| row_rollup(result, total));
        Some(Metrics {
            columns: out,
            overall,
            movement: movement(result),
        })
    }

    /// The metric rows appended to the table's footer, one cell per column.
    ///
    /// This is the flat-format rendering — CSV has one table and no room for a
    /// header block, and the live grids in both front-ends already draw summary
    /// rows. Richer formats draw the same numbers as cards above the table
    /// instead, and must not also append these.
    pub fn summary_rows(&self, columns: &[OutputColumn]) -> Vec<SummaryRow> {
        let text = |m: &ColumnMetrics, which: usize| -> Option<String> {
            match which {
                0 => Some(format!("{} of {}", m.compared, m.total)),
                1 => Some(m.incorrect.to_string()),
                _ => m.accuracy_text(),
            }
        };
        let labels = [COMPARED_LABEL, INCORRECT_LABEL, ACCURACY_LABEL];
        let mut out = Vec::new();
        for (which, label) in labels.iter().enumerate() {
            let mut cells: Vec<Option<StatValue>> = vec![None; columns.len()];
            let mut any = false;
            for (ci, col) in columns.iter().enumerate() {
                let m = if col.header == super::compare::CORRECT_COLUMN {
                    self.overall.as_ref()
                } else {
                    self.columns.iter().find(|m| m.header == col.header)
                };
                if let Some(m) = m
                    && let Some(t) = text(m, which)
                {
                    any = true;
                    cells[ci] = Some(StatValue {
                        text: t,
                        // Not a `STATISTICS(…)` value: there is no formula a
                        // spreadsheet could recompute it with, and pretending
                        // otherwise would have xlsx write one that recomputed
                        // the wrong thing.
                        stat: None,
                        numeric: false,
                        match_value: None,
                    });
                }
            }
            if any {
                // A `SummaryRow` shows its label in the first column only when
                // that column has no value of its own — and here it often does,
                // because the roll-up lands in `Correct`, which is usually
                // first. Rather than let the row go out unlabelled ("3 of 4"
                // with nothing saying what of what), the label moves to the
                // first free column, or is prefixed onto the first cell when
                // every column is taken.
                if cells[0].is_some()
                    && let Some(free) = cells.iter().position(Option::is_none)
                {
                    cells[free] = Some(StatValue {
                        text: label.to_string(),
                        stat: None,
                        numeric: false,
                        match_value: None,
                    });
                } else if let Some(first) = cells[0].as_mut() {
                    first.text = format!("{label}: {}", first.text);
                }
                out.push(SummaryRow {
                    label: label.to_string(),
                    cells,
                });
            }
        }
        out
    }
}

/// How many rows moved which way, rolled up per row exactly as the `Trend`
/// column is — a row with one fixed column and one regressed column counts once,
/// as a regression, because that is what it is and what the column says.
///
/// `None` when no row has a trend at all: a run with no baseline hasn't "not
/// moved", it has nothing to have moved from, and a summary of four zeroes
/// would say the opposite.
fn movement(result: &ReportResult) -> Option<Movement> {
    use super::model::Trend;
    let mut m = Movement::default();
    let mut any = false;
    for r in 0..result.rows.len() {
        if result.pending.contains(&r) {
            continue;
        }
        let Some(t) = result.row_trend(r) else {
            continue;
        };
        any = true;
        match t {
            Trend::Fixed => m.fixed += 1,
            Trend::Regressed => m.regressed += 1,
            Trend::StillWrong => m.still_wrong += 1,
            Trend::Unchanged => m.unchanged += 1,
        }
    }
    any.then_some(m)
}

/// The per-row roll-up, read back off the reserved `Correct` column rather than
/// recomputed — the run has already decided what each row's verdict is, and two
/// answers to that question is exactly the failure this module exists to avoid.
fn row_rollup(result: &ReportResult, total: usize) -> ColumnMetrics {
    let mut m = ColumnMetrics {
        header: super::compare::CORRECT_COLUMN.to_string(),
        total,
        compared: 0,
        correct: 0,
        incorrect: 0,
        matrix: None,
    };
    for (r, row) in result.rows.iter().enumerate() {
        if result.pending.contains(&r) {
            continue;
        }
        match row
            .cells
            .get(super::compare::CORRECT_COLUMN)
            .map(String::as_str)
        {
            Some(v) if v == Verdict::Correct.as_str() => {
                m.correct += 1;
                m.compared += 1;
            }
            Some(v) if v == Verdict::Incorrect.as_str() => {
                m.incorrect += 1;
                m.compared += 1;
            }
            _ => {}
        }
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::flow::Header;
    use crate::report::model::ReportRow;

    fn row(cells: &[(&str, &str)]) -> ReportRow {
        ReportRow {
            cells: cells
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            vars: HashMap::new(),
            key: Vec::new(),
            path: Vec::new(),
            target: None,
        }
    }

    /// Four scored rows, one unlabelled: three correct of four compared.
    fn fixture() -> (ReportResult, Vec<OutputColumn>, LabelMap) {
        let mut res = ReportResult::default();
        res.rows = vec![
            row(&[("Verdict", "Low Risk"), ("Correct", "correct")]),
            row(&[("Verdict", "High Risk"), ("Correct", "correct")]),
            row(&[("Verdict", "Low Risk"), ("Correct", "incorrect")]),
            row(&[("Verdict", "Low Risk"), ("Correct", "correct")]),
            row(&[("Verdict", "Low Risk")]),
        ];
        res.column_order = vec!["Correct".into(), "Verdict".into()];
        res.column_truths
            .insert("Verdict".into(), "{{ e }}".to_string());
        for (r, (v, truth)) in [
            (Verdict::Correct, "real"),
            (Verdict::Correct, "fake"),
            (Verdict::Incorrect, "fake"),
            (Verdict::Correct, "real"),
        ]
        .into_iter()
        .enumerate()
        {
            res.verdicts.insert((r, "Verdict".into()), v);
            res.truths.insert((r, "Verdict".into()), truth.to_string());
        }
        res.verdicts
            .insert((4, "Verdict".into()), Verdict::Untested);
        let labels = LabelMap::parse(&[
            "Pass = pass, real, low risk",
            "Fail = fail, fake, high risk",
        ]);
        let cols = res.resolved_columns(&Header::default());
        (res, cols, labels)
    }

    /// Two runs can score the same and be entirely different runs. The
    /// movement summary is what tells them apart, and it is rolled up per row
    /// exactly as the `Trend` column is -- a row that fixed one column and
    /// broke another counts once, as a regression.
    #[test]
    fn movement_counts_the_rows_that_moved_and_which_way() {
        use crate::report::model::Trend;
        let (mut res, cols, labels) = fixture();
        res.trends.insert((0, "Verdict".into()), Trend::Unchanged);
        res.trends.insert((1, "Verdict".into()), Trend::Fixed);
        res.trends.insert((2, "Verdict".into()), Trend::StillWrong);
        res.trends.insert((3, "Verdict".into()), Trend::Regressed);
        let mv = Metrics::compute(&res, &cols, &labels)
            .expect("metrics")
            .movement
            .expect("a run with trends has moved somehow");
        assert_eq!(
            (mv.fixed, mv.regressed, mv.still_wrong, mv.unchanged),
            (1, 1, 1, 1)
        );
        assert!(!mv.is_still(), "one row got better and one got worse");

        // The same figures a reader would get from a run that touched nothing
        // -- and the one line that says so instead of four zeroes.
        let mut still = res.clone();
        still.trends.clear();
        for r in 0..4 {
            still.trends.insert((r, "Verdict".into()), Trend::Unchanged);
        }
        let mv = Metrics::compute(&still, &cols, &labels)
            .unwrap()
            .movement
            .unwrap();
        assert!(mv.is_still() && mv.unchanged == 4);

        // A run with no baseline hasn't "not moved" -- it has nothing to have
        // moved from, and four zeroes would say the opposite.
        let (res, cols, labels) = fixture();
        assert!(
            Metrics::compute(&res, &cols, &labels)
                .unwrap()
                .movement
                .is_none()
        );
    }

    #[test]
    fn accuracy_counts_only_the_rows_that_had_a_truth() {
        let (res, cols, labels) = fixture();
        let m = Metrics::compute(&res, &cols, &labels).expect("metrics");
        let v = &m.columns[0];
        assert_eq!(v.total, 5, "every row is in the table");
        assert_eq!(v.compared, 4, "the unlabelled row is not evidence");
        assert_eq!(v.correct, 3);
        assert_eq!(v.incorrect, 1);
        assert_eq!(v.accuracy_text().as_deref(), Some("75.0%"));
        let overall = m.overall.as_ref().expect("the Correct column rolls up");
        assert_eq!((overall.compared, overall.correct), (4, 3));
    }

    /// The matrix axis is the declared order, and each scored row lands on
    /// (its truth, what was predicted).
    #[test]
    fn the_confusion_matrix_takes_the_declared_axis_order() {
        let (res, cols, labels) = fixture();
        let m = Metrics::compute(&res, &cols, &labels).expect("metrics");
        let matrix = m.columns[0].matrix.as_ref().expect("a matrix");
        assert_eq!(matrix.axis, ["Pass".to_string(), "Fail".to_string()]);
        // Row 0: truth real (Pass), predicted Low Risk (Pass)  → [0][0]
        // Row 1: truth fake (Fail), predicted High Risk (Fail) → [1][1]
        // Row 2: truth fake (Fail), predicted Low Risk (Pass)  → [1][0]
        // Row 3: truth real (Pass), predicted Low Risk (Pass)  → [0][0]
        assert_eq!(matrix.counts, vec![vec![2, 0], vec![1, 1]]);
        assert_eq!(matrix.max(), 2);
        assert_eq!(matrix.total(), 4);
        assert!(!matrix.is_diagonal(), "one row was misclassified");
    }

    /// Without a declared vocabulary there is no meaningful axis order, so
    /// there is no matrix — but the accuracy figures still stand.
    #[test]
    fn no_declared_labels_means_no_matrix() {
        let (res, cols, _) = fixture();
        let m = Metrics::compute(&res, &cols, &LabelMap::default()).expect("metrics");
        assert!(m.columns[0].matrix.is_none());
        assert_eq!(m.columns[0].correct, 3);
    }

    /// An answer nobody declared has to be visible: silently dropping it would
    /// make the matrix disagree with the accuracy beside it.
    #[test]
    fn an_undeclared_answer_gets_its_own_axis_entry() {
        let (mut res, _, labels) = fixture();
        res.rows[0]
            .cells
            .insert("Verdict".into(), "Needs Review".into());
        let cols = res.resolved_columns(&Header::default());
        let m = Metrics::compute(&res, &cols, &labels).expect("metrics");
        let matrix = m.columns[0].matrix.as_ref().expect("a matrix");
        assert_eq!(
            matrix.axis,
            [
                "Pass".to_string(),
                "Fail".to_string(),
                "needs review".to_string()
            ]
        );
        assert_eq!(matrix.total(), 4, "no scored row is lost");
    }

    #[test]
    fn a_report_without_a_truth_has_no_metrics_at_all() {
        let mut res = ReportResult::default();
        res.rows = vec![row(&[("V", "a")])];
        res.column_order = vec!["V".into()];
        let cols = res.resolved_columns(&Header::default());
        assert!(Metrics::compute(&res, &cols, &LabelMap::default()).is_none());
    }

    /// A perfect run says so, rather than making the reader check a grid of
    /// zeroes.
    #[test]
    fn a_clean_diagonal_is_recognised() {
        let (mut res, cols, labels) = fixture();
        res.verdicts.insert((2, "Verdict".into()), Verdict::Correct);
        res.truths.insert((2, "Verdict".into()), "real".into());
        let m = Metrics::compute(&res, &cols, &labels).expect("metrics");
        assert!(m.columns[0].matrix.as_ref().unwrap().is_diagonal());
    }

    #[test]
    fn footer_rows_carry_the_figures_into_the_flat_formats() {
        let (res, cols, labels) = fixture();
        let m = Metrics::compute(&res, &cols, &labels).expect("metrics");
        let rows = m.summary_rows(&cols);
        assert_eq!(rows.len(), 3);
        let verdict_col = cols.iter().position(|c| c.header == "Verdict").unwrap();
        assert_eq!(rows[0].text_cell(verdict_col), "4 of 5");
        assert_eq!(rows[1].text_cell(verdict_col), "1");
        assert_eq!(rows[2].text_cell(verdict_col), "75.0%");
        assert!(
            rows[0].cells.iter().flatten().all(|c| c.stat.is_none()),
            "a metric is not a statistic a spreadsheet could recompute"
        );
    }
}
