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
            differs: cell(super::compare::RESULT_COLUMN)
                .is_some_and(|v| !v.trim().is_empty() && v != super::compare::MATCH),
            verdict: cell(super::compare::CORRECT_COLUMN).and_then(|v| match v {
                v if v == Verdict::Correct.as_str() => Some(Verdict::Correct),
                v if v == Verdict::Incorrect.as_str() => Some(Verdict::Incorrect),
                v if v == Verdict::Untested.as_str() => Some(Verdict::Untested),
                _ => None,
            }),
            trend: cell(super::compare::TREND_COLUMN).and_then(|v| match v {
                v if v == Trend::Unchanged.as_str() => Some(Trend::Unchanged),
                v if v == Trend::Fixed.as_str() => Some(Trend::Fixed),
                v if v == Trend::Regressed.as_str() => Some(Trend::Regressed),
                v if v == Trend::StillWrong.as_str() => Some(Trend::StillWrong),
                _ => None,
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
    /// Rows the comparison found a difference on.
    Differ,
    /// Rows whose ground-truth roll-up is `incorrect`. Untested rows are *not*
    /// included: "wrong" and "unchecked" are different problems, and merging
    /// them is how an unlabelled corpus comes to look like a broken engine.
    Incorrect,
    /// Rows that were right before and are wrong now — the ones a release
    /// decision turns on.
    Regressed,
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
            RowFilter::Differ => "Differences".to_string(),
            RowFilter::Incorrect => "Incorrect".to_string(),
            RowFilter::Regressed => "Regressions".to_string(),
            RowFilter::MatrixCell { truth, answer, .. } => format!("{truth} → {answer}"),
        }
    }

    /// The filters worth offering for `result`, in the order they should be
    /// shown. A filter that could only ever select nothing is left out: an
    /// always-empty "Regressions" button on a report with no ground truth
    /// invites the reader to conclude there are none.
    pub fn available(result: &ReportResult) -> Vec<RowFilter> {
        let mut out = vec![RowFilter::All];
        let facts: Vec<RowFacts> = (0..result.rows.len())
            .map(|r| RowFacts::of(result, r))
            .collect();
        if facts.iter().any(|f| f.differs) {
            out.push(RowFilter::Differ);
        }
        if facts.iter().any(|f| f.verdict == Some(Verdict::Incorrect)) {
            out.push(RowFilter::Incorrect);
        }
        if facts.iter().any(|f| f.trend == Some(Trend::Regressed)) {
            out.push(RowFilter::Regressed);
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
            RowFilter::Differ => RowFacts::of(result, r).differs,
            RowFilter::Incorrect => RowFacts::of(result, r).verdict == Some(Verdict::Incorrect),
            RowFilter::Regressed => RowFacts::of(result, r).trend == Some(Trend::Regressed),
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

#[cfg(test)]
mod tests {
    use super::*;
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
                    (TREND_COLUMN, "still wrong"),
                    ("Name", "delta"),
                    ("Verdict", "High Risk"),
                ]),
            ],
            ..Default::default()
        }
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
    /// always-empty "Regressions" button reads as "there are none".
    #[test]
    fn only_the_filters_a_report_can_answer_are_offered() {
        assert_eq!(
            RowFilter::available(&fixture()),
            vec![
                RowFilter::All,
                RowFilter::Differ,
                RowFilter::Incorrect,
                RowFilter::Regressed
            ]
        );
        let plain = ReportResult {
            rows: vec![row(&[("Name", "a")])],
            ..Default::default()
        };
        assert_eq!(RowFilter::available(&plain), vec![RowFilter::All]);
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
