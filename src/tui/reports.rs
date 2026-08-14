//! The TUI-side wrapper around a PaperTrail [`Report`] plus the "Reports view"
//! — a full-screen tab kind that lives in the same tab strip as the collection
//! tabs but shows only report content (no environments / response / raw-view
//! panels, so it fits small monitors, per the design).
//!
//! The core [`Report`] stays front-end agnostic; everything ratatui-specific
//! (cached diagnostics, the inline source editor, drawing) lives here so a
//! future GUI can reuse the core unchanged. A report tab shows the flow source
//! plus its live validation. Editing is *focus-based inline* (like the request
//! wizard's text cells): pressing `e`/Enter gives the source panel edit focus
//! so keystrokes type directly into it, and Esc returns to navigation mode
//! where single letters act as view shortcuts again. Edits apply live (the
//! validation panel refreshes as you type); Esc just leaves edit focus.

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};
use tui_panel_select::{MultiSelectPanel, WrapMode};

use super::app::{
    ConfirmAction, MouseHitTarget, MouseLayer, MouseScrollTarget, Overlay, Pane, PromptKind, TuiApp,
};
use super::draw::panel;
use super::editor::{
    Editor, apply_edit_key_full, render_editor_highlighted, word_left, word_right,
};
use super::new_request::draw_scrollbar;
use super::report_nodes::{SettingMenu, SettingMenuStep};
use super::theme::Theme;
use crate::i18n::{Status, Strings};
use crate::report::Report;
use crate::report::flow::Header;
use crate::report::indent::{
    INDENT_UNIT, ReformatError, indent_for_new_line, is_end_line, matching_opener_indent,
};
use crate::report::model::{ReportResult, ReportRow, TARGET_COLUMN, parse_columns};
use crate::report::run::{
    DryRunner, EntryRunner, LiveRunner, RowEvent, RunContext, finalize, run_flow, run_flow_raw,
};
use crate::report::validate::{Diagnostic, Severity};
use crate::report::writer::{
    CsvWriter, export_path, report_output_extension, writer_for_extension,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// A streaming update from a background report run, delivered from the worker
/// thread back to the event loop (see [`TuiApp::poll_report_run_updates`]). Each
/// variant is keyed by the report's process-unique `report_id` so it lands on
/// the right tab even if tabs were reordered or closed while it ran.
///
/// A run streams as: one [`Skeleton`](Self::Skeleton) (the full row set, all
/// cells still placeholder, so the grid appears immediately greyed-out), then
/// one [`Row`](Self::Row) per completed iteration (matched into its skeleton
/// slot by [`ReportRow::path`] and un-greyed), then one [`Done`](Self::Done)
/// carrying the finalized (comparison/baseline-collapsed) result that replaces
/// the streamed grid.
pub(crate) enum ReportRunUpdate {
    /// The projected row set from a no-HTTP dry expansion: every row present but
    /// unfilled. Shown immediately as a greyed-out grid so the user sees the
    /// shape and size of the run before any request completes.
    Skeleton {
        report_id: u64,
        result: ReportResult,
    },
    /// A leaf row has *started* running its requests (fired before any of them
    /// complete). Routed to its skeleton slot by `path` and drawn with a
    /// "running" marker so the grid shows which rows are in flight — several at
    /// once under a `PARALLEL` loop. Followed later by a [`Row`](Self::Row).
    RowStarted {
        report_id: u64,
        path: Vec<(usize, usize)>,
    },
    /// One completed iteration's row, matched into the skeleton by
    /// [`ReportRow::path`] and un-greyed. May arrive out of order under a
    /// `PARALLEL` loop (the path still identifies the target slot).
    Row { report_id: u64, row: Box<ReportRow> },
    /// The authoritative finalized result (after the comparison/baseline
    /// collapse), which replaces the streamed grid when the run finishes.
    Done {
        report_id: u64,
        result: ReportResult,
    },
}

impl ReportRunUpdate {
    /// The report this update belongs to (used to route it to the right tab).
    fn report_id(&self) -> u64 {
        match self {
            ReportRunUpdate::Skeleton { report_id, .. }
            | ReportRunUpdate::RowStarted { report_id, .. }
            | ReportRunUpdate::Row { report_id, .. }
            | ReportRunUpdate::Done { report_id, .. } => *report_id,
        }
    }
}

// Everything a report run needs, owned (no borrow of `TuiApp`), so the whole
// run can be moved onto a background thread. Defined in the front-end-agnostic
// `report::context` (shared with the GUI) and assembled on the main thread by
// [`TuiApp::build_report_run_inputs`]; the worker rebuilds a [`RunContext`]
// that borrows these.
use crate::report::context::ReportRunInputs;

/// Wraps a real [`EntryRunner`] with a cancel flag so a running report can be
/// stopped mid-flight: once `cancel` flips, every subsequent request returns a
/// benign "cancelled" outcome instead of hitting the network, so the flow winds
/// down quickly (an in-flight request still finishes, but no new ones start).
/// The delivered result is discarded by the poller when the run was cancelled.
/// Generic over the inner runner so tests can wrap a fake instead of a
/// [`LiveRunner`].
struct CancellableRunner<R: EntryRunner> {
    inner: R,
    cancel: Arc<AtomicBool>,
}

impl<R: EntryRunner> EntryRunner for CancellableRunner<R> {
    fn run(
        &self,
        base: &crate::hurl::HurlEntry,
        vars: &HashMap<String, String>,
    ) -> crate::hurl::RunOutput {
        if self.cancel.load(Ordering::Relaxed) {
            return crate::hurl::RunOutput {
                entries: Vec::new(),
                error: Some("cancelled".to_string()),
            };
        }
        self.inner.run(base, vars)
    }
}

/// A report tab: the owned [`Report`] plus TUI-only view state (cached
/// diagnostics and, when the source fails to parse, the parser message). The
/// diagnostics are recomputed by [`TuiApp::revalidate_report`] whenever the
/// source, the bound collection, or the loaded environments change.
pub(crate) struct ReportTab {
    pub(crate) report: Report,
    /// Validation diagnostics (errors + warnings) from the last revalidation.
    pub(crate) diagnostics: Vec<Diagnostic>,
    /// The report's aliased helper collections, loaded at the last revalidation.
    ///
    /// Cached rather than loaded on demand because loading reads files from
    /// disk, and the pickers, completion and known/unknown tinting that need it
    /// all run while drawing. Refreshed by `revalidate_report`, which already
    /// fires on every change that could affect it.
    pub(crate) helpers: Vec<crate::report::run::HelperCollection>,
    /// Parser error message, if the source doesn't currently parse (shown in
    /// place of `diagnostics`, which can't be computed without a parse tree).
    pub(crate) parse_error: Option<String>,
    /// 1-based source line the parser rejected, if any — the syntax highlighter
    /// underlines it so a malformed script is obvious at a glance.
    pub(crate) parse_error_line: Option<usize>,
    /// When `Some`, the source panel has *edit focus*: keystrokes type into
    /// this live buffer (mirrored into `report.text` on every edit so the
    /// validation panel and tab name stay current) instead of acting as view
    /// shortcuts. `None` = navigation mode.
    pub(crate) editor: Option<Editor>,
    /// The `(row, col)` the source editor's cursor last sat at, remembered when
    /// leaving edit mode so re-entering (`e`) restores the caret there instead
    /// of jumping to the buffer end (clamped to the current text on restore).
    /// `None` until the tab has been edited once.
    pub(crate) edit_cursor: Option<(usize, usize)>,
    /// Selection/scroll panel backing the read-only source view (so it renders
    /// with the same wrapping, scrollbar and mouse-selection feel as the
    /// collection view's panels).
    pub(crate) source_panel: MultiSelectPanel,
    /// Selection/scroll panel backing the validation output.
    pub(crate) validation_panel: MultiSelectPanel,
    /// Which pane the report body shows: the flow source (default) or the
    /// results grid from the last run. Switching is a view toggle — a run's
    /// [`result`](Self::result) is retained when the user flips back to the
    /// source to tweak the flow.
    pub(crate) view: ReportView,
    /// Which of the two *editor* views (`Source` or `Nodes`) to restore when
    /// flipping back from the results grid — so `Tab`/`v` return to whichever
    /// editor the user last used rather than always the source.
    pub(crate) editor_view: ReportView,
    /// The selected row in the [`ReportView::Nodes`] outline (index into the
    /// flattened node rows; clamped on draw). Structural edits move it to track
    /// the affected node.
    pub(crate) node_selected: usize,
    /// Where the node editor's cursor is when it sits in the **settings**
    /// section above the flow rather than in the outline below it: `Some(i)`
    /// selects the i-th settings row (the trailing "add setting" row included),
    /// `None` means the cursor is on `node_selected` in the flow.
    ///
    /// Two indices rather than one combined one because everything structural —
    /// insert positions, node paths, the undo snapshots — is expressed in flow
    /// row numbers. Folding the settings in would have shifted every one of
    /// them by a count that changes as settings are added and removed.
    pub(crate) node_setting: Option<usize>,
    /// Undo stack for the structured node editor. Every structural node edit
    /// snapshots the pre-edit source text + node selection here (via
    /// [`ReportTab::set_text_undoable`]) so **Ctrl+Z** can restore the previous
    /// state exactly — the node editor's counterpart to the source editor's
    /// in-buffer undo. In-memory and per-tab (not persisted), like text undo.
    pub(crate) node_undo: Vec<NodeUndo>,
    /// The values this report's `PARAM`s will be run with. Seeded when the run
    /// settings first open — from what this report was last run with, falling
    /// back to each declaration's own default — and thereafter whatever the
    /// user has set. Keyed by the parameter's raw name, never its prompt.
    pub(crate) param_values: crate::report::params::ParamValues,
    /// Whether [`param_values`](Self::param_values) has been seeded yet. A
    /// separate flag rather than "is it empty", because a report whose
    /// parameters are all empty strings has been seeded just as much as one
    /// with values, and re-seeding would undo the clearing.
    pub(crate) params_seeded: bool,
    /// The selected row in the run settings view.
    pub(crate) param_selected: usize,
    /// The last run's output, if the report has been run this session. Rendered
    /// as a grid in [`ReportView::Results`] and the source of an `Export CSV`.
    pub(crate) result: Option<ReportResult>,
    /// The last dry-run preview, shown *in place of* the results grid until it
    /// is dismissed (Esc) or superseded by a real run. It deliberately reuses
    /// the results pane rather than a popup: a preview the user wants to read
    /// alongside the flow — and drill into with the cell viewer — shouldn't be
    /// a modal that steals every key and stacks above the windows it spawns.
    pub(crate) dry_run: Option<Box<DryRunReport>>,
    /// Whether the current [`result`](Self::result) has been exported (CSV / JSON
    /// / HTML / XLSX) since it was produced. Set `false` every time a run yields
    /// a fresh result and `true` once an export of it completes, so a rerun can
    /// warn before it discards results the user hasn't saved anywhere. A result
    /// that's only ever been viewed on screen counts as unexported.
    pub(crate) results_exported: bool,
    /// The file the last successful export of this tab's result wrote, so
    /// Ctrl+O can hand it to the desktop without asking where it went. Cleared
    /// whenever a fresh run replaces the result, because the file on disk then
    /// describes a run that is no longer on screen. Not persisted: it belongs
    /// to a result that doesn't outlive the session either.
    pub(crate) last_export: Option<std::path::PathBuf>,
    /// Live streaming state while a background run is in flight: which of the
    /// pre-built skeleton rows have been filled yet (so the grid greys the
    /// pending ones) and the path→row-index lookup that routes each streamed row
    /// to its slot. `None` when no run is streaming (never run / finished /
    /// stopped). Clearing this field is sufficient to stop highlighting rows as
    /// "running"; the partial grid in `result` is retained automatically.
    pub(crate) run_progress: Option<RunProgress>,
    /// Selection/scroll panel backing the results grid (clip-wrapped so each
    /// row stays on one line and columns line up, like program output).
    pub(crate) results_panel: MultiSelectPanel,
    /// Keyboard/mouse cell cursor in the results grid: `(row, col)` where both
    /// are 0-indexed over the data rows and columns respectively — row 0 is the
    /// first data row, not the header. `None` until the user first navigates or
    /// clicks. Clamped on draw to the current grid bounds (which grow as rows
    /// stream in). Reset to `None` each time a new run starts so the cursor
    /// begins fresh on the new grid.
    pub(crate) cell_cursor: Option<(usize, usize)>,
    /// The `cell_cursor` value the results panel was last auto-scrolled to keep
    /// visible. The draw code only re-centres the panel on the cursor when this
    /// differs from the current `cell_cursor` — i.e. after keyboard navigation
    /// moved it — so a mouse-wheel scroll (which scrolls the panel directly
    /// without touching the cursor) is *not* immediately yanked back to the
    /// highlighted cell. `None` forces a re-centre on the next draw.
    pub(crate) results_scrolled_to: Option<(usize, usize)>,
    /// Which rows the results grid is showing. `f` cycles through the filters
    /// the run actually offers ([`crate::report::filter::RowFilter::available`]),
    /// the same set the GUI's filter bar and the interactive HTML export draw —
    /// so "show me only the wrong rows" means the same rows everywhere. Reset
    /// to `All` whenever a new run starts, because the rows it selected are
    /// gone. Runtime-only.
    pub(crate) results_filter: crate::report::filter::RowFilter,
    /// Index of the leftmost grid column drawn in the results pane. A report
    /// can easily have more columns than fit, and the grid clips rather than
    /// wraps, so without this the columns past the right edge were simply
    /// unreachable. Recomputed on draw to keep `cell_cursor`'s column fully
    /// visible (the viewport follows the cursor, which Left/Right move), and
    /// clamped to the current column count. Runtime-only.
    pub(crate) results_col_offset: usize,
    /// When this report was opened from a Workspace tree, the workspace root it
    /// belongs to. This is a *link*, not UI state: the report is shown in the
    /// right pane of the Workspace collection tab rooted here, while that tab's
    /// own file-tree (drawn by `draw_collection_left`) stays on the left and
    /// drives all navigation. `None` for a standalone (non-workspace) report
    /// tab, which is a full-screen strip tab with no tree.
    pub(crate) workspace_root: Option<std::path::PathBuf>,
    /// For a Workspace-embedded report (`workspace_root.is_some()`): whether
    /// this report is the one currently *displayed* in its Workspace collection
    /// tab's right pane (`true`), or merely retained while that tab shows its
    /// request/response view (`false`, set once the user opens a
    /// collection/folder/request from the tab's tree). Keeping the hidden
    /// `ReportTab` around preserves its edits/results/in-flight run, so
    /// re-selecting the same `.trail` restores state instead of reloading from
    /// disk. A Workspace tab embeds at most one report at a time (opening a
    /// different one replaces it). Ignored for standalone report tabs, which
    /// always occupy their own strip slot regardless of this flag.
    pub(crate) embedded_active: bool,
}

/// The live state of one streaming report row, drawn as a status icon beside
/// the row and used to grey rows that haven't finished yet.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RowState {
    /// Placed in the skeleton but not started yet (its requests are queued).
    Scheduled,
    /// Its requests are in flight (a [`RowStarted`](ReportRunUpdate::RowStarted)
    /// arrived). Under `PARALLEL` several rows can be running at once.
    Running,
    /// Its result has streamed in ([`Row`](ReportRunUpdate::Row)); cells filled.
    Finished,
}

/// Per-tab live-streaming bookkeeping for an in-flight background report run.
/// The skeleton rows are stored on [`ReportTab::result`] (so the grid renders
/// them immediately, greyed); this tracks each row's [`RowState`] (for the
/// status icon + greying) and how to route a streamed row to its slot. When the
/// run is stopped, `run_progress` is simply cleared: completed rows already
/// live in `result`, so the partial grid is retained automatically.
pub(crate) struct RunProgress {
    /// One [`RowState`] per skeleton row (index-aligned with `result.rows`):
    /// `Scheduled` until it starts, `Running` while its requests are in flight,
    /// `Finished` once its real result has streamed in. Non-`Finished` rows are
    /// drawn greyed so the grid doubles as a live progress indicator.
    pub(crate) states: Vec<RowState>,
    /// Maps a row's structural [`ReportRow::path`] to its index in `result.rows`,
    /// so an out-of-order streamed row (under `PARALLEL`) still lands in the
    /// right slot.
    pub(crate) index: HashMap<Vec<(usize, usize)>, usize>,
    /// How many rows have finished so far (for the progress status).
    pub(crate) done: usize,
}

/// One entry on a report's node-editor undo stack: a full snapshot of the
/// source text plus the node-outline selection, captured immediately before a
/// structural edit so [`TuiApp::undo_report_node`] can restore both. Whole-text
/// snapshots keep undo trivially correct (every restored state is a valid flow)
/// at the cost of a little memory — fine for the handful of edits in a report.
#[derive(Clone)]
pub(crate) struct NodeUndo {
    pub(crate) text: String,
    pub(crate) node_selected: usize,
}

/// The most snapshots a single report keeps for node-editor undo. Generous for
/// interactive editing while bounding memory on a long session.
const NODE_UNDO_LIMIT: usize = 200;

/// Which pane a report tab's body shows.
/// One row of the run settings view: a declared `PARAM`, the value this run
/// will use for it, and what (if anything) is wrong with that value. Built on
/// demand from the flow, never stored, so it can't drift from the source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ParamRow {
    /// The raw name — what `{{NAME}}` reads and what the value is remembered
    /// under.
    pub(crate) name: String,
    /// What the row asks for, in words (the `LABEL`, or derived from the name).
    pub(crate) prompt: String,
    pub(crate) kind: crate::report::flow::ParamKind,
    pub(crate) value: String,
    /// Why this value wouldn't be accepted, if it wouldn't.
    pub(crate) problem: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub(crate) enum ReportView {
    /// The PaperTrail flow source (editable) plus its live validation.
    #[default]
    Source,
    /// The structured node editor: the flow rendered as a navigable outline of
    /// nodes ("Begin" → statements → `FOR` blocks), edited by inserting/removing/
    /// moving nodes rather than typing text. The TUI-native realisation of the
    /// "Scratch-like" authoring goal; shares the [`ReportFlow`] AST with the
    /// source view (both round-trip through the text).
    Nodes,
    /// The grid of rows produced by the last run.
    Results,
    /// The run settings: the values this run will use for the report's
    /// `PARAM` declarations. A report that declares parameters opens here, so
    /// what it is about to run against is seen before anything is sent.
    RunSettings,
}

impl ReportView {
    /// Whether this is one of the two *editor* views (source or nodes) as
    /// opposed to the results grid — used to remember which editor to return to
    /// after flipping to the grid and back.
    pub(crate) fn is_editor(self) -> bool {
        matches!(self, ReportView::Source | ReportView::Nodes)
    }
}

/// The three text panels of the full-screen report view, used to index
/// [`TuiApp::report_pane_areas`]/`report_pane_bars` for mouse hit-testing
/// (text selection + scrollbar drag). Source and Validation are both shown in
/// [`ReportView::Source`]; Results is shown in [`ReportView::Results`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ReportPane {
    Source,
    Validation,
    Results,
}

impl ReportPane {
    pub(crate) fn idx(self) -> usize {
        match self {
            ReportPane::Source => 0,
            ReportPane::Validation => 1,
            ReportPane::Results => 2,
        }
    }

    pub(crate) const ALL: [ReportPane; 3] = [
        ReportPane::Source,
        ReportPane::Validation,
        ReportPane::Results,
    ];
}

impl ReportTab {
    pub(crate) fn new(report: Report) -> Self {
        let mut results_panel = MultiSelectPanel::new();
        // A grid wants each row on exactly one line with columns aligned, so the
        // panel clips overflow rather than wrapping cells onto extra rows.
        results_panel.set_wrap_mode(WrapMode::Clip);
        // The source view is code-like (short, structured lines), so it clips
        // rather than wraps too — and, crucially, clip mode renders a blank
        // source line as one empty row (the wrap path drops empty lines), so the
        // read view's rows stay 1:1 with the buffer's and selection/highlight
        // land on the right lines even when the flow has blank separators.
        let mut source_panel = MultiSelectPanel::new();
        source_panel.set_wrap_mode(WrapMode::Clip);
        let report_asks_for_values = report.flow().is_ok_and(|f| !f.params().is_empty());
        Self {
            report,
            diagnostics: Vec::new(),
            helpers: Vec::new(),
            parse_error: None,
            parse_error_line: None,
            editor: None,
            edit_cursor: None,
            source_panel,
            validation_panel: MultiSelectPanel::new(),
            // A report that asks for something opens on the question rather
            // than on its source: what it is about to run against is the first
            // thing worth seeing, and the values are remembered per report so
            // this is usually a glance and an `r`.
            view: if report_asks_for_values {
                ReportView::RunSettings
            } else {
                ReportView::Source
            },
            editor_view: ReportView::Source,
            node_selected: 0,
            node_setting: None,
            node_undo: Vec::new(),
            param_values: Default::default(),
            params_seeded: false,
            param_selected: 0,
            result: None,
            dry_run: None,
            results_exported: false,
            last_export: None,
            run_progress: None,
            results_panel,
            cell_cursor: None,
            results_scrolled_to: None,
            results_filter: crate::report::filter::RowFilter::All,
            results_col_offset: 0,
            workspace_root: None,
            embedded_active: true,
        }
    }

    /// A report opened from a Workspace tree: as [`Self::new`] but linked to the
    /// Workspace `root` it belongs to, so it embeds in that Workspace collection
    /// tab's right pane (the tab's own file-tree drives navigation).
    pub(crate) fn new_in_workspace(report: Report, root: std::path::PathBuf) -> Self {
        let mut rt = Self::new(report);
        rt.workspace_root = Some(root);
        rt
    }

    /// Snapshot the current source text + node selection onto the node-editor
    /// undo stack, then swap in `new_text`. The single choke point every
    /// structural node edit routes its re-serialized flow through, so
    /// [`TuiApp::undo_report_node`] (Ctrl+Z) can revert the prior state exactly.
    /// The stack is bounded to [`NODE_UNDO_LIMIT`].
    pub(crate) fn set_text_undoable(&mut self, new_text: String) {
        self.node_undo.push(NodeUndo {
            text: self.report.text.clone(),
            node_selected: self.node_selected,
        });
        if self.node_undo.len() > NODE_UNDO_LIMIT {
            let overflow = self.node_undo.len() - NODE_UNDO_LIMIT;
            self.node_undo.drain(0..overflow);
        }
        self.report.set_text(new_text);
    }

    /// The rows the results grid is showing, as indices into `result.rows`.
    ///
    /// Everything that addresses a row by position — the cell cursor, the
    /// drill-down popup, the mouse hit-test — goes through this list, so a
    /// filtered grid's "row 3" is the third row *on screen*.
    ///
    /// A streaming run is always shown whole, for the reason the GUI shows it
    /// whole: [`crate::report::filter::visible_rows`] drops pending rows, so
    /// filtering a live run would empty the very view being watched.
    pub(crate) fn visible_result_rows(&self) -> Vec<usize> {
        let Some(result) = &self.result else {
            return Vec::new();
        };
        let n = result.rows.len();
        if self.run_progress.is_some()
            || self.results_filter == crate::report::filter::RowFilter::All
        {
            return (0..n).collect();
        }
        let header = self.report.flow().map(|f| f.header).unwrap_or_default();
        let columns = result.resolved_columns(&header);
        let labels = crate::report::labels::LabelMap::parse(&header.labels());
        crate::report::filter::visible_rows(result, &columns, &labels, &self.results_filter, "")
    }

    /// The filters this run offers, in the order `f` cycles them.
    ///
    /// Only the button filters ([`crate::report::filter::RowFilter::available`]),
    /// not the per-matrix-cell ones the GUI and the HTML export add: those are
    /// reached by clicking the cell that counted them, and there is no matrix
    /// to click in a terminal.
    pub(crate) fn result_filters(&self) -> Vec<crate::report::filter::RowFilter> {
        match &self.result {
            Some(result) if self.run_progress.is_none() => {
                crate::report::filter::RowFilter::available(result)
            }
            _ => Vec::new(),
        }
    }
}

impl TuiApp {
    /// Indices into `self.reports` of the *standalone* report tabs — the ones
    /// that occupy their own slot in the tab strip (collections first, then
    /// these). Workspace-embedded reports (`workspace_root.is_some()`) are shown
    /// inside their Workspace collection tab's right pane rather than as a
    /// separate strip tab, so they're excluded here (and from
    /// [`Self::tab_count`]). Order-preserving, so slot `k` past the collections
    /// maps to `reports[standalone_report_indices()[k]]`.
    pub(crate) fn standalone_report_indices(&self) -> Vec<usize> {
        self.reports
            .iter()
            .enumerate()
            .filter(|(_, rt)| rt.workspace_root.is_none())
            .map(|(i, _)| i)
            .collect()
    }

    /// The `self.reports` index of the report embedded in — and currently
    /// *shown* by — Workspace collection tab `ci`, if any. `None` when `ci`
    /// isn't a Workspace tab, has no embedded report, or is currently showing
    /// its request/response view (the embedded report retained but hidden, i.e.
    /// `embedded_active == false`).
    pub(crate) fn embedded_report_index(&self, ci: usize) -> Option<usize> {
        let root = self.collections.get(ci)?.workspace_root.as_deref()?;
        self.reports
            .iter()
            .position(|rt| rt.embedded_active && rt.workspace_root.as_deref() == Some(root))
    }

    /// Total number of tabs in the strip (collection tabs plus *standalone*
    /// report tabs). Workspace-embedded reports don't add a strip tab — they
    /// ride inside their Workspace collection tab — so they aren't counted.
    pub(crate) fn tab_count(&self) -> usize {
        self.collections.len() + self.standalone_report_indices().len()
    }

    /// Whether the active tab is currently showing a report — either a
    /// standalone report strip tab, or a Workspace collection tab displaying an
    /// embedded report in its right pane. Drives the report draw/key routing.
    pub(crate) fn active_is_report(&self) -> bool {
        self.active_report_index().is_some()
    }

    /// Whether the active tab is a *standalone* report strip tab (its unified
    /// index falls past the collection tabs), as opposed to a Workspace
    /// collection tab showing an embedded report (whose `active_tab` is a
    /// collection index). Used by tab-strip operations (move/close) that reason
    /// about strip position rather than "am I looking at a report".
    pub(crate) fn active_is_strip_report(&self) -> bool {
        self.active_tab >= self.collections.len()
    }

    /// The `self.reports` index of the report the active tab is showing, if any
    /// — resolving both a standalone report strip tab and a Workspace
    /// collection tab with an embedded report on display.
    pub(crate) fn active_report_index(&self) -> Option<usize> {
        let c = self.collections.len();
        if self.active_tab >= c {
            // A standalone report strip tab: map its strip slot to the reports
            // index, skipping embedded reports (which aren't in the strip).
            self.standalone_report_indices()
                .into_iter()
                .nth(self.active_tab - c)
        } else {
            // A collection tab: it shows an embedded report only if it's a
            // Workspace tab currently displaying one.
            self.embedded_report_index(self.active_tab)
        }
    }

    pub(crate) fn active_report(&self) -> Option<&ReportTab> {
        self.active_report_index().and_then(|i| self.reports.get(i))
    }

    /// Whether the report *body* (as opposed to the workspace tree, for an
    /// embedded report) currently holds focus — drives whether the body panels'
    /// borders are lit. A standalone report owns the whole view, so its body is
    /// always focused; an embedded report's body is focused only while
    /// `focus == Pane::Main` (the tree is `Pane::List`).
    pub(crate) fn report_body_focused(&self) -> bool {
        if self.active_is_strip_report() {
            true
        } else {
            self.focus == Pane::Main
        }
    }

    /// Create a new scratch report tab, make it active, validate it and persist.
    pub(crate) fn new_report_tab(&mut self) {
        let s = Strings::for_language(&self.language);
        let report = Report::scratch(s.report_default_name);
        self.reports.push(ReportTab::new(report));
        // A scratch report is standalone (no workspace), so it occupies the last
        // strip slot past the collection tabs. Count standalone reports rather
        // than `reports.len()`, which also includes embedded workspace reports
        // that don't appear in the strip.
        self.active_tab = self.collections.len() + self.standalone_report_indices().len() - 1;
        self.focus = Pane::Tabs;
        let idx = self.reports.len() - 1;
        self.revalidate_report(idx);
        self.save_state();
    }

    /// Push an already-loaded [`Report`] (from a `.trail` file or, later, git)
    /// as a new report tab, make it active, validate it and persist. Mirrors
    /// [`Self::new_report_tab`] but keeps the loaded report's provenance (path /
    /// git origin) so a subsequent "Save" writes back in place.
    pub(crate) fn open_loaded_report(&mut self, report: Report) {
        self.reports.push(ReportTab::new(report));
        // Standalone report (see `new_report_tab`): its strip slot is the last
        // among the standalone reports, not `reports.len()` (which counts the
        // embedded workspace reports that aren't in the strip).
        self.active_tab = self.collections.len() + self.standalone_report_indices().len() - 1;
        self.focus = Pane::Tabs;
        let idx = self.reports.len() - 1;
        self.revalidate_report(idx);
        self.save_state();
        self.status = Some(Status::Loaded);
    }

    /// Show the `.trail` at `path` **in place**, embedded in the right pane of
    /// its Workspace collection tab (rooted at `root`) — the pane that normally
    /// holds the request editor + response is replaced by the report body while
    /// the same file-tree stays on the left and keeps focus. This deliberately
    /// does *not* spawn a separate report tab (mirroring how selecting a
    /// collection/request from a Workspace tab doesn't spawn one); the
    /// standalone report path is [`Self::open_loaded_report`] instead.
    ///
    /// Selection follows the tree highlight, exactly like requests: landing the
    /// cursor on a report row *shows* it (no `Enter` needed). So this is called
    /// as the highlight moves, and must be cheap and non-destructive — it never
    /// discards a report or drops edits. Each visited report is loaded once and
    /// then **retained** in `self.reports` (hidden when the highlight moves off
    /// it, via [`Self::hide_embedded_report_for_root`]); moving back re-shows the
    /// retained tab with its edits/results intact. A Workspace tab shows at most
    /// one report at a time — the one flagged `embedded_active` — so showing a
    /// different report just flips the active flag (the previous one stays
    /// retained), and only a never-visited report triggers a disk load.
    ///
    /// The embedded `ReportTab` carries `workspace_root`, which excludes it from
    /// the tab strip and links it to its Workspace tab, so it reuses every
    /// report handler unchanged. Focus and the tree cursor are left untouched
    /// (the user is already standing on the report's row), keeping all
    /// navigation on the single collection-side tree.
    pub(crate) fn show_embedded_report(
        &mut self,
        path: std::path::PathBuf,
        root: std::path::PathBuf,
    ) {
        // The Workspace collection tab this report embeds into (create one if
        // the workspace isn't open as a tab yet — e.g. restored/edge cases).
        let ci = self.workspace_collection_tab(root.clone());

        // Selecting a report hides (but retains) whichever report this tab was
        // showing, so at most one is `embedded_active` per root. A no-op when
        // the same report is already the shown one.
        for rt in self
            .reports
            .iter_mut()
            .filter(|rt| rt.workspace_root.as_deref() == Some(root.as_path()))
        {
            rt.embedded_active = false;
        }

        // Re-show a previously-visited report (edits/results intact) or load a
        // never-seen one from disk, retaining it for future re-selection.
        if let Some(i) = self.reports.iter().position(|rt| {
            rt.workspace_root.as_deref() == Some(root.as_path())
                && rt.report.path.as_deref() == Some(path.as_path())
        }) {
            self.reports[i].embedded_active = true;
            self.revalidate_report(i);
        } else {
            match Report::load_local(&path) {
                Ok(report) => {
                    self.reports.push(ReportTab::new_in_workspace(report, root));
                    let idx = self.reports.len() - 1;
                    self.revalidate_report(idx);
                }
                Err(e) => {
                    self.status = Some(Status::Error(e));
                    return;
                }
            }
        }
        self.finish_show_embedded_report(ci);
    }

    /// Point the active tab at Workspace collection tab `ci` and keep focus on
    /// its file-tree, after (re)attaching an embedded report to it. Focus stays
    /// on the tree (`Pane::List`) so the single collection-side tree keeps
    /// driving navigation; the tree cursor is deliberately left where the user
    /// was. Selection follows the highlight, so this runs on every cursor step
    /// onto a report row — it stays lightweight (no `save_state`/status spill;
    /// the choice is persisted with the rest of the session on exit).
    fn finish_show_embedded_report(&mut self, ci: usize) {
        self.active_tab = ci;
        self.focus = Pane::List;
    }

    /// Find the Workspace collection tab rooted at `root`, creating an empty one
    /// if the workspace isn't open as a tab yet.
    pub(crate) fn workspace_collection_tab(&mut self, root: std::path::PathBuf) -> usize {
        if let Some(ci) = self
            .collections
            .iter()
            .position(|c| c.workspace_root.as_deref() == Some(root.as_path()))
        {
            return ci;
        }
        let name = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.to_string_lossy().into_owned());
        let mut col = crate::collection::Collection::new(name, Vec::new());
        col.workspace_root = Some(root);
        self.collections.push(col);
        self.collections.len() - 1
    }

    /// Point Workspace tab `ci`'s file-tree browse breadcrumb and cursor onto
    /// the row for report `path` — its containing folder relative to the root,
    /// then the report's own row. Used on restore so a reopened embedded report
    /// resumes with the tree highlighting it (the tab's `list_cursor` isn't
    /// persisted separately; it's normally derived from the loaded collection
    /// file, which is `None` while a report is showing).
    pub(crate) fn focus_workspace_tree_on_report(&mut self, ci: usize, path: &std::path::Path) {
        let Some(col) = self.collections.get_mut(ci) else {
            return;
        };
        let Some(root) = col.workspace_root.clone() else {
            return;
        };
        // Expand all ancestor folders of this report so it is visible in the
        // tree, then position the cursor on its row.
        if let Some(parent) = path.parent()
            && let Ok(rel) = parent.strip_prefix(&root)
        {
            let mut cur = root.clone();
            for component in rel.components() {
                cur.push(component);
                col.workspace_expanded.insert(cur.clone());
            }
        }
        col.list_cursor = col
            .ws_rows()
            .iter()
            .position(
                |r| matches!(r, crate::collection::WsRow::Report { path: p, .. } if p == path),
            )
            .unwrap_or(0);
    }

    /// Remove the embedded report at `self.reports[i]`, cleaning up a live run
    /// first (the poller routes updates to open tabs by id, so a stray channel
    /// for a removed tab must be retired). Used when a Workspace tab swaps one
    /// embedded report for another, or is closed.
    fn discard_embedded_report(&mut self, i: usize) {
        let rt = self.reports.remove(i);
        let report_id = rt.report.id;
        if let Some(cancel) = self.running_reports.remove(&report_id) {
            cancel.store(true, Ordering::Relaxed);
            self.pending_report_runs.retain(|(id, _)| *id != report_id);
        }
    }

    /// Drop every embedded report belonging to Workspace root `root`. With
    /// selection following the tree highlight, a root can accumulate several
    /// retained reports (one per report the user has visited), so this scans
    /// until none remain rather than assuming a single one. Called when the
    /// Workspace collection tab is closed so its retained reports don't outlive
    /// the tab in `self.reports`.
    pub(crate) fn discard_embedded_reports_for_root(&mut self, root: &std::path::Path) {
        while let Some(i) = self
            .reports
            .iter()
            .position(|rt| rt.workspace_root.as_deref() == Some(root))
        {
            self.discard_embedded_report(i);
        }
    }

    /// Hide (but retain) whichever report Workspace root `root` is showing, so
    /// its collection tab returns to the request/response view. Every retained
    /// `ReportTab` for the root is flipped to `embedded_active = false` (there
    /// may be several visited-and-retained ones — only one is ever active, but
    /// clearing them all is the robust "show no report" operation), preserving
    /// their state for when a `.trail` is re-selected from the tree.
    pub(crate) fn hide_embedded_report_for_root(&mut self, root: &std::path::Path) {
        for rt in self
            .reports
            .iter_mut()
            .filter(|rt| rt.workspace_root.as_deref() == Some(root))
        {
            rt.embedded_active = false;
        }
    }

    /// Create a brand-new scratch `.trail` inside Workspace tab `ci`'s root at
    /// the relative path `rel`, write it to disk immediately (so it becomes a
    /// real member of the workspace tree), and open it as a workspace-aware
    /// report pinned to that tree. Subfolders are allowed; a missing extension
    /// defaults to `.trail`. Absolute paths or ones escaping the root via `..`
    /// are rejected. Mirrors [`Self::create_workspace_collection`], but a report
    /// is a single self-contained file so it is saved right away rather than
    /// staying in memory until Ctrl+S.
    pub(crate) fn create_workspace_report(&mut self, ci: usize, rel: String) {
        let Some(root) = self
            .collections
            .get(ci)
            .and_then(|c| c.workspace_root.clone())
        else {
            return;
        };
        let rel = rel.trim();
        if rel.is_empty() {
            return;
        }
        let mut rel_path = std::path::PathBuf::from(rel);
        if rel_path.extension().is_none() {
            rel_path.set_extension("trail");
        }
        // Reject absolute paths or any `..`/root component that would let the
        // new file escape the workspace root (same rule as new collections).
        let safe = rel_path.components().all(|c| {
            matches!(
                c,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        });
        if !safe {
            self.status = Some(Status::Error(rel_path.display().to_string()));
            return;
        }
        let full_path = root.join(&rel_path);
        // Physical-containment guard. The `..`/absolute check above is purely
        // lexical, so it can't catch a destination that *resolves* outside the
        // workspace through a symlinked path component (a symlink is a `Normal`
        // component and slips through). Resolve the deepest existing ancestor of
        // the target and refuse to write if its real path escapes the real
        // workspace root — otherwise a report that looks "inside" the workspace
        // would land somewhere else entirely on disk.
        if crate::workspace::escapes_root(&root, &full_path) {
            self.status = Some(Status::WorkspaceReportEscaped(
                rel_path.display().to_string(),
            ));
            return;
        }
        let s = Strings::for_language(&self.language);
        let mut report = Report::scratch(s.report_default_name);
        // Create any parent folders inside the workspace before writing.
        if let Some(parent) = full_path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            self.status = Some(Status::Error(format!("{}: {e}", parent.display())));
            return;
        }
        if let Err(e) = report.save_local(&full_path) {
            self.status = Some(Status::Error(e));
            return;
        }
        // Show it embedded in this workspace tab's right pane and highlight its
        // new row in the tree (selection follows the highlight). Persist the new
        // selection now — creation is a deliberate action, unlike plain cursor
        // movement which relies on the session save at exit.
        self.show_embedded_report(full_path.clone(), root);
        self.focus_workspace_tree_on_report(ci, &full_path);
        self.save_state();
        self.status = Some(Status::WorkspaceReportCreated(
            rel_path.display().to_string(),
        ));
    }

    /// Create a brand-new empty report at the absolute `path` chosen in the
    /// new-report folder browser, then open it. If `path` lies inside an open
    /// Workspace tab's root, the report is created **embedded** in that
    /// workspace's tree (reusing [`Self::create_workspace_report`], so it shows
    /// in the tree and is workspace-aware); otherwise it's written and opened as
    /// a **standalone** report tab bound to `path`. A missing extension defaults
    /// to `.trail`.
    pub(crate) fn create_report_at_path(&mut self, path: &std::path::Path) {
        let mut path = path.to_path_buf();
        if path.extension().is_none() {
            path.set_extension("trail");
        }
        // Prefer creating inside an enclosing open workspace so the new report
        // joins that tree and is workspace-aware. If several roots contain the
        // path (nested workspaces), the deepest wins.
        let enclosing = self
            .collections
            .iter()
            .enumerate()
            .filter_map(|(ci, c)| {
                let root = c.workspace_root.as_ref()?;
                let rel = path.strip_prefix(root).ok()?;
                Some((ci, root.components().count(), rel.to_path_buf()))
            })
            .max_by_key(|(_, depth, _)| *depth);
        if let Some((ci, _, rel)) = enclosing
            && let Some(rel_str) = rel.to_str()
        {
            self.create_workspace_report(ci, rel_str.to_string());
            return;
        }
        // Standalone: write the empty report and open it as its own tab.
        let s = Strings::for_language(&self.language);
        let mut report = Report::scratch(s.report_default_name);
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            self.status = Some(Status::Error(format!("{}: {e}", parent.display())));
            return;
        }
        if let Err(e) = report.save_local(&path) {
            self.status = Some(Status::Error(e));
            return;
        }
        self.open_loaded_report(report);
    }

    /// when possible, else as an absolute path) against each open collection's
    /// path. Falls back to matching a collection by *name* so a report bound
    /// before the collection was ever saved to disk (common in tests / scratch
    /// work) still resolves. Git refs (`git:…`) aren't auto-resolved here.
    pub(crate) fn resolve_bound_collection(&self, report: &Report) -> Option<usize> {
        let cref = report.collection_ref()?;
        if cref.starts_with("git:") {
            return None;
        }
        let target = resolve_ref_path(report.path.as_deref(), &cref);
        self.collections.iter().position(|c| {
            c.path.as_ref().is_some_and(|p| paths_equal(p, &target)) || c.name == cref
        })
    }

    /// Recompute the diagnostics (and parse-error state) for report tab `idx`
    /// against the currently-loaded collections and environments.
    pub(crate) fn revalidate_report(&mut self, idx: usize) {
        // Cheap and idempotent: the values a parameterised report will run with
        // are filled in from what it was last run with the first time anything
        // touches the tab, so the run settings show real answers rather than
        // bare defaults however the tab was opened.
        self.seed_report_params(idx);
        // Compute the parse-error / diagnostics up front so the immutable reads
        // of `self.collections` / `self.global_envs` don't overlap the mutable
        // borrow of `self.reports[idx]` that stores the result.
        let (parse_error, parse_error_line, diagnostics, helpers) = {
            let Some(rt) = self.reports.get(idx) else {
                return;
            };
            match rt.report.flow() {
                Err(e) => (Some(e.to_string()), Some(e.line), Vec::new(), Vec::new()),
                Ok(flow) => {
                    // The full validation-context assembly (bound collection,
                    // request titles / [Reports] fields, env names + variable
                    // availability, filesystem anchoring) lives in the shared
                    // `report::context` so the GUI validates identically.
                    let diags = crate::report::context::report_diagnostics(
                        &self.collections,
                        &self.global_envs,
                        self.active_env_id,
                        &flow,
                        rt.report.path.as_deref(),
                        &crate::i18n::Strings::for_language(&self.language),
                    );
                    let (helpers, _) = crate::report::context::load_helpers(
                        &self.collections,
                        &flow,
                        rt.report.path.as_deref(),
                        &crate::i18n::Strings::for_language(&self.language),
                    );
                    (None, None, diags, helpers)
                }
            }
        };
        if let Some(rt) = self.reports.get_mut(idx) {
            rt.parse_error = parse_error;
            rt.parse_error_line = parse_error_line;
            rt.diagnostics = diagnostics;
            rt.helpers = helpers;
        }
    }

    /// Why report `idx` can't be run right now, as a user-facing message, or
    /// `None` when it is runnable. A report needs to parse, be bound to a
    /// loaded collection, and have no error-severity validation problems (a
    /// blocked run would only produce error rows). Warnings don't block.
    pub(crate) fn report_run_blocker(&self, idx: usize) -> Option<String> {
        let s = Strings::for_language(&self.language);
        let rt = self.reports.get(idx)?;
        if let Some(err) = &rt.parse_error {
            return Some(format!("{} {err}", s.report_run_parse_error));
        }
        if self.resolve_bound_collection(&rt.report).is_none() {
            return Some(s.report_run_unbound.to_string());
        }
        if rt.diagnostics.iter().any(|d| d.severity == Severity::Error) {
            return Some(s.report_run_has_errors.to_string());
        }
        None
    }

    /// Assemble the [`RunContext`] pieces for report `idx` (front-end-agnostic
    /// core inputs) and run its flow via `runner`, returning the produced
    /// [`ReportResult`]. The `runner` seam lets tests drive a fake without a
    /// network; production passes a [`LiveRunner`]. Returns `Err` with a
    /// user-facing reason when the report isn't runnable (see
    /// [`Self::report_run_blocker`]). Test-only: production runs go through the
    /// threaded [`Self::run_active_report`].
    #[cfg(test)]
    pub(crate) fn run_report_flow(
        &self,
        idx: usize,
        runner: &dyn EntryRunner,
    ) -> Result<ReportResult, String> {
        if let Some(reason) = self.report_run_blocker(idx) {
            return Err(reason);
        }
        self.flow_result(idx, runner)
    }

    /// Expand report `idx`'s flow without sending any HTTP (a [`DryRunner`]),
    /// returning the fully-materialised [`ReportResult`] so a preview can show
    /// the projected row count, the resolved per-iteration bindings and any
    /// producer/resolution problems. Unlike [`Self::run_report_flow`] this lets
    /// *validation errors* (e.g. an unresolved request name) through so they
    /// surface in the preview — only a parse error or an unbound collection (without
    /// either, the flow can't be expanded at all) blocks it.
    pub(crate) fn dry_run_report_flow(&self, idx: usize) -> Result<ReportResult, String> {
        let s = Strings::for_language(&self.language);
        let rt = self.reports.get(idx).ok_or(s.report_run_unbound)?;
        if let Some(err) = &rt.parse_error {
            return Err(format!("{} {err}", s.report_run_parse_error));
        }
        if self.resolve_bound_collection(&rt.report).is_none() {
            return Err(s.report_run_unbound.to_string());
        }
        self.flow_result(idx, &DryRunner)
    }

    /// Shared core of [`Self::run_report_flow`]/[`Self::dry_run_report_flow`]:
    /// assemble the [`RunContext`] for report `idx` and run its flow via
    /// `runner`. Assumes the report parses and is bound (the callers gate that);
    /// see them for the differing pre-run checks.
    fn flow_result(&self, idx: usize, runner: &dyn EntryRunner) -> Result<ReportResult, String> {
        let inputs = self.build_report_run_inputs(idx)?;
        let strings = Strings::for_language(&self.language);
        let ctx = RunContext {
            entries: &inputs.entries,
            helpers: &inputs.helpers,
            base_vars: inputs.base_vars,
            named_envs: inputs.named_envs,
            root: inputs.root,
            runner,
            strings: &strings,
            params: inputs.params,
            sink: None,
        };
        Ok(run_flow(&inputs.flow, &ctx))
    }

    /// Assemble the fully-owned [`ReportRunInputs`] for report `idx` — the flow,
    /// a clone of the bound collection's entries, the resolved base/named
    /// environment layers, the producer-path root and the runner's file root —
    /// so a run can be handed to a background thread with no borrow of `self`.
    /// Shared by the synchronous [`Self::flow_result`] (dry runs / tests) and
    /// the threaded [`Self::run_active_report`].
    fn build_report_run_inputs(&self, idx: usize) -> Result<ReportRunInputs, String> {
        let s = Strings::for_language(&self.language);
        let rt = self.reports.get(idx).ok_or(s.report_run_unbound)?;
        let flow = rt.report.flow().map_err(|e| e.to_string())?;
        // The actual assembly (bound-collection resolution, base/named env
        // layers, producer root, runner file-root) lives in the shared
        // `report::context` so the GUI runs reports identically.
        crate::report::context::report_run_inputs(
            &self.collections,
            &self.global_envs,
            self.active_env_id,
            &flow,
            rt.report.path.as_deref(),
        )
        .map(|mut inputs| {
            // The core has no language of its own; only the app knows which one
            // the user picked, so the run's own errors are set to it here.
            inputs.language = self.language.clone();
            inputs.params = self
                .report_param_rows(idx)
                .into_iter()
                .map(|r| (r.name, r.value))
                .collect();
            inputs
        })
        .map_err(|e| match e {
            crate::report::context::RunInputError::Unbound => s.report_run_unbound.to_string(),
        })
    }

    /// Run the active report against its bound collection on a **background
    /// thread** so the UI stays responsive during a long run (previously this
    /// ran inline and froze the whole app). Pressing `r` again while a run is in
    /// flight cancels it. A run that can't even start (parse error / unbound /
    /// validation errors) reports why in the status bar and keeps the source
    /// view; the delivered result is folded in by [`Self::poll_report_run_updates`].
    pub(crate) fn run_active_report(&mut self) {
        // A report that asks for values stops at the questions on the way to
        // its run — before the discard guard below, because opening the
        // questions discards nothing: the results stay exactly where they are
        // until the *second* Run actually starts.
        if self.report_run_needs_settings() {
            let idx = self.active_report_index().expect("checked above");
            self.seed_report_params(idx);
            self.reports[idx].view = ReportView::RunSettings;
            self.status = Some(Status::ReportRunSettingsFirst);
            return;
        }
        // Guard against a rerun silently discarding on-screen results the user
        // hasn't saved anywhere: ask first (#2). A run already in flight, or a
        // report with nothing worth keeping, skips straight through.
        if self.rerun_would_discard_unexported() {
            self.overlay = Some(Overlay::Confirm {
                action: ConfirmAction::RerunReport,
                sel: 0,
            });
            return;
        }
        self.start_active_report_run();
    }

    /// Start (or cancel) a run of the active report without the unexported-result
    /// guard. Reached directly once the rerun warning has been confirmed, and by
    /// [`Self::run_active_report`] when there's nothing to warn about.
    pub(crate) fn start_active_report_run(&mut self) {
        if let Some((report_id, inputs)) = self.prepare_report_run() {
            self.spawn_report_run(report_id, inputs, |file_root| LiveRunner { file_root });
        }
    }

    /// Whether Run on the active report should open its run settings instead of
    /// starting: true when the report declares parameters and they aren't the
    /// thing on screen. A run already in flight is exempt — that key is a
    /// cancel ([`Self::prepare_report_run`]), and a cancel must never be read
    /// as "show me the questions".
    pub(crate) fn report_run_needs_settings(&self) -> bool {
        let Some(idx) = self.active_report_index() else {
            return false;
        };
        !self
            .running_reports
            .contains_key(&self.reports[idx].report.id)
            && self.reports[idx].view != ReportView::RunSettings
            && self.report_has_params(idx)
    }

    /// Whether pressing "run" on the active report would throw away results the
    /// user hasn't exported. True only when the active report holds a result
    /// that hasn't been saved since it was produced *and* no run is currently in
    /// flight — a second `r` during a run is a cancel (handled in
    /// [`Self::prepare_report_run`]), not a rerun, so it must not be gated.
    pub(crate) fn rerun_would_discard_unexported(&self) -> bool {
        let Some(idx) = self.active_report_index() else {
            return false;
        };
        let rt = &self.reports[idx];
        rt.result.is_some()
            && !rt.results_exported
            && !self.running_reports.contains_key(&rt.report.id)
    }

    /// Test seam: start a background run of the active report with an injected
    /// runner (so tests exercise the real thread + poll plumbing without a
    /// network). Mirrors [`Self::run_active_report`] but for a fake runner.
    #[cfg(test)]
    pub(crate) fn start_report_run_faked<R, F>(&mut self, make_runner: F)
    where
        R: EntryRunner + Send + Sync + 'static,
        F: FnOnce(Option<PathBuf>) -> R + Send + 'static,
    {
        if let Some((report_id, inputs)) = self.prepare_report_run() {
            self.spawn_report_run(report_id, inputs, make_runner);
        }
    }

    /// Shared pre-flight for a background run: handle a re-run-while-running as a
    /// cancel, gate on run blockers, and assemble the owned run inputs. Returns
    /// `None` (having set the appropriate status) when the run shouldn't start.
    fn prepare_report_run(&mut self) -> Option<(u64, ReportRunInputs)> {
        let idx = self.active_report_index()?;
        let report_id = self.reports[idx].report.id;
        // A second `r` while running is a cancel. Set the worker's flag (so it
        // fires no more requests and winds down) and retire the run *now* —
        // drop our receiver and clear the running marker — rather than waiting
        // for the worker's `Done` to arrive. This lets the very next `r` start a
        // fresh run instead of being read as another cancel (the old behaviour,
        // which left the id in `running_reports` until the wind-down finished —
        // slow when a `PARALLEL` batch is still in flight). The detached worker
        // keeps draining in the background; its remaining messages land on the
        // dropped receiver and are ignored. Retain the partial grid: completed
        // rows keep their real responses and unstarted rows remain as greyed
        // skeleton placeholders — the user can view, save, or export the partial
        // output. Mirrors `close_active_report_tab`.
        if let Some(cancel) = self.running_reports.remove(&report_id) {
            cancel.store(true, Ordering::Relaxed);
            self.pending_report_runs.retain(|(id, _)| *id != report_id);
            let rt = &mut self.reports[idx];
            // Clear streaming progress so no row is left rendering as "running".
            // The partial grid in `rt.result` is intentionally kept.
            rt.run_progress = None;
            self.status = Some(Status::ReportRunStopped);
            return None;
        }
        if let Some(reason) = self.report_run_blocker(idx) {
            self.status = Some(Status::ReportRunBlocked(reason));
            return None;
        }
        // A parameterised report is run with whatever the run settings hold —
        // seeded here too, so `r` straight from the source view still runs with
        // the declared defaults and what this report was last run with.
        self.seed_report_params(idx);
        // A report that asks for values stops at the run settings on the way to
        // its run: Run opens them, Run again (from the settings) starts. The
        // values decide what the run *means*, so they deserve a look before
        // several minutes of requests go out under them — and this is also the
        // only reliable way back to them once a run has filled the screen with
        // results. A run already under way is unaffected: the cancel path above
        // returns before this.
        if self.report_has_params(idx) && self.reports[idx].view != ReportView::RunSettings {
            self.reports[idx].view = ReportView::RunSettings;
            self.status = Some(Status::ReportRunSettingsFirst);
            return None;
        }
        if let Some(problem) = self
            .report_param_rows(idx)
            .into_iter()
            .find_map(|r| r.problem.map(|p| format!("{}: {p}", r.prompt)))
        {
            // Refuse rather than run: a report that asks for something and is
            // given nothing produces plausible-looking rows built around a hole.
            self.status = Some(Status::ReportRunBlocked(problem));
            self.reports[idx].view = ReportView::RunSettings;
            return None;
        }
        self.remember_active_report_params(idx);
        match self.build_report_run_inputs(idx) {
            Ok(inputs) => Some((report_id, inputs)),
            Err(reason) => {
                self.status = Some(Status::ReportRunBlocked(reason));
                None
            }
        }
    }

    /// Spawn the worker thread for a prepared run. The run streams back over a
    /// channel drained by [`Self::poll_report_run_updates`] as: (1) a
    /// [`Skeleton`](ReportRunUpdate::Skeleton) from a no-HTTP dry expansion (so
    /// the grid appears immediately, greyed), (2) one
    /// [`Row`](ReportRunUpdate::Row) per completed iteration via the run's
    /// [`sink`](RunContext::sink) (un-greying its slot), then (3) a
    /// [`Done`](ReportRunUpdate::Done) with the finalized
    /// (comparison/baseline-collapsed) result. Requests are sent through a
    /// [`CancellableRunner`] around `make_runner`'s runner; the cancel flag +
    /// receiver are recorded and the "running" status set.
    fn spawn_report_run<R, F>(&mut self, report_id: u64, inputs: ReportRunInputs, make_runner: F)
    where
        R: EntryRunner + Send + Sync + 'static,
        F: FnOnce(Option<PathBuf>) -> R + Send + 'static,
    {
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_worker = cancel.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let ReportRunInputs {
                flow,
                entries,
                helpers,
                base_vars,
                named_envs,
                root,
                file_root,
                language,
                params,
            } = inputs;
            let strings = crate::i18n::Strings::for_language(&language);

            // 1. Skeleton: expand the flow with no HTTP (a `DryRunner`) to get
            //    the full, canonical row set up front. The base layers are
            //    cloned so the live run below can reuse the originals. The
            //    skeleton rows map 1:1 (by `path`) to the live rows the sink
            //    will stream, so the front-end can pre-build the grid and fill
            //    it in place.
            let skeleton = {
                let dry_ctx = RunContext {
                    entries: &entries,
                    helpers: &helpers,
                    base_vars: base_vars.clone(),
                    named_envs: named_envs.clone(),
                    root: root.clone(),
                    runner: &DryRunner,
                    strings: &strings,
                    params: params.clone(),
                    sink: None,
                };
                run_flow_raw(&flow, &dry_ctx)
            };
            if tx
                .send(ReportRunUpdate::Skeleton {
                    report_id,
                    result: skeleton,
                })
                .is_err()
            {
                return; // Receiver gone (tab closed) — nothing more to do.
            }

            // 2. Live run: stream each row's lifecycle through a `Sync` sink (the
            //    `PARALLEL` workers call it from several threads, and an
            //    `mpsc::Sender` is `Send` but not `Sync`, so it's wrapped in a
            //    `Mutex`). A row is announced `Started` (its slot goes "running")
            //    before its requests fire, then `Completed` when it lands. Events
            //    may arrive out of iteration order under `PARALLEL`; each row's
            //    `path` still identifies the target slot.
            let runner = CancellableRunner {
                inner: make_runner(file_root),
                cancel: cancel_worker,
            };
            let row_tx = Mutex::new(tx.clone());
            let sink = move |ev: RowEvent| {
                if let Ok(tx) = row_tx.lock() {
                    let msg = match ev {
                        RowEvent::Started(path) => ReportRunUpdate::RowStarted {
                            report_id,
                            path: path.to_vec(),
                        },
                        RowEvent::Completed(row) => ReportRunUpdate::Row {
                            report_id,
                            row: Box::new(row.clone()),
                        },
                    };
                    let _ = tx.send(msg);
                }
            };
            let ctx = RunContext {
                entries: &entries,
                helpers: &helpers,
                base_vars,
                named_envs,
                root,
                runner: &runner,
                strings: &strings,
                params: params.clone(),
                sink: Some(&sink),
            };
            let mut result = run_flow_raw(&flow, &ctx);
            // 3. Finalize (comparison/baseline collapse) off the raw rows, then
            //    hand back the authoritative result to replace the streamed grid.
            finalize(&mut result, &flow, &ctx);
            // A closed receiver (the tab was closed mid-run) is fine to ignore.
            let _ = tx.send(ReportRunUpdate::Done { report_id, result });
        });
        self.running_reports.insert(report_id, cancel);
        self.pending_report_runs.push((report_id, rx));
        self.status = Some(Status::ReportRunning);
    }

    /// Drain any pending background report-run updates and fold them into their
    /// tabs (matched by `report_id`, so a reordered/kept tab still gets them).
    /// Each run streams many messages (a skeleton, a row per iteration, a final
    /// result), so every buffered message is drained per call rather than one
    /// per event-loop iteration — otherwise a fast run would lag the grid. A
    /// worker that dropped its sender (finished or panicked) has its receiver
    /// retired and, if still marked running, its flag cleared so the indicator
    /// can't wedge on. Called once per event-loop iteration.
    pub(crate) fn poll_report_run_updates(&mut self) {
        if self.pending_report_runs.is_empty() {
            return;
        }
        let mut still = Vec::new();
        for (report_id, rx) in std::mem::take(&mut self.pending_report_runs) {
            let mut alive = true;
            loop {
                match rx.try_recv() {
                    Ok(update) => self.apply_report_run_update(update),
                    // No more buffered messages for now — keep draining next tick.
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    // Sender dropped: the run finished (its `Done` already
                    // arrived above) or the worker panicked. Retire the receiver,
                    // clear any lingering running flag, and drop leftover
                    // streaming progress so a panicked run can't wedge the grid
                    // in its greyed, half-filled state.
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        self.running_reports.remove(&report_id);
                        if let Some(rt) = self
                            .report_index_by_id(report_id)
                            .map(|i| &mut self.reports[i])
                        {
                            rt.run_progress = None;
                        }
                        alive = false;
                        break;
                    }
                }
            }
            if alive {
                still.push((report_id, rx));
            }
        }
        self.pending_report_runs = still;
    }

    /// Apply one streamed report-run update to its tab. Routes on the update
    /// kind (see [`ReportRunUpdate`]): a **Skeleton** installs the greyed
    /// projected grid and switches to Results; a **Row** fills (and un-greys) its
    /// slot and advances the progress status; **Done** swaps in the finalized
    /// result. When the user stops a run mid-flight, the partial grid is retained
    /// (completed rows keep their real responses; unstarted rows stay as greyed
    /// skeleton placeholders) so the user can view, save, or export the work done
    /// so far.
    fn apply_report_run_update(&mut self, update: ReportRunUpdate) {
        let report_id = update.report_id();
        match update {
            ReportRunUpdate::Skeleton { result, .. } => {
                // If the run was cancelled before its skeleton arrived, drop it.
                if self.report_run_cancelled(report_id) {
                    return;
                }
                let Some(idx) = self.report_index_by_id(report_id) else {
                    return;
                };
                let n = result.rows.len();
                let index = result
                    .rows
                    .iter()
                    .enumerate()
                    .map(|(i, row)| (row.path.clone(), i))
                    .collect();
                let rt = &mut self.reports[idx];
                // Every slot is outstanding until its row streams in, so a live
                // `STATISTICS(…)` measures what has actually run rather than the
                // dry placeholders (`Time` 0, `status` 0) filling the rest.
                let mut result = result;
                result.pending = (0..n).collect();
                rt.result = Some(result);
                // A real run supersedes the projection it was previewing.
                rt.dry_run = None;
                // A fresh run's output starts life unexported, so a later rerun
                // can warn before discarding it (#2).
                rt.results_exported = false;
                rt.last_export = None;
                rt.run_progress = Some(RunProgress {
                    states: vec![RowState::Scheduled; n],
                    index,
                    done: 0,
                });
                // A new run invalidates the cell cursor (column layout may
                // change) — reset it so the cursor starts fresh on the new grid.
                rt.cell_cursor = None;
                rt.results_col_offset = 0;
                // The rows a filter selected belong to the run that is being
                // replaced, so the new grid starts unfiltered rather than
                // opening on an empty table.
                rt.results_filter = crate::report::filter::RowFilter::All;
                // Show the (greyed) grid straight away so the run's shape/size
                // is visible before any request completes — unless the user is
                // mid-edit, in which case just stage it (they can flip with Tab).
                if rt.editor.is_none() {
                    rt.view = ReportView::Results;
                    rt.results_panel.set_scroll(0);
                }
                self.status = Some(Status::ReportRunProgress { done: 0, total: n });
            }
            ReportRunUpdate::RowStarted { path, .. } => {
                if self.report_run_cancelled(report_id) {
                    return;
                }
                let Some(idx) = self.report_index_by_id(report_id) else {
                    return;
                };
                let rt = &mut self.reports[idx];
                let Some(prog) = rt.run_progress.as_mut() else {
                    return;
                };
                // Mark this row's slot "running" (unless it already finished —
                // a very fast row's Row can race ahead of its RowStarted).
                if let Some(&ri) = prog.index.get(&path)
                    && prog.states.get(ri) == Some(&RowState::Scheduled)
                {
                    prog.states[ri] = RowState::Running;
                }
            }
            ReportRunUpdate::Row { row, .. } => {
                if self.report_run_cancelled(report_id) {
                    return;
                }
                let Some(idx) = self.report_index_by_id(report_id) else {
                    return;
                };
                let rt = &mut self.reports[idx];
                let (Some(result), Some(prog)) = (rt.result.as_mut(), rt.run_progress.as_mut())
                else {
                    return;
                };
                // Route the streamed row into its skeleton slot by structural
                // path (stable + unique even under out-of-order `PARALLEL`).
                if let Some(&ri) = prog.index.get(&row.path)
                    && ri < result.rows.len()
                {
                    result.rows[ri] = *row;
                    result.pending.remove(&ri);
                    if prog.states[ri] != RowState::Finished {
                        prog.states[ri] = RowState::Finished;
                        prog.done += 1;
                    }
                }
                let done = prog.done;
                let total = prog.states.len();
                self.status = Some(Status::ReportRunProgress { done, total });
            }
            ReportRunUpdate::Done { result, .. } => {
                // Done is the run's terminal message: clear the running flag now.
                let cancelled = self
                    .running_reports
                    .remove(&report_id)
                    .map(|c| c.load(Ordering::Relaxed))
                    .unwrap_or(false);
                let Some(idx) = self.report_index_by_id(report_id) else {
                    return;
                };
                let rt = &mut self.reports[idx];
                // Take run_progress to clear the running-row highlight regardless of
                // whether the run completed normally or was stopped.
                let _progress = rt.run_progress.take();
                if cancelled {
                    // Run was stopped: `progress` has already been taken, so no
                    // row is left rendering as "running". Keep `rt.result` as the
                    // partial grid — completed rows have their real responses and
                    // unstarted rows remain as greyed skeleton placeholders. Stay
                    // on Results so the user can immediately view, save, or export
                    // the work done so far.
                    self.status = Some(Status::ReportRunStopped);
                    return;
                }
                let rows = result.rows.len();
                let errors = result.errors.len();
                rt.result = Some(result);
                // A real run supersedes the projection it was previewing.
                rt.dry_run = None;
                // The finalized result supersedes the skeleton and is likewise
                // unexported until the user saves it somewhere (#2).
                rt.results_exported = false;
                rt.last_export = None;
                // The finalized grid may have different columns/rows than the
                // streamed skeleton — reset cursor so it starts fresh.
                rt.cell_cursor = None;
                rt.results_col_offset = 0;
                // The rows a filter selected belong to the run that is being
                // replaced, so the new grid starts unfiltered rather than
                // opening on an empty table.
                rt.results_filter = crate::report::filter::RowFilter::All;
                if rt.editor.is_none() {
                    rt.view = ReportView::Results;
                    rt.results_panel.set_scroll(0);
                }
                self.status = Some(Status::ReportRunDone { rows, errors });
            }
        }
    }

    /// Whether the run for `report_id` has been stopped by the user (its cancel
    /// flag is set). A run with no live flag is treated as stopped/finished so
    /// stray late messages (e.g. from a run whose channel was already dropped)
    /// are silently ignored.
    fn report_run_cancelled(&self, report_id: u64) -> bool {
        self.running_reports
            .get(&report_id)
            .map(|c| c.load(Ordering::Relaxed))
            .unwrap_or(true)
    }

    /// Run report `idx` via `runner` and fold the outcome into the tab: on
    /// success, store the result, switch to the grid and report the row/error
    /// counts; on a blocked run, keep the source view and report why. Test-only:
    /// it drives the full store/switch/status path *synchronously* with a fake
    /// runner (no network); the production path is the threaded
    /// [`Self::run_active_report`] + [`Self::poll_report_run_updates`].
    #[cfg(test)]
    pub(crate) fn apply_report_run(&mut self, idx: usize, runner: &dyn EntryRunner) {
        match self.run_report_flow(idx, runner) {
            Ok(result) => {
                let rows = result.rows.len();
                let errors = result.errors.len();
                let rt = &mut self.reports[idx];
                rt.result = Some(result);
                // A real run supersedes the projection it was previewing.
                rt.dry_run = None;
                rt.results_exported = false;
                rt.last_export = None;
                rt.cell_cursor = None;
                rt.results_col_offset = 0;
                // The rows a filter selected belong to the run that is being
                // replaced, so the new grid starts unfiltered rather than
                // opening on an empty table.
                rt.results_filter = crate::report::filter::RowFilter::All;
                rt.view = ReportView::Results;
                rt.results_panel.set_scroll(0);
                self.status = Some(Status::ReportRunDone { rows, errors });
            }
            Err(reason) => self.status = Some(Status::ReportRunBlocked(reason)),
        }
    }

    /// One row of the run settings view.
    // Built fresh on every draw and key press from the flow + the tab's chosen
    // values, so it can never drift from the source the way a cached copy of a
    // declaration would.
    pub(crate) fn report_param_rows(&self, idx: usize) -> Vec<ParamRow> {
        let Some(rt) = self.reports.get(idx) else {
            return Vec::new();
        };
        let Ok(flow) = rt.report.flow() else {
            return Vec::new();
        };
        let s = Strings::for_language(&self.language);
        flow.params()
            .into_iter()
            .map(|p| {
                let value = rt
                    .param_values
                    .get(&p.name)
                    .cloned()
                    .or_else(|| p.default.clone())
                    .unwrap_or_default();
                ParamRow {
                    name: p.name.clone(),
                    prompt: p.prompt(),
                    kind: p.kind.clone(),
                    // The same check that would stop the run, shown while
                    // there is still someone here to fix it.
                    problem: crate::report::params::check(p, &value, &s)
                        .err()
                        .or_else(|| {
                            (value.trim().is_empty() && p.default.is_none())
                                .then(|| s.param_row_required.to_string())
                        }),
                    value,
                }
            })
            .collect()
    }

    /// Whether the active report declares any parameters — the question every
    /// "should the run settings exist here?" decision asks.
    pub(crate) fn report_has_params(&self, idx: usize) -> bool {
        self.reports
            .get(idx)
            .and_then(|rt| rt.report.flow().ok())
            .is_some_and(|f| !f.params().is_empty())
    }

    /// Open the run settings for the active report, seeding the values the
    /// first time from what this report was last run with (and, failing that,
    /// from each declaration's own default). Pressing the key again on the run
    /// settings goes back to the report itself: a report that asks for values
    /// *opens* on them, so its user never pressed anything to get here and
    /// needs a way out that isn't "undo a step you didn't take" — the same
    /// toggle `v` gives the results grid.
    pub(crate) fn open_report_run_settings(&mut self) {
        let Some(idx) = self.active_report_index() else {
            return;
        };
        if self.reports[idx].view == ReportView::RunSettings {
            self.reports[idx].view = self.reports[idx].editor_view;
            return;
        }
        if !self.report_has_params(idx) {
            self.status = Some(Status::ReportRunBlocked(
                Strings::for_language(&self.language)
                    .param_none_declared
                    .to_string(),
            ));
            return;
        }
        self.seed_report_params(idx);
        self.reports[idx].view = ReportView::RunSettings;
    }

    /// Fill in a report's parameter values from what it was last run with,
    /// once. Anything the remembered set doesn't cover keeps the declared
    /// default, so adding a parameter to a report someone has already run
    /// doesn't leave it blank.
    pub(crate) fn seed_report_params(&mut self, idx: usize) {
        if self.reports.get(idx).is_none_or(|rt| rt.params_seeded) {
            return;
        }
        let key = self.reports[idx].report.param_key();
        let remembered = self.remembered_params(&key);
        let declared: Vec<(String, Option<String>)> = self.reports[idx]
            .report
            .flow()
            .map(|f| {
                f.params()
                    .into_iter()
                    .map(|p| (p.name.clone(), p.default.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let rt = &mut self.reports[idx];
        for (name, default) in declared {
            // Never over-write what is already there: seeding can happen after
            // the user has already typed into the run settings (a report opened
            // straight onto them), and their answer outranks both the
            // remembered one and the declared default.
            if rt.param_values.contains_key(&name) {
                continue;
            }
            let value = remembered
                .get(&name)
                .cloned()
                .or(default)
                .unwrap_or_default();
            rt.param_values.insert(name, value);
        }
        rt.params_seeded = true;
    }

    /// Move the run settings cursor, clamped to the rows that exist.
    pub(crate) fn param_cursor_move(&mut self, delta: i32) {
        let Some(idx) = self.active_report_index() else {
            return;
        };
        let last = self.report_param_rows(idx).len().saturating_sub(1);
        let rt = &mut self.reports[idx];
        let next = (rt.param_selected as i32 + delta).clamp(0, last as i32);
        rt.param_selected = next as usize;
    }

    /// Enter on a run settings row: open whichever editor suits the parameter's
    /// declared type — a list for the ones with a closed set of answers, the
    /// file browser for the two that name paths, a text prompt for the rest.
    /// The type is what makes a parameter worth declaring rather than
    /// assigning, so this is where it earns its keep.
    pub(crate) fn configure_selected_param(&mut self) {
        let Some(idx) = self.active_report_index() else {
            return;
        };
        let rows = self.report_param_rows(idx);
        let sel = self.reports[idx].param_selected.min(rows.len());
        let Some(row) = rows.get(sel) else { return };
        let report_id = self.reports[idx].report.id;
        match &row.kind {
            crate::report::flow::ParamKind::Choice(options) if !options.is_empty() => {
                let selected = options.iter().position(|o| *o == row.value).unwrap_or(0);
                self.overlay = Some(Overlay::ReportSettingMenu(Box::new(SettingMenu {
                    step: SettingMenuStep::PickParam {
                        name: row.name.clone(),
                    },
                    options: options.clone(),
                    filter: String::new(),
                    selected,
                    report_id,
                })));
            }
            crate::report::flow::ParamKind::Env => {
                let options: Vec<String> =
                    self.global_envs.iter().map(|e| e.name.clone()).collect();
                if options.is_empty() {
                    // Nothing is loaded to choose between. Typing a name by
                    // hand still works, and is the right answer for an
                    // environment that lives on another machine.
                    self.status = Some(Status::ReportSettingNoChoices);
                    self.open_param_text_prompt(report_id, row);
                    return;
                }
                let selected = options.iter().position(|o| *o == row.value).unwrap_or(0);
                self.overlay = Some(Overlay::ReportSettingMenu(Box::new(SettingMenu {
                    step: SettingMenuStep::PickParam {
                        name: row.name.clone(),
                    },
                    options,
                    filter: String::new(),
                    selected,
                    report_id,
                })));
            }
            crate::report::flow::ParamKind::Folder | crate::report::flow::ParamKind::File => {
                self.pending_param_path = Some((report_id, row.name.clone()));
                // A parameter's paths almost always live beside the report.
                if let Some(dir) = self.active_report_base_dir() {
                    self.last_browse_dir = Some(dir);
                }
                self.open_browser(match row.kind {
                    crate::report::flow::ParamKind::Folder => {
                        super::app::FileAction::PickReportParamFolder
                    }
                    _ => super::app::FileAction::PickReportParamFile,
                });
            }
            _ => self.open_param_text_prompt(report_id, row),
        }
    }

    fn open_param_text_prompt(&mut self, report_id: u64, row: &ParamRow) {
        self.overlay = Some(Overlay::Prompt {
            kind: PromptKind::ReportParamValue {
                report_id,
                name: row.name.clone(),
            },
            editor: Editor::new(&row.value, false),
            title: row.prompt.clone(),
            mask: false,
            reset_to: None,
            secret_intact: false,
            secret_checkbox: None,
        });
    }

    /// Set one parameter's value for the next run, by report id so a tab
    /// reorder while a prompt is open can't misroute it.
    pub(crate) fn set_report_param(&mut self, report_id: u64, name: &str, value: String) {
        if let Some(rt) = self.reports.iter_mut().find(|rt| rt.report.id == report_id) {
            rt.param_values.insert(name.to_string(), value);
        }
    }

    /// Remember what this report was just run with, so reopening it offers the
    /// same answers rather than asking again from scratch.
    fn remember_active_report_params(&mut self, idx: usize) {
        let rows = self.report_param_rows(idx);
        if rows.is_empty() {
            return;
        }
        let key = self.reports[idx].report.param_key();
        let values: crate::report::params::ParamValues =
            rows.into_iter().map(|r| (r.name, r.value)).collect();
        if self.session.remember_params(&key, &values) {
            self.save_state();
        }
    }

    /// Key handling for [`ReportView::RunSettings`]. Returns `true` when it
    /// consumed the key; anything it doesn't take falls through to the shared
    /// report shortcuts (so `r`, the tab keys and the menus still work here).
    fn on_key_report_run_settings(&mut self, key: KeyEvent, idx: usize) -> bool {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.param_cursor_move(-1),
            KeyCode::Down | KeyCode::Char('j') => self.param_cursor_move(1),
            KeyCode::Home => self.param_cursor_move(i32::MIN),
            KeyCode::End => self.param_cursor_move(i32::MAX),
            KeyCode::Enter | KeyCode::Char(' ') => self.configure_selected_param(),
            KeyCode::Esc => {
                let back = self.reports[idx].editor_view;
                self.reports[idx].view = back;
            }
            _ => return false,
        }
        true
    }

    /// Toggle the active report between its current *editor* view and the
    /// results grid. Flipping to the results view is a no-op when there's
    /// nothing to show; flipping back restores whichever editor (source or
    /// nodes) was last used.
    pub(crate) fn toggle_report_view(&mut self) {
        if let Some(idx) = self.active_report_index() {
            let rt = &mut self.reports[idx];
            rt.view = match rt.view {
                v if v.is_editor() && rt.result.is_some() => {
                    rt.editor_view = v;
                    ReportView::Results
                }
                ReportView::Results => rt.editor_view,
                v => v,
            };
        }
    }

    /// Open the structured node editor for the active report — the Enter key,
    /// mirroring how Enter opens the request wizard. A report that doesn't parse
    /// has no node outline, so Enter drops into the raw text editor instead (the
    /// one editor that can fix the source). Esc (see `on_key_report`) backs out
    /// of the node view to the source view; there is no `n` toggle any more.
    pub(crate) fn open_report_node_editor(&mut self) {
        let Some(idx) = self.active_report_index() else {
            return;
        };
        if self.reports[idx].report.flow().is_err() {
            self.enter_report_edit();
            return;
        }
        let rt = &mut self.reports[idx];
        rt.view = ReportView::Nodes;
        rt.editor_view = ReportView::Nodes;
    }

    /// Export the active report's last run to CSV. Opens the folder picker
    /// (seeded with a `<report>.csv` filename and the report's own directory)
    /// so the user chooses where it lands, rather than silently writing into
    /// the process working directory. Reports why nothing can be written (no
    /// run yet) in the status bar.
    pub(crate) fn export_active_report_csv(&mut self) {
        let Some(idx) = self.active_report_index() else {
            return;
        };
        let s = Strings::for_language(&self.language);
        if self.reports[idx].result.is_none() {
            self.status = Some(Status::ReportRunBlocked(
                s.report_export_no_result.to_string(),
            ));
            return;
        }
        self.open_browser(super::app::FileAction::SaveReportCsvChooseFolder);
    }

    /// The default filename offered when exporting the active report's CSV: the
    /// saved report's stem (or its tab name) with a `.csv` extension.
    pub(crate) fn default_report_csv_filename(&self) -> String {
        match self.active_report_index() {
            Some(idx) => export_path(
                &self.reports[idx].report,
                &report_output_extension(&self.reports[idx].report),
            )
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "report.csv".to_string()),
            None => "report.csv".to_string(),
        }
    }

    /// The directory the active report's relative paths resolve against (its
    /// own folder, its `# root:`, or the process working directory) — used to
    /// seed the CSV-export folder picker.
    pub(crate) fn active_report_base_dir(&self) -> Option<std::path::PathBuf> {
        self.active_report_index()
            .map(|idx| report_base_dir(&self.reports[idx].report).0)
    }

    /// Write the active report's last run to `path`, choosing the output format
    /// from the file extension (csv/json/xlsx; unknown ⇒ CSV). Reports the
    /// destination — or the failure — in the status bar. Called by the folder
    /// picker once a destination is chosen.
    pub(crate) fn write_active_report_csv(&mut self, path: &std::path::Path) {
        let Some(idx) = self.active_report_index() else {
            return;
        };
        let s = Strings::for_language(&self.language);
        let rt = &self.reports[idx];
        let Some(result) = &rt.result else {
            self.status = Some(Status::ReportRunBlocked(
                s.report_export_no_result.to_string(),
            ));
            return;
        };
        // Columns come from the flow header's `columns:` directive; a parse
        // error can't happen here (a result only exists after a good run) but
        // fall back to the produced order just in case.
        let header = rt.report.flow().map(|f| f.header).unwrap_or_default();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("csv")
            .to_ascii_lowercase();
        let writer = writer_for_extension(&ext).unwrap_or_else(|| Box::new(CsvWriter));
        let bytes = match writer.write(result, &header) {
            Ok(b) => b,
            Err(e) => {
                self.status = Some(Status::Error(format!("{}: {e}", path.display())));
                return;
            }
        };
        match std::fs::write(path, bytes) {
            Ok(()) => {
                // The result now lives on disk, so a rerun needn't warn about
                // discarding it (#2).
                self.reports[idx].results_exported = true;
                self.reports[idx].last_export = Some(path.to_path_buf());
                self.status = Some(Status::ReportExported(path.display().to_string()));
            }
            Err(e) => self.status = Some(Status::Error(format!("{}: {e}", path.display()))),
        }
    }

    /// Save the active report's last run as a `.baseline` JSON snapshot
    /// (PaperTrail "Source B"). Opens the folder picker seeded with a
    /// `<report>.baseline` filename and the report's own directory; a later run
    /// diffs against it once its `# baseline:` header points at the file.
    /// Reports why nothing can be saved (no run yet) in the status bar.
    pub(crate) fn save_active_report_baseline(&mut self) {
        let Some(idx) = self.active_report_index() else {
            return;
        };
        let s = Strings::for_language(&self.language);
        if self.reports[idx].result.is_none() {
            self.status = Some(Status::ReportRunBlocked(
                s.report_baseline_no_result.to_string(),
            ));
            return;
        }
        self.open_browser(super::app::FileAction::SaveReportBaselineChooseFolder);
    }

    /// The default filename offered when saving the active report's baseline:
    /// the saved report's stem (or its tab name) with a `.baseline` extension.
    pub(crate) fn default_report_baseline_filename(&self) -> String {
        match self.active_report_index() {
            Some(idx) => export_path(&self.reports[idx].report, "baseline")
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "report.baseline".to_string()),
            None => "report.baseline".to_string(),
        }
    }

    /// Write the active report's last run to `path` as a `.baseline` JSON
    /// snapshot, reporting the destination — or the failure — in the status
    /// bar. Called by the folder picker once a destination is chosen.
    pub(crate) fn write_active_report_baseline(&mut self, path: &std::path::Path) {
        let Some(idx) = self.active_report_index() else {
            return;
        };
        let s = Strings::for_language(&self.language);
        let Some(result) = &self.reports[idx].result else {
            self.status = Some(Status::ReportRunBlocked(
                s.report_baseline_no_result.to_string(),
            ));
            return;
        };
        let baseline = crate::report::Baseline::from_result(result);
        match baseline.save(path) {
            Ok(()) => {
                // Saving a baseline snapshot persists the result to disk, so a
                // rerun needn't warn about discarding it (#2).
                self.reports[idx].results_exported = true;
                self.status = Some(Status::ReportBaselineSaved(path.display().to_string()));
            }
            Err(e) => self.status = Some(Status::Error(format!("{}: {e}", path.display()))),
        }
    }

    /// Hand the active report's last exported file to the desktop's default
    /// application (a browser for the interactive HTML, a spreadsheet for the
    /// xlsx). Bound to Ctrl+O.
    ///
    /// Only ever opens a file *this* tab exported, and only while it still
    /// describes the run on screen — a rerun clears it — because "open the
    /// report" silently showing a previous run's numbers is worse than not
    /// opening anything. Without one, says how to make one rather than
    /// doing nothing.
    pub(crate) fn open_exported_report(&mut self) {
        let Some(idx) = self.active_report_index() else {
            return;
        };
        let s = Strings::for_language(&self.language);
        let Some(path) = self.reports[idx].last_export.clone() else {
            self.status = Some(Status::ReportRunBlocked(
                s.report_open_no_export.to_string(),
            ));
            return;
        };
        match crate::shared_utils::open_in_desktop(&path) {
            Ok(()) => self.status = Some(Status::ReportOpened(path.display().to_string())),
            Err(e) => self.status = Some(Status::Error(format!("{}: {e}", path.display()))),
        }
    }

    /// Dry-run the active report: expand its flow with a no-op runner (no HTTP)
    /// and show, in the results pane, the projected output grid (identical
    /// to what a real run would produce, but with all HTTP-response fields
    /// blank), plus any variable-availability warnings from static analysis and
    /// any producer / request-resolution problems — so misaligned `ZIP`s,
    /// empty globs, Cartesian-product blow-ups and likely-undefined variables
    /// are caught before firing real requests. A run that can't even be
    /// expanded (parse error / unbound collection) reports why in the status
    /// bar instead.
    pub(crate) fn open_report_dry_run(&mut self) {
        let Some(idx) = self.active_report_index() else {
            return;
        };
        match self.dry_run_report_flow(idx) {
            Ok(result) => {
                // The flow header is needed to resolve `# columns:` for the
                // preview grid; fall back to an empty header if the flow is
                // somehow unparseable (shouldn't happen since dry_run_report_flow
                // already checked for parse errors).
                let header = self.reports[idx]
                    .report
                    .flow()
                    .map(|f| f.header)
                    .unwrap_or_default();
                // Extract the variable-availability warnings that validate()
                // computed at the last revalidation; these are Warning-severity
                // diagnostics and are non-blocking by design.
                let var_warnings: Vec<String> = self.reports[idx]
                    .diagnostics
                    .iter()
                    .filter(|d| d.severity == Severity::Warning)
                    .map(|d| d.message.clone())
                    .collect();
                let preview = DryRunReport::from_result(result, header, var_warnings);
                self.reports[idx].dry_run = Some(Box::new(preview));
                // Show it where results live, so the preview and the real thing
                // occupy the same place and the same keys scroll both.
                self.reports[idx].view = ReportView::Results;
                self.reports[idx].results_panel.set_scroll(0);
                self.reports[idx].results_col_offset = 0;
            }
            Err(reason) => self.status = Some(Status::ReportRunBlocked(reason)),
        }
    }

    /// Scroll the dry-run preview's grid sideways by `dc` columns, clamped so
    /// the leftmost column never goes past the last one. The preview has no
    /// cell cursor to follow, so this is the only thing that moves its
    /// viewport.
    fn preview_scroll_cols(&mut self, dc: i32) {
        let Some(idx) = self.active_report_index() else {
            return;
        };
        let Some(preview) = self.reports[idx].dry_run.as_ref() else {
            return;
        };
        let ncols = preview.result.resolved_columns(&preview.header).len();
        if ncols == 0 {
            return;
        }
        let cur = self.reports[idx].results_col_offset as i32;
        self.reports[idx].results_col_offset = (cur + dc).clamp(0, ncols as i32 - 1) as usize;
    }

    /// Move the cell cursor in the active report's Results grid by `(dr, dc)`.    /// Initialises the cursor at `(0, 0)` if it has no position yet. Clamps to
    /// the data-row/column count so it never points outside the grid.
    pub(crate) fn result_cursor_move(&mut self, dr: i32, dc: i32) {
        let Some(idx) = self.active_report_index() else {
            return;
        };
        let Some(result) = &self.reports[idx].result else {
            return;
        };
        let header = self.reports[idx]
            .report
            .flow()
            .map(|f| f.header)
            .unwrap_or_default();
        let ncols = result.resolved_columns(&header).len();
        // Row positions are on-screen positions: with a filter up, the grid has
        // fewer rows than the run does, and a cursor clamped to the run's count
        // would walk off the bottom of what is drawn.
        let nrows = self.reports[idx].visible_result_rows().len();
        if nrows == 0 || ncols == 0 {
            return;
        }
        let (cur_row, cur_col) = self.reports[idx].cell_cursor.unwrap_or((0, 0));
        let new_row = (cur_row as i32 + dr).clamp(0, nrows as i32 - 1) as usize;
        let new_col = (cur_col as i32 + dc).clamp(0, ncols as i32 - 1) as usize;
        self.reports[idx].cell_cursor = Some((new_row, new_col));
    }

    /// Step the active report's results grid to the next filter it offers,
    /// wrapping at the end.
    ///
    /// Inert unless a finished run is on screen with more than one filter to
    /// choose between: a report with no comparison and no ground truth offers
    /// only "All", and a key that silently does nothing is worse than one that
    /// isn't bound.
    pub(crate) fn cycle_report_row_filter(&mut self) {
        let Some(idx) = self.active_report_index() else {
            return;
        };
        if self.reports[idx].view != ReportView::Results {
            return;
        }
        let filters = self.reports[idx].result_filters();
        if filters.len() < 2 {
            return;
        }
        let at = filters
            .iter()
            .position(|f| *f == self.reports[idx].results_filter)
            .unwrap_or(0);
        let next = filters[(at + 1) % filters.len()].clone();
        self.reports[idx].results_filter = next;
        // Every row position on screen has just changed meaning, so the cursor
        // and the scroll start again at the top rather than pointing at
        // whichever row happens to have taken that slot.
        self.reports[idx].cell_cursor = None;
        self.reports[idx].results_scrolled_to = None;
        self.reports[idx].results_panel.set_scroll(0);
    }

    /// Move the cell cursor by a whole page (Ctrl+Up / Ctrl+Down). The page
    /// size is the number of visible data rows in the results pane — its inner
    /// height minus the sticky header line — so a page-move lands roughly one
    /// screenful away, clamped to the grid. Falls back to a single row if the
    /// pane height isn't known yet.
    pub(crate) fn result_cursor_page(&mut self, dir: i32) {
        let inner_h = self.report_pane_areas[ReportPane::Results.idx()].height;
        // Subtract the header line; keep at least one row of overlap for
        // orientation, and never a page of zero.
        let page = (inner_h.saturating_sub(2)).max(1) as i32;
        self.result_cursor_move(dir * page, 0);
    }
    /// current column. If there is no cursor yet, lands at `(0, 0)`.
    fn result_cursor_jump_home(&mut self) {
        let Some(idx) = self.active_report_index() else {
            return;
        };
        if !self.reports[idx].visible_result_rows().is_empty() {
            let col = self.reports[idx].cell_cursor.map(|(_, c)| c).unwrap_or(0);
            self.reports[idx].cell_cursor = Some((0, col));
        }
    }

    /// Jump the cell cursor to the last data row while keeping the current
    /// column. If there is no cursor yet, lands at the last row, column 0.
    fn result_cursor_jump_end(&mut self) {
        let Some(idx) = self.active_report_index() else {
            return;
        };
        let nrows = self.reports[idx].visible_result_rows().len();
        if nrows > 0 {
            let col = self.reports[idx].cell_cursor.map(|(_, c)| c).unwrap_or(0);
            self.reports[idx].cell_cursor = Some((nrows - 1, col));
        }
    }

    /// Open the cell drill-down popup ([`Overlay::ReportCellPopup`]) for the
    /// currently-selected cell. If no cell is selected yet, selects `(0, 0)` and
    /// waits for the user's next Enter press.
    pub(crate) fn open_result_cell_popup(&mut self) {
        let Some(idx) = self.active_report_index() else {
            return;
        };
        // Lazily initialise cursor on the first Enter press so the first Enter
        // simply selects a cell rather than immediately opening the popup.
        let Some((row, col)) = self.reports[idx].cell_cursor else {
            self.result_cursor_move(0, 0); // lands at (0, 0)
            return;
        };
        let visible = self.reports[idx].visible_result_rows();
        let Some(result) = &self.reports[idx].result else {
            return;
        };
        let header = self.reports[idx]
            .report
            .flow()
            .map(|f| f.header)
            .unwrap_or_default();
        let columns = result.resolved_columns(&header);
        let Some(col_def) = columns.get(col) else {
            return;
        };
        // `row` counts rows on screen; with a filter up that is not the run's
        // own numbering, so it is mapped back before the row is read.
        let Some(data_row) = visible.get(row).and_then(|&r| result.rows.get(r)) else {
            return;
        };
        let title = col_def.header.clone();
        // Full (unflattened) cell value — may be multi-line.
        let content = col_def.value(data_row, &result.no_match_marker);
        // Pretty-print the cell when its whole trimmed value is a single JSON
        // document (e.g. a captured response body), so drilling into it shows
        // an indented, one-field-per-line view instead of a dense single line.
        let content = pretty_print_json_cell(&content);
        let mut panel = Box::new(MultiSelectPanel::new());
        // Wrap mode so long values wrap to multiple lines inside the popup.
        panel.set_wrap_mode(WrapMode::Wrap);
        self.overlay = Some(Overlay::ReportCellPopup {
            title,
            content,
            panel,
        });
    }

    /// Key handling for the cell drill-down popup ([`Overlay::ReportCellPopup`]).
    /// Up/Down/PageUp/PageDown/Home/End scroll the panel; `y` copies the
    /// selection (or whole content) to the clipboard; Esc closes. All other keys
    /// keep the popup open (so the user can interact with the panel). The overlay
    /// was already `take`n by the dispatcher, so closing is just not restoring it.
    pub(crate) fn result_cell_popup_key_handler(
        &mut self,
        key: KeyEvent,
        title: String,
        content: String,
        mut panel: Box<MultiSelectPanel>,
    ) {
        let keep = |app: &mut TuiApp, title, content, panel| {
            app.overlay = Some(Overlay::ReportCellPopup {
                title,
                content,
                panel,
            });
        };
        match key.code {
            // Esc closes the popup.
            KeyCode::Esc => {}
            // Scroll the popup panel (the draw pass clamps overshoot via
            // `clamp_scroll` so over-large offsets are safe here).
            KeyCode::Up => {
                panel.set_scroll(panel.scroll().saturating_sub(1));
                keep(self, title, content, panel);
            }
            KeyCode::Down => {
                panel.set_scroll(panel.scroll().saturating_add(1));
                keep(self, title, content, panel);
            }
            KeyCode::PageUp => {
                panel.set_scroll(panel.scroll().saturating_sub(10));
                keep(self, title, content, panel);
            }
            KeyCode::PageDown => {
                panel.set_scroll(panel.scroll().saturating_add(10));
                keep(self, title, content, panel);
            }
            KeyCode::Home => {
                panel.set_scroll(0);
                keep(self, title, content, panel);
            }
            KeyCode::End => {
                panel.set_scroll(u16::MAX);
                keep(self, title, content, panel);
            }
            // `y` copies the panel selection to the clipboard, or — when
            // nothing is selected — the entire cell content.
            KeyCode::Char('y') => {
                use crate::tui::clipboard::copy_to_clipboard;
                let text = panel
                    .selected_text(None)
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| content.clone());
                if !text.is_empty() {
                    copy_to_clipboard(&text);
                    self.status = Some(crate::i18n::Status::Copied);
                }
                keep(self, title, content, panel);
            }
            // Any other key keeps the popup open (supports Shift+Arrow
            // text-selection extension, etc.).
            _ => keep(self, title, content, panel),
        }
    }

    /// Open the column-picker overlay for the active report. Available columns
    /// come from the last run, so a report must have been run first; otherwise
    /// a hint is shown. A parse error (which can't normally coexist with a
    /// result) falls back to that hint too.
    /// Re-indent the whole report source to its true block depth.
    ///
    /// Useful after a structural edit that changes nesting — wrapping an
    /// existing block in a new outer loop leaves its whole body one level short,
    /// and re-indenting it by hand in the source view is tedious.
    ///
    /// Only leading whitespace moves (see [`crate::report::indent::reformat`]),
    /// so comments and blank lines survive; a script that doesn't parse is left
    /// alone rather than guessed at.
    pub(crate) fn reformat_active_report(&mut self) {
        let Some(idx) = self.active_report_index() else {
            return;
        };
        match crate::report::indent::reformat(&self.reports[idx].report.text) {
            Ok(Some(text)) => {
                self.reports[idx].set_text_undoable(text);
                self.revalidate_report(idx);
                self.save_state();
                self.status = Some(Status::ReportReformatted);
            }
            Ok(None) => self.status = Some(Status::ReportAlreadyTidy),
            Err(ReformatError::Unparseable(msg)) => {
                self.status = Some(Status::ReportReformatFailed(msg));
            }
            Err(ReformatError::WouldChangeMeaning) => {
                let s = Strings::for_language(&self.language);
                self.status = Some(Status::ReportReformatFailed(
                    s.report_reformat_unsafe.to_string(),
                ));
            }
        }
    }

    pub(crate) fn open_report_columns(&mut self) {
        let Some(idx) = self.active_report_index() else {
            return;
        };
        let rt = &self.reports[idx];
        let Some(result) = &rt.result else {
            self.status = Some(Status::ReportColumnsNeedRun);
            return;
        };
        let Ok(flow) = rt.report.flow() else {
            self.status = Some(Status::ReportColumnsNeedRun);
            return;
        };
        let picker = ColumnPicker::build(&flow.header, result);
        self.overlay = Some(Overlay::ReportColumns(Box::new(picker)));
    }

    /// Key handling for the column-picker overlay ([`Overlay::ReportColumns`]).
    /// Up/Down move the cursor; Space (or `x`) toggles inclusion; Shift+Up/Down
    /// reorder the selected column; Enter applies the selection to the flow's
    /// `# columns:` directive and closes; Esc/`q` cancels. The overlay was
    /// already `take`n by the dispatcher, so closing is just not putting it back.
    pub(crate) fn report_columns_key_handler(
        &mut self,
        key: KeyEvent,
        mut picker: Box<ColumnPicker>,
    ) {
        let keep = |app: &mut TuiApp, picker| {
            app.overlay = Some(Overlay::ReportColumns(picker));
        };
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let last = picker.rows.len().saturating_sub(1);
        match key.code {
            KeyCode::Up if shift => {
                // Move the selected column up one place.
                if picker.selected > 0 {
                    picker.rows.swap(picker.selected, picker.selected - 1);
                    picker.selected -= 1;
                }
                keep(self, picker);
            }
            KeyCode::Down if shift => {
                if picker.selected < last {
                    picker.rows.swap(picker.selected, picker.selected + 1);
                    picker.selected += 1;
                }
                keep(self, picker);
            }
            KeyCode::Up => {
                picker.selected = picker.selected.saturating_sub(1);
                keep(self, picker);
            }
            KeyCode::Down => {
                picker.selected = (picker.selected + 1).min(last);
                keep(self, picker);
            }
            KeyCode::Home => {
                picker.selected = 0;
                keep(self, picker);
            }
            KeyCode::End => {
                picker.selected = last;
                keep(self, picker);
            }
            KeyCode::Char(' ') | KeyCode::Char('x') => {
                if let Some(row) = picker.rows.get_mut(picker.selected) {
                    row.included = !row.included;
                }
                keep(self, picker);
            }
            KeyCode::Enter => self.apply_report_columns(picker),
            // Esc / q / any other key: cancel (overlay stays taken).
            _ => {}
        }
    }

    /// Write the picker's selection back to the active report's `# columns:`
    /// directive (a surgical text edit), then revalidate and persist. With
    /// nothing included the directive is left untouched, a hint is shown, and
    /// the overlay stays open so the user can pick something.
    fn apply_report_columns(&mut self, picker: Box<ColumnPicker>) {
        let Some(idx) = self.active_report_index() else {
            return;
        };
        let Some(spec) = picker.spec() else {
            self.status = Some(Status::ReportColumnsNoneSelected);
            self.overlay = Some(Overlay::ReportColumns(picker));
            return;
        };
        let new_text = set_flow_columns_directive(&self.reports[idx].report.text, &spec);
        self.reports[idx].report.text = new_text;
        self.revalidate_report(idx);
        self.save_state();
        self.status = Some(Status::ReportColumnsApplied);
    }

    /// Open the collection-binding picker for the active report. Lists every
    /// open collection tab; a hint is shown (and the picker not opened) when
    /// none are loaded, since there'd be nothing to bind to.
    pub(crate) fn open_report_bind(&mut self) {
        if self.active_report_index().is_none() {
            self.status = Some(Status::NotReport);
            return;
        }
        if self.collections.is_empty() {
            self.status = Some(Status::ReportBindNoCollections);
            return;
        }
        let options: Vec<BindOption> = self
            .collections
            .iter()
            .map(|c| BindOption {
                name: c.name.clone(),
                path: c.path.clone(),
            })
            .collect();
        // Preselect the currently-bound collection, if any, so re-binding lands
        // on it first.
        let selected = self
            .active_report()
            .and_then(|rt| self.resolve_bound_collection(&rt.report))
            .unwrap_or(0)
            .min(options.len().saturating_sub(1));
        self.overlay = Some(Overlay::ReportBind(Box::new(ReportBindPicker {
            options,
            selected,
        })));
    }

    /// Key handling for the collection-binding picker ([`Overlay::ReportBind`]).
    /// Up/Down (and `j`/`k`, Home/End) move; Enter binds the selected
    /// collection; Esc/`q` cancels. The overlay was already `take`n by the
    /// dispatcher, so cancelling is just not putting it back.
    pub(crate) fn report_bind_key_handler(
        &mut self,
        key: KeyEvent,
        mut picker: Box<ReportBindPicker>,
    ) {
        let last = picker.options.len().saturating_sub(1);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                picker.selected = picker.selected.saturating_sub(1);
                self.overlay = Some(Overlay::ReportBind(picker));
            }
            KeyCode::Down | KeyCode::Char('j') => {
                picker.selected = (picker.selected + 1).min(last);
                self.overlay = Some(Overlay::ReportBind(picker));
            }
            KeyCode::Home => {
                picker.selected = 0;
                self.overlay = Some(Overlay::ReportBind(picker));
            }
            KeyCode::End => {
                picker.selected = last;
                self.overlay = Some(Overlay::ReportBind(picker));
            }
            KeyCode::Enter => self.apply_report_bind(*picker),
            // Esc / q / any other key: cancel (overlay stays taken).
            _ => {}
        }
    }

    /// Re-point the active report's `# collection:` header at the picker's
    /// selected collection (relative path preferred, then absolute, then the
    /// collection's name), then revalidate and persist.
    fn apply_report_bind(&mut self, picker: ReportBindPicker) {
        let Some(idx) = self.active_report_index() else {
            return;
        };
        let Some(option) = picker.options.get(picker.selected) else {
            return;
        };
        let report_path = self.reports[idx].report.path.clone();
        let cref =
            collection_ref_for_report(report_path.as_deref(), option.path.as_deref(), &option.name);
        let name = option.name.clone();
        let new_text = set_flow_directive(&self.reports[idx].report.text, "collection", &cref);
        self.reports[idx].report.set_text(new_text);
        self.revalidate_report(idx);
        self.save_state();
        self.status = Some(Status::ReportBound(name));
    }

    /// Give the active report tab's source panel edit focus, seeding an
    /// [`Editor`] from the current source text. While focused, keystrokes type
    /// directly into the panel (see [`TuiApp::on_key_report_editing`]).
    pub(crate) fn enter_report_edit(&mut self) {
        if let Some(idx) = self.active_report_index() {
            // Editing always happens in the source view; flip back to it if the
            // user was looking at the results grid.
            self.reports[idx].view = ReportView::Source;
            let text = self.reports[idx].report.text.clone();
            let mut editor = Editor::new(&text, true);
            // Restore the caret to where the user last left it (clamped to the
            // possibly-edited text) rather than always jumping to the end.
            if let Some((row, col)) = self.reports[idx].edit_cursor {
                editor.set_cursor(row, col);
            }
            self.reports[idx].editor = Some(editor);
        }
        // For an embedded report, editing means the body (not the tree) has
        // focus.
        if self.active_report_index().is_some() && !self.active_is_strip_report() {
            self.focus = Pane::Main;
        }
    }

    /// Close the active report tab, remembering it for reopen (`u`) and moving
    /// focus to the previous tab. Unlike collection tabs, a report tab at any
    /// position is closable (only the built-in Request collection tab is fixed).
    /// Only reachable for a *standalone* report strip tab — an embedded report
    /// is closed with its Workspace collection tab (see `finish_close_tab`).
    pub(crate) fn close_active_report_tab(&mut self) {
        let Some(ridx) = self.active_report_index() else {
            return;
        };
        // The strip slot the closed report occupied (standalone reports only),
        // so we can land on the neighbouring strip tab after removing it.
        let slot = self
            .standalone_report_indices()
            .iter()
            .position(|&i| i == ridx)
            .unwrap_or(0);
        let mut rt = self.reports.remove(ridx);
        // If this tab was still streaming a run, detach it cleanly before
        // stashing. The poller matches updates to *open* tabs by id and can't
        // reach a closed/stashed tab, so a live `run_progress` would reopen as a
        // permanently greyed grid. Cancel the worker, retire its channel, and
        // retain the partial grid (completed rows keep their real responses,
        // unstarted rows stay as greyed placeholders) so reopening the tab with
        // `u` shows the work done so far.
        let report_id = rt.report.id;
        if let Some(cancel) = self.running_reports.remove(&report_id) {
            cancel.store(true, Ordering::Relaxed);
            self.pending_report_runs.retain(|(id, _)| *id != report_id);
            // Clear streaming progress so no row is left rendering as "running".
            // The partial grid in `rt.result` is intentionally kept.
            rt.run_progress = None;
        }
        self.closed_tabs
            .push(super::app::ClosedTab::Report(ridx, Box::new(rt)));
        if self.closed_tabs.len() > 20 {
            self.closed_tabs.remove(0);
        }
        // Prefer staying on the neighbouring standalone report; otherwise fall
        // back to the last collection tab. Strip slots count collections, then
        // the standalone reports (embedded reports aren't in the strip).
        let base = self.collections.len();
        let remaining = self.standalone_report_indices().len();
        self.active_tab = if remaining == 0 {
            base.saturating_sub(1)
        } else {
            base + slot.min(remaining - 1)
        };
        self.focus = Pane::Tabs;
        self.status = Some(crate::i18n::Status::TabClosed);
        self.save_state();
    }

    /// Key handling for a *standalone* report strip tab (its unified tab index
    /// falls past the collection tabs). Kept separate from `on_key_normal`
    /// (which is full of `collections[active_tab]` accesses that would panic on
    /// a report's unified index) — the normal handler dispatches here at its
    /// very top for a standalone report. Left/Right cycle tabs (the report is
    /// full-screen, so the arrows are free to move across the strip); Tab is
    /// inert (there is no second pane to move focus to).
    pub(crate) fn on_key_report(&mut self, key: KeyEvent) {
        self.handle_report_key(key, false);
    }

    /// Key handling for a workspace report embedded in the right pane of its
    /// collection tab, while that report body holds focus (`Pane::Main`). The
    /// same body keys as [`Self::on_key_report`] minus the tab-cycling arrows:
    /// the single collection-side tree drives tab/pane navigation instead, so
    /// Left/Right are inert here and Tab rotates focus back to the tree (via
    /// `cycle_focus`). The tree-focused case never reaches this handler — it
    /// falls through to the normal collection handler in `on_key_normal`.
    pub(crate) fn on_key_report_body(&mut self, key: KeyEvent) {
        self.handle_report_key(key, true);
    }

    /// Shared report key map for both the standalone strip tab (`embedded ==
    /// false`) and the workspace-embedded right pane (`embedded == true`). The
    /// only difference is the arrow/Tab handling (see the two public wrappers);
    /// every body key — edit, node editor, run, dry-run, columns, bind, view
    /// flip, export, baseline, copy, scroll, resize — is identical.
    fn handle_report_key(&mut self, key: KeyEvent, embedded: bool) {
        // When the source panel has edit focus, keystrokes type into it rather
        // than acting as view shortcuts (Esc leaves edit focus).
        if let Some(idx) = self.active_report_index()
            && self.reports[idx].editor.is_some()
        {
            self.on_key_report_editing(key, idx);
            return;
        }
        // In the structured node editor, most keys drive node navigation/editing
        // (see `on_key_report_nodes`); it returns `true` when it consumed the
        // key, otherwise we fall through to the shared shortcuts below (global
        // menus, tab navigation, run, the `n` toggle, …).
        if let Some(idx) = self.active_report_index()
            && self.reports[idx].view == ReportView::Nodes
            && self.on_key_report_nodes(key, idx)
        {
            return;
        }
        // Same arrangement for the run settings: it owns the cursor keys and
        // Enter, and lets everything else through.
        if let Some(idx) = self.active_report_index()
            && self.reports[idx].view == ReportView::RunSettings
            && self.on_key_report_run_settings(key, idx)
        {
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        // A dry-run preview has no cell cursor, but its grid clips just like
        // the real one — so Left/Right scroll it sideways directly, keeping the
        // preview's off-screen columns reachable.
        if let Some(idx) = self.active_report_index()
            && self.reports[idx].view == ReportView::Results
            && self.reports[idx].dry_run.is_some()
            && !ctrl
            && !shift
        {
            match key.code {
                KeyCode::Left => {
                    self.preview_scroll_cols(-1);
                    return;
                }
                KeyCode::Right => {
                    self.preview_scroll_cols(1);
                    return;
                }
                _ => {}
            }
        }
        // When the results grid is visible and has data, plain (unmodified)
        // arrow keys drive the cell cursor rather than cycling tabs or
        // scrolling the panel. Home/End also jump to the first/last data row.
        // Shift+arrows still extend panel text selections (unchanged), and
        // Ctrl+arrows still cycle tabs for a standalone report.
        if let Some(idx) = self.active_report_index()
            && self.reports[idx].view == ReportView::Results
            && self.reports[idx].result.is_some()
            && !ctrl
            && !shift
        {
            match key.code {
                KeyCode::Up => {
                    self.result_cursor_move(-1, 0);
                    return;
                }
                KeyCode::Down => {
                    self.result_cursor_move(1, 0);
                    return;
                }
                KeyCode::Left => {
                    self.result_cursor_move(0, -1);
                    return;
                }
                KeyCode::Right => {
                    self.result_cursor_move(0, 1);
                    return;
                }
                KeyCode::Home => {
                    self.result_cursor_jump_home();
                    return;
                }
                KeyCode::End => {
                    self.result_cursor_jump_end();
                    return;
                }
                KeyCode::Enter => {
                    self.open_result_cell_popup();
                    return;
                }
                _ => {}
            }
        }
        // Ctrl+Up / Ctrl+Down page the cell cursor a whole screenful at a time
        // in the results grid (Ctrl+Left/Right still cycle tabs for a
        // standalone report, handled below).
        if let Some(idx) = self.active_report_index()
            && self.reports[idx].view == ReportView::Results
            && self.reports[idx].result.is_some()
            && ctrl
            && !shift
        {
            match key.code {
                KeyCode::Up => {
                    self.result_cursor_page(-1);
                    return;
                }
                KeyCode::Down => {
                    self.result_cursor_page(1);
                    return;
                }
                _ => {}
            }
        }
        match key.code {
            KeyCode::Char('q') => self.request_quit(),
            // Tab navigation (mirrors the collection-view bindings). `[`/`]`
            // and PageUp/Down work in both modes (an embedded report can still
            // switch tabs with the brackets).
            KeyCode::Char('[') | KeyCode::PageUp => self.cycle_tab(false),
            KeyCode::Char(']') | KeyCode::PageDown => self.cycle_tab(true),
            KeyCode::Left if ctrl && shift && !embedded => self.move_active_tab(false),
            KeyCode::Right if ctrl && shift && !embedded => self.move_active_tab(true),
            KeyCode::Left if ctrl && !embedded => self.cycle_tab(false),
            KeyCode::Right if ctrl && !embedded => self.cycle_tab(true),
            // Shift+Arrow adjusts the end of a mouse-started panel selection
            // (same as the collection view's body panels).
            KeyCode::Left if shift => self.extend_report_selection(KeyCode::Left),
            KeyCode::Right if shift => self.extend_report_selection(KeyCode::Right),
            KeyCode::Up if shift => self.extend_report_selection(KeyCode::Up),
            KeyCode::Down if shift => self.extend_report_selection(KeyCode::Down),
            // Plain Left/Right also move across tabs *for a standalone report*:
            // it's full-screen (no left/right panes to traverse), so — unlike
            // the collection view — arrows are free to drive tab navigation. An
            // embedded report shares the collection tree's left pane, so its
            // Left/Right must NOT cycle tabs (the tree owns them) — inert here.
            KeyCode::Left | KeyCode::Right if !embedded => {
                if key.code == KeyCode::Left {
                    self.cycle_tab(false);
                } else {
                    self.cycle_tab(true);
                }
            }
            KeyCode::Left | KeyCode::Right => {}
            KeyCode::Char('w') if ctrl => self.close_active_tab(),
            KeyCode::Char('u') => self.reopen_closed_tab(),
            // `/` walks the results filters -- the key filters a list in k9s,
            // lazygit and their neighbours, and nothing in the report view
            // wanted it. The same filters the GUI's bar offers and the
            // interactive HTML export writes buttons for, so "show me only the
            // wrong rows" selects the same rows in all three.
            KeyCode::Char('/') => self.cycle_report_row_filter(),
            // Global menus, unchanged from the collection view.
            // Ctrl+O opens what Ctrl+S wrote, so an exported HTML report is one
            // keystroke from the browser it was written for.
            KeyCode::Char('o') if ctrl => self.open_exported_report(),
            KeyCode::Char('f') => self.overlay = Some(Overlay::FileMenu(0)),
            KeyCode::Char('s') if !ctrl => self.overlay = Some(Overlay::Options(0)),
            KeyCode::Char('?') | KeyCode::F(1) => {
                self.overlay = Some(Overlay::Help(0));
                self.help_scroll = 0;
            }
            // Open another new report.
            KeyCode::Char('R') => self.new_report_tab(),
            // `e` gives the source panel raw-text edit focus. Enter opens the
            // structured node editor instead — mirroring how Enter opens the
            // request wizard — falling back to raw editing on a report that
            // doesn't parse (so Enter always opens *an* editor). Esc backs out
            // of the node editor again. `n` is deliberately left unbound here,
            // reserved for a future "new request" binding.
            KeyCode::Char('e') => self.enter_report_edit(),
            KeyCode::Enter => self.open_report_node_editor(),
            KeyCode::Esc => {
                if let Some(idx) = self.active_report_index() {
                    // A dry-run preview is occupying the results pane: Esc
                    // dismisses it (revealing the last real run's grid again)
                    // before it means anything else.
                    if self.reports[idx].dry_run.is_some() {
                        self.reports[idx].dry_run = None;
                        self.reports[idx].results_panel.set_scroll(0);
                        self.reports[idx].results_col_offset = 0;
                    } else if self.reports[idx].view == ReportView::Nodes {
                        self.reports[idx].view = ReportView::Source;
                        self.reports[idx].editor_view = ReportView::Source;
                    }
                }
            }
            // Run the report against its bound collection and show the grid.
            KeyCode::Char('r') | KeyCode::F(5) => self.run_active_report(),
            // Dry-run: preview the projected rows/bindings without sending HTTP.
            KeyCode::Char('d') => self.open_report_dry_run(),
            // Column picker: choose/reorder which columns the report outputs.
            KeyCode::Char('c') => self.open_report_columns(),
            // Bind: (re)point the report at one of the open collections.
            KeyCode::Char('b') => self.open_report_bind(),
            // The run settings: the values this report's PARAMs will be run
            // with. `p` because it's what the report asks the user for.
            KeyCode::Char('p') => self.open_report_run_settings(),
            // Flip between the source and the last run's results grid.
            KeyCode::Char('v') => self.toggle_report_view(),
            // Reformat: re-indent the source to its real block depth. Shift+F
            // because a bare `f` already opens the File menu.
            KeyCode::Char('F') => self.reformat_active_report(),
            // Tab moves focus. A standalone report has a single (full-screen)
            // body, so Tab is inert. An embedded report shares its tab with the
            // collection tree, so Tab rotates focus back to that tree (and on
            // round to the body / env), via the shared `cycle_focus`.
            KeyCode::Tab if embedded => self.cycle_focus(true),
            KeyCode::BackTab if embedded => self.cycle_focus(false),
            KeyCode::Tab | KeyCode::BackTab => {}
            // Export the last run to CSV next to the report. `Ctrl+S` (rather
            // than a bare `x`) so it can't be confused with — or fat-fingered
            // into — the collection view's `x` = delete-environment / delete-
            // request binding, which felt unsafe sitting one pane away.
            KeyCode::Char('s') if ctrl => self.export_active_report_csv(),
            // Save the last run as a `.baseline` snapshot (Shift+B) — `b` is
            // already BIND. A `# baseline:` directive later diffs runs against it.
            KeyCode::Char('B') => self.save_active_report_baseline(),
            // Copy the active panel selection (or, with nothing selected, the
            // whole visible panel) to the clipboard — parity with the
            // collection view's `y`.
            KeyCode::Char('y') => self.copy_report_selection_to_clipboard(),
            // Scroll the visible panel (source or results grid). Edit focus uses
            // these to move the cursor instead. Overshoot is clamped on draw.
            KeyCode::Up => self.scroll_report(-1),
            KeyCode::Down => self.scroll_report(1),
            KeyCode::Home => self.scroll_report(i32::MIN),
            KeyCode::End => self.scroll_report(i32::MAX),
            // `<`/`>` resize the request-list / results column width, mirroring
            // the collection view's request-list resize (same 20..80 clamp).
            KeyCode::Char('>') => {
                self.list_width = (self.list_width + 2).min(80);
                self.save_state();
            }
            KeyCode::Char('<') => {
                self.list_width = self.list_width.saturating_sub(2).max(20);
                self.save_state();
            }
            _ => {}
        }
    }

    /// Nudge the active report's *visible* panel scroll by `delta` rows
    /// (`i32::MIN`/`MAX` jump to the top/bottom) — the source panel in the
    /// source view, the results grid in the results view. The draw pass clamps
    /// to the real content height, so this doesn't need the viewport size.
    fn scroll_report(&mut self, delta: i32) {
        if let Some(idx) = self.active_report_index() {
            let rt = &mut self.reports[idx];
            let panel = match rt.view {
                ReportView::Results => &mut rt.results_panel,
                ReportView::Source => &mut rt.source_panel,
                // The node outline and the run settings both scroll to follow
                // their selection cursor (moved by Up/Down in their own
                // handlers), not a free scroll offset.
                ReportView::Nodes | ReportView::RunSettings => return,
            };
            let next = (panel.scroll() as i32).saturating_add(delta).max(0);
            panel.set_scroll(next.min(u16::MAX as i32) as u16);
        }
    }

    /// Key handling while the active report's source panel has edit focus.
    /// Esc leaves edit focus (keeping the typed text — edits are applied live);
    /// most keys are delegated to the shared multi-line handler, but two are
    /// intercepted first: Ctrl+Left/Right move a word at a time (rather than to
    /// the line ends), and Right accepts a pending `REQUEST`-name completion.
    /// After each edit the buffer is mirrored into `report.text` and the tab is
    /// revalidated so the validation panel stays live.
    fn on_key_report_editing(&mut self, key: KeyEvent, idx: usize) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        // A pending ghost completion (accepted with a plain Right-arrow or Tab).
        // This is computed *before* the `&mut editor` borrow below, since it
        // needs an immutable borrow of `self` (the bound collection's request
        // names).
        let completion = if matches!(key.code, KeyCode::Right | KeyCode::Tab) && !ctrl && !shift {
            self.report_completion(idx)
        } else {
            None
        };

        let mut leave = false;
        let mut leave_cursor: Option<(usize, usize)> = None;
        let mut copy: Option<String> = None;
        // Apply the keystroke to the editor, then read back the (possibly)
        // updated text; the borrow of `self.reports[idx].editor` ends with this
        // block so the follow-up `revalidate_report`/`save_state` calls (which
        // borrow all of `self`) are free to run.
        let new_text: Option<String> = {
            let Some(rt) = self.reports.get_mut(idx) else {
                return;
            };
            let Some(editor) = rt.editor.as_mut() else {
                return;
            };
            match key.code {
                // Esc leaves edit focus (the live-applied text is kept), first
                // remembering the caret so re-entering edit mode restores it.
                KeyCode::Esc => {
                    leave = true;
                    leave_cursor = Some((editor.row, editor.col));
                    Some(editor.text())
                }
                // Ctrl+Left/Right move one word (not to the line ends), keeping
                // Shift's selection-extend behaviour.
                KeyCode::Left if ctrl => {
                    editor.set_selecting(shift);
                    word_left(editor);
                    None
                }
                KeyCode::Right if ctrl => {
                    editor.set_selecting(shift);
                    word_right(editor);
                    None
                }
                // Right arrow / Tab at end of a `REQUEST` line fills in the
                // completion (auto-quoting the name when it contains spaces).
                KeyCode::Right | KeyCode::Tab if completion.is_some() => {
                    editor.checkpoint();
                    accept_request_completion(editor, completion.as_ref().unwrap());
                    Some(editor.text())
                }
                // Tab with no pending completion indents one level (4 spaces)
                // rather than moving focus — the report body is code-like, so
                // Tab is expected to indent while editing. Snap a bare `END`
                // back afterwards so an over-indent can't leave it detached from
                // its opener (matching the backspace/space behaviour below).
                KeyCode::Tab if !ctrl && !shift => {
                    editor.checkpoint();
                    editor.insert_str(INDENT_UNIT);
                    reindent_end_line(editor);
                    Some(editor.text())
                }
                // Backspace over a run of spaces deletes back to the previous
                // 4-space stop within that run (mirroring the Tab indent), so
                // one press clears a whole level rather than a single space —
                // whether the spaces lead the line or trail earlier content
                // (e.g. the padding Tab leaves after an `END`). Anywhere else
                // it's an ordinary character delete.
                KeyCode::Backspace if !ctrl => {
                    let chars: Vec<char> = editor.lines[editor.row].chars().collect();
                    if editor.col > 0 && chars.get(editor.col - 1) == Some(&' ') {
                        let mut run_start = editor.col;
                        while run_start > 0 && chars[run_start - 1] == ' ' {
                            run_start -= 1;
                        }
                        let within = editor.col - run_start;
                        let remove = (within - 1) % 4 + 1;
                        editor.checkpoint();
                        for _ in 0..remove {
                            editor.backspace();
                        }
                        reindent_end_line(editor);
                        Some(editor.text())
                    } else {
                        let resp = apply_edit_key_full(editor, key);
                        copy = resp.copy;
                        if resp.changed {
                            reindent_end_line(editor);
                            Some(editor.text())
                        } else {
                            None
                        }
                    }
                }
                // Enter keeps the current line's indentation on the new line
                // (and adds one level after a `FOR` that opens a block), so
                // nested regions stay visually aligned. Only auto-indents when
                // the cursor is at the line end (the normal typing flow); a
                // mid-line split stays a plain newline.
                KeyCode::Enter if !ctrl && !shift => {
                    let line = &editor.lines[editor.row];
                    if editor.col == line.chars().count() {
                        let new_indent = indent_for_new_line(line);
                        editor.checkpoint();
                        editor.newline();
                        editor.insert_str(&new_indent);
                        Some(editor.text())
                    } else {
                        let resp = apply_edit_key_full(editor, key);
                        copy = resp.copy;
                        if resp.changed {
                            Some(editor.text())
                        } else {
                            None
                        }
                    }
                }
                // Everything else goes to the shared multi-line key handler,
                // which reports whether the text changed and any selection the
                // host should copy. After the edit, a line that now reads `END`
                // is snapped back to its matching `FOR`'s indent (so finishing a
                // block dedents one level, per the grammar's cosmetic layout).
                _ => {
                    let resp = apply_edit_key_full(editor, key);
                    copy = resp.copy;
                    if resp.changed {
                        reindent_end_line(editor);
                        Some(editor.text())
                    } else {
                        None
                    }
                }
            }
        };

        if let Some(text) = copy {
            crate::tui::clipboard::copy_to_clipboard(&text);
            self.status = Some(crate::i18n::Status::Copied);
        }
        if let Some(text) = new_text {
            if let Some(rt) = self.reports.get_mut(idx) {
                if leave {
                    rt.editor = None;
                    rt.edit_cursor = leave_cursor;
                }
                rt.report.set_text(text);
            }
            self.revalidate_report(idx);
            if leave {
                self.save_state();
            }
        }
    }

    /// The completion to offer while typing a name in the source editor — a
    /// request name on a `REQUEST` / `REPORT REQUEST` line, or an environment
    /// name on a `FOR … ENVS` clause (including inside `BASELINE(…)` /
    /// `COMPARISON(…)`). `None` unless the cursor is at the end of such a line
    /// and a candidate matches. The report view can't show the collection's
    /// request list or the loaded-env list (the flow takes the whole body), so
    /// this keeps both discoverable and correctly spelled.
    ///
    /// PaperTrail requires a spaced request name to be quoted, and *every* env
    /// name to be quoted, so completion is quote-aware and always yields a
    /// parseable line:
    /// - a bare request fragment matching a space-free title completes bare;
    /// - a bare fragment matching a spaced title (or any env name) auto-quotes
    ///   it — the opening quote is inserted before the fragment on accept, so
    ///   typing `Up` completes to `"Upload document"` and `pr` to `"prod"`;
    /// - inside an opened quote, any candidate completes and the closing quote
    ///   is appended.
    pub(crate) fn report_completion(&self, idx: usize) -> Option<RequestCompletion> {
        let rt = self.reports.get(idx)?;
        let editor = rt.editor.as_ref()?;
        let line = editor.lines.get(editor.row)?;
        // Only complete when the cursor sits at the very end of the line.
        if editor.col != line.chars().count() {
            return None;
        }
        // Request names on a REQUEST / REPORT REQUEST line (bare or quoted).
        if let Some(partial) = request_name_partial(line) {
            let entries = self
                .resolve_bound_collection(&rt.report)
                .map(|ci| self.collections[ci].entries.as_slice())
                .unwrap_or(&[]);
            // Qualified names, so a helper completes as `alias/request` — the
            // only form that resolves. Matching stays a plain prefix match on
            // that whole string: the alias has to be typed to reach a helper,
            // which is deliberate, since the ghost can only ever *extend* what
            // has been typed and could not insert an alias in front of it.
            let names = crate::report::context::request_choices(entries, &rt.helpers)
                .into_iter()
                .map(|c| c.qualified);
            return complete_name(&partial, names, false);
        }
        // Environment names on a FOR … ENVS clause (always quoted).
        if let Some(partial) = env_name_partial(line) {
            let envs = self.global_envs.iter().map(|e| e.name.clone());
            return complete_name(&partial, envs, true);
        }
        None
    }
}

/// Build a [`RequestCompletion`] for `partial` against the first candidate that
/// extends it, matched **case-insensitively**. When `always_quote` is set (env
/// names, which must be quoted) or the matched candidate contains whitespace,
/// the completed name is wrapped in quotes. Because the match ignores case, the
/// completion rewrites the whole typed fragment with the candidate's canonical
/// spelling on accept (so typing `r` completes to `Report value`, not
/// `report value`) — see [`accept_request_completion`].
fn complete_name(
    partial: &NamePartial,
    mut candidates: impl Iterator<Item = String>,
    always_quote: bool,
) -> Option<RequestCompletion> {
    match partial {
        NamePartial::Bare { text: p, start } => {
            let pl = p.to_lowercase();
            let pchars = p.chars().count();
            let t = candidates.find(|t| t.chars().count() > pchars && ci_prefix(t, &pl))?;
            let suffix: String = t.chars().skip(pchars).collect();
            let quote = always_quote || t.chars().any(char::is_whitespace);
            Some(RequestCompletion {
                // The ghost shows the plain suffix only (the leading/closing
                // quotes, when auto-quoting, are added on accept), so it stays
                // visually balanced as the line's continuation.
                ghost: suffix,
                replacement: if quote { format!("\"{t}\"") } else { t },
                start: *start,
            })
        }
        NamePartial::Quoted { text: p, start } => {
            let pl = p.to_lowercase();
            let pchars = p.chars().count();
            let t = candidates.find(|t| t.chars().count() >= pchars && ci_prefix(t, &pl))?;
            let suffix: String = t.chars().skip(pchars).collect();
            Some(RequestCompletion {
                ghost: format!("{suffix}\""),
                replacement: format!("\"{t}\""),
                start: *start,
            })
        }
    }
}

/// Whether `candidate` starts with the already-lowercased prefix `lower`,
/// ignoring case.
fn ci_prefix(candidate: &str, lower: &str) -> bool {
    candidate.to_lowercase().starts_with(lower)
}

/// A pending request-name completion in the source editor.
pub(crate) struct RequestCompletion {
    /// The dim text shown starting at the cursor — the candidate's suffix past
    /// what the author has already typed (plus a closing quote when quoting).
    pub(crate) ghost: String,
    /// The character column where the name token begins. On accept the typed
    /// fragment from here to the cursor is replaced by `replacement`, so a
    /// case-insensitive match adopts the candidate's canonical casing.
    start: usize,
    /// The full canonical name to write in place of the typed fragment, already
    /// wrapped in quotes when the name needs quoting.
    replacement: String,
}

/// Apply `comp` to `ed`: replace the typed fragment — from the recorded name
/// start column to the cursor (which sits at the line end when a completion is
/// offered) — with the canonical `replacement`. Replacing (rather than merely
/// appending a suffix) lets a case-insensitive match correct the casing of what
/// was already typed and, when quoting, wrap the whole name in one step.
fn accept_request_completion(ed: &mut Editor, comp: &RequestCompletion) {
    ed.clear_selection();
    let line = &ed.lines[ed.row];
    let start = Editor::byte_idx(line, comp.start);
    let end = Editor::byte_idx(line, ed.col);
    ed.lines[ed.row].replace_range(start..end, &comp.replacement);
    ed.col = comp.start + comp.replacement.chars().count();
}

/// The partially-typed request name on a `REQUEST`/`REPORT REQUEST` line, and
/// whether the author has opened a quote (so a spaced name is being written).
enum NamePartial {
    /// A bare (unquoted) token, along with the character column in the source
    /// line where the name begins (used to auto-quote a spaced completion).
    Bare { text: String, start: usize },
    /// The text after an as-yet-unclosed opening quote (may contain spaces),
    /// with the character column of that opening quote (so accepting can
    /// rewrite the whole quoted name — e.g. to adopt the match's casing).
    Quoted { text: String, start: usize },
}

/// Pretty-print `raw` when its whole trimmed text is a single valid JSON
/// document, so a drilled-into cell holding a JSON body is shown indented (one
/// field per line) instead of a dense single line. Content that isn't valid
/// JSON — or is only partially JSON — is returned unchanged, so plain text,
/// numbers and multi-value cells are never mangled.
fn pretty_print_json_cell(raw: &str) -> String {
    let trimmed = raw.trim();
    // A bare scalar like `42` or `"x"` is technically valid JSON but gains
    // nothing from pretty-printing; only objects/arrays are worth reflowing.
    if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
        return raw.to_string();
    }
    match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(value) => serde_json::to_string_pretty(&value).unwrap_or_else(|_| raw.to_string()),
        Err(_) => raw.to_string(),
    }
}

/// If the editor's current line now reads exactly `END` (ignoring case and
/// surrounding whitespace), snap its indentation to that of the block opener it
/// closes, so finishing a block dedents one level. Idempotent: an `END` already
/// aligned to its opener is left untouched. Does nothing when the block nesting
/// above the cursor is unbalanced. The cursor stays at the line end.
fn reindent_end_line(ed: &mut Editor) {
    let row = ed.row;
    let Some(line) = ed.lines.get(row) else {
        return;
    };
    if !is_end_line(line) {
        return;
    }
    let Some(indent) = matching_opener_indent(&ed.lines[..row]) else {
        return;
    };
    let trimmed = ed.lines[row].trim_start().to_string();
    ed.lines[row] = format!("{indent}{trimmed}");
    ed.col = ed.lines[row].chars().count();
}

/// If `s` (ignoring case) starts with the keyword `kw` followed by whitespace,
/// return the remainder with that leading whitespace trimmed; else `None`.
fn strip_keyword<'a>(s: &'a str, kw: &str) -> Option<&'a str> {
    let head = s.get(..kw.len())?;
    if head.eq_ignore_ascii_case(kw) {
        let rest: &str = &s[kw.len()..];
        if rest.starts_with(char::is_whitespace) {
            return Some(rest.trim_start());
        }
    }
    None
}

/// Extract the partially-typed request name from a source line, if it is a
/// `REQUEST <name>` or `REPORT REQUEST <name>` line whose name is still being
/// typed (cursor-at-end is checked by the caller). Distinguishes a bare token
/// from an opened-quote fragment so completion can stay grammar-valid.
fn request_name_partial(line: &str) -> Option<NamePartial> {
    let t = line.trim_start();
    let after_report = strip_keyword(t, "REPORT").unwrap_or(t);
    let name = strip_keyword(after_report, "REQUEST")?;
    if let Some(inner) = name.strip_prefix('"') {
        // An opened quote: complete inside it until a closing quote is typed.
        if inner.contains('"') {
            return None; // already closed — the name is finished
        }
        // The name (and its opening quote) begin where `name` starts; every
        // byte before it (indentation + keywords) is ASCII, so its byte offset
        // equals its character column.
        let start = name.as_ptr() as usize - line.as_ptr() as usize;
        Some(NamePartial::Quoted {
            text: inner.to_string(),
            start,
        })
    } else if name.is_empty() {
        None
    } else {
        // A bare token — possibly already containing spaces once the user has
        // typed past one (autocomplete keeps working and auto-quotes on
        // accept). `name` is a subslice of `line`, and every byte before it
        // (indentation + keywords) is ASCII, so its byte offset equals its
        // character column.
        let start = name.as_ptr() as usize - line.as_ptr() as usize;
        Some(NamePartial::Bare {
            text: name.to_string(),
            start,
        })
    }
}

/// Byte index just past the first standalone occurrence of the keyword `kw`
/// (matched case-insensitively, bounded by non-word characters) in `line`, or
/// `None`. Used to find where an `ENVS` clause's env-name list begins. Only
/// ASCII letters change case under `to_ascii_uppercase`, so the returned index
/// is a valid byte offset into `line`.
fn keyword_word_end(line: &str, kw: &str) -> Option<usize> {
    let up = line.to_ascii_uppercase();
    let kwu = kw.to_ascii_uppercase();
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let bytes = up.as_bytes();
    let mut from = 0;
    while let Some(rel) = up[from..].find(&kwu) {
        let s = from + rel;
        let e = s + kwu.len();
        let before_ok = s == 0 || !is_word(bytes[s - 1]);
        let after_ok = e == bytes.len() || !is_word(bytes[e]);
        if before_ok && after_ok {
            return Some(e);
        }
        from = e;
    }
    None
}

/// Extract the partially-typed environment name from a source line, if it is a
/// `FOR … ENVS …` clause whose trailing token (after the last `,`/`(`, or after
/// the `ENVS` keyword) is still being typed. Mirrors [`request_name_partial`]:
/// distinguishes a bare token from an opened-quote fragment. The caller checks
/// the cursor is at the line end, so the trailing token runs to the line end.
fn env_name_partial(line: &str) -> Option<NamePartial> {
    let envs_end = keyword_word_end(line, "ENVS")?;
    // The current token starts after the last list separator, or after ENVS.
    let region_start = match line.rfind([',', '(']) {
        Some(pos) if pos + 1 > envs_end => pos + 1,
        _ => envs_end,
    };
    let region = &line[region_start..];
    let lead = region.len() - region.trim_start().len();
    let token_byte = region_start + lead;
    let token = &line[token_byte..];
    let start = line[..token_byte].chars().count();
    if let Some(inner) = token.strip_prefix('"') {
        if inner.contains('"') {
            return None; // this env name is already closed
        }
        Some(NamePartial::Quoted {
            text: inner.to_string(),
            start,
        })
    } else if token.is_empty() {
        None
    } else {
        Some(NamePartial::Bare {
            text: token.to_string(),
            start,
        })
    }
}

// The pure path/env helpers used to assemble a report's run and validation
// contexts live in the front-end-agnostic `report::context` module so the GUI
// shares one implementation; re-exported here under their historical names so
// this file's call sites (and tests) read unchanged.
use crate::report::context::{paths_equal, resolve_ref_path};

/// Compute the `# collection:` reference to store when BINDing a report to a
/// loaded collection, preferring a path **relative to the report's own
/// directory** so a report + collection committed together stay linked when the
/// repo is cloned elsewhere (the design's portability requirement). Falls back
/// to the collection's absolute path when a relative form can't be computed
/// (e.g. the report is an unsaved scratch with no directory yet), and to the
/// collection's display *name* when it has no path at all (a scratch
/// collection) so the existing name-based resolution in
/// [`TuiApp::resolve_bound_collection`] still finds it.
fn collection_ref_for_report(
    report_path: Option<&std::path::Path>,
    collection_path: Option<&std::path::Path>,
    collection_name: &str,
) -> String {
    let Some(cpath) = collection_path else {
        return collection_name.to_string();
    };
    if let Some(dir) = report_path.and_then(|rp| rp.parent())
        && let Some(rel) = relative_path(dir, cpath)
    {
        return rel.to_string_lossy().into_owned();
    }
    cpath.to_string_lossy().into_owned()
}

/// Build a path to `to` relative to the directory `from_dir`, using `..`
/// segments to climb to a common ancestor. Both are canonicalised first (so
/// symlinks/`.`/`..` resolve consistently), which requires them to exist on
/// disk; returns `None` when either can't be canonicalised or they share no
/// common root (e.g. different drives), so the caller falls back to an absolute
/// path.
fn relative_path(from_dir: &std::path::Path, to: &std::path::Path) -> Option<std::path::PathBuf> {
    let from = from_dir.canonicalize().ok()?;
    let to = to.canonicalize().ok()?;
    let from_comps: Vec<_> = from.components().collect();
    let to_comps: Vec<_> = to.components().collect();

    // A shared root component (the `/` or drive prefix) is required.
    if from_comps.first() != to_comps.first() {
        return None;
    }
    let common = from_comps
        .iter()
        .zip(&to_comps)
        .take_while(|(a, b)| a == b)
        .count();

    let mut rel = std::path::PathBuf::new();
    for _ in common..from_comps.len() {
        rel.push("..");
    }
    for comp in &to_comps[common..] {
        rel.push(comp.as_os_str());
    }
    if rel.as_os_str().is_empty() {
        None
    } else {
        Some(rel)
    }
}

use crate::report::dry_run::DryRunReport;

impl DryRunReport {
    /// Render the preview body as themed lines for the results pane, split
    /// into `(head, grid, tail)`.
    ///
    /// The split exists because the two parts want opposite treatment: the
    /// prose is a paragraph and must wrap or a long producer error is silently
    /// cut off, while the grid is a grid and must *clip* — wrapping it folds
    /// every row over several lines and destroys the column alignment, which
    /// is why the preview used to look nothing like the real results view.
    ///
    /// Layout:
    /// 1. Preview-notice label (marks this as a dry run, not a real result).
    /// 2. Projected row count.
    /// 3. The output grid (same format as the Results view) — loop-resolved
    ///    variables and structure are visible; HTTP intrinsics are blank.
    /// 4. Variable-availability warnings (if any) — yellow `!` prefix.
    /// 5. Producer/expansion errors (if any) — red `•` prefix.
    /// 6. "No problems found." when both 4 and 5 are empty.
    ///
    /// `col_offset` scrolls the grid sideways, exactly as in the Results view.
    pub(crate) fn line_sections(
        &self,
        s: &Strings,
        th: &Theme,
        col_offset: usize,
    ) -> (Vec<Line<'static>>, Vec<Line<'static>>, Vec<Line<'static>>) {
        let mut head: Vec<Line<'static>> = Vec::new();

        // Dry-run notice: distinguish the preview grid from a real-run result.
        head.push(Line::from(Span::styled(
            s.report_dry_run_preview_notice.to_string(),
            Style::default().fg(th.dim),
        )));
        head.push(Line::from(""));

        // Row count.
        head.push(Line::from(Span::styled(
            format!("{} {}", s.report_dry_run_rows, self.rows),
            Style::default().fg(th.accent).add_modifier(Modifier::BOLD),
        )));
        head.push(Line::from(""));

        // Output grid — identical path to the Results view.
        let mut grid: Vec<Line<'static>> = Vec::new();
        if self.rows == 0 {
            head.push(Line::from(Span::styled(
                s.report_dry_run_no_rows.to_string(),
                Style::default().fg(th.dim),
            )));
        } else {
            // Pass `None` for states (no streaming progress in a dry run) so
            // the grid renders without status icons, exactly like a finished run.
            // A projection has nothing to filter by (no run, no verdicts), so
            // every row of it is shown.
            let visible: Vec<usize> = (0..self.result.rows.len()).collect();
            grid.extend(report_grid_lines(
                &self.result,
                &self.header,
                None,
                th,
                None,
                col_offset,
                &visible,
            ));
        }

        let mut lines: Vec<Line<'static>> = Vec::new();
        lines.push(Line::from(""));

        // Warnings and errors sections.
        let has_warnings = !self.var_warnings.is_empty();
        let has_errors = !self.errors.is_empty();

        if !has_warnings && !has_errors {
            lines.push(Line::from(Span::styled(
                s.report_dry_run_no_problems.to_string(),
                Style::default().fg(th.accent),
            )));
        } else {
            if has_warnings {
                lines.push(Line::from(Span::styled(
                    s.report_dry_run_warnings_heading.to_string(),
                    Style::default().fg(th.pending).add_modifier(Modifier::BOLD),
                )));
                for w in &self.var_warnings {
                    lines.push(Line::from(vec![
                        Span::styled("! ", Style::default().fg(th.pending)),
                        Span::styled(w.clone(), Style::default().fg(th.text)),
                    ]));
                }
                if has_errors {
                    lines.push(Line::from(""));
                }
            }
            if has_errors {
                lines.push(Line::from(Span::styled(
                    s.report_dry_run_problems_heading.to_string(),
                    Style::default().fg(th.err).add_modifier(Modifier::BOLD),
                )));
                for err in &self.errors {
                    lines.push(Line::from(Span::styled(
                        format!("• {err}"),
                        Style::default().fg(th.err),
                    )));
                }
            }
        }

        (head, grid, lines)
    }
}

/// One candidate output column in the column-picker overlay: a coalesced set of
/// `sources` (usually one) with a display `header` and whether it is currently
/// `included`. The picker's row order is the output column order it writes back
/// to the flow's `# columns:` directive.
pub(crate) struct ColumnChoice {
    pub(crate) header: String,
    pub(crate) sources: Vec<String>,
    pub(crate) included: bool,
}

impl ColumnChoice {
    /// The `# columns:` fragment for this column: `a|b`, plus ` AS <header>`
    /// when the header differs from the (default) first source. The header is
    /// quoted when it isn't a bare source token (spaces/commas/`|`).
    fn to_spec(&self) -> String {
        let src = self.sources.join("|");
        if self.header == self.sources.first().map(String::as_str).unwrap_or("") {
            src
        } else {
            format!("{src} AS {}", quote_header(&self.header))
        }
    }
}

/// Quote a `columns:` header only when it needs it (contains whitespace, comma,
/// pipe or a quote) — otherwise emit it bare so simple renames stay readable.
fn quote_header(h: &str) -> String {
    if h.is_empty()
        || h.chars()
            .any(|c| c.is_whitespace() || matches!(c, ',' | '|' | '"'))
    {
        let escaped = h.replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{escaped}\"")
    } else {
        h.to_string()
    }
}

/// The column-picker overlay ([`Overlay::ReportColumns`]): an interactive
/// checklist of every column a run produced (plus the flow's raw loop/assign
/// variables), whose include/exclude/reorder state is written back to the
/// report's `# columns:` header directive. Backed by a run's [`ReportResult`]
/// so "available columns" is exactly what the last run emitted.
pub(crate) struct ColumnPicker {
    pub(crate) rows: Vec<ColumnChoice>,
    pub(crate) selected: usize,
}

/// One choice in the collection-binding picker: an open collection tab's
/// display `name` and its local file `path` (if it has one — a scratch
/// collection has none and is bound by name).
pub(crate) struct BindOption {
    pub(crate) name: String,
    pub(crate) path: Option<std::path::PathBuf>,
}

/// The collection-binding picker overlay ([`Overlay::ReportBind`]): a list of
/// the currently-open collection tabs, one of which the user selects to become
/// the active report's bound collection (its `# collection:` header).
pub(crate) struct ReportBindPicker {
    pub(crate) options: Vec<BindOption>,
    pub(crate) selected: usize,
}

impl ColumnPicker {
    /// Build the picker from the flow header's current `columns:` directive (if
    /// any) and the columns a run produced. Existing directive columns come
    /// first (included, in their authored order); every other available source
    /// is appended unchecked so it can be added with a keystroke.
    pub(crate) fn build(header: &Header, result: &ReportResult) -> Self {
        // The available source keys: produced cells (first-seen order, includes
        // `Result`), then the special `TARGET` if any row carried one.
        let mut available: Vec<String> = result.column_order.clone();
        if result.rows.iter().any(|r| r.target.is_some())
            && !available.iter().any(|k| k == TARGET_COLUMN)
        {
            available.push(TARGET_COLUMN.to_string());
        }

        let mut rows: Vec<ColumnChoice> = Vec::new();
        let mut covered: std::collections::HashSet<String> = std::collections::HashSet::new();

        match header.columns() {
            Some(spec) => {
                // Seed from the directive: each spec is an included column.
                for col in parse_columns(spec) {
                    for s in &col.sources {
                        covered.insert(s.clone());
                    }
                    rows.push(ColumnChoice {
                        header: col.header,
                        sources: col.sources,
                        included: true,
                    });
                }
            }
            None => {
                // No directive: the default output is every produced column, so
                // seed those as included (raw vars remain available-to-add).
                for key in &available {
                    covered.insert(key.clone());
                    rows.push(ColumnChoice {
                        header: key.clone(),
                        sources: vec![key.clone()],
                        included: true,
                    });
                }
            }
        }

        // Append any remaining available source not already covered, unchecked.
        for key in available {
            if !covered.contains(&key) {
                covered.insert(key.clone());
                rows.push(ColumnChoice {
                    header: key.clone(),
                    sources: vec![key.clone()],
                    included: false,
                });
            }
        }

        ColumnPicker { rows, selected: 0 }
    }

    /// The `# columns:` directive value for the current included rows in order,
    /// or `None` when nothing is included (the caller then leaves the report's
    /// output at its default).
    fn spec(&self) -> Option<String> {
        let parts: Vec<String> = self
            .rows
            .iter()
            .filter(|c| c.included)
            .map(ColumnChoice::to_spec)
            .collect();
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(", "))
        }
    }
}

/// Insert or replace the `# columns:` header directive in raw flow `text`,
/// preserving the body verbatim (a surgical edit rather than a full
/// re-serialize, so the user's own formatting/comments survive). An existing
/// `# columns:` line is rewritten in place; otherwise the directive is appended
/// to the leading comment block (or the top of the file).
fn set_flow_columns_directive(text: &str, spec: &str) -> String {
    set_flow_directive(text, "columns", spec)
}

/// Set (or insert) a `# <key>: <value>` header directive in a report's source,
/// as a surgical text edit that preserves the rest of the flow verbatim. An
/// existing directive with the same key (case-insensitive) is rewritten in
/// place; otherwise a new line is inserted right after the contiguous leading
/// `#` comment block (so it stays in the header). Used by the column picker
/// (`columns:`) and BIND (`collection:`).
fn set_flow_directive(text: &str, key: &str, value: &str) -> String {
    let new_line = format!("# {key}: {value}");
    let prefix = format!("{key}:");
    let trailing_nl = text.ends_with('\n');
    let mut lines: Vec<String> = text.lines().map(String::from).collect();

    // Rewrite an existing `# key:` directive if present.
    for line in &mut lines {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix('#') {
            let rest = rest.trim_start();
            if rest
                .as_bytes()
                .get(..prefix.len())
                .is_some_and(|b| b.eq_ignore_ascii_case(prefix.as_bytes()))
            {
                *line = new_line.clone();
                return rejoin(lines, trailing_nl);
            }
        }
    }

    // No directive yet: insert after the contiguous leading `#` comment block.
    let mut insert_at = 0;
    for line in &lines {
        if line.trim_start().starts_with('#') {
            insert_at += 1;
        } else {
            break;
        }
    }
    lines.insert(insert_at, new_line);
    rejoin(lines, trailing_nl)
}

fn rejoin(lines: Vec<String>, trailing_nl: bool) -> String {
    let mut out = lines.join("\n");
    if trailing_nl {
        out.push('\n');
    }
    out
}

/// Draw a standalone report tab's full-screen body: the binding status, the
/// flow source (syntax-highlighted, editable in place), and the live
/// validation panel. A standalone report owns the whole area and always holds
/// body focus. The embedded-in-a-workspace path calls [`draw_report_content`]
/// directly for the right pane instead (the workspace file-tree stays on the
/// left).
pub(crate) fn draw_report_body(
    f: &mut Frame,
    area: Rect,
    app: &mut TuiApp,
    s: &Strings,
    th: &Theme,
) {
    let Some(idx) = app.active_report_index() else {
        return;
    };
    draw_report_content(f, area, app, idx, s, th);
}

/// Draw the report *content* — everything to the right of any workspace tree:
/// the binding row plus either the results grid or the source/nodes editor with
/// the validation panel below. Shared by the standalone full-screen report
/// ([`draw_report_body`]) and the workspace-embedded right pane (see
/// [`super::draw::draw_body`]). Whether the body border is lit follows
/// [`TuiApp::report_body_focused`] (always true for a standalone report; for an
/// embedded one, only while the report body — not the tree — holds focus).
pub(crate) fn draw_report_content(
    f: &mut Frame,
    area: Rect,
    app: &mut TuiApp,
    idx: usize,
    s: &Strings,
    th: &Theme,
) {
    // Reset the mouse hit-test areas each frame; the specific panel draws below
    // record the ones actually shown (a panel not drawn this frame stays
    // `Rect::default()`, so it can never be hit).
    app.report_pane_areas = [Rect::default(); 3];
    app.report_pane_bars = [Rect::default(); 3];

    // The results grid is shown full-height (no binding/validation at the top)
    // when the user has flipped to it; otherwise the source + binding +
    // validation stack (binding and validation moved to the bottom for stable
    // layout when scrolling past different reports in a workspace).
    if app.reports[idx].view == ReportView::Results {
        let rows = Layout::vertical([Constraint::Min(3), Constraint::Length(4)]).split(area);
        draw_report_results(f, rows[0], app, idx, s, th);
        draw_report_binding(f, rows[1], app, idx, s, th);
        return;
    }

    // The validation panel is sized to the text it actually draws — wrapped, so
    // a single long parse error gets the rows it needs — and capped so it can
    // never take over the pane. It scrolls, so anything past the cap is still
    // reachable.
    let diag_h = validation_height(&app.reports[idx], s, th, area);

    let rows = Layout::vertical([
        Constraint::Min(3),
        Constraint::Length(4),
        Constraint::Length(diag_h),
    ])
    .split(area);

    if app.reports[idx].view == ReportView::RunSettings {
        draw_report_run_settings(f, rows[0], app, idx, s, th);
    } else if app.reports[idx].view == ReportView::Nodes {
        super::report_nodes::draw_report_nodes(f, rows[0], app, idx, s, th);
    } else {
        draw_report_source(f, rows[0], app, idx, s, th);
    }
    draw_report_binding(f, rows[1], app, idx, s, th);
    draw_report_validation(f, rows[2], app, idx, s, th);
}

/// Draw the run settings: one row per declared `PARAM` — what it asks for, the
/// value this run will use, and what is wrong with it, if anything. The raw
/// name and the declared type are shown dimmed beside the prompt: the prompt is
/// what the row means, but the name is what `{{…}}` and `--param` say, so
/// neither can be the only one on screen.
fn draw_report_run_settings(
    f: &mut Frame,
    area: Rect,
    app: &mut TuiApp,
    idx: usize,
    s: &Strings,
    th: &Theme,
) {
    let focused = app.report_body_focused();
    let title = format!("{} — {}", s.param_view_title, s.param_view_hint);
    let block = panel(title, focused, th);
    let inner = block.inner(area);
    f.render_widget(block, area);
    // Not a text panel: nothing here is selectable or scrollable with the
    // mouse, so it claims no hit-test area (as the node outline doesn't). Its
    // *rows* are clickable, though — see the hit targets pushed below.
    app.report_pane_areas[ReportPane::Source.idx()] = Rect::default();
    app.report_pane_bars[ReportPane::Source.idx()] = Rect::default();
    app.push_mouse_hit(
        MouseLayer::Base,
        inner,
        MouseHitTarget::FocusPane(Pane::Main),
    );
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let rows = app.report_param_rows(idx);
    if rows.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                s.param_view_empty.to_string(),
                Style::default().fg(th.dim),
            ))),
            inner,
        );
        return;
    }
    let sel = app.reports[idx]
        .param_selected
        .min(rows.len().saturating_sub(1));
    app.reports[idx].param_selected = sel;

    let prompt_w = rows
        .iter()
        .map(|r| r.prompt.chars().count())
        .max()
        .unwrap_or(0);
    let lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let selected = i == sel;
            let mark = if selected { "> " } else { "  " };
            let mut spans = vec![
                Span::styled(mark, Style::default().fg(th.accent)),
                Span::styled(
                    format!("{:<prompt_w$}  ", row.prompt),
                    if selected {
                        Style::default().fg(th.text).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(th.text)
                    },
                ),
            ];
            if row.value.is_empty() {
                spans.push(Span::styled(
                    s.param_value_unset.to_string(),
                    Style::default().fg(th.dim),
                ));
            } else {
                spans.push(Span::styled(row.value.clone(), Style::default().fg(th.ok)));
            }
            spans.push(Span::styled(
                format!("  {} {}", row.name, row.kind.keyword()),
                Style::default().fg(th.dim),
            ));
            if let Some(problem) = &row.problem {
                spans.push(Span::styled(
                    format!("  {problem}"),
                    Style::default().fg(th.err),
                ));
            }
            Line::from(spans)
        })
        .collect();

    // One cursor, one list: scroll just enough to keep the selected row on
    // screen, the same way the node outline follows its own selection.
    let h = inner.height as usize;
    let first = sel.saturating_sub(h.saturating_sub(1));
    let visible: Vec<Line> = lines.into_iter().skip(first).take(h).collect();
    let shown = visible.len();
    f.render_widget(Paragraph::new(visible), inner);
    // One hit target per drawn row: a click selects it, a second click on the
    // same row opens its editor — the same one-click/two-click gesture the node
    // outline and the settings rows above it use.
    for i in 0..shown {
        app.push_mouse_hit(
            MouseLayer::Base,
            Rect::new(inner.x, inner.y + i as u16, inner.width, 1),
            MouseHitTarget::ReportParamRow(first + i),
        );
    }
}

/// row. Any run-level errors are surfaced in the panel title so a partly-failed
/// run isn't silently presented as clean.
fn draw_report_results(
    f: &mut Frame,
    area: Rect,
    app: &mut TuiApp,
    idx: usize,
    s: &Strings,
    th: &Theme,
) {
    let inner_h = area.height.saturating_sub(2) as usize;

    // A dry-run preview takes over the pane until it is dismissed, so the
    // projection and the real thing are read in the same place. It renders as
    // plain lines (notice, row count, grid, problems), so there is no sticky
    // header and no cell cursor over it.
    // The prose around the grid — a long producer error or binding line —
    // would be silently cut off by the clipping panel, so it is pre-wrapped to
    // the pane width with an explicit `↵` marker. The *grid* is deliberately
    // left alone: wrapping it folds each row over several lines and destroys
    // the column alignment, so it clips and scrolls sideways exactly as the
    // real results grid does.
    let preview_lines = app.reports[idx].dry_run.as_ref().map(|preview| {
        let col_offset = app.reports[idx].results_col_offset;
        let (head, grid, tail) = preview.line_sections(s, th, col_offset);
        let width = area.width.saturating_sub(2);
        let mut lines = crate::tui::draw::wrap_lines_with_marker(head, width, th);
        lines.extend(grid);
        lines.extend(crate::tui::draw::wrap_lines_with_marker(tail, width, th));
        lines
    });
    if let Some(lines) = preview_lines {
        let title = format!("{} — {}", s.report_dry_run_title, s.report_dry_run_hint);
        let focused = app.report_body_focused();
        let block = panel(title, focused, th);
        let (inner, bar) = draw_report_panel(
            f,
            area,
            block,
            &mut app.reports[idx].results_panel,
            &lines,
            th,
        );
        app.report_pane_areas[ReportPane::Results.idx()] = inner;
        app.report_pane_bars[ReportPane::Results.idx()] = bar;
        return;
    }

    // Slide the grid sideways so the cursor's column is on screen. The grid
    // clips rather than wraps, so without this the columns past the right edge
    // are unreachable; recomputed every frame because it is cheap and, unlike
    // the vertical scroll, nothing else moves it.
    if let Some(result) = &app.reports[idx].result {
        let rt = &app.reports[idx];
        let header = rt.report.flow().map(|flow| flow.header).unwrap_or_default();
        let widths = result_column_widths(result, &header, &rt.visible_result_rows());
        let show_icons = rt.run_progress.is_some();
        let avail =
            (area.width.saturating_sub(2) as usize).saturating_sub(if show_icons { 2 } else { 0 });
        let cursor_col = rt.cell_cursor.map(|(_, c)| c).unwrap_or(0);
        app.reports[idx].results_col_offset =
            follow_col_offset(&widths, cursor_col, avail, rt.results_col_offset);
    }

    let (lines, head, title) = {
        let rt = &app.reports[idx];
        match &rt.result {
            None => (
                vec![Line::from(Span::styled(
                    s.report_results_empty.to_string(),
                    Style::default().fg(th.dim),
                ))],
                Vec::new(),
                s.report_results_heading.to_string(),
            ),
            Some(result) => {
                let header = rt.report.flow().map(|flow| flow.header).unwrap_or_default();
                let visible = rt.visible_result_rows();
                // While a run streams, each row carries a live `RowState`: the
                // grid greys unfinished rows and shows a status icon per row so
                // it doubles as a live progress indicator.
                let states = rt.run_progress.as_ref().map(|p| p.states.as_slice());
                let lines = report_grid_lines(
                    result,
                    &header,
                    states,
                    th,
                    rt.cell_cursor,
                    rt.results_col_offset,
                    &visible,
                );
                let count = if result.errors.is_empty() {
                    format!("{}", result.rows.len())
                } else {
                    format!(
                        "{}, {} {}",
                        result.rows.len(),
                        result.errors.len(),
                        s.report_status_errors
                    )
                };
                // The filter belongs in the panel's title, not in a line of
                // its own inside the grid: it describes the pane rather than
                // the run, and a row of prose between the summary and the
                // table costs a row of the table on every screen.
                let filter = filter_title(rt, result, visible.len(), s)
                    .map(|f| format!(" — {f}"))
                    .unwrap_or_default();
                let title = format!(
                    "{} ({}){} — {}",
                    s.report_results_heading, count, filter, s.report_hint_results
                );
                let head = results_head_lines(result, &header, s, th);
                (lines, head, title)
            }
        }
    };

    // When there's a real result the grid's header row (grid line 0) is pinned
    // at the top of the pane and only the data rows below it scroll. This keeps
    // the column titles visible while scrolling a long report. `lines[0]` is the
    // header; `lines[1..]` are the data rows fed to the scrolling body panel.
    // The metric/filter summary is pinned above the header for the same reason
    // the header is pinned: it describes the rows being scrolled past, so it
    // has to stay on screen while they move. It is dropped entirely when the
    // pane is too short to show it *and* a few rows -- a summary that leaves no
    // room for the table it summarises is worse than no summary.
    let head: Vec<Line<'static>> = if inner_h >= head.len() + 3 {
        head
    } else {
        Vec::new()
    };
    let sticky = app.reports[idx].result.is_some() && lines.len() > 1 && inner_h >= 2;
    // How many lines are pinned in total: the summary, plus the grid's header
    // row when it is being pinned.
    let pinned = head.len() + usize::from(sticky);

    // Auto-scroll the results panel to keep the cell cursor visible — but only
    // when the cursor *moved* since we last scrolled to it (i.e. keyboard
    // navigation). A mouse-wheel scroll moves the panel directly without
    // touching `cell_cursor`, so leaving the cursor put here means the wheel is
    // no longer fought by an unconditional re-centre every frame.
    let cursor = app.reports[idx].cell_cursor;
    if cursor != app.reports[idx].results_scrolled_to
        && let Some((cursor_row, _)) = cursor
    {
        let scroll = app.reports[idx].results_panel.scroll() as usize;
        if sticky {
            // With the sticky header the panel scroll is a DATA-ROW offset (0 ==
            // first data row shown just under the fixed header). Keep the cursor
            // row inside the body window left under everything pinned.
            let body_h = inner_h.saturating_sub(pinned);
            if body_h > 0 {
                if cursor_row < scroll {
                    app.reports[idx].results_panel.set_scroll(cursor_row as u16);
                } else if cursor_row >= scroll + body_h {
                    let new_scroll = (cursor_row + 1).saturating_sub(body_h) as u16;
                    app.reports[idx].results_panel.set_scroll(new_scroll);
                }
                app.reports[idx].results_scrolled_to = cursor;
            }
        } else if inner_h > 0 {
            // Non-sticky fallback (no result / too short to pin a header): the
            // panel scroll is a grid-line offset, so grid line 0 is the header
            // and data row `cursor_row` maps to grid line `cursor_row + 1`.
            let grid_line = cursor_row + 1;
            if grid_line < scroll {
                app.reports[idx].results_panel.set_scroll(grid_line as u16);
            } else if grid_line >= scroll + inner_h {
                let new_scroll = (grid_line + 1).saturating_sub(inner_h) as u16;
                app.reports[idx].results_panel.set_scroll(new_scroll);
            }
            app.reports[idx].results_scrolled_to = cursor;
        }
    }

    // Dim the grid's border unless the report body actually holds focus (for an
    // embedded report the workspace tree can hold it instead), so the lit
    // border always marks the pane that has focus.
    let focused = app.report_body_focused();
    let block = panel(title, focused, th);
    let (inner, bar) = if sticky {
        let inner = block.inner(area);
        f.render_widget(block, area);
        // Pin the summary lines and then the header row at the top of the inner
        // rect.
        let mut pinned_lines = head.clone();
        pinned_lines.push(lines[0].clone());
        let header_area = Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: pinned as u16,
        };
        f.render_widget(
            Paragraph::new(pinned_lines.clone()).style(Style::default().fg(th.text)),
            header_area,
        );
        // The pinned rows clip exactly like the scrolling ones below them, so
        // they carry the same marker — a header row that stops mid-column
        // otherwise reads as the last column there is.
        mark_clipped_rows(f, header_area, &pinned_lines, 0, th);
        // Scroll only the data rows in the area below everything pinned.
        let body_area = Rect {
            x: inner.x,
            y: inner.y + pinned as u16,
            width: inner.width,
            height: inner.height - pinned as u16,
        };
        let bar = render_panel_lines(
            f,
            body_area,
            area.x + area.width - 1,
            &mut app.reports[idx].results_panel,
            &lines[1..],
            th,
        );
        (inner, bar)
    } else {
        draw_report_panel(
            f,
            area,
            block,
            &mut app.reports[idx].results_panel,
            &lines,
            th,
        )
    };
    app.report_pane_areas[ReportPane::Results.idx()] = inner;
    app.report_pane_bars[ReportPane::Results.idx()] = bar;
    app.push_mouse_hit(
        MouseLayer::Base,
        bar,
        MouseHitTarget::Scroll(MouseScrollTarget::ReportPane(ReportPane::Results)),
    );
    app.push_mouse_hit(
        MouseLayer::Base,
        inner,
        MouseHitTarget::FocusPane(Pane::Main),
    );
    app.push_mouse_hit(MouseLayer::Base, inner, MouseHitTarget::ReportResultsCell);
}

/// The lines pinned above the results grid: one metric line per ground-truthed
/// column, then the filter line when the run offers a filter.
///
/// The GUI says these things in cards and a button bar; a terminal has a line
/// each, but the figures and the filter set come from the same two shared
/// modules ([`crate::report::metrics`], [`crate::report::filter`]) so the two
/// front-ends and the HTML export cannot quote different accuracies or offer
/// different filters.
///
/// Empty for a report with no ground truth and nothing to filter — which is
/// most reports, and they must not pay a line for a summary of nothing.
fn results_head_lines(
    result: &ReportResult,
    header: &crate::report::flow::Header,
    s: &Strings,
    th: &Theme,
) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let columns = result.resolved_columns(header);
    if let Some(metrics) = result.metrics(&columns, header) {
        let label = Style::default().fg(th.dim);
        let value = Style::default().fg(th.text).add_modifier(Modifier::BOLD);
        // How the run moved comes first when there is a baseline to have moved
        // from: the accuracy lines below cannot tell a run that fixed three
        // rows and broke three others from one that touched nothing.
        if let Some(mv) = &metrics.movement {
            let mut spans = vec![Span::styled(
                format!("{} ", s.report_metric_movement),
                value,
            )];
            if mv.is_still() {
                spans.push(Span::styled(
                    s.report_metric_nothing_moved.to_string(),
                    Style::default().fg(th.dim),
                ));
            } else {
                spans.push(Span::styled(format!("{} ", s.report_metric_fixed), label));
                spans.push(Span::styled(
                    format!("{}  ", mv.fixed),
                    Style::default().fg(th.ok).add_modifier(Modifier::BOLD),
                ));
                spans.push(Span::styled(
                    format!("{} ", s.report_metric_regressed),
                    label,
                ));
                spans.push(Span::styled(
                    format!("{}  ", mv.regressed),
                    // A regression is the one figure here anybody is scanning
                    // for, so it is the one that stays plain when it is zero:
                    // red on a `0` teaches the eye to ignore the colour.
                    if mv.regressed > 0 {
                        Style::default().fg(th.err).add_modifier(Modifier::BOLD)
                    } else {
                        value
                    },
                ));
            }
            if mv.still_wrong > 0 {
                spans.push(Span::styled(
                    format!("{} ", s.report_metric_still_wrong),
                    label,
                ));
                spans.push(Span::styled(format!("{}", mv.still_wrong), value));
            }
            out.push(Line::from(spans));
        }
        // The roll-up first when there is one: it answers "did this run pass?",
        // and the per-column breakdown is the follow-up question.
        for m in metrics.overall.iter().chain(metrics.columns.iter()) {
            out.push(Line::from(vec![
                Span::styled(format!("{} ", m.header), value),
                Span::styled(format!("{} ", s.report_metric_compared), label),
                Span::styled(format!("{}/{}  ", m.compared, m.total), value),
                Span::styled(format!("{} ", s.report_metric_incorrect), label),
                Span::styled(format!("{}  ", m.incorrect), value),
                Span::styled(format!("{} ", s.report_metric_accuracy), label),
                Span::styled(
                    m.accuracy_text().unwrap_or_else(|| "\u{2014}".to_string()),
                    // A run with nothing scored is neither good nor bad news,
                    // so it stays plain rather than borrowing either colour.
                    match m.accuracy() {
                        Some(a) if a >= 1.0 => Style::default().fg(th.ok),
                        Some(_) => Style::default().fg(th.pending),
                        None => value,
                    },
                ),
            ]));
        }
    }
    out
}

/// How the results panel's title says which rows it is showing — `None` for a
/// run that offers no filter worth naming (nothing to compare, nothing wrong),
/// where a title saying "All rows" would be noise.
///
/// The row counts are always given, even under `All`: a grid showing a subset
/// of its run must never be mistakable for the whole of it, and the only place
/// left to say so is here.
fn filter_title(
    rt: &ReportTab,
    result: &ReportResult,
    visible: usize,
    s: &Strings,
) -> Option<String> {
    if rt.result_filters().len() < 2 {
        return None;
    }
    let rows = s
        .report_rows_shown
        .replace("{shown}", &visible.to_string())
        .replace("{total}", &result.rows.len().to_string());
    Some(
        s.report_filter_title
            .replace("{f}", &filter_label(s, &rt.results_filter))
            .replace("{r}", &rows),
    )
}

/// [`results_head_lines`] as plain strings — the summary the results view
/// pins, without its styling. Test-only: it is what a test can assert on,
/// since a `Line`'s spans carry the text in pieces.
#[cfg(test)]
pub(crate) fn results_head_text(rt: &ReportTab, s: &Strings) -> Vec<String> {
    let Some(result) = &rt.result else {
        return Vec::new();
    };
    let header = rt.report.flow().map(|f| f.header).unwrap_or_default();
    let th = crate::tui::theme::theme(&crate::i18n::Language::English);
    results_head_lines(result, &header, s, &th)
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|sp| sp.content.as_ref())
                .collect::<String>()
        })
        .collect()
}

/// [`filter_title`] for a tab, as the results panel's title bar would show it.
/// Test-only: the title is assembled inside the draw pass, which a test can't
/// call, but what it says about the filter is exactly what wants asserting.
#[cfg(test)]
pub(crate) fn results_title_filter(rt: &ReportTab, s: &Strings) -> Option<String> {
    let result = rt.result.as_ref()?;
    filter_title(rt, result, rt.visible_result_rows().len(), s)
}

/// A filter's name in the user's language.
///
/// [`crate::report::filter::RowFilter::label`] is deliberately English — it
/// names buttons in an exported document read by whoever it is sent to — so the
/// in-app views translate the fixed filters themselves, exactly as the GUI's
/// filter bar does.
fn filter_label(s: &Strings, f: &crate::report::filter::RowFilter) -> String {
    use crate::report::filter::RowFilter;
    match f {
        RowFilter::All => s.report_filter_all.to_string(),
        RowFilter::Differ => s.report_filter_differences.to_string(),
        RowFilter::Incorrect => s.report_filter_incorrect.to_string(),
        RowFilter::Regressed => s.report_filter_regressions.to_string(),
        RowFilter::MatrixCell { .. } => f.label(),
    }
}

/// Build the grid's styled lines: a bold header row of the resolved column
/// headers followed by one line per data row, each cell padded to its column's
/// width (capped) so the columns line up under [`WrapMode::Clip`]. Newlines in
/// a cell (e.g. a multi-line response body) are collapsed to a marker so a row
/// stays on one grid line. When `states` is `Some` (a run is streaming), each
/// row gets a leading status icon (scheduled/running/finished) and the
/// still-pending rows are drawn dimmed — so the grid doubles as a live progress
/// indicator. When `None` (a completed or static result) no icon column is
/// drawn and every row uses the normal text colour. `cursor` highlights the
/// selected cell using the theme's selection colours so the active cell reads
/// as distinct from its row's text style. `col_offset` is the index of the
/// leftmost column to draw, so columns that don't fit can be scrolled into
/// view; the status-icon prefix stays pinned to the left edge regardless.
fn report_grid_lines(
    result: &ReportResult,
    header: &crate::report::flow::Header,
    states: Option<&[RowState]>,
    th: &Theme,
    cursor: Option<(usize, usize)>, // (visible_row, col) 0-indexed
    col_offset: usize,
    visible: &[usize],
) -> Vec<Line<'static>> {
    let columns = result.resolved_columns(header);
    if columns.is_empty() {
        return vec![Line::from(Span::styled(
            String::new(),
            Style::default().fg(th.dim),
        ))];
    }

    // Materialise every cell so column widths can be measured once.
    let headers: Vec<String> = columns.iter().map(|c| c.header.clone()).collect();
    // Only the rows the filter selected are materialised: the widths are
    // measured off what is drawn, so a filtered grid is as narrow as the rows
    // it is showing rather than carrying columns sized for hidden ones.
    let body: Vec<Vec<String>> = visible
        .iter()
        .filter_map(|&r| result.rows.get(r))
        .map(|row| {
            columns
                .iter()
                .map(|c| flatten_cell(&c.value(row, &result.no_match_marker)))
                .collect()
        })
        .collect();
    // STATISTICS(…) summary rows are appended after the data rows; they share
    // the same columns and are measured into the widths so they line up.
    let summary_body = summary_grid_body(result, &columns);

    // Column widths are factored out so the mouse hit-test shares the same
    // computation (see `result_column_widths` / `grid_col_at_x`).
    let widths = grid_column_widths(&headers, &body, &summary_body);
    let cursor_style = Style::default().bg(th.select_bg).fg(th.select_fg);

    let mut lines = Vec::with_capacity(body.len() + 1);
    // While a run streams, every line carries a leading status-icon cell
    // (icon glyph + trailing space); the header's is blank so columns still
    // line up. Off-stream results draw no icon column at all.
    let show_icons = states.is_some();
    let header_style = Style::default().fg(th.accent).add_modifier(Modifier::BOLD);
    let mut header_spans: Vec<Span<'static>> = Vec::new();
    if show_icons {
        header_spans.push(Span::styled("  ".to_string(), header_style));
    }
    // Header row: no cursor highlighting (cursor is on data rows only).
    header_spans.extend(grid_row_cell_spans(
        &headers,
        &widths,
        header_style,
        None,
        cursor_style,
        col_offset,
    ));
    lines.push(Line::from(header_spans));
    for (i, row) in body.iter().enumerate() {
        // The status icons are per *result* row, so they are looked up by the
        // original index rather than the on-screen one.
        let state = states
            .and_then(|s| visible.get(i).and_then(|&r| s.get(r)))
            .copied();
        // A running row is highlighted in pending colour and bold so it stands out
        // from queued/finished rows; scheduled rows stay dim, finished stay normal.
        let text_style = match state {
            Some(RowState::Running) => Style::default().fg(th.pending).add_modifier(Modifier::BOLD),
            Some(RowState::Scheduled) => Style::default().fg(th.dim),
            _ => Style::default().fg(th.text),
        };
        let mut spans: Vec<Span<'static>> = Vec::new();
        if show_icons {
            // Mirror the collection view's Run-All markers (…/✓) for a
            // consistent visual language; a dim dot marks a queued row.
            let (glyph, color) = match state {
                Some(RowState::Running) => (ROW_RUNNING_ICON, th.pending),
                Some(RowState::Finished) => (ROW_FINISHED_ICON, th.ok),
                _ => (ROW_SCHEDULED_ICON, th.dim),
            };
            spans.push(Span::styled(
                format!("{glyph} "),
                Style::default().fg(color),
            ));
        }
        // Highlight the cursor column for this row if the cursor is here.
        let cursor_col = cursor.and_then(|(r, c)| if r == i { Some(c) } else { None });
        spans.extend(grid_row_cell_spans(
            row,
            &widths,
            text_style,
            cursor_col,
            cursor_style,
            col_offset,
        ));
        lines.push(Line::from(spans));
    }
    // Append the STATISTICS summary rows below the data. They read as a footer
    // rather than data: accent + bold + italic distinguishes them from both the
    // (accent+bold) header and the (plain) data rows, and they carry no status
    // icon glyph (only the alignment prefix) and no cursor highlight.
    let summary_style = Style::default()
        .fg(th.accent)
        .add_modifier(Modifier::BOLD | Modifier::ITALIC);
    for srow in &summary_body {
        let mut spans: Vec<Span<'static>> = Vec::new();
        if show_icons {
            spans.push(Span::styled("  ".to_string(), summary_style));
        }
        spans.extend(grid_row_cell_spans(
            srow,
            &widths,
            summary_style,
            None,
            cursor_style,
            col_offset,
        ));
        lines.push(Line::from(spans));
    }
    lines
}

/// Materialise the STATISTICS summary rows' cell text (one inner Vec per
/// summary row, one flattened String per output column) so both the width
/// computation and the grid renderer share identical values. Empty when no
/// column requested statistics.
fn summary_grid_body(
    result: &ReportResult,
    columns: &[crate::report::model::OutputColumn],
) -> Vec<Vec<String>> {
    result
        .summary_rows(columns)
        .iter()
        .map(|sr| {
            (0..columns.len())
                .map(|c| flatten_cell(&sr.text_cell(c)))
                .collect()
        })
        .collect()
}
const ROW_SCHEDULED_ICON: &str = "\u{00B7}"; // ·
const ROW_RUNNING_ICON: &str = "\u{2026}"; // …
const ROW_FINISHED_ICON: &str = "\u{2713}"; // ✓

/// Per-column display width cap: a response body can easily be thousands of
/// characters, so each column is capped so one wide cell can't push everything
/// else off-screen. Shared by the renderer and the mouse hit-test.
const MAX_COL_WIDTH: usize = 32;

/// Compute per-column display widths from pre-materialised headers, body, and
/// the appended summary rows. Width = max(header length, max(cell length) over
/// body and summary rows) clamped to [`MAX_COL_WIDTH`]. Measuring the summary
/// rows here keeps them aligned under the same columns as the data.
/// Private: callers outside this module use [`result_column_widths`].
fn grid_column_widths(
    headers: &[String],
    body: &[Vec<String>],
    summary: &[Vec<String>],
) -> Vec<usize> {
    (0..headers.len())
        .map(|c| {
            let mut w = headers[c].chars().count();
            for row in body.iter().chain(summary.iter()) {
                w = w.max(row.get(c).map(|s| s.chars().count()).unwrap_or(0));
            }
            w.clamp(1, MAX_COL_WIDTH)
        })
        .collect()
}

/// Return the display column widths for the `visible` rows of `result`'s
/// resolved grid — the same
/// widths [`report_grid_lines`] uses — so the mouse hit-test in
/// [`crate::tui::input`] can map a click's x offset to a column index without
/// duplicating the width computation.
pub(crate) fn result_column_widths(
    result: &ReportResult,
    header: &crate::report::flow::Header,
    visible: &[usize],
) -> Vec<usize> {
    let columns = result.resolved_columns(header);
    let headers: Vec<String> = columns.iter().map(|c| c.header.clone()).collect();
    let body: Vec<Vec<String>> = visible
        .iter()
        .filter_map(|&r| result.rows.get(r))
        .map(|row| {
            columns
                .iter()
                .map(|c| flatten_cell(&c.value(row, &result.no_match_marker)))
                .collect()
        })
        .collect();
    let summary_body = summary_grid_body(result, &columns);
    grid_column_widths(&headers, &body, &summary_body)
}

/// The leftmost grid column to draw so that `cursor_col` is fully visible in
/// `avail` display columns, given the current offset. Scrolls left whenever the
/// cursor is left of the viewport and right by the fewest columns that bring
/// the cursor's right edge back inside it — so a column wider than the whole
/// pane still becomes the leftmost one rather than pinning the view.
pub(crate) fn follow_col_offset(
    widths: &[usize],
    cursor_col: usize,
    avail: usize,
    current: usize,
) -> usize {
    if widths.is_empty() {
        return 0;
    }
    let last = widths.len() - 1;
    let cursor_col = cursor_col.min(last);
    // Never leave the cursor off the left edge.
    let mut off = current.min(cursor_col);
    // Width of columns `off..=cursor_col`, including the two-space gutters
    // between them.
    let span = |off: usize| -> usize {
        widths[off..=cursor_col].iter().sum::<usize>() + 2 * (cursor_col - off)
    };
    while off < cursor_col && span(off) > avail {
        off += 1;
    }
    off
}

/// Map an x offset within a grid row to a column index. The grid layout has/// each column occupying `widths[i]` characters followed by a two-space gutter
/// (before the next column); an optional 2-character status-icon prefix is
/// present when `show_icons` is true. Clicks that fall in a gutter are
/// assigned to the preceding column; clicks past the last column's end return
/// the last column index. `col_offset` is the leftmost drawn column, so the
/// returned index is absolute even when the grid is scrolled sideways.
pub(crate) fn grid_col_at_x(
    widths: &[usize],
    x_off: usize,
    show_icons: bool,
    col_offset: usize,
) -> usize {
    if widths.is_empty() {
        return 0;
    }
    let col_offset = col_offset.min(widths.len() - 1);
    let widths = &widths[col_offset..];
    // Strip the icon prefix so `x` is relative to the first column's start.
    let x = if show_icons {
        x_off.saturating_sub(2)
    } else {
        x_off
    };
    let mut pos = 0usize;
    for (ci, &w) in widths.iter().enumerate() {
        // Column `ci` occupies [pos, pos+w-1]; the gutter is [pos+w, pos+w+1].
        // Clicks within the column or the gutter after it map to column `ci`,
        // except when we're on the last column (no gutter after it).
        let next_col_start = pos + w + 2;
        if ci + 1 == widths.len() || x < next_col_start {
            return col_offset + ci;
        }
        pos = next_col_start;
    }
    col_offset + widths.len() - 1
}

/// Produce the per-cell spans for one grid row. Each cell is padded (or
/// truncated with `…`) to its column width; columns are joined with a two-space
/// gutter. The column at `cursor_col` (if any) uses `cursor_style` instead of
/// `base_style` so the selected cell is visually highlighted. Used for both the
/// header row (always `cursor_col = None`) and each data row. Columns before
/// `col_offset` are skipped entirely, which is how the grid scrolls sideways;
/// `cursor_col` is still an absolute column index.
fn grid_row_cell_spans(
    fields: &[String],
    widths: &[usize],
    base_style: Style,
    cursor_col: Option<usize>,
    cursor_style: Style,
    col_offset: usize,
) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (i, field) in fields.iter().enumerate().skip(col_offset) {
        if i > col_offset {
            spans.push(Span::styled("  ".to_string(), base_style));
        }
        let w = widths[i];
        let count = field.chars().count();
        let cell_text: String = if count > w {
            // Truncate with an ellipsis so a clipped value reads as clipped.
            let take = w.saturating_sub(1);
            let mut s: String = field.chars().take(take).collect();
            s.push('…');
            s
        } else {
            let mut s = field.clone();
            s.extend(std::iter::repeat_n(' ', w - count));
            s
        };
        let style = if cursor_col == Some(i) {
            cursor_style
        } else {
            base_style
        };
        spans.push(Span::styled(cell_text, style));
    }
    spans
}

/// Collapse a possibly multi-line cell value onto one line (newlines → `⏎`) so a
/// response body doesn't break the grid; the full value stays in the exported
/// CSV and, later, a drill-down overlay.
fn flatten_cell(value: &str) -> String {
    if value.contains(['\n', '\r']) {
        value.replace("\r\n", "⏎").replace(['\n', '\r'], "⏎")
    } else {
        value.to_string()
    }
}

/// Render styled `lines` into an already-computed inner `area` (no block /
/// border) through `panel`, returning the scrollbar Rect (`Rect::default()`
/// when none is needed). This is the block-less core the sticky-header results
/// grid uses: it renders its header row separately and scrolls only the body
/// rows through this helper, so the header stays pinned while the body moves.
/// `bar_x` is the column the scrollbar is drawn in (the block's right border).
fn render_panel_lines(
    f: &mut Frame,
    area: Rect,
    bar_x: u16,
    panel: &mut MultiSelectPanel,
    lines: &[Line<'static>],
    th: &Theme,
) -> Rect {
    if area.width == 0 || area.height == 0 {
        return Rect::default();
    }
    panel.set_wrap_marker(Some(super::draw::wrap_marker(th)));
    panel.set_styled_content(lines, area.width as usize);
    panel.clamp_scroll(area.height);
    let visible = panel.visible_rows(area.height);
    f.render_widget(
        Paragraph::new(visible).style(Style::default().fg(th.text)),
        area,
    );
    if panel.wrap_mode() == WrapMode::Clip {
        mark_clipped_rows(f, area, lines, panel.scroll(), th);
    }
    let mut bar_area = Rect::default();
    if panel.max_scroll(area.height) > 0 {
        let total = panel.total_rows().min(u16::MAX as u32) as usize;
        let bar = Rect {
            x: bar_x,
            y: area.y,
            width: 1,
            height: area.height,
        };
        draw_scrollbar(
            f,
            bar,
            total,
            area.height as usize,
            panel.scroll() as usize,
            th,
        );
        bar_area = bar;
    }
    bar_area
}

/// Render styled `lines` into `block`'s inner area through `panel`, so the read
/// content wraps, scrolls and shows a scrollbar exactly like the collection
/// view's panels. Returns the inner text Rect and the scrollbar Rect (the
/// latter `Rect::default()` when no scrollbar is needed) so the caller can
/// record them for mouse hit-testing (text selection + scrollbar drag).
/// The marker painted in the last column of a row whose content runs off the
/// right edge of a clipping panel.
///
/// The report's Source view (and the results grid) clip rather than wrap, so a
/// long line simply stopped at the panel edge with nothing to say it had been
/// cut — the only way to find out was to enter edit mode and walk the cursor
/// along it. The glyph is the ellipsis the wizard's clipped cells already use
/// (see `editor::render_clipped_line`) rather than the `‹ ›` pair, which this
/// codebase reserves for text you can actually scroll sideways; drawn dim like
/// the soft-wrap marker so it never competes with the content.
const CLIP_MARKER: char = '\u{2026}';

/// Paint [`CLIP_MARKER`] on every visible row whose line is cut off by the
/// panel's right edge.
///
/// Only meaningful for a clipping panel: a wrapping one has nothing off-screen
/// to point at. Rows are 1:1 with logical lines under `WrapMode::Clip`, so the
/// line behind visible row `n` is `lines[scroll + n]`.
///
/// A row is only marked when there is something other than blanks past the
/// edge — the results grid pads its cells out to fixed column widths, and a
/// row of trailing spaces is not something the reader is missing.
fn mark_clipped_rows(f: &mut Frame, inner: Rect, lines: &[Line<'static>], scroll: u16, th: &Theme) {
    let width = inner.width as usize;
    if width == 0 {
        return;
    }
    for row in 0..inner.height {
        let Some(line) = lines.get(scroll as usize + row as usize) else {
            break;
        };
        let mut chars = line.spans.iter().flat_map(|sp| sp.content.chars());
        if chars.by_ref().take(width).count() < width {
            continue;
        }
        if !chars.any(|c| c != ' ') {
            continue;
        }
        let pos = ratatui::layout::Position::new(inner.x + inner.width - 1, inner.y + row);
        if let Some(cell) = f.buffer_mut().cell_mut(pos) {
            cell.set_char(CLIP_MARKER);
            cell.set_style(Style::default().fg(th.dim));
        }
    }
}

fn draw_report_panel(
    f: &mut Frame,
    area: Rect,
    block: Block<'static>,
    panel: &mut MultiSelectPanel,
    lines: &[Line<'static>],
    th: &Theme,
) -> (Rect, Rect) {
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return (Rect::default(), Rect::default());
    }
    // Soft-wrapped logical lines get the same dim ↵ marker as the collection
    // view's panels (a no-op for a Clip-mode grid panel, which never wraps).
    panel.set_wrap_marker(Some(super::draw::wrap_marker(th)));
    panel.set_styled_content(lines, inner.width as usize);
    panel.clamp_scroll(inner.height);
    let visible = panel.visible_rows(inner.height);
    f.render_widget(
        Paragraph::new(visible).style(Style::default().fg(th.text)),
        inner,
    );
    if panel.wrap_mode() == WrapMode::Clip {
        mark_clipped_rows(f, inner, lines, panel.scroll(), th);
    }
    let mut bar_area = Rect::default();
    if panel.max_scroll(inner.height) > 0 {
        let total = panel.total_rows().min(u16::MAX as u32) as usize;
        let bar = Rect {
            x: area.x + area.width - 1,
            y: inner.y,
            width: 1,
            height: inner.height,
        };
        draw_scrollbar(
            f,
            bar,
            total,
            inner.height as usize,
            panel.scroll() as usize,
            th,
        );
        bar_area = bar;
    }
    (inner, bar_area)
}

/// Draw the cell drill-down popup overlay ([`crate::tui::app::Overlay::ReportCellPopup`]):
/// the selected cell's full (untruncated, unflattened) content in a scrollable,
/// selectable panel. The popup title shows the column header name plus a hint.
/// Scroll/selection state is held in `panel` across frames. Called from
/// `draw.rs`'s `draw_overlay` handler.
pub(crate) fn draw_result_cell_popup_overlay(
    f: &mut Frame,
    title: &str,
    content: &str,
    panel: &mut MultiSelectPanel,
    s: &Strings,
    th: &Theme,
) -> Rect {
    use super::draw::centered_rect;
    let box_w = f.area().width.saturating_sub(8).clamp(40, 90);
    // Convert the raw content into styled lines (one per line in the value).
    let content_lines: Vec<Line<'static>> = content
        .lines()
        .map(|l| Line::from(Span::styled(l.to_string(), Style::default().fg(th.text))))
        .collect();
    // Guard against empty content (e.g. the no-match marker is empty string).
    let content_lines = if content_lines.is_empty() {
        vec![Line::from(Span::styled(
            String::new(),
            Style::default().fg(th.dim),
        ))]
    } else {
        content_lines
    };
    // Size the box to fit the *wrapped* content, capping at the terminal
    // height. The popup wraps (`WrapMode::Wrap`), so a single very long line
    // occupies several rows — counting logical lines alone would make the box
    // far too short and force the user to scroll a mostly-empty popup. Estimate
    // the wrapped-row count from each line's length against the inner width.
    let inner_w = box_w.saturating_sub(2).max(1) as usize;
    let wrapped_rows: usize = content
        .lines()
        .map(|l| {
            let cols = l.chars().count();
            if cols == 0 { 1 } else { cols.div_ceil(inner_w) }
        })
        .sum::<usize>()
        .max(1);
    let box_h = (wrapped_rows as u16 + 2)
        .max(4)
        .min(f.area().height.saturating_sub(4).max(4));
    let area = centered_rect(box_w, box_h, f.area());
    f.render_widget(ratatui::widgets::Clear, area);
    let popup_title = format!("{title}  ({})", s.report_cell_popup_hint);
    let (inner, _bar) = draw_report_panel(
        f,
        area,
        super::draw::panel(popup_title, true, th),
        panel,
        &content_lines,
        th,
    );
    inner
}
/// cursor, on top of the already-rendered editor. Mirrors the horizontal /
/// vertical scroll maths [`render_editor_highlighted`] uses so it lands exactly
/// at the cursor cell (the completion is only offered when the cursor is at the
/// end of the line, so it reads as the line's continuation).
fn draw_editor_ghost(f: &mut Frame, area: Rect, ed: &Editor, ghost: &str, th: &Theme) {
    let w = area.width as usize;
    let h = area.height as usize;
    if w == 0 || h == 0 {
        return;
    }
    let col_off = ed.col.saturating_sub(w.saturating_sub(1));
    let row_off = ed.row.saturating_sub(h.saturating_sub(1));
    let screen_col = ed.col - col_off;
    let screen_row = ed.row - row_off;
    let avail = w.saturating_sub(screen_col);
    if avail == 0 || screen_row >= h {
        return;
    }
    let shown: String = ghost.chars().take(avail).collect();
    let rect = Rect {
        x: area.x + screen_col as u16,
        y: area.y + screen_row as u16,
        width: shown.chars().count() as u16,
        height: 1,
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            shown,
            Style::default().fg(th.dim).add_modifier(Modifier::DIM),
        ))),
        rect,
    );
}

/// The directory that a report's relative producer paths (`FILES`, `FOLDERS`,
/// `TUPLES FROM`) resolve against, plus whether it is *anchored* (a saved
/// report file or an explicit `# root:`). When unanchored — a never-saved
/// scratch report with no `# root:` — paths resolve against the process working
/// directory, which the UI flags so the user knows to save or set `# root:`.
pub(crate) fn report_base_dir(report: &Report) -> (std::path::PathBuf, bool) {
    match report.flow() {
        Ok(flow) => crate::report::context::report_base_dir(&flow, report.path.as_deref()),
        // A report whose text doesn't parse still has a directory to anchor
        // filesystem checks against (or falls back to the working directory,
        // unanchored) — mirror the shared helper's non-`# root:` branches.
        Err(_) => match report.path.as_deref().and_then(|p| p.parent()) {
            Some(dir) => (dir.to_path_buf(), true),
            None => (std::env::current_dir().unwrap_or_default(), false),
        },
    }
}

fn draw_report_binding(
    f: &mut Frame,
    area: Rect,
    app: &TuiApp,
    idx: usize,
    s: &Strings,
    th: &Theme,
) {
    let rt = &app.reports[idx];
    let binding = match app.resolve_bound_collection(&rt.report) {
        Some(ci) => {
            let mut spans = vec![
                Span::styled(s.report_bound_prefix, Style::default().fg(th.dim)),
                Span::raw(" "),
                Span::styled(
                    app.collections[ci].name.clone(),
                    Style::default().fg(th.ok).add_modifier(Modifier::BOLD),
                ),
            ];
            // Show the report's declared `# environment:` (if any), flagging one
            // that isn't currently loaded so the base-var source is obvious.
            if let Some(env) = rt.report.environment_ref() {
                let loaded = app.global_envs.iter().any(|e| e.name == env);
                spans.push(Span::styled("  ·  ", Style::default().fg(th.dim)));
                spans.push(Span::styled(
                    s.report_env_prefix,
                    Style::default().fg(th.dim),
                ));
                spans.push(Span::raw(" "));
                spans.push(Span::styled(
                    env,
                    Style::default()
                        .fg(if loaded { th.ok } else { th.pending })
                        .add_modifier(Modifier::BOLD),
                ));
                if !loaded {
                    spans.push(Span::raw(" "));
                    spans.push(Span::styled(
                        s.report_env_not_loaded,
                        Style::default().fg(th.pending),
                    ));
                }
            }
            Line::from(spans)
        }
        None => {
            let msg = if rt.report.collection_ref().is_some() {
                s.report_collection_missing
            } else {
                s.report_unbound
            };
            Line::from(Span::styled(msg, Style::default().fg(th.pending)))
        }
    };
    // A second line names the directory relative producer paths resolve
    // against, so the user can write relative `FILES`/`FOLDERS` paths with
    // confidence (and knows when an unsaved report falls back to the process
    // working directory).
    let (base, anchored) = report_base_dir(&rt.report);
    let mut dir_spans = vec![
        Span::styled(s.report_base_dir_prefix, Style::default().fg(th.dim)),
        Span::raw(" "),
        Span::styled(
            base.display().to_string(),
            Style::default().fg(if anchored { th.text } else { th.pending }),
        ),
    ];
    if !anchored {
        dir_spans.push(Span::raw(" "));
        dir_spans.push(Span::styled(
            s.report_base_dir_unsaved,
            Style::default().fg(th.pending),
        ));
    }
    let lines = vec![binding, Line::from(dir_spans)];
    let title = if app.running_reports.contains_key(&rt.report.id) {
        format!(
            "{} — {}",
            s.report_binding_heading, s.report_running_indicator
        )
    } else {
        s.report_binding_heading.to_string()
    };
    f.render_widget(
        Paragraph::new(lines)
            .block(panel(title, false, th))
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn draw_report_source(
    f: &mut Frame,
    area: Rect,
    app: &mut TuiApp,
    idx: usize,
    s: &Strings,
    th: &Theme,
) {
    // The tab is named for the report; the source panel advertises only the
    // report-specific action (edit / leave-edit) — the tab-navigation/close
    // shortcuts already live in the footer, so they're not repeated here.
    let editing = app.reports[idx].editor.is_some();
    let hint = if editing {
        s.report_hint_leave.to_string()
    } else {
        let mut hint = format!(
            "{} · {} · {} · {} · {} · {}",
            s.report_hint_edit,
            s.report_hint_run,
            s.report_hint_dry,
            s.report_hint_bind,
            s.report_hint_nodes,
            s.report_hint_format
        );
        // Once a run has produced a grid, advertise `v` as the source↔output
        // swap (the discoverable replacement for the old Tab-into-results).
        if app.reports[idx].result.is_some() {
            hint.push_str(" · ");
            hint.push_str(s.report_hint_view);
        }
        // Only offered by a report that actually asks for something — the key
        // does nothing on the rest, so advertising it everywhere would be noise.
        if app.report_has_params(idx) {
            hint.push_str(" · ");
            hint.push_str(s.param_open_hint);
        }
        hint
    };
    let title = format!("{} — {}", s.report_source_heading, hint);
    // Dim the source panel's border unless it holds focus (for an embedded
    // report the workspace tree can hold it instead), so the focused area is
    // unambiguous; editing always keeps it lit.
    let block = panel(title, editing || app.report_body_focused(), th);
    // Context so the highlighter can colour the `# collection:`/`# environment:`
    // references (and `ENVS` names) by whether they currently resolve. Built
    // before any `&mut app` borrow below.
    let ctx = super::report_highlight::HlCtx {
        error_line: app.reports[idx].parse_error_line,
        collection_resolves: app
            .resolve_bound_collection(&app.reports[idx].report)
            .is_some(),
        loaded_envs: app.global_envs.iter().map(|e| e.name.clone()).collect(),
        request_names: app
            .resolve_bound_collection(&app.reports[idx].report)
            .map(|ci| {
                app.collections[ci]
                    .entries
                    .iter()
                    .map(|e| e.title.clone())
                    .collect()
            })
            .unwrap_or_default(),
    };

    if editing {
        // Edit focus: render the live editor (with cursor) inside the panel,
        // keeping the same syntax highlighting as the read view.
        let inner = block.inner(area);
        f.render_widget(block, area);
        // A pending `REQUEST`-name completion, drawn dim after the cursor.
        let completion = app.report_completion(idx);
        if let Some(editor) = app.reports[idx].editor.as_ref() {
            render_editor_highlighted(f, inner, editor, th, |row, line| {
                super::report_highlight::highlight_row(row, line, &ctx, th)
            });
            if let Some(completion) = completion {
                draw_editor_ghost(f, inner, editor, &completion.ghost, th);
            }
        }
        return;
    }

    let body = app.reports[idx].report.text.clone();
    let trimmed = body.trim_end();
    let lines = if trimmed.is_empty() {
        vec![Line::from(Span::styled(
            s.report_empty_source,
            Style::default().fg(th.dim),
        ))]
    } else {
        super::report_highlight::highlight_source(trimmed, &ctx, th)
    };
    let (inner, bar) = draw_report_panel(
        f,
        area,
        block,
        &mut app.reports[idx].source_panel,
        &lines,
        th,
    );
    app.report_pane_areas[ReportPane::Source.idx()] = inner;
    app.report_pane_bars[ReportPane::Source.idx()] = bar;
    app.push_mouse_hit(
        MouseLayer::Base,
        bar,
        MouseHitTarget::Scroll(MouseScrollTarget::ReportPane(ReportPane::Source)),
    );
    app.push_mouse_hit(
        MouseLayer::Base,
        inner,
        MouseHitTarget::FocusPane(Pane::Main),
    );
    app.push_mouse_hit(
        MouseLayer::Base,
        inner,
        MouseHitTarget::Scroll(MouseScrollTarget::ReportPane(ReportPane::Source)),
    );
}

/// The validation panel's content: the parse error if the source doesn't parse,
/// else one line per diagnostic, else the "no problems" note.
///
/// Split out of the draw so [`draw_report_content`] can measure it — the panel
/// soft-wraps, so how *tall* it needs to be is a question about the wrapped
/// text, not about how many diagnostics there are.
fn report_validation_lines(rt: &ReportTab, s: &Strings, th: &Theme) -> Vec<Line<'static>> {
    if let Some(err) = &rt.parse_error {
        return vec![Line::from(Span::styled(
            err.clone(),
            Style::default().fg(th.err),
        ))];
    }
    if rt.diagnostics.is_empty() {
        return vec![Line::from(Span::styled(
            s.report_no_diagnostics,
            Style::default().fg(th.ok),
        ))];
    }
    rt.diagnostics
        .iter()
        .map(|d| {
            let (icon, colour) = match d.severity {
                Severity::Error => ("✗ ", th.err),
                Severity::Warning => ("! ", th.pending),
            };
            Line::from(vec![
                Span::styled(icon, Style::default().fg(colour)),
                Span::styled(d.message.clone(), Style::default().fg(th.text)),
            ])
        })
        .collect()
}

/// The most rows of validation text to show before the panel starts scrolling.
///
/// Small on purpose: the panel sits under the editor, so every row it takes is
/// a row of the report you can no longer see. Five is enough to take in a
/// handful of problems at a glance; past that the list is something you scroll
/// through deliberately rather than something you want permanently covering the
/// thing you're fixing.
const VALIDATION_MAX_ROWS: u16 = 5;

/// How tall the validation panel should be drawn, borders included.
///
/// Measured from the **wrapped** text rather than the number of diagnostics,
/// which is what a parse error needs: there is only ever one of those, but it
/// is a sentence long and used to be given a single row and then clipped —
/// so the panel was stuck one row tall in exactly the state you most need to
/// read it.
fn validation_height(rt: &ReportTab, s: &Strings, th: &Theme, area: Rect) -> u16 {
    let lines = report_validation_lines(rt, s, th);
    // Same width the panel itself will wrap to (the block's borders take two).
    let width = area.width.saturating_sub(2) as usize;
    let rows = if width == 0 {
        lines.len() as u16
    } else {
        let mut probe = MultiSelectPanel::new();
        probe.set_styled_content(&lines, width);
        probe.total_rows().min(u16::MAX as u32) as u16
    };
    // Always keep a workable editor above: the binding block is 4 rows and the
    // editor wants at least 5, so the panel only grows into whatever is left.
    let room = area.height.saturating_sub(4 + 5).max(3);
    (rows + 2).min(room).min(VALIDATION_MAX_ROWS + 2).max(3)
}

fn draw_report_validation(
    f: &mut Frame,
    area: Rect,
    app: &mut TuiApp,
    idx: usize,
    s: &Strings,
    th: &Theme,
) {
    let lines = report_validation_lines(&app.reports[idx], s, th);
    let block = panel(s.report_validation_heading.to_string(), false, th);
    let (inner, bar) = draw_report_panel(
        f,
        area,
        block,
        &mut app.reports[idx].validation_panel,
        &lines,
        th,
    );
    app.report_pane_areas[ReportPane::Validation.idx()] = inner;
    app.report_pane_bars[ReportPane::Validation.idx()] = bar;
    app.push_mouse_hit(
        MouseLayer::Base,
        bar,
        MouseHitTarget::Scroll(MouseScrollTarget::ReportPane(ReportPane::Validation)),
    );
    app.push_mouse_hit(
        MouseLayer::Base,
        inner,
        MouseHitTarget::FocusPane(Pane::Main),
    );
    app.push_mouse_hit(
        MouseLayer::Base,
        inner,
        MouseHitTarget::Scroll(MouseScrollTarget::ReportPane(ReportPane::Validation)),
    );
}

#[cfg(test)]
mod export_path_tests {
    use super::*;

    #[test]
    fn time_token_name_overrides_a_saved_report_filename_and_stays_in_its_folder() {
        // A saved report whose file is `sample.trail` but whose name carries the
        // `{time}` token: the export must use the expanded name (not `sample`) and
        // land next to the report file.
        let mut report = Report::from_text("sample", "# name: run_{time}\n# collection: c.hurl\n");
        report.path = Some(std::path::PathBuf::from("/tmp/reports/sample.trail"));

        let csv = export_path(&report, &report_output_extension(&report));
        assert_eq!(csv.parent(), Some(std::path::Path::new("/tmp/reports")));
        let file = csv.file_name().unwrap().to_string_lossy().into_owned();
        assert!(file.starts_with("run_"), "expanded name used: {file}");
        assert!(file.ends_with(".csv"));
        assert!(!file.contains("{time}"), "token expanded: {file}");
        assert!(
            !file.starts_with("sample"),
            "name wins over the file stem: {file}"
        );
    }

    #[test]
    fn without_a_token_a_saved_report_keeps_its_own_stem() {
        let mut report = Report::from_text("sample", "# name: My Report\n# collection: c.hurl\n");
        report.path = Some(std::path::PathBuf::from("/tmp/reports/sample.trail"));
        assert_eq!(
            export_path(&report, &report_output_extension(&report)),
            std::path::PathBuf::from("/tmp/reports/sample.csv"),
            "unchanged behaviour when the name has no token"
        );
    }

    #[test]
    fn set_flow_directive_handles_non_ascii_comment_lines() {
        // A `#` comment whose multi-byte char straddles the directive-key
        // length must not trip a non-char-boundary slice (BIND / column-apply).
        let text = "# aaaaaaaé note\n# collection: old.hurl\nREQUEST A\n";
        let bound = set_flow_directive(text, "collection", "new.hurl");
        assert!(bound.contains("# collection: new.hurl"));
        // Inserting a brand-new directive above a non-ASCII comment is also safe.
        let cols = set_flow_directive("# café ☕ notes\nREQUEST A\n", "columns", "FILE");
        assert!(cols.contains("# columns: FILE"));
    }
}

#[cfg(test)]
mod source_panel_tests {
    use super::*;
    use crate::i18n::Language;
    use crate::tui::report_highlight::{HlCtx, highlight_source};
    use crate::tui::theme::theme;

    /// Item 10 (rep-blank-highlight): the read-only source panel must render one
    /// row per source line — *including blank separators* — so its display stays
    /// 1:1 with the buffer and mouse selection/highlight land on the right
    /// lines. Clip mode gives an empty line one empty row; the wrap path drops
    /// it, which is the bug this guards against.
    #[test]
    fn source_panel_keeps_blank_lines_as_rows() {
        let th = theme(&Language::English);
        let ctx = HlCtx::default();
        let text = "REQUEST A\n\nREQUEST B\n\nREQUEST C";
        let lines = highlight_source(text, &ctx, &th);
        assert_eq!(lines.len(), 5, "five source lines, two of them blank");

        // Build the panel exactly as `ReportTab::new` does for the source view.
        let mut panel = ReportTab::new(Report::from_text("t", text)).source_panel;
        panel.set_styled_content(&lines, 40);
        assert_eq!(
            panel.total_rows(),
            5,
            "the selection geometry counts every line, blanks included"
        );
        assert_eq!(
            panel.visible_rows(10).len(),
            5,
            "the rendered rows match — blank lines are not dropped from the view"
        );
    }
}
