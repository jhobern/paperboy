//! The paired statistics panel: each `STATISTICS` figure stated with the
//! comparison's value and the baseline's side by side, plus the request time
//! each side spent.
//!
//! The grid's footer states every statistic under the column it measures. That
//! is right, but it puts a comparison's two halves wherever their columns
//! happen to sit, and a renamed field's baseline copy (named after the *query*)
//! can land several columns away from the candidate's. The panel restates the
//! same numbers as before/after pairs. The in-app results view and the HTML
//! export both draw it, so building it lives here: if each renderer had its own
//! copy of the pairing rules, the two copies would drift.

use super::model::{OutputColumn, ReportResult, StatKind, SummaryRow};

/// One statistic: its label (`Mean`), the comparison's figure, and the
/// baseline's when the run has one.
#[derive(Debug, Clone, PartialEq)]
pub struct StatLine {
    pub label: String,
    pub comparison: String,
    pub baseline: Option<String>,
}

/// Every statistic one column asked for.
#[derive(Debug, Clone, PartialEq)]
pub struct StatBlock {
    pub header: String,
    pub lines: Vec<StatLine>,
}

/// The request time each side of the run spent, in one shared unit.
#[derive(Debug, Clone, PartialEq)]
pub struct TotalTime {
    pub comparison: String,
    pub baseline: Option<String>,
    pub unit: &'static str,
}

/// The whole panel. Empty when the run asked for no statistics and spent no
/// measurable time, in which case nothing should be drawn.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PairedStats {
    pub blocks: Vec<StatBlock>,
    pub total: Option<TotalTime>,
}

impl PairedStats {
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty() && self.total.is_none()
    }

    /// Whether there is a baseline half to state. An ordinary report gets one
    /// column of figures, with no heading saying which side it is and no
    /// `Baseline` column full of blanks.
    pub fn two_sided(&self) -> bool {
        self.total.as_ref().is_some_and(|t| t.baseline.is_some())
            || self
                .blocks
                .iter()
                .any(|b| b.lines.iter().any(|l| l.baseline.is_some()))
    }
}

/// Build the panel for `result` as shown through `columns` (the grid's
/// columns).
///
/// `extra` carries the baseline half of a *live* grid's statistics (see
/// [`ReportResult::baseline_summary_rows`]). A live grid has no `baseline.*`
/// columns of its own to pair through. A finished result passes `&[]`.
pub fn paired_stats(
    result: &ReportResult,
    columns: &[OutputColumn],
    extra: &[SummaryRow],
) -> PairedStats {
    // Read from the same rows the grid's footer is built from, so the panel
    // can never state a different number for the same statistic.
    let rows = result.summary_rows(columns);
    let paired: std::collections::HashMap<usize, usize> =
        result.baseline_column_pairs(columns).into_iter().collect();

    let mut blocks = Vec::new();
    for (ci, col) in columns.iter().enumerate() {
        if col.stats.is_empty() || col.header.starts_with("baseline.") {
            continue;
        }
        let mut lines = Vec::new();
        for r in &rows {
            let Some(cell) = r.cells.get(ci).and_then(Option::as_ref) else {
                continue;
            };
            // A distribution row is one line per distinct value, already named
            // by its own label: a tally, not a pair of measurements.
            if cell.stat == Some(StatKind::Distribution) {
                continue;
            }
            // A collapsed table pairs through its own `baseline.*` column; a
            // live one has none, so the re-keyed rows carry the other half.
            let baseline = paired
                .get(&ci)
                .and_then(|bi| r.cells.get(*bi))
                .and_then(Option::as_ref)
                .map(|v| v.text.clone())
                .or_else(|| {
                    extra
                        .iter()
                        .find(|e| e.label == format!("{} (baseline)", r.label))
                        .and_then(|e| e.cells.get(ci).and_then(Option::as_ref))
                        .map(|v| v.text.clone())
                });
            lines.push(StatLine {
                label: r.label.clone(),
                comparison: cell.text.clone(),
                baseline,
            });
        }
        if !lines.is_empty() {
            blocks.push(StatBlock {
                header: col.header.clone(),
                lines,
            });
        }
    }

    // Measured on the grid's own rows rather than a projection's: the
    // collapse folds the baseline rows away, and they are what the baseline's
    // total is made of.
    let (cand_ms, base_ms) = result.side_durations_ms();
    let total = (cand_ms > 0.0).then(|| {
        let unit = duration_unit(cand_ms.max(base_ms.unwrap_or(0.0)));
        TotalTime {
            comparison: unit.render(cand_ms),
            baseline: base_ms.map(|ms| unit.render(ms)),
            unit: unit.suffix,
        }
    });
    PairedStats { blocks, total }
}

/// The unit a pair of durations is stated in.
///
/// Chosen once, from the larger of the two, and then applied to both: the
/// panel exists to be compared across the row, and two totals in different
/// units force the reader to convert one by hand before comparing them.
struct DurationUnit {
    per_ms: f64,
    decimals: usize,
    suffix: &'static str,
}

impl DurationUnit {
    fn render(&self, ms: f64) -> String {
        format!("{:.*}", self.decimals, ms / self.per_ms)
    }
}

fn duration_unit(largest_ms: f64) -> DurationUnit {
    if largest_ms >= 60_000.0 {
        DurationUnit {
            per_ms: 60_000.0,
            decimals: 1,
            suffix: "min",
        }
    } else if largest_ms >= 1_000.0 {
        DurationUnit {
            per_ms: 1_000.0,
            decimals: 2,
            suffix: "s",
        }
    } else {
        DurationUnit {
            per_ms: 1.0,
            decimals: 0,
            suffix: "ms",
        }
    }
}
