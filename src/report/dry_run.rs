//! The dry-run preview: what a report would emit if it were run, worked out
//! without sending a single request.
//!
//! Expanding a flow against [`crate::report::run::DryRunner`] exercises the
//! whole interpreter — producer expansion, loop nesting and products, ZIP and
//! tuple pairing, scoping, request-name resolution — so the projected rows, the
//! resolved per-iteration bindings and any producer or resolution problems all
//! fall out of it. Only the HTTP intrinsics are blank, because nothing was
//! sent.
//!
//! This module holds the front-end-agnostic *shape* of that preview. Each
//! front-end renders it its own way (the terminal UI as an overlay of themed
//! lines, the graphical one through its results grid), but both count the same
//! rows and report the same problems.

use crate::report::flow::Header;
use crate::report::model::ReportResult;

/// What a report *would* produce if it were run: the dry-run preview both
/// front-ends show.
///
/// The full [`ReportResult`] produced by expanding the flow with a no-op
/// runner (so all loop iterations, ZIP pairings and nested scopes are resolved
/// but no HTTP is sent) plus any variable-availability warnings from static
/// analysis. The result's column model is populated with everything knowable
/// without HTTP; intrinsic response fields (`HttpStatus`, `Time`, etc.) are
/// blank, matching a real grid cell that received no response.
pub struct DryRunReport {
    /// Total rows the flow would emit (`0` = empty glob, mismatched ZIP, …).
    pub rows: usize,
    /// The full dry-run result, used to render the same output grid the real
    /// run would show (via [`report_grid_lines`]).
    pub result: ReportResult,
    /// The flow's header (needed to resolve the `# columns:` directive for
    /// [`report_grid_lines`]).
    /// The flow's header, needed to resolve the `# columns:` directive when
    /// the preview grid is drawn.
    pub header: Header,
    /// Deduplicated producer / resolution problems (empty glob, ZIP length
    /// mismatch, unresolved request name, unloaded environment, …).
    pub errors: Vec<String>,
    /// Variable-availability warnings from static analysis (any `{{VAR}}`
    /// that may not be defined when the request that references it runs).
    pub var_warnings: Vec<String>,
}

impl DryRunReport {
    /// Build the preview from an expanded [`ReportResult`] (no HTTP), the
    /// flow's [`Header`] (for column resolution), and the variable-availability
    /// `var_warnings` already extracted from the report's diagnostics.
    pub fn from_result(result: ReportResult, header: Header, var_warnings: Vec<String>) -> Self {
        // A Cartesian product can repeat the same producer error on every
        // iteration — collapse duplicates while keeping first-seen order.
        let mut seen = std::collections::HashSet::new();
        let errors: Vec<String> = result
            .errors
            .iter()
            .filter(|e| seen.insert((*e).clone()))
            .cloned()
            .collect();
        let rows = result.rows.len();
        Self {
            rows,
            result,
            header,
            errors,
            var_warnings,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DryRunReport;
    use crate::report::model::{ReportResult, ReportRow};

    fn result_with(errors: Vec<&str>, rows: usize) -> ReportResult {
        ReportResult {
            rows: (0..rows).map(|_| ReportRow::default()).collect(),
            errors: errors.into_iter().map(String::from).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn a_preview_counts_the_rows_the_run_would_emit() {
        let preview =
            DryRunReport::from_result(result_with(Vec::new(), 6), Default::default(), Vec::new());
        assert_eq!(
            preview.rows, 6,
            "the projected row count is the whole point of a dry run"
        );
    }

    #[test]
    fn a_problem_repeated_by_every_iteration_is_only_reported_once() {
        // A Cartesian product hits the same unresolved request on every one of
        // its iterations; listing it a hundred times would bury everything else.
        let preview = DryRunReport::from_result(
            result_with(
                vec![
                    "no such request: login",
                    "no such request: login",
                    "empty glob",
                ],
                3,
            ),
            Default::default(),
            Vec::new(),
        );
        assert_eq!(
            preview.errors,
            vec![
                "no such request: login".to_string(),
                "empty glob".to_string()
            ],
            "duplicates are collapsed, and first-seen order is kept so the causes read in flow order"
        );
    }

    #[test]
    fn variable_warnings_are_carried_through_untouched() {
        // These come from static analysis, not from the expansion, so the
        // preview must not filter or reorder them.
        let warnings = vec!["{{TOKEN}} may not be set".to_string()];
        let preview = DryRunReport::from_result(
            result_with(Vec::new(), 1),
            Default::default(),
            warnings.clone(),
        );
        assert_eq!(preview.var_warnings, warnings);
    }
}
