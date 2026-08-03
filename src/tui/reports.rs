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

use super::app::{MouseHitTarget, MouseLayer, MouseScrollTarget, Overlay, Pane, TuiApp};
use super::draw::panel;
use super::editor::{
    Editor, apply_edit_key_full, render_editor_highlighted, word_left, word_right,
};
use super::new_request::draw_scrollbar;
use super::theme::Theme;
use crate::i18n::{Status, Strings};
use crate::report::Report;
use crate::report::flow::{Header, ReportFlow};
use crate::report::model::{ReportResult, ReportRow, TARGET_COLUMN, parse_columns};
use crate::report::parser::opens_block;
use crate::report::run::{
    DryRunner, EntryRunner, LiveRunner, RowEvent, RunContext, finalize, run_flow, run_flow_raw,
};
use crate::report::validate::{Context, Diagnostic, Severity, validate};
use crate::report::writer::{CsvWriter, writer_for_extension};
use crate::report::{expand_output_tokens, name_has_output_token};
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

/// Everything a report run needs, owned (no borrow of `TuiApp`), so the whole
/// run can be moved onto a background thread. Assembled on the main thread by
/// [`TuiApp::build_report_run_inputs`]; the worker rebuilds a [`RunContext`]
/// that borrows these.
struct ReportRunInputs {
    flow: ReportFlow,
    entries: Vec<crate::hurl::HurlEntry>,
    base_vars: HashMap<String, String>,
    named_envs: HashMap<String, HashMap<String, String>>,
    root: Option<PathBuf>,
    file_root: Option<PathBuf>,
}

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
    /// Undo stack for the structured node editor. Every structural node edit
    /// snapshots the pre-edit source text + node selection here (via
    /// [`ReportTab::set_text_undoable`]) so **Ctrl+Z** can restore the previous
    /// state exactly — the node editor's counterpart to the source editor's
    /// in-buffer undo. In-memory and per-tab (not persisted), like text undo.
    pub(crate) node_undo: Vec<NodeUndo>,
    /// The last run's output, if the report has been run this session. Rendered
    /// as a grid in [`ReportView::Results`] and the source of an `Export CSV`.
    pub(crate) result: Option<ReportResult>,
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
        Self {
            report,
            diagnostics: Vec::new(),
            parse_error: None,
            parse_error_line: None,
            editor: None,
            edit_cursor: None,
            source_panel,
            validation_panel: MultiSelectPanel::new(),
            view: ReportView::Source,
            editor_view: ReportView::Source,
            node_selected: 0,
            node_undo: Vec::new(),
            result: None,
            run_progress: None,
            results_panel,
            cell_cursor: None,
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
        if report_escapes_root(&root, &full_path) {
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
        // Compute the parse-error / diagnostics up front so the immutable reads
        // of `self.collections` / `self.global_envs` don't overlap the mutable
        // borrow of `self.reports[idx]` that stores the result.
        let (parse_error, parse_error_line, diagnostics) = {
            let Some(rt) = self.reports.get(idx) else {
                return;
            };
            match rt.report.flow() {
                Err(e) => (Some(e.to_string()), Some(e.line), Vec::new()),
                Ok(flow) => {
                    let bound = self.resolve_bound_collection(&rt.report);
                    let titles: Option<Vec<String>> = bound.map(|ci| {
                        self.collections[ci]
                            .entries
                            .iter()
                            .map(|e| e.title.clone())
                            .collect()
                    });
                    // Each entry's [Reports] field names, so a SHOW(...) selector
                    // can be validated against what the request can produce.
                    let fields: Option<Vec<(String, Vec<String>)>> = bound.map(|ci| {
                        self.collections[ci]
                            .entries
                            .iter()
                            .map(|e| {
                                (
                                    e.title.clone(),
                                    e.reports.iter().map(|(n, _)| n.clone()).collect(),
                                )
                            })
                            .collect()
                    });
                    let env_names: Vec<String> =
                        self.global_envs.iter().map(|e| e.name.clone()).collect();
                    // Anchor the filesystem checks (e.g. the `# baseline:`
                    // snapshot existence warning) at the report's resolved base
                    // directory, but only when it's anchored (saved / `# root:`)
                    // so a scratch report doesn't warn against the CWD.
                    let (base_dir, anchored) = report_base_dir(&rt.report);

                    // Variable-availability analysis: compute the effective base
                    // variable names. Mirrors `build_report_run_inputs` — a
                    // `# environment:` directive names a single env; otherwise
                    // fall back to the bound collection's active+pinned merge.
                    // `None` when the collection is unbound (check skipped).
                    let base_var_names: Option<Vec<String>> =
                        match (bound, flow.header.environment()) {
                            (_, Some(name)) => {
                                let name = name.trim();
                                self.global_envs
                                    .iter()
                                    .find(|e| e.name == name)
                                    .map(|env| env.vars.iter().map(|v| v.key.clone()).collect())
                            }
                            (Some(ci), None) => Some(
                                self.effective_env(ci)
                                    .map(|env| env.vars.iter().map(|v| v.key.clone()).collect())
                                    .unwrap_or_default(),
                            ),
                            (None, _) => None,
                        };
                    // Union of ALL loaded env variable names — used
                    // conservatively inside `FOR … IN ENVS` bodies so we don't
                    // false-warn when any of the named envs might supply a var.
                    let mut all_env_var_names: Vec<String> = self
                        .global_envs
                        .iter()
                        .flat_map(|e| e.vars.iter().map(|v| v.key.clone()))
                        .collect();
                    all_env_var_names.sort();
                    all_env_var_names.dedup();
                    // The bound collection's entries, for `{{VAR}}` scanning
                    // and capture-name extraction.
                    let request_entries_owned: Option<Vec<crate::hurl::HurlEntry>> =
                        bound.map(|ci| self.collections[ci].entries.clone());

                    let ctx = Context {
                        request_titles: titles.as_deref(),
                        env_names: Some(&env_names),
                        request_fields: fields.as_deref(),
                        root: anchored.then_some(base_dir.as_path()),
                        base_var_names: base_var_names.as_deref(),
                        all_env_var_names: Some(&all_env_var_names),
                        request_entries: request_entries_owned.as_deref(),
                    };
                    (None, None, validate(&flow, &ctx))
                }
            }
        };
        if let Some(rt) = self.reports.get_mut(idx) {
            rt.parse_error = parse_error;
            rt.parse_error_line = parse_error_line;
            rt.diagnostics = diagnostics;
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
        let ctx = RunContext {
            entries: &inputs.entries,
            base_vars: inputs.base_vars,
            named_envs: inputs.named_envs,
            root: inputs.root,
            runner,
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
        let ci = self
            .resolve_bound_collection(&rt.report)
            .ok_or(s.report_run_unbound)?;

        // Base variable layer. A `# environment:` directive names a single
        // loaded environment to use for a plain, no-comparison run — that env
        // alone, so the run is reproducible regardless of what's active/pinned
        // in the app. Without it, fall back to the bound collection's effective
        // (active global + pinned) environment. Loop bindings / assignments
        // layer on top of this inside the interpreter either way.
        let base_vars = match flow
            .header
            .environment()
            .map(str::trim)
            .filter(|e| !e.is_empty())
        {
            Some(name) => self
                .global_envs
                .iter()
                .find(|e| e.name == name)
                .map(flatten_env)
                .unwrap_or_default(),
            None => self
                .effective_env(ci)
                .map(|env| flatten_env(&env))
                .unwrap_or_default(),
        };
        // Every loaded global environment is selectable by name in a `FOR … IN
        // ENVS` loop.
        let named_envs = self
            .global_envs
            .iter()
            .map(|e| (e.name.clone(), flatten_env(e)))
            .collect();
        // Relative producer paths resolve against `# root:` if set, else the
        // report file's own directory.
        let report_dir = rt
            .report
            .path
            .as_deref()
            .and_then(|p| p.parent())
            .map(std::path::Path::to_path_buf);
        let root = match flow.header.root() {
            Some(r) if !r.trim().is_empty() => Some(resolve_ref_path(rt.report.path.as_deref(), r)),
            _ => report_dir,
        };
        // The live runner is rooted at the bound collection's directory so
        // relative form-file paths in its requests resolve as they would when
        // the request is sent by hand.
        let file_root = self.collections[ci]
            .path
            .as_deref()
            .and_then(|p| p.parent())
            .map(std::path::Path::to_path_buf);

        Ok(ReportRunInputs {
            flow,
            entries: self.collections[ci].entries.clone(),
            base_vars,
            named_envs,
            root,
            file_root,
        })
    }

    /// Run the active report against its bound collection on a **background
    /// thread** so the UI stays responsive during a long run (previously this
    /// ran inline and froze the whole app). Pressing `r` again while a run is in
    /// flight cancels it. A run that can't even start (parse error / unbound /
    /// validation errors) reports why in the status bar and keeps the source
    /// view; the delivered result is folded in by [`Self::poll_report_run_updates`].
    pub(crate) fn run_active_report(&mut self) {
        if let Some((report_id, inputs)) = self.prepare_report_run() {
            self.spawn_report_run(report_id, inputs, |file_root| LiveRunner { file_root });
        }
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
                base_vars,
                named_envs,
                root,
                file_root,
            } = inputs;

            // 1. Skeleton: expand the flow with no HTTP (a `DryRunner`) to get
            //    the full, canonical row set up front. The base layers are
            //    cloned so the live run below can reuse the originals. The
            //    skeleton rows map 1:1 (by `path`) to the live rows the sink
            //    will stream, so the front-end can pre-build the grid and fill
            //    it in place.
            let skeleton = {
                let dry_ctx = RunContext {
                    entries: &entries,
                    base_vars: base_vars.clone(),
                    named_envs: named_envs.clone(),
                    root: root.clone(),
                    runner: &DryRunner,
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
                base_vars,
                named_envs,
                root,
                runner: &runner,
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
                rt.result = Some(result);
                rt.run_progress = Some(RunProgress {
                    states: vec![RowState::Scheduled; n],
                    index,
                    done: 0,
                });
                // A new run invalidates the cell cursor (column layout may
                // change) — reset it so the cursor starts fresh on the new grid.
                rt.cell_cursor = None;
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
                // The finalized grid may have different columns/rows than the
                // streamed skeleton — reset cursor so it starts fresh.
                rt.cell_cursor = None;
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
                rt.cell_cursor = None;
                rt.view = ReportView::Results;
                rt.results_panel.set_scroll(0);
                self.status = Some(Status::ReportRunDone { rows, errors });
            }
            Err(reason) => self.status = Some(Status::ReportRunBlocked(reason)),
        }
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
            Some(idx) => csv_export_path(&self.reports[idx].report)
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
            Ok(()) => self.status = Some(Status::ReportExported(path.display().to_string())),
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
            Some(idx) => baseline_export_path(&self.reports[idx].report)
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
            Ok(()) => self.status = Some(Status::ReportBaselineSaved(path.display().to_string())),
            Err(e) => self.status = Some(Status::Error(format!("{}: {e}", path.display()))),
        }
    }

    /// Dry-run the active report: expand its flow with a no-op runner (no HTTP)
    /// and open a preview overlay showing the projected output grid (identical
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
                self.dry_run_scroll = 0;
                self.overlay = Some(Overlay::ReportDryRun(Box::new(preview)));
            }
            Err(reason) => self.status = Some(Status::ReportRunBlocked(reason)),
        }
    }

    /// Move the cell cursor in the active report's Results grid by `(dr, dc)`.
    /// Initialises the cursor at `(0, 0)` if it has no position yet. Clamps to
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
        let nrows = result.rows.len();
        if nrows == 0 || ncols == 0 {
            return;
        }
        let (cur_row, cur_col) = self.reports[idx].cell_cursor.unwrap_or((0, 0));
        let new_row = (cur_row as i32 + dr).clamp(0, nrows as i32 - 1) as usize;
        let new_col = (cur_col as i32 + dc).clamp(0, ncols as i32 - 1) as usize;
        self.reports[idx].cell_cursor = Some((new_row, new_col));
    }

    /// Jump the cell cursor to the first data row (row 0) while keeping the
    /// current column. If there is no cursor yet, lands at `(0, 0)`.
    fn result_cursor_jump_home(&mut self) {
        let Some(idx) = self.active_report_index() else {
            return;
        };
        if self.reports[idx]
            .result
            .as_ref()
            .is_some_and(|r| !r.rows.is_empty())
        {
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
        let nrows = self.reports[idx]
            .result
            .as_ref()
            .map(|r| r.rows.len())
            .unwrap_or(0);
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
        let Some(data_row) = result.rows.get(row) else {
            return;
        };
        let title = col_def.header.clone();
        // Full (unflattened) cell value — may be multi-line.
        let content = col_def.value(data_row, &result.no_match_marker);
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
                use tui_panel_select::clipboard::copy_to_clipboard;
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

    /// Mirrors the Help overlay: Up/Down/PageUp/PageDown/Home/End scroll the
    /// preview (the draw pass clamps overshoot against the real content height);
    /// Esc, `q` or Enter close it, and — as with Help — any other key dismisses
    /// it too. The overlay was already `take`n by the dispatcher, so closing is
    /// just declining to put it back.
    pub(crate) fn report_dry_run_key_handler(&mut self, key: KeyEvent, preview: Box<DryRunReport>) {
        let keep = |app: &mut TuiApp, preview| {
            app.overlay = Some(Overlay::ReportDryRun(preview));
        };
        match key.code {
            KeyCode::Up => {
                self.dry_run_scroll = self.dry_run_scroll.saturating_sub(1);
                keep(self, preview);
            }
            KeyCode::Down => {
                self.dry_run_scroll = self.dry_run_scroll.saturating_add(1);
                keep(self, preview);
            }
            KeyCode::PageUp => {
                self.dry_run_scroll = self.dry_run_scroll.saturating_sub(10);
                keep(self, preview);
            }
            KeyCode::PageDown => {
                self.dry_run_scroll = self.dry_run_scroll.saturating_add(10);
                keep(self, preview);
            }
            KeyCode::Home => {
                self.dry_run_scroll = 0;
                keep(self, preview);
            }
            KeyCode::End => {
                self.dry_run_scroll = u16::MAX;
                keep(self, preview);
            }
            // Esc / q / Enter / any other key: close (overlay stays taken).
            _ => {}
        }
    }

    /// Open the column-picker overlay for the active report. Available columns
    /// come from the last run, so a report must have been run first; otherwise
    /// a hint is shown. A parse error (which can't normally coexist with a
    /// result) falls back to that hint too.
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
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
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
            // Global menus, unchanged from the collection view.
            KeyCode::Char('f') => self.overlay = Some(Overlay::FileMenu(0)),
            KeyCode::Char('s') => self.overlay = Some(Overlay::Options(0)),
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
                if let Some(idx) = self.active_report_index()
                    && self.reports[idx].view == ReportView::Nodes
                {
                    self.reports[idx].view = ReportView::Source;
                    self.reports[idx].editor_view = ReportView::Source;
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
            // Flip between the source and the last run's results grid.
            KeyCode::Char('v') => self.toggle_report_view(),
            // Tab moves focus. A standalone report has a single (full-screen)
            // body, so Tab is inert. An embedded report shares its tab with the
            // collection tree, so Tab rotates focus back to that tree (and on
            // round to the body / env), via the shared `cycle_focus`.
            KeyCode::Tab if embedded => self.cycle_focus(true),
            KeyCode::BackTab if embedded => self.cycle_focus(false),
            KeyCode::Tab | KeyCode::BackTab => {}
            // Export the last run to CSV next to the report.
            KeyCode::Char('x') => self.export_active_report_csv(),
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
                // The node outline scrolls to follow its selection cursor (moved
                // by Up/Down in the node handler), not a free scroll offset.
                ReportView::Nodes => return,
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
                        let mut new_indent = leading_ws(line);
                        if opens_block(line) {
                            new_indent.push_str(INDENT_UNIT);
                        }
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
            tui_panel_select::clipboard::copy_to_clipboard(&text);
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
            let ci = self.resolve_bound_collection(&rt.report)?;
            let titles = self.collections[ci].entries.iter().map(|e| e.title.clone());
            return complete_name(&partial, titles, false);
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

/// One level of source indentation (matches the flow serializer's four spaces).
const INDENT_UNIT: &str = "    ";

/// The leading whitespace of `line`, as an owned string (used to inherit the
/// current line's indentation onto a freshly-inserted newline).
fn leading_ws(line: &str) -> String {
    line.chars().take_while(|c| c.is_whitespace()).collect()
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
    if !line.trim().eq_ignore_ascii_case("END") {
        return;
    }
    // Walk upward tracking opener/END balance to find the matching opener.
    let mut depth = 0i32;
    let mut target: Option<String> = None;
    for prev in ed.lines[..row].iter().rev() {
        if prev.trim().eq_ignore_ascii_case("END") {
            depth += 1;
        } else if opens_block(prev) {
            if depth == 0 {
                target = Some(leading_ws(prev));
                break;
            }
            depth -= 1;
        }
    }
    let Some(indent) = target else {
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

/// Join a `# collection:` reference against the report's own directory (when the
/// ref is relative and the report has a known path), so relative links stay
/// valid regardless of the process's working directory.
fn resolve_ref_path(report_path: Option<&std::path::Path>, cref: &str) -> std::path::PathBuf {
    let p = std::path::Path::new(cref);
    if p.is_absolute() {
        return p.to_path_buf();
    }
    if let Some(dir) = report_path.and_then(|rp| rp.parent()) {
        return dir.join(p);
    }
    p.to_path_buf()
}

/// Compare two paths, canonicalising when possible (so `./a.hurl` and an
/// absolute form of the same file match) but falling back to a plain equality
/// check for paths that don't yet exist on disk.
fn paths_equal(a: &std::path::Path, b: &std::path::Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

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

/// Flatten an [`Environment`](crate::environment::Environment) into a plain
/// `KEY → value` map for the interpreter's variable layers.
fn flatten_env(env: &crate::environment::Environment) -> std::collections::HashMap<String, String> {
    env.vars
        .iter()
        .map(|v| (v.key.clone(), v.value.clone()))
        .collect()
}

/// Where an exported CSV lands: alongside a saved report (same stem, `.csv`
/// extension), else `<name>.csv` in the current directory for a scratch report.
/// When the report *name* carries an output token (`{time}`), the expanded name
/// wins — even for a saved report — and lands in the report's own folder (or the
/// current directory for a scratch report), so repeated runs write distinct
/// timestamped files rather than overwriting one export.
fn csv_export_path(report: &Report) -> std::path::PathBuf {
    let ext = report_output_extension(report);
    if let Some(p) = tokened_output_path(report, &ext) {
        return p;
    }
    if let Some(path) = &report.path {
        return path.with_extension(&ext);
    }
    let stem = sanitize_file_stem(&report.name);
    std::path::PathBuf::from(format!("{stem}.{ext}"))
}

/// The preferred output extension for `report`: its `# output:` header format
/// when that names a supported writer (csv/json/xlsx), else `csv`. Used to seed
/// the export picker so a report declaring `# output: xlsx` exports `.xlsx` by
/// default (the user can still type another extension).
fn report_output_extension(report: &Report) -> String {
    report
        .flow()
        .ok()
        .and_then(|f| f.header.output().map(|o| o.trim().to_ascii_lowercase()))
        .filter(|ext| writer_for_extension(ext).is_some())
        .unwrap_or_else(|| "csv".to_string())
}

/// The default `.baseline` snapshot path for `report`: its own file with a
/// `.baseline` extension, or a sanitised `<name>.baseline` for a scratch report
/// with no file yet. Mirrors [`csv_export_path`] (including the `{time}` token).
fn baseline_export_path(report: &Report) -> std::path::PathBuf {
    if let Some(p) = tokened_output_path(report, "baseline") {
        return p;
    }
    if let Some(path) = &report.path {
        return path.with_extension("baseline");
    }
    let stem = sanitize_file_stem(&report.name);
    std::path::PathBuf::from(format!("{stem}.baseline"))
}

/// The output path when the report name carries an output token (`{time}`): the
/// token-expanded, sanitised name as the file stem with extension `ext`, placed
/// in the saved report's own folder (or the current directory for a scratch
/// report). `None` when the name has no token, so callers fall back to their
/// normal (file-stem-based) derivation.
fn tokened_output_path(report: &Report, ext: &str) -> Option<std::path::PathBuf> {
    if !name_has_output_token(&report.name) {
        return None;
    }
    let stem = sanitize_file_stem(&expand_output_tokens(&report.name));
    let file = format!("{stem}.{ext}");
    let dir = report.path.as_ref().and_then(|p| p.parent());
    Some(match dir {
        Some(d) => d.join(file),
        None => std::path::PathBuf::from(file),
    })
}

/// Turn a display name into a safe single-segment file stem (replacing path
/// separators and other awkward characters with `_`), so a scratch report's
/// name can't escape the current directory when exported.
fn sanitize_file_stem(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "report".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Preview state for the report dry-run overlay ([`Overlay::ReportDryRun`]):
/// the full [`ReportResult`] produced by expanding the flow with a no-op
/// runner (so all loop iterations, ZIP pairings and nested scopes are resolved
/// but no HTTP is sent) plus any variable-availability warnings from static
/// analysis. The result's column model is populated with everything knowable
/// without HTTP; intrinsic response fields (`HttpStatus`, `Time`, etc.) are
/// blank, matching a real grid cell that received no response.
pub(crate) struct DryRunReport {
    /// Total rows the flow would emit (`0` = empty glob, mismatched ZIP, …).
    pub(crate) rows: usize,
    /// The full dry-run result, used to render the same output grid the real
    /// run would show (via [`report_grid_lines`]).
    pub(crate) result: ReportResult,
    /// The flow's header (needed to resolve the `# columns:` directive for
    /// [`report_grid_lines`]).
    header: Header,
    /// Deduplicated producer / resolution problems (empty glob, ZIP length
    /// mismatch, unresolved request name, unloaded environment, …).
    pub(crate) errors: Vec<String>,
    /// Variable-availability warnings from static analysis (any `{{VAR}}`
    /// that may not be defined when the request that references it runs).
    pub(crate) var_warnings: Vec<String>,
}

impl DryRunReport {
    /// Build the preview from an expanded [`ReportResult`] (no HTTP), the
    /// flow's [`Header`] (for column resolution), and the variable-availability
    /// `var_warnings` already extracted from the report's diagnostics.
    fn from_result(result: ReportResult, header: Header, var_warnings: Vec<String>) -> Self {
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

    /// Render the preview body as themed lines for the overlay draw pass.
    ///
    /// Layout:
    /// 1. Preview-notice label (marks this as a dry run, not a real result).
    /// 2. Projected row count.
    /// 3. The output grid (same format as the Results view) — loop-resolved
    ///    variables and structure are visible; HTTP intrinsics are blank.
    /// 4. Variable-availability warnings (if any) — yellow `!` prefix.
    /// 5. Producer/expansion errors (if any) — red `•` prefix.
    /// 6. "No problems found." when both 4 and 5 are empty.
    pub(crate) fn lines(&self, s: &Strings, th: &Theme) -> Vec<Line<'static>> {
        let mut lines: Vec<Line<'static>> = Vec::new();

        // Dry-run notice: distinguish the preview grid from a real-run result.
        lines.push(Line::from(Span::styled(
            s.report_dry_run_preview_notice.to_string(),
            Style::default().fg(th.dim),
        )));
        lines.push(Line::from(""));

        // Row count.
        lines.push(Line::from(Span::styled(
            format!("{} {}", s.report_dry_run_rows, self.rows),
            Style::default().fg(th.accent).add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));

        // Output grid — identical path to the Results view.
        if self.rows == 0 {
            lines.push(Line::from(Span::styled(
                s.report_dry_run_no_rows.to_string(),
                Style::default().fg(th.dim),
            )));
        } else {
            // Pass `None` for states (no streaming progress in a dry run) so
            // the grid renders without status icons, exactly like a finished run.
            lines.extend(report_grid_lines(
                &self.result,
                &self.header,
                None,
                th,
                None,
            ));
        }

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

        lines
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

    // Number of validation rows to reserve at the bottom (bounded so a long
    // list of problems can't crowd out the source).
    let diag_count = {
        let rt = &app.reports[idx];
        if rt.parse_error.is_some() {
            1
        } else {
            rt.diagnostics.len().max(1)
        }
    };
    let diag_h = (diag_count as u16 + 2).min(10);

    let rows = Layout::vertical([
        Constraint::Min(3),
        Constraint::Length(4),
        Constraint::Length(diag_h),
    ])
    .split(area);

    if app.reports[idx].view == ReportView::Nodes {
        super::report_nodes::draw_report_nodes(f, rows[0], app, idx, s, th);
    } else {
        draw_report_source(f, rows[0], app, idx, s, th);
    }
    draw_report_binding(f, rows[1], app, idx, s, th);
    draw_report_validation(f, rows[2], app, idx, s, th);
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
    // Auto-scroll the results panel to keep the cell cursor visible. The inner
    // height is `area.height - 2` (one-pixel border top + bottom from `panel`).
    let inner_h = area.height.saturating_sub(2) as usize;
    if inner_h > 0
        && let Some((cursor_row, _)) = app.reports[idx].cell_cursor
    {
        // Grid line 0 is the header row; data row `cursor_row` maps to grid
        // line `cursor_row + 1`.
        let grid_line = cursor_row + 1;
        let scroll = app.reports[idx].results_panel.scroll() as usize;
        if grid_line < scroll {
            app.reports[idx].results_panel.set_scroll(grid_line as u16);
        } else if grid_line >= scroll + inner_h {
            let new_scroll = (grid_line + 1).saturating_sub(inner_h) as u16;
            app.reports[idx].results_panel.set_scroll(new_scroll);
        }
    }

    let (lines, title) = {
        let rt = &app.reports[idx];
        match &rt.result {
            None => (
                vec![Line::from(Span::styled(
                    s.report_results_empty.to_string(),
                    Style::default().fg(th.dim),
                ))],
                s.report_results_heading.to_string(),
            ),
            Some(result) => {
                let header = rt.report.flow().map(|flow| flow.header).unwrap_or_default();
                // While a run streams, each row carries a live `RowState`: the
                // grid greys unfinished rows and shows a status icon per row so
                // it doubles as a live progress indicator.
                let states = rt.run_progress.as_ref().map(|p| p.states.as_slice());
                let lines = report_grid_lines(result, &header, states, th, rt.cell_cursor);
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
                let title = format!(
                    "{} ({}) — {}",
                    s.report_results_heading, count, s.report_hint_results
                );
                (lines, title)
            }
        }
    };
    // Dim the grid's border unless the report body actually holds focus (for an
    // embedded report the workspace tree can hold it instead), so the lit
    // border always marks the pane that has focus.
    let block = panel(title, app.report_body_focused(), th);
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
/// as distinct from its row's text style.
fn report_grid_lines(
    result: &ReportResult,
    header: &crate::report::flow::Header,
    states: Option<&[RowState]>,
    th: &Theme,
    cursor: Option<(usize, usize)>, // (data_row, col) 0-indexed
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
    let body: Vec<Vec<String>> = result
        .rows
        .iter()
        .map(|row| {
            columns
                .iter()
                .map(|c| flatten_cell(&c.value(row, &result.no_match_marker)))
                .collect()
        })
        .collect();

    // Column widths are factored out so the mouse hit-test shares the same
    // computation (see `result_column_widths` / `grid_col_at_x`).
    let widths = grid_column_widths(&headers, &body);
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
    ));
    lines.push(Line::from(header_spans));
    for (i, row) in body.iter().enumerate() {
        let state = states.and_then(|s| s.get(i)).copied();
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
        ));
        lines.push(Line::from(spans));
    }
    lines
}

/// Status icons drawn beside each streaming report row. Scheduled reuses a dim
/// dot; running/finished reuse the collection view's Run-All markers (`…`/`✓`)
/// so the two progress indicators read the same way.
const ROW_SCHEDULED_ICON: &str = "\u{00B7}"; // ·
const ROW_RUNNING_ICON: &str = "\u{2026}"; // …
const ROW_FINISHED_ICON: &str = "\u{2713}"; // ✓

/// Per-column display width cap: a response body can easily be thousands of
/// characters, so each column is capped so one wide cell can't push everything
/// else off-screen. Shared by the renderer and the mouse hit-test.
const MAX_COL_WIDTH: usize = 32;

/// Compute per-column display widths from pre-materialised headers and body.
/// Width = max(header length, max(cell length)) clamped to [`MAX_COL_WIDTH`].
/// Private: callers outside this module use [`result_column_widths`].
fn grid_column_widths(headers: &[String], body: &[Vec<String>]) -> Vec<usize> {
    (0..headers.len())
        .map(|c| {
            let mut w = headers[c].chars().count();
            for row in body {
                w = w.max(row[c].chars().count());
            }
            w.clamp(1, MAX_COL_WIDTH)
        })
        .collect()
}

/// Return the display column widths for `result`'s resolved grid — the same
/// widths [`report_grid_lines`] uses — so the mouse hit-test in
/// [`crate::tui::input`] can map a click's x offset to a column index without
/// duplicating the width computation.
pub(crate) fn result_column_widths(
    result: &ReportResult,
    header: &crate::report::flow::Header,
) -> Vec<usize> {
    let columns = result.resolved_columns(header);
    let headers: Vec<String> = columns.iter().map(|c| c.header.clone()).collect();
    let body: Vec<Vec<String>> = result
        .rows
        .iter()
        .map(|row| {
            columns
                .iter()
                .map(|c| flatten_cell(&c.value(row, &result.no_match_marker)))
                .collect()
        })
        .collect();
    grid_column_widths(&headers, &body)
}

/// Map an x offset within a grid row to a column index. The grid layout has
/// each column occupying `widths[i]` characters followed by a two-space gutter
/// (before the next column); an optional 2-character status-icon prefix is
/// present when `show_icons` is true. Clicks that fall in a gutter are
/// assigned to the preceding column; clicks past the last column's end return
/// the last column index.
pub(crate) fn grid_col_at_x(widths: &[usize], x_off: usize, show_icons: bool) -> usize {
    if widths.is_empty() {
        return 0;
    }
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
            return ci;
        }
        pos = next_col_start;
    }
    widths.len() - 1
}

/// Produce the per-cell spans for one grid row. Each cell is padded (or
/// truncated with `…`) to its column width; columns are joined with a two-space
/// gutter. The column at `cursor_col` (if any) uses `cursor_style` instead of
/// `base_style` so the selected cell is visually highlighted. Used for both the
/// header row (always `cursor_col = None`) and each data row.
fn grid_row_cell_spans(
    fields: &[String],
    widths: &[usize],
    base_style: Style,
    cursor_col: Option<usize>,
    cursor_style: Style,
) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (i, field) in fields.iter().enumerate() {
        if i > 0 {
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

/// Render styled `lines` into `block`'s inner area through `panel`, so the read
/// content wraps, scrolls and shows a scrollbar exactly like the collection
/// view's panels. Returns the inner text Rect and the scrollbar Rect (the
/// latter `Rect::default()` when no scrollbar is needed) so the caller can
/// record them for mouse hit-testing (text selection + scrollbar drag).
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
    // Size the box to fit the content, capping at the terminal height.
    let box_h = (content_lines.len() as u16 + 2)
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
fn report_base_dir(report: &Report) -> (std::path::PathBuf, bool) {
    if let Ok(flow) = report.flow()
        && let Some(r) = flow.header.root()
        && !r.trim().is_empty()
    {
        return (resolve_ref_path(report.path.as_deref(), r), true);
    }
    if let Some(dir) = report.path.as_deref().and_then(|p| p.parent()) {
        return (dir.to_path_buf(), true);
    }
    (std::env::current_dir().unwrap_or_default(), false)
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
            "{} · {} · {} · {} · {}",
            s.report_hint_edit,
            s.report_hint_run,
            s.report_hint_dry,
            s.report_hint_bind,
            s.report_hint_nodes
        );
        // Once a run has produced a grid, advertise `v` as the source↔output
        // swap (the discoverable replacement for the old Tab-into-results).
        if app.reports[idx].result.is_some() {
            hint.push_str(" · ");
            hint.push_str(s.report_hint_view);
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

fn draw_report_validation(
    f: &mut Frame,
    area: Rect,
    app: &mut TuiApp,
    idx: usize,
    s: &Strings,
    th: &Theme,
) {
    let lines: Vec<Line<'static>> = {
        let rt = &app.reports[idx];
        if let Some(err) = &rt.parse_error {
            vec![Line::from(Span::styled(
                err.clone(),
                Style::default().fg(th.err),
            ))]
        } else if rt.diagnostics.is_empty() {
            vec![Line::from(Span::styled(
                s.report_no_diagnostics,
                Style::default().fg(th.ok),
            ))]
        } else {
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
    };
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
        MouseHitTarget::Scroll(MouseScrollTarget::ReportPane(ReportPane::Validation)),
    );
}

/// Whether writing to `target` would physically escape workspace `root` once
/// symlinks are resolved. `target` (a not-yet-created file) is checked via its
/// **deepest existing ancestor**: the closest parent that exists on disk is
/// canonicalised and compared against the canonicalised `root`. A symlinked
/// directory component therefore fails the check even though a lexical `..`
/// scan would pass it, and any subfolders still to be created underneath a
/// real, in-root ancestor are inherently contained. Returns `false` (don't
/// block) when `root` can't be canonicalised — an open workspace root always
/// exists, so that only happens in degenerate cases where the later write will
/// surface the real error.
fn report_escapes_root(root: &std::path::Path, target: &std::path::Path) -> bool {
    let Ok(canon_root) = root.canonicalize() else {
        return false;
    };
    let mut ancestor = target;
    loop {
        if ancestor.exists() {
            return match ancestor.canonicalize() {
                Ok(real) => !real.starts_with(&canon_root),
                Err(_) => true,
            };
        }
        match ancestor.parent() {
            Some(parent) => ancestor = parent,
            None => return true,
        }
    }
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

        let csv = csv_export_path(&report);
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
            csv_export_path(&report),
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
