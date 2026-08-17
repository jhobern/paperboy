//! What a row's drill-down holds: the things too big for a grid cell.
//!
//! The grid answers "what happened across the run"; the drill-down answers
//! "what happened in *this* row" — its pictures at full size, its `DETAIL`
//! columns in full, and, when the run compared against a baseline, a
//! field-by-field diff of whichever of them are JSON on both sides.
//!
//! Deciding *what* goes in the panel lives here rather than in a renderer,
//! because more than one renderer shows it: the interactive HTML export and the
//! in-app results views. The selection rules are fiddly (a `DETAIL` column with
//! an empty value is not worth a heading; a diff of identical JSON is not worth
//! a table; a row with none of the three should not offer an expander at all)
//! and two copies of them would drift.

use super::jsondiff::FieldDiff;
use super::model::{ImageData, OutputColumn, ReportResult, Verdict};

/// Split a run's columns into the ones the grid shows and the ones the
/// drill-down does.
///
/// A `DETAIL` column leaves the grid, *unless* every column is one: an empty
/// grid helps nobody, so the flag is ignored rather than obeyed into an
/// unreadable report. Shared so the in-app grid and the export agree about
/// which columns are where — a column that vanished from one and not the other
/// would look like a bug in whichever you noticed second.
pub fn split_columns(all: &[OutputColumn]) -> (Vec<OutputColumn>, Vec<&OutputColumn>) {
    let grid: Vec<OutputColumn> = if all.iter().all(|c| c.detail) {
        all.to_vec()
    } else {
        all.iter().filter(|c| !c.detail).cloned().collect()
    };
    (grid, all.iter().filter(|c| c.detail).collect())
}

/// One section of a row's drill-down panel, in the order the panel shows them.
#[derive(Debug, Clone, PartialEq)]
pub enum DetailSection<'a> {
    /// A picture, shown larger than its grid cell can manage.
    Image {
        header: &'a str,
        image: &'a ImageData,
        /// The cell's text — the path or URL the picture was resolved from,
        /// which is what a reader hovers to find out where it came from.
        value: String,
    },
    /// A `DETAIL` column's full value, pretty-printed when it is JSON.
    Text {
        header: &'a str,
        value: String,
        /// How this cell scored, when its column declares a `TRUTH`, and what
        /// it was scored against. The grid tints such a cell by whether it is
        /// *right*; the drill-down is where the same cell is actually read, so
        /// it has to say the same thing — a full value read out of a panel
        /// that stayed silent about it would be taken for a value nobody
        /// checked. [`Verdict::Untested`] is `None`: an unlabelled row must
        /// never borrow the appearance of one that passed.
        verdict: Option<(Verdict, String)>,
    },
    /// A `DETAIL` column's fields against the baseline's. Unchanged fields are
    /// kept: the reader can then find the field they care about whether or not
    /// it moved, and the *highlight* rather than the omission points at the
    /// difference.
    Diff {
        header: &'a str,
        fields: Vec<FieldDiff>,
    },
}

/// Everything row `r` has to show in its drill-down, or an empty vector when it
/// has nothing — an expander that opens onto an empty panel teaches the reader
/// to stop clicking.
///
/// `all_columns` is every column the run produced, `detail_columns` those
/// flagged `DETAIL`. Pictures are collected from `all_columns`, deliberately: a
/// picture is worth showing larger whether or not its column is `DETAIL`, and
/// the grid cell it also appears in is a thumbnail.
pub fn sections<'a>(
    result: &'a ReportResult,
    r: usize,
    all_columns: &'a [OutputColumn],
    detail_columns: &[&'a OutputColumn],
) -> Vec<DetailSection<'a>> {
    let Some(row) = result.rows.get(r) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for c in all_columns {
        if let Some(image) = result.images.get(&(r, c.header.clone())) {
            out.push(DetailSection::Image {
                header: &c.header,
                image,
                value: c.value(row, &result.no_match_marker),
            });
        }
    }
    for c in detail_columns {
        let value = c.value(row, &result.no_match_marker);
        if !value.trim().is_empty() {
            out.push(DetailSection::Text {
                header: &c.header,
                value: pretty_json(&value),
                verdict: verdict_for(result, r, &c.header),
            });
        }
    }
    if let Some(b) = result.baseline_rows.get(&r) {
        for c in detail_columns {
            let was = c.value(b, &result.no_match_marker);
            let now = c.value(row, &result.no_match_marker);
            let Some(fields) = super::jsondiff::diff_texts(&was, &now) else {
                continue;
            };
            if fields.iter().any(|f| f.differs()) {
                out.push(DetailSection::Diff {
                    header: &c.header,
                    fields,
                });
            }
        }
    }
    out
}

/// How a ground-truthed cell scored and what it was scored against, or `None`
/// when its column declares no `TRUTH` or the row wasn't labelled.
fn verdict_for(result: &ReportResult, r: usize, header: &str) -> Option<(Verdict, String)> {
    let key = (r, header.to_string());
    match result.verdicts.get(&key)? {
        Verdict::Untested => None,
        v => Some((*v, result.truths.get(&key).cloned().unwrap_or_default())),
    }
}

/// What a ground-truthed detail section says beside its heading: the verdict,
/// and — when it is wrong — what was expected instead, which is otherwise only
/// findable by going back to the grid or reading the script.
pub fn verdict_label(v: Verdict, truth: &str) -> String {
    match (v, truth.trim()) {
        (Verdict::Incorrect, t) if !t.is_empty() => {
            format!("{} \u{2014} expected {t}", v.as_str())
        }
        _ => v.as_str().to_string(),
    }
}

/// `text` pretty-printed if it is JSON, and unchanged if it isn't. A detail
/// column is very often a whole response body, which arrives on one line.
pub fn pretty_json(text: &str) -> String {
    serde_json::from_str::<serde_json::Value>(text.trim())
        .ok()
        .and_then(|v| serde_json::to_string_pretty(&v).ok())
        .unwrap_or_else(|| text.to_string())
}

#[cfg(test)]
mod tests {
    /// Every column being `DETAIL` would leave an empty grid, so the flag is
    /// ignored rather than obeyed off a cliff.
    #[test]
    fn an_all_detail_run_keeps_its_grid() {
        let mut a = super::tests::col("A");
        let mut b = super::tests::col("B");
        a.detail = true;
        b.detail = true;
        let cols = [a, b];
        let (grid, detail) = super::split_columns(&cols);
        assert_eq!(grid.len(), 2, "the grid keeps every column");
        assert_eq!(detail.len(), 2, "and the panel still offers them in full");
    }

    /// The ordinary case: a `DETAIL` column leaves the grid for the panel.
    #[test]
    fn a_detail_column_leaves_the_grid() {
        let a = super::tests::col("A");
        let mut b = super::tests::col("Body");
        b.detail = true;
        let cols = [a, b];
        let (grid, detail) = super::split_columns(&cols);
        assert_eq!(grid.len(), 1, "only the plain column is in the grid");
        assert_eq!(detail.len(), 1);
        assert_eq!(detail[0].header, "Body");
    }

    use super::*;
    use crate::report::model::{ReportRow, StatKind};

    fn col(header: &str) -> OutputColumn {
        OutputColumn {
            header: header.to_string(),
            sources: vec![header.to_string()],
            stats: Vec::<StatKind>::new(),
            image: None,
            truth: None,
            detail: false,
        }
    }

    fn row(pairs: &[(&str, &str)]) -> ReportRow {
        let mut r = ReportRow::default();
        for (k, v) in pairs {
            r.cells.insert(k.to_string(), v.to_string());
        }
        r
    }

    /// A row with nothing worth drilling into gets no sections, so no renderer
    /// offers an expander onto an empty panel.
    #[test]
    fn an_ordinary_row_has_nothing_to_drill_into() {
        let cols = vec![col("Name")];
        let result = ReportResult {
            column_order: vec!["Name".into()],
            rows: vec![row(&[("Name", "a")])],
            ..Default::default()
        };
        assert!(sections(&result, 0, &cols, &[]).is_empty());
    }

    /// A `DETAIL` column whose value is blank is not worth a heading -- the
    /// panel would be a title over nothing.
    #[test]
    fn a_blank_detail_column_is_left_out() {
        let cols = vec![col("Body")];
        let detail: Vec<&OutputColumn> = cols.iter().collect();
        let result = ReportResult {
            column_order: vec!["Body".into()],
            rows: vec![row(&[("Body", "   ")])],
            ..Default::default()
        };
        assert!(sections(&result, 0, &cols, &detail).is_empty());
    }

    /// The panel leads with pictures, then the detail text: the picture is what
    /// the reader opened the row to see.
    #[test]
    fn pictures_come_before_the_detail_text() {
        let cols = vec![col("Frame"), col("Body")];
        let detail = vec![&cols[1]];
        let mut result = ReportResult {
            column_order: vec!["Frame".into(), "Body".into()],
            rows: vec![row(&[("Frame", "shots/a.png"), ("Body", "{\"a\":1}")])],
            ..Default::default()
        };
        result.images.insert(
            (0, "Frame".to_string()),
            ImageData {
                bytes: crate::report::image::tests::png_1x1(),
                mime: "image/png".to_string(),
                natural: (1, 1),
            },
        );
        let got = sections(&result, 0, &cols, &detail);
        assert!(
            matches!(got[0], DetailSection::Image { header, .. } if header == "Frame"),
            "picture first: {got:?}"
        );
        match &got[1] {
            DetailSection::Text { header, value, .. } => {
                assert_eq!(*header, "Body");
                assert!(value.contains('\n'), "JSON is pretty-printed: {value}");
            }
            other => panic!("expected the detail text second, got {other:?}"),
        }
    }

    /// A `DETAIL` column that declares a `TRUTH` carries its verdict into the
    /// panel: the grid tints such a cell by whether it is right, and the panel
    /// is where the value is actually read.
    #[test]
    fn a_ground_truthed_detail_column_carries_its_verdict() {
        let mut c = col("Body");
        c.detail = true;
        c.truth = Some("Low Risk".to_string());
        let cols = vec![c];
        let mut result = ReportResult {
            column_order: vec!["Body".into()],
            rows: vec![row(&[("Body", "High Risk")])],
            ..Default::default()
        };
        result
            .verdicts
            .insert((0, "Body".into()), Verdict::Incorrect);
        result
            .truths
            .insert((0, "Body".into()), "Low Risk".to_string());
        let (_, detail) = split_columns(&cols);
        let got = sections(&result, 0, &cols, &detail);
        let DetailSection::Text { verdict, .. } = &got[0] else {
            panic!("expected the detail text, got {got:?}");
        };
        assert_eq!(
            verdict.as_ref().map(|(v, t)| (*v, t.as_str())),
            Some((Verdict::Incorrect, "Low Risk")),
            "the verdict and what it was scored against travel together"
        );
        // What the reader is shown: the verdict, and the answer that was
        // wanted — otherwise the only way to find it is to read the script.
        assert_eq!(
            verdict_label(Verdict::Incorrect, "Low Risk"),
            "incorrect \u{2014} expected Low Risk"
        );
        assert_eq!(
            verdict_label(Verdict::Correct, "Low Risk"),
            "correct",
            "a right answer doesn't repeat the answer back"
        );
    }

    /// An unlabelled row is not a pass: it carries no verdict at all, so no
    /// renderer can paint it like one.
    #[test]
    fn an_unlabelled_row_carries_no_verdict() {
        let mut c = col("Body");
        c.detail = true;
        let cols = vec![c];
        let mut result = ReportResult {
            column_order: vec!["Body".into()],
            rows: vec![row(&[("Body", "High Risk")])],
            ..Default::default()
        };
        result
            .verdicts
            .insert((0, "Body".into()), Verdict::Untested);
        let (_, detail) = split_columns(&cols);
        let got = sections(&result, 0, &cols, &detail);
        assert!(
            matches!(&got[0], DetailSection::Text { verdict: None, .. }),
            "an untested cell says nothing: {got:?}"
        );
    }

    /// A picture is worth showing larger whether or not its column is `DETAIL`,
    /// so the panel collects pictures from every column.
    #[test]
    fn a_grid_picture_still_appears_in_the_panel() {
        let cols = vec![col("Frame")];
        let mut result = ReportResult {
            column_order: vec!["Frame".into()],
            rows: vec![row(&[("Frame", "shots/a.png")])],
            ..Default::default()
        };
        result.images.insert(
            (0, "Frame".to_string()),
            ImageData {
                bytes: crate::report::image::tests::png_1x1(),
                mime: "image/png".to_string(),
                natural: (1, 1),
            },
        );
        // No DETAIL columns at all, and the picture is still offered.
        assert_eq!(sections(&result, 0, &cols, &[]).len(), 1);
    }

    /// A diff of two identical bodies says nothing, so it is not a section --
    /// otherwise every unchanged row would sprout a table of unchanged fields.
    #[test]
    fn an_unchanged_body_produces_no_diff_section() {
        let cols = vec![col("Body")];
        let detail: Vec<&OutputColumn> = cols.iter().collect();
        let mut result = ReportResult {
            column_order: vec!["Body".into()],
            rows: vec![row(&[("Body", "{\"a\":1}")])],
            ..Default::default()
        };
        result
            .baseline_rows
            .insert(0, row(&[("Body", "{\"a\":1}")]));
        let got = sections(&result, 0, &cols, &detail);
        assert!(
            got.iter().all(|s| !matches!(s, DetailSection::Diff { .. })),
            "identical bodies differ in nothing: {got:?}"
        );

        result
            .baseline_rows
            .insert(0, row(&[("Body", "{\"a\":2}")]));
        let got = sections(&result, 0, &cols, &detail);
        assert!(
            got.iter().any(|s| matches!(s, DetailSection::Diff { .. })),
            "but a changed one does: {got:?}"
        );
    }
}
