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
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph, Wrap};
use tui_panel_select::{MultiSelectPanel, WrapMode};

use super::app::{Overlay, Pane, TuiApp};
use super::draw::panel;
use super::editor::{Editor, apply_edit_key_full, render_editor_highlighted};
use super::new_request::draw_scrollbar;
use super::theme::Theme;
use crate::i18n::{Status, Strings};
use crate::report::Report;
use crate::report::flow::{FlowNode, Header, Producer, ReportFlow};
use crate::report::model::{ReportResult, ReportRow, TARGET_COLUMN, parse_columns};
use crate::report::run::{
    DryRunner, EntryRunner, LiveRunner, RunContext, finalize, run_flow, run_flow_raw,
};
use crate::report::validate::{Context, Diagnostic, Severity, validate};
use crate::report::writer::{CsvWriter, ReportWriter};
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
    /// pending ones), the path→row-index lookup that routes each streamed row to
    /// its slot, and the prior result to restore if the run is cancelled.
    /// `None` when no run is streaming (never run / finished / cancelled).
    pub(crate) run_progress: Option<RunProgress>,
    /// Selection/scroll panel backing the results grid (clip-wrapped so each
    /// row stays on one line and columns line up, like program output).
    pub(crate) results_panel: MultiSelectPanel,
    /// When this report was opened from a Workspace tree, the workspace context
    /// pins that tree to the left of the report view (so the user can jump
    /// between the workspace's collections and reports without leaving). `None`
    /// for an ordinary (non-workspace) report tab.
    pub(crate) workspace: Option<ReportWorkspace>,
}

/// The Workspace context carried by a report opened from a Workspace tree: the
/// root folder, the browsed sub-path, and the tree cursor. Drives the pinned
/// left-hand tree in the report view (see [`super::draw`]); its rows are the
/// same filesystem file-tree the collection Workspace tab shows, so the user
/// can navigate folders and open other collections/reports from within the
/// report view.
#[derive(Clone)]
pub(crate) struct ReportWorkspace {
    /// The workspace root folder the tree is rooted at.
    pub(crate) root: std::path::PathBuf,
    /// Breadcrumb of subfolder names below `root` currently being browsed.
    pub(crate) browse: Vec<String>,
    /// The highlighted row in the tree (index into [`Self::rows`]; clamped on
    /// use).
    pub(crate) cursor: usize,
}

/// One row of the report view's pinned Workspace tree — the filesystem
/// file-tree of the browsed folder: `../` (unless at the root), subfolders,
/// then the collection and report files. Mirrors [`crate::collection::WsRow`]
/// but without inlined requests (the report view doesn't show a collection's
/// requests) and with the currently-open report marked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReportTreeRow {
    /// Go up to the parent folder (only when not at the workspace root).
    Up,
    /// Descend into an immediate subfolder.
    Folder(String),
    /// A collection file (`.hurl`/`.json`) — opens/activates a Workspace
    /// collection tab for it.
    Collection {
        path: std::path::PathBuf,
        name: String,
    },
    /// A report file (`.report`). `open` marks the report this view is showing.
    Report {
        path: std::path::PathBuf,
        name: String,
        open: bool,
    },
}

impl ReportWorkspace {
    /// Build the tree rows for the currently-browsed folder. `open_path` is the
    /// path of the report this view is showing (so its row is marked open).
    pub(crate) fn rows(&self, open_path: Option<&std::path::Path>) -> Vec<ReportTreeRow> {
        let mut dir = self.root.clone();
        for seg in &self.browse {
            dir.push(seg);
        }
        let mut rows = Vec::new();
        if !self.browse.is_empty() {
            rows.push(ReportTreeRow::Up);
        }
        for e in crate::workspace::list_dir(&dir, true) {
            if e.is_dir {
                rows.push(ReportTreeRow::Folder(e.display_name));
            } else if crate::workspace::is_report_file(&e.path) {
                let open = open_path == Some(e.path.as_path());
                rows.push(ReportTreeRow::Report {
                    path: e.path,
                    name: e.display_name,
                    open,
                });
            } else {
                rows.push(ReportTreeRow::Collection {
                    path: e.path,
                    name: e.display_name,
                });
            }
        }
        rows
    }
}

/// Per-tab live-streaming bookkeeping for an in-flight background report run.
/// The skeleton rows are stored on [`ReportTab::result`] (so the grid renders
/// them immediately, greyed); this tracks which have completed, how to route a
/// streamed row to its slot, and what to restore on cancel.
pub(crate) struct RunProgress {
    /// One flag per skeleton row (index-aligned with `result.rows`): `true` once
    /// that row's real result has streamed in. Pending (`false`) rows are drawn
    /// greyed so the grid doubles as a progress indicator.
    pub(crate) filled: Vec<bool>,
    /// Maps a row's structural [`ReportRow::path`] to its index in `result.rows`,
    /// so an out-of-order streamed row (under `PARALLEL`) still lands in the
    /// right slot.
    pub(crate) index: HashMap<Vec<(usize, usize)>, usize>,
    /// How many rows have been filled so far (for the progress status).
    pub(crate) done: usize,
    /// The result shown before this run started, restored verbatim if the run is
    /// cancelled (so a cancel discards the partial run and leaves the prior grid,
    /// matching the pre-streaming cancel semantics).
    pub(crate) prev_result: Option<ReportResult>,
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
        Self {
            report,
            diagnostics: Vec::new(),
            parse_error: None,
            parse_error_line: None,
            editor: None,
            source_panel: MultiSelectPanel::new(),
            validation_panel: MultiSelectPanel::new(),
            view: ReportView::Source,
            editor_view: ReportView::Source,
            node_selected: 0,
            node_undo: Vec::new(),
            result: None,
            run_progress: None,
            results_panel,
            workspace: None,
        }
    }

    /// A report tab opened from a Workspace tree: as [`Self::new`] but carrying
    /// the [`ReportWorkspace`] context that pins the workspace file-tree to the
    /// left of the report view.
    pub(crate) fn new_in_workspace(report: Report, workspace: ReportWorkspace) -> Self {
        let mut rt = Self::new(report);
        rt.workspace = Some(workspace);
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
    /// Total number of tabs across both kinds (collections first, then reports).
    pub(crate) fn tab_count(&self) -> usize {
        self.collections.len() + self.reports.len()
    }

    /// Whether the active tab is a report tab (its unified index falls past the
    /// collection tabs).
    pub(crate) fn active_is_report(&self) -> bool {
        self.active_tab >= self.collections.len() && !self.reports.is_empty()
    }

    /// The `self.reports` index of the active tab, if it is a report tab.
    pub(crate) fn active_report_index(&self) -> Option<usize> {
        if self.active_is_report() {
            Some(self.active_tab - self.collections.len())
        } else {
            None
        }
    }

    pub(crate) fn active_report(&self) -> Option<&ReportTab> {
        self.active_report_index().and_then(|i| self.reports.get(i))
    }

    /// Create a new scratch report tab, make it active, validate it and persist.
    pub(crate) fn new_report_tab(&mut self) {
        let s = Strings::for_language(&self.language);
        let report = Report::scratch(s.report_default_name);
        self.reports.push(ReportTab::new(report));
        self.active_tab = self.collections.len() + self.reports.len() - 1;
        self.focus = Pane::Tabs;
        self.report_tabbar_focus = false;
        let idx = self.reports.len() - 1;
        self.revalidate_report(idx);
        self.save_state();
    }

    /// Push an already-loaded [`Report`] (from a `.report` file or, later, git)
    /// as a new report tab, make it active, validate it and persist. Mirrors
    /// [`Self::new_report_tab`] but keeps the loaded report's provenance (path /
    /// git origin) so a subsequent "Save" writes back in place.
    pub(crate) fn open_loaded_report(&mut self, report: Report) {
        self.reports.push(ReportTab::new(report));
        self.active_tab = self.collections.len() + self.reports.len() - 1;
        self.focus = Pane::Tabs;
        self.report_tabbar_focus = false;
        let idx = self.reports.len() - 1;
        self.revalidate_report(idx);
        self.save_state();
        self.status = Some(Status::Loaded);
    }

    /// Open (or re-activate) a `.report` file selected from a Workspace tree.
    /// The report carries the workspace context so the report view pins that
    /// tree to its left. If a report tab for the same file is already open it is
    /// re-activated (with a refreshed workspace browse) instead of duplicated.
    /// `root`/`browse` come from the tab the report was opened from so the
    /// pinned tree opens focused on the report's own folder.
    pub(crate) fn open_workspace_report(
        &mut self,
        path: std::path::PathBuf,
        root: std::path::PathBuf,
        browse: Vec<String>,
    ) {
        let ws = ReportWorkspace {
            root,
            browse,
            cursor: 0,
        };
        // Re-activate an already-open tab for this file rather than opening a
        // second copy (matching how the collection tree re-opens a loaded file).
        if let Some(i) = self
            .reports
            .iter()
            .position(|rt| rt.report.path.as_deref() == Some(path.as_path()))
        {
            self.reports[i].workspace = Some(ws);
            self.active_tab = self.collections.len() + i;
            self.focus = Pane::Tabs;
            self.report_tabbar_focus = false;
            self.report_tree_focus = true;
            self.revalidate_report(i);
            self.save_state();
            self.status = Some(Status::Loaded);
            return;
        }
        match Report::load_local(&path) {
            Ok(report) => {
                self.reports.push(ReportTab::new_in_workspace(report, ws));
                self.active_tab = self.collections.len() + self.reports.len() - 1;
                self.focus = Pane::Tabs;
                self.report_tabbar_focus = false;
                self.report_tree_focus = true;
                let idx = self.reports.len() - 1;
                self.revalidate_report(idx);
                self.save_state();
                self.status = Some(Status::Loaded);
            }
            Err(e) => self.status = Some(Status::Error(e)),
        }
    }
    /// report's `# collection:` reference (relative to the report's own path
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
                    let ctx = Context {
                        request_titles: titles.as_deref(),
                        env_names: Some(&env_names),
                        request_fields: fields.as_deref(),
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
        // A second `r` while running is a cancel: flip the flag the worker's
        // runner checks between requests, so it winds down and its result is
        // discarded on arrival.
        if let Some(cancel) = self.running_reports.get(&report_id) {
            cancel.store(true, Ordering::Relaxed);
            self.status = Some(Status::ReportRunCancelled);
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

            // 2. Live run: stream each completed row through a `Sync` sink (the
            //    `PARALLEL` workers call it from several threads, and an
            //    `mpsc::Sender` is `Send` but not `Sync`, so it's wrapped in a
            //    `Mutex`). Rows may arrive out of iteration order under
            //    `PARALLEL`; their `path` still identifies the target slot.
            let runner = CancellableRunner {
                inner: make_runner(file_root),
                cancel: cancel_worker,
            };
            let row_tx = Mutex::new(tx.clone());
            let sink = move |row: &ReportRow| {
                if let Ok(tx) = row_tx.lock() {
                    let _ = tx.send(ReportRunUpdate::Row {
                        report_id,
                        row: Box::new(row.clone()),
                    });
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
    /// result. A run cancelled by the user discards its streamed rows and
    /// restores the prior grid (matching the pre-streaming cancel semantics).
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
                let prev_result = rt.result.take();
                rt.result = Some(result);
                rt.run_progress = Some(RunProgress {
                    filled: vec![false; n],
                    index,
                    done: 0,
                    prev_result,
                });
                // Show the (greyed) grid straight away so the run's shape/size
                // is visible before any request completes — unless the user is
                // mid-edit, in which case just stage it (they can flip with Tab).
                if rt.editor.is_none() {
                    rt.view = ReportView::Results;
                    rt.results_panel.set_scroll(0);
                    self.report_tabbar_focus = false;
                }
                self.status = Some(Status::ReportRunProgress { done: 0, total: n });
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
                    if !prog.filled[ri] {
                        prog.filled[ri] = true;
                        prog.done += 1;
                    }
                }
                let done = prog.done;
                let total = prog.filled.len();
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
                let progress = rt.run_progress.take();
                if cancelled {
                    // Discard the partial streamed run: restore whatever grid was
                    // showing before it started (may be nothing).
                    rt.result = progress.and_then(|p| p.prev_result);
                    if rt.result.is_none() && rt.view == ReportView::Results {
                        rt.view = rt.editor_view;
                    }
                    return;
                }
                let rows = result.rows.len();
                let errors = result.errors.len();
                rt.result = Some(result);
                if rt.editor.is_none() {
                    rt.view = ReportView::Results;
                    rt.results_panel.set_scroll(0);
                    self.report_tabbar_focus = false;
                }
                self.status = Some(Status::ReportRunDone { rows, errors });
            }
        }
    }

    /// Whether the run for `report_id` has been cancelled by the user (its
    /// cancel flag is set). A run with no live flag counts as cancelled/finished
    /// so a stray late message is ignored.
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
                rt.view = ReportView::Results;
                rt.results_panel.set_scroll(0);
                self.report_tabbar_focus = false;
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
        // `v` always lands on the body (the tab bar is only reached via `Tab`).
        self.report_tabbar_focus = false;
    }

    /// Toggle the active report between the source (text) editor and the
    /// structured node editor — the `n` key. Both are "editor" views over the
    /// same flow, so switching just re-renders the same AST a different way.
    pub(crate) fn toggle_report_nodes_view(&mut self) {
        if let Some(idx) = self.active_report_index() {
            // Committing any in-progress text edit first, so the node view sees
            // the latest source.
            if self.reports[idx].editor.is_some() {
                self.reports[idx].editor = None;
            }
            let rt = &mut self.reports[idx];
            rt.view = match rt.view {
                ReportView::Nodes => ReportView::Source,
                _ => ReportView::Nodes,
            };
            rt.editor_view = rt.view;
        }
        self.report_tabbar_focus = false;
    }

    /// Rotate keyboard focus across the report view's areas: (for a
    /// workspace-aware report) the pinned Workspace tree, then the active editor
    /// (source or nodes), the results grid, and the tab bar ("Tab List").
    /// Forward order is Tree → Editor → Results → Tab List → Tree; `forward ==
    /// false` reverses it. The Tree stop is present only for a workspace report;
    /// the Results stop is skipped when the report hasn't produced a grid yet.
    /// The body shown (`ReportView`) is kept in step with the focused body area
    /// so flipping to the grid and back is one continuous cycle; while the tree
    /// or tab bar is focused the body keeps showing whatever it last did.
    pub(crate) fn cycle_report_focus(&mut self, forward: bool) {
        #[derive(PartialEq, Clone, Copy)]
        enum Focus {
            Tree,
            Editor,
            Results,
            TabBar,
        }
        let Some(idx) = self.active_report_index() else {
            return;
        };
        let has_tree = self.reports[idx].workspace.is_some();
        let has_results = self.reports[idx].result.is_some();
        // Remember which editor (Source/Nodes) is showing so returning to the
        // body lands on the one the user last used.
        if self.reports[idx].view.is_editor() {
            self.reports[idx].editor_view = self.reports[idx].view;
        }
        // The ordered focus stops that exist for this report.
        let mut stops: Vec<Focus> = Vec::new();
        if has_tree {
            stops.push(Focus::Tree);
        }
        stops.push(Focus::Editor);
        if has_results {
            stops.push(Focus::Results);
        }
        stops.push(Focus::TabBar);
        // Where focus is now.
        let cur = if self.report_tree_focus && has_tree {
            Focus::Tree
        } else if self.report_tabbar_focus {
            Focus::TabBar
        } else if self.reports[idx].view == ReportView::Results {
            Focus::Results
        } else {
            Focus::Editor
        };
        let n = stops.len();
        let pos = stops.iter().position(|s| *s == cur).unwrap_or(0);
        let next = if forward {
            (pos + 1) % n
        } else {
            (pos + n - 1) % n
        };
        let target = stops[next];
        self.report_tree_focus = target == Focus::Tree;
        self.report_tabbar_focus = target == Focus::TabBar;
        match target {
            Focus::Editor => self.reports[idx].view = self.reports[idx].editor_view,
            Focus::Results => self.reports[idx].view = ReportView::Results,
            // Tree / TabBar leave the body showing whatever it last did.
            _ => {}
        }
    }

    /// Key handling while a workspace-aware report's pinned tree has focus.
    /// Arrows/`jk` move the cursor, `Enter`/`Right`/`l` open the selected row
    /// (descend a folder, open a collection tab, or load another report),
    /// `Left`/`Backspace`/`h` go up a folder. Returns `true` when it consumed
    /// the key; `false` lets it fall through to the shared report shortcuts
    /// (Tab, run, menus, …) so those keep working while the tree is focused.
    fn on_key_report_tree(&mut self, key: KeyEvent) -> bool {
        let Some(idx) = self.active_report_index() else {
            return false;
        };
        let open_path = self.reports[idx].report.path.clone();
        let Some(ws) = self.reports[idx].workspace.as_ref() else {
            return false;
        };
        let rows = ws.rows(open_path.as_deref());
        let len = rows.len();
        let cursor = ws.cursor.min(len.saturating_sub(1));
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(ws) = self.reports[idx].workspace.as_mut() {
                    ws.cursor = cursor.saturating_sub(1);
                }
                true
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(ws) = self.reports[idx].workspace.as_mut() {
                    ws.cursor = if len == 0 {
                        0
                    } else {
                        (cursor + 1).min(len - 1)
                    };
                }
                true
            }
            KeyCode::Home => {
                if let Some(ws) = self.reports[idx].workspace.as_mut() {
                    ws.cursor = 0;
                }
                true
            }
            KeyCode::End => {
                if let Some(ws) = self.reports[idx].workspace.as_mut() {
                    ws.cursor = len.saturating_sub(1);
                }
                true
            }
            KeyCode::Left | KeyCode::Backspace | KeyCode::Char('h') => {
                if let Some(ws) = self.reports[idx].workspace.as_mut()
                    && !ws.browse.is_empty()
                {
                    ws.browse.pop();
                    ws.cursor = 0;
                }
                true
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                self.activate_report_tree_row(idx, rows.into_iter().nth(cursor));
                true
            }
            _ => false,
        }
    }

    /// Act on the selected report-tree row: `../` and folders navigate the
    /// breadcrumb, a collection opens/activates a Workspace collection tab, and
    /// a report loads into (or re-activates) a report tab. A no-op if the row is
    /// gone (empty folder) or is the report already showing.
    fn activate_report_tree_row(&mut self, idx: usize, row: Option<ReportTreeRow>) {
        let Some(row) = row else {
            return;
        };
        match row {
            ReportTreeRow::Up => {
                if let Some(ws) = self.reports[idx].workspace.as_mut() {
                    ws.browse.pop();
                    ws.cursor = 0;
                }
            }
            ReportTreeRow::Folder(name) => {
                if let Some(ws) = self.reports[idx].workspace.as_mut() {
                    ws.browse.push(name);
                    ws.cursor = 0;
                }
            }
            ReportTreeRow::Report { path, open, .. } => {
                // Re-selecting the report already on screen does nothing.
                if open {
                    return;
                }
                let Some(ws) = self.reports[idx].workspace.as_ref() else {
                    return;
                };
                let root = ws.root.clone();
                let browse = ws.browse.clone();
                self.open_workspace_report(path, root, browse);
            }
            ReportTreeRow::Collection { path, .. } => {
                let Some(ws) = self.reports[idx].workspace.as_ref() else {
                    return;
                };
                let root = ws.root.clone();
                self.activate_report_tree_collection(root, path);
            }
        }
    }

    /// Open a collection selected from a report's Workspace tree: reuse an
    /// already-open Workspace collection tab rooted at the same folder if there
    /// is one, otherwise create one, then load the chosen file into it and
    /// switch to it (so the user jumps from the report back to its collections).
    fn activate_report_tree_collection(
        &mut self,
        root: std::path::PathBuf,
        path: std::path::PathBuf,
    ) {
        let ci = self
            .collections
            .iter()
            .position(|c| c.workspace_root.as_deref() == Some(root.as_path()));
        let ci = match ci {
            Some(ci) => ci,
            None => {
                let name = root
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| root.to_string_lossy().into_owned());
                let mut col = crate::collection::Collection::new(name, Vec::new());
                col.workspace_root = Some(root);
                self.collections.push(col);
                self.collections.len() - 1
            }
        };
        self.report_tree_focus = false;
        self.load_workspace_file(ci, path);
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

    /// Write the active report's last run to `path` as CSV, reporting the
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
        let bytes = CsvWriter.write(result, &header);
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
    /// and open a preview overlay summarising the projected row count, a sample
    /// of the first few iterations' resolved bindings, and any producer /
    /// request-resolution problems — so misaligned `ZIP`s, empty globs and
    /// Cartesian-product blow-ups are caught before firing real requests. A run
    /// that can't even be expanded (parse error / unbound collection) reports
    /// why in the status bar instead.
    pub(crate) fn open_report_dry_run(&mut self) {
        let Some(idx) = self.active_report_index() else {
            return;
        };
        match self.dry_run_report_flow(idx) {
            Ok(result) => {
                let names = self.reports[idx]
                    .report
                    .flow()
                    .map(|f| flow_local_names(&f.nodes))
                    .unwrap_or_default();
                let preview = DryRunReport::from_result(
                    &Strings::for_language(&self.language),
                    &result,
                    &names,
                );
                self.dry_run_scroll = 0;
                self.overlay = Some(Overlay::ReportDryRun(Box::new(preview)));
            }
            Err(reason) => self.status = Some(Status::ReportRunBlocked(reason)),
        }
    }

    /// Key handling for the report dry-run overlay ([`Overlay::ReportDryRun`]).
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
            self.reports[idx].editor = Some(Editor::new(&text, true));
        }
        // Editing focuses the body, so drop any tab-bar / tree focus.
        self.report_tabbar_focus = false;
        self.report_tree_focus = false;
    }

    /// Close the active report tab, remembering it for reopen (`u`) and moving
    /// focus to the previous tab. Unlike collection tabs, a report tab at any
    /// position is closable (only the built-in Request collection tab is fixed).
    pub(crate) fn close_active_report_tab(&mut self) {
        let Some(ridx) = self.active_report_index() else {
            return;
        };
        let rt = self.reports.remove(ridx);
        self.closed_tabs
            .push(super::app::ClosedTab::Report(ridx, Box::new(rt)));
        if self.closed_tabs.len() > 20 {
            self.closed_tabs.remove(0);
        }
        // Prefer staying on the neighbouring report; otherwise fall back to the
        // last collection tab. `active_tab` counts collections then reports.
        let base = self.collections.len();
        self.active_tab = if self.reports.is_empty() {
            base - 1
        } else {
            base + ridx.min(self.reports.len() - 1)
        };
        self.focus = Pane::Tabs;
        self.status = Some(crate::i18n::Status::TabClosed);
        self.save_state();
    }

    /// Key handling for the main view while a report tab is active. Kept
    /// separate from `on_key_normal` (which is full of `collections[active_tab]`
    /// accesses that would panic on a report's unified index) — the normal
    /// handler dispatches here at its very top. Only tab-navigation, global
    /// menu, and report-specific keys are honoured.
    pub(crate) fn on_key_report(&mut self, key: KeyEvent) {
        // A focused Workspace tree (workspace-aware report, not editing) drives
        // navigation/selection with the arrows + Enter; anything it doesn't
        // consume falls through to the shared shortcuts below (Tab, run, …) so
        // those still work while the tree is focused.
        if self.report_tree_focus
            && self.active_report_index().is_some_and(|i| {
                self.reports[i].workspace.is_some() && self.reports[i].editor.is_none()
            })
            && self.on_key_report_tree(key)
        {
            return;
        }
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
            && !self.report_tabbar_focus
            && self.on_key_report_nodes(key, idx)
        {
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Char('q') => self.request_quit(),
            // Tab navigation (mirrors the collection-view bindings).
            KeyCode::Char('[') | KeyCode::PageUp => self.cycle_tab(false),
            KeyCode::Char(']') | KeyCode::PageDown => self.cycle_tab(true),
            KeyCode::Left if ctrl && shift => self.move_active_tab(false),
            KeyCode::Right if ctrl && shift => self.move_active_tab(true),
            KeyCode::Left if ctrl => self.cycle_tab(false),
            KeyCode::Right if ctrl => self.cycle_tab(true),
            // Shift+Arrow adjusts the end of a mouse-started panel selection
            // (same as the collection view's body panels).
            KeyCode::Left if shift => self.extend_report_selection(KeyCode::Left),
            KeyCode::Right if shift => self.extend_report_selection(KeyCode::Right),
            KeyCode::Up if shift => self.extend_report_selection(KeyCode::Up),
            KeyCode::Down if shift => self.extend_report_selection(KeyCode::Down),
            // Plain Left/Right also move across tabs: the report view is
            // full-screen (no left/right panes to traverse), so — unlike the
            // collection view — arrows are free to drive tab navigation, and
            // users expect them to move *past* a report tab in the bar.
            KeyCode::Left => self.cycle_tab(false),
            KeyCode::Right => self.cycle_tab(true),
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
            // Report-specific: give the source panel edit focus.
            KeyCode::Char('e') | KeyCode::Enter => self.enter_report_edit(),
            // Toggle between the text (source) editor and the structured node
            // editor — two ways to edit the same flow.
            KeyCode::Char('n') => self.toggle_report_nodes_view(),
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
            // Tab rotates focus across the report's areas — Editor (source) →
            // Results grid → Tab List (the tab bar) → Editor — so the tab bar
            // is reachable from the keyboard without leaving the report. Shift+
            // Tab rotates the other way.
            KeyCode::Tab => self.cycle_report_focus(true),
            KeyCode::BackTab => self.cycle_report_focus(false),
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
                // Esc leaves edit focus (the live-applied text is kept).
                KeyCode::Esc => {
                    leave = true;
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
            super::clipboard::copy_to_clipboard(&text);
            self.status = Some(crate::i18n::Status::Copied);
        }
        if let Some(text) = new_text {
            if let Some(rt) = self.reports.get_mut(idx) {
                if leave {
                    rt.editor = None;
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

/// Build a [`RequestCompletion`] for `partial` against the first matching
/// `candidate`. When `always_quote` is set (env names, which must be quoted) or
/// the matched candidate contains whitespace, a bare fragment is auto-quoted:
/// the opening quote is inserted at the fragment's start column on accept and a
/// closing quote is appended. Inside an already-opened quote the closing quote
/// alone is appended.
fn complete_name(
    partial: &NamePartial,
    mut candidates: impl Iterator<Item = String>,
    always_quote: bool,
) -> Option<RequestCompletion> {
    match partial {
        NamePartial::Bare { text: p, start } => {
            let t = candidates.find(|t| t.len() > p.len() && t.starts_with(p.as_str()))?;
            let suffix = t[p.len()..].to_string();
            if always_quote || t.chars().any(char::is_whitespace) {
                // Show the plain suffix (so the ghost stays visually balanced);
                // on accept, wrap the whole name in quotes by inserting the
                // opening quote at its start column.
                Some(RequestCompletion {
                    ghost: suffix.clone(),
                    insert: format!("{suffix}\""),
                    quote_at: Some(*start),
                })
            } else {
                Some(RequestCompletion {
                    ghost: suffix.clone(),
                    insert: suffix,
                    quote_at: None,
                })
            }
        }
        NamePartial::Quoted(p) => {
            let t = candidates.find(|t| t.len() >= p.len() && t.starts_with(p.as_str()))?;
            let ghost = format!("{}\"", &t[p.len()..]);
            Some(RequestCompletion {
                insert: ghost.clone(),
                ghost,
                quote_at: None,
            })
        }
    }
}

/// A pending request-name completion in the source editor.
pub(crate) struct RequestCompletion {
    /// The dim text shown starting at the cursor.
    pub(crate) ghost: String,
    /// The text inserted at the cursor when the completion is accepted (may add
    /// a closing quote the ghost doesn't show, for the auto-quote case).
    pub(crate) insert: String,
    /// When `Some(col)`, an opening quote is inserted at this character column
    /// (the start of the bare name) on accept — auto-quoting a name that
    /// contains spaces so the completed line stays grammar-valid.
    pub(crate) quote_at: Option<usize>,
}

/// Apply `comp` to `ed`: optionally wrap the current bare name token in an
/// opening quote (at the recorded name-start column), then insert the
/// completion text at the cursor.
fn accept_request_completion(ed: &mut Editor, comp: &RequestCompletion) {
    ed.clear_selection();
    if let Some(col) = comp.quote_at {
        let byte = Editor::byte_idx(&ed.lines[ed.row], col);
        ed.lines[ed.row].insert(byte, '"');
        ed.col += 1;
    }
    ed.insert_str(&comp.insert);
}

/// The partially-typed request name on a `REQUEST`/`REPORT REQUEST` line, and
/// whether the author has opened a quote (so a spaced name is being written).
enum NamePartial {
    /// A bare (unquoted) token, along with the character column in the source
    /// line where the name begins (used to auto-quote a spaced completion).
    Bare { text: String, start: usize },
    /// The text after an as-yet-unclosed opening quote (may contain spaces).
    Quoted(String),
}

/// Move the editor cursor left to the start of the previous word: skip any
/// whitespace immediately to the left, then the run of non-whitespace. At
/// column 0 this falls back to a plain left move (wrapping to the previous
/// line), matching a normal cursor.
fn word_left(ed: &mut Editor) {
    if ed.col == 0 {
        ed.left();
        return;
    }
    let chars: Vec<char> = ed.lines[ed.row].chars().collect();
    let mut c = ed.col;
    while c > 0 && chars[c - 1].is_whitespace() {
        c -= 1;
    }
    while c > 0 && !chars[c - 1].is_whitespace() {
        c -= 1;
    }
    ed.col = c;
}

/// Move the editor cursor right past the current/next word: skip any whitespace
/// under the cursor, then the run of non-whitespace. At the line end this falls
/// back to a plain right move (wrapping to the next line).
fn word_right(ed: &mut Editor) {
    let len = ed.line_len(ed.row);
    if ed.col >= len {
        ed.right();
        return;
    }
    let chars: Vec<char> = ed.lines[ed.row].chars().collect();
    let mut c = ed.col;
    while c < len && chars[c].is_whitespace() {
        c += 1;
    }
    while c < len && !chars[c].is_whitespace() {
        c += 1;
    }
    ed.col = c;
}

/// One level of source indentation (matches the flow serializer's four spaces).
const INDENT_UNIT: &str = "    ";

/// The leading whitespace of `line`, as an owned string (used to inherit the
/// current line's indentation onto a freshly-inserted newline).
fn leading_ws(line: &str) -> String {
    line.chars().take_while(|c| c.is_whitespace()).collect()
}

/// Whether `line` opens a nested block — i.e. begins (after indentation) with
/// the `FOR` keyword. A newline after such a line gets one extra indent level.
fn opens_block(line: &str) -> bool {
    let t = line.trim_start();
    strip_keyword(t, "FOR").is_some()
}

/// If the editor's current line now reads exactly `END` (ignoring case and
/// surrounding whitespace), snap its indentation to that of the `FOR` it
/// closes, so finishing a block dedents one level. Idempotent: an `END` already
/// aligned to its `FOR` is left untouched. Does nothing when the `FOR`/`END`
/// nesting is unbalanced above the cursor. The cursor stays at the line end.
fn reindent_end_line(ed: &mut Editor) {
    let row = ed.row;
    let Some(line) = ed.lines.get(row) else {
        return;
    };
    if !line.trim().eq_ignore_ascii_case("END") {
        return;
    }
    // Walk upward tracking FOR/END balance to find the matching FOR.
    let mut depth = 0i32;
    let mut target: Option<String> = None;
    for prev in ed.lines[..row].iter().rev() {
        let t = prev.trim_start();
        if t.trim().eq_ignore_ascii_case("END") {
            depth += 1;
        } else if strip_keyword(t, "FOR").is_some() {
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
        let rest = &s[kw.len()..];
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
        Some(NamePartial::Quoted(inner.to_string()))
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
        Some(NamePartial::Quoted(inner.to_string()))
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
    if let Some(p) = tokened_output_path(report, "csv") {
        return p;
    }
    if let Some(path) = &report.path {
        return path.with_extension("csv");
    }
    let stem = sanitize_file_stem(&report.name);
    std::path::PathBuf::from(format!("{stem}.csv"))
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

/// How many sample iterations the dry-run preview lists before collapsing the
/// rest into an "… +N more" line.
const DRY_RUN_SAMPLE_CAP: usize = 12;

/// Preview state for the report dry-run overlay ([`Overlay::ReportDryRun`]): the
/// projected row count, a sample of the first few iterations' resolved
/// bindings, and any producer / request-resolution problems the expansion hit —
/// all computed by expanding the flow with a no-op runner (no HTTP). `scroll`
/// is the overlay's own vertical offset, so a long preview scrolls rather than
/// clipping silently.
pub(crate) struct DryRunReport {
    /// Total rows the flow would emit (`0` = nothing would run, e.g. an empty
    /// glob at the outermost loop).
    pub(crate) rows: usize,
    /// One line per sampled iteration (`FILE=a.jpg, PREFIX=…`), capped at
    /// [`DRY_RUN_SAMPLE_CAP`].
    pub(crate) samples: Vec<String>,
    /// How many rows beyond the sampled ones exist (drives the "… +N more"
    /// note); `0` when every row is shown.
    pub(crate) more: usize,
    /// Deduplicated producer / resolution problems (empty glob, ZIP length
    /// mismatch, unresolved request name, unloaded environment, …).
    pub(crate) errors: Vec<String>,
}

impl DryRunReport {
    /// Summarise an expanded [`ReportResult`] into the preview. `names` is the
    /// set of flow-defined variable names ([`flow_local_names`]) so each sample
    /// shows just the per-iteration bindings, hiding the inherited environment
    /// variables that also live in a row's `vars` snapshot.
    fn from_result(
        s: &Strings,
        result: &ReportResult,
        names: &std::collections::HashSet<String>,
    ) -> Self {
        let samples: Vec<String> = result
            .rows
            .iter()
            .take(DRY_RUN_SAMPLE_CAP)
            .map(|row| {
                let mut parts: Vec<(&String, &String)> = row
                    .vars
                    .iter()
                    .filter(|(k, _)| names.contains(k.as_str()))
                    .collect();
                parts.sort_by(|a, b| a.0.cmp(b.0));
                if parts.is_empty() {
                    s.report_dry_run_no_bindings.to_string()
                } else {
                    parts
                        .iter()
                        .map(|(k, v)| format!("{k}={v}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            })
            .collect();
        let more = result.rows.len().saturating_sub(samples.len());
        // A Cartesian product can repeat the same producer error on every
        // iteration, so collapse duplicates while keeping first-seen order.
        let mut seen = std::collections::HashSet::new();
        let errors: Vec<String> = result
            .errors
            .iter()
            .filter(|e| seen.insert((*e).clone()))
            .cloned()
            .collect();
        Self {
            rows: result.rows.len(),
            samples,
            more,
            errors,
        }
    }

    /// Render the preview body as themed lines (used by the overlay draw pass
    /// and, via its length, for scroll clamping).
    pub(crate) fn lines(&self, s: &Strings, th: &Theme) -> Vec<Line<'static>> {
        let mut lines: Vec<Line<'static>> = Vec::new();
        lines.push(Line::from(Span::styled(
            format!("{} {}", s.report_dry_run_rows, self.rows),
            Style::default().fg(th.accent).add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            s.report_dry_run_samples_heading.to_string(),
            Style::default().fg(th.text).add_modifier(Modifier::BOLD),
        )));
        if self.samples.is_empty() {
            lines.push(Line::from(Span::styled(
                s.report_dry_run_no_rows.to_string(),
                Style::default().fg(th.dim),
            )));
        } else {
            for (i, sample) in self.samples.iter().enumerate() {
                lines.push(Line::from(vec![
                    Span::styled(format!("#{}  ", i + 1), Style::default().fg(th.dim)),
                    Span::styled(sample.clone(), Style::default().fg(th.text)),
                ]));
            }
            if self.more > 0 {
                lines.push(Line::from(Span::styled(
                    format!("… +{} {}", self.more, s.report_dry_run_more),
                    Style::default().fg(th.dim),
                )));
            }
        }
        lines.push(Line::from(""));
        if self.errors.is_empty() {
            lines.push(Line::from(Span::styled(
                s.report_dry_run_no_problems.to_string(),
                Style::default().fg(th.accent),
            )));
        } else {
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
        lines
    }
}

/// Collect the flow-defined variable names — loop binders, `KEY=` assignments
/// (excluding the `PRELUDE_*` engine settings), `ENVS` vars and `FOLDERS … WITH`
/// role names — so the dry-run preview shows just those per-iteration bindings,
/// filtering out the inherited environment variables that also live in a row's
/// variable snapshot.
fn flow_local_names(nodes: &[FlowNode]) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    collect_flow_names(nodes, &mut out);
    out
}

fn collect_flow_names(nodes: &[FlowNode], out: &mut std::collections::HashSet<String>) {
    for node in nodes {
        match node {
            FlowNode::Assign { key, .. } if !key.starts_with("PRELUDE_") => {
                out.insert(key.clone());
            }
            FlowNode::ForEach {
                pattern,
                producer,
                body,
                ..
            } => {
                for n in pattern.named() {
                    out.insert(n.to_string());
                }
                collect_producer_names(producer, out);
                collect_flow_names(body, out);
            }
            FlowNode::ForEnvs { var, body, .. } => {
                out.insert(var.clone());
                collect_flow_names(body, out);
            }
            FlowNode::ListDecl { producer, .. } => collect_producer_names(producer, out),
            _ => {}
        }
    }
}

fn collect_producer_names(producer: &Producer, out: &mut std::collections::HashSet<String>) {
    match producer {
        Producer::Folders { roles, .. } => {
            for (role, _) in roles {
                out.insert(role.clone());
            }
        }
        Producer::Zip(inner) => inner.iter().for_each(|p| collect_producer_names(p, out)),
        Producer::Concat(inner) => inner.iter().for_each(|p| collect_producer_names(p, out)),
        _ => {}
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
            if rest.len() >= prefix.len() && rest[..prefix.len()].eq_ignore_ascii_case(&prefix) {
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

/// Draw a report tab's full-screen body: the binding status, the flow source
/// (syntax-highlighted, editable in place), and the live validation panel.
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

    // A workspace-aware report pins the workspace file-tree to the left; the
    // report body (binding + source/nodes/results) fills the rest. Split it off
    // first so the body layout below is unchanged, just narrower.
    let area = if app.reports[idx].workspace.is_some() {
        let cols = Layout::horizontal([Constraint::Length(app.list_width), Constraint::Min(20)])
            .split(area);
        draw_report_workspace_tree(f, cols[0], app, idx, s, th);
        cols[1]
    } else {
        area
    };

    // Reset the mouse hit-test areas each frame; the specific panel draws below
    // record the ones actually shown (a panel not drawn this frame stays
    // `Rect::default()`, so it can never be hit).
    app.report_pane_areas = [Rect::default(); 3];
    app.report_pane_bars = [Rect::default(); 3];

    // The results grid is shown full-height (below the binding row) when the
    // user has flipped to it; otherwise the source + validation split.
    if app.reports[idx].view == ReportView::Results {
        let rows = Layout::vertical([Constraint::Length(4), Constraint::Min(3)]).split(area);
        draw_report_binding(f, rows[0], app, idx, s, th);
        draw_report_results(f, rows[1], app, idx, s, th);
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
        Constraint::Length(4),
        Constraint::Min(3),
        Constraint::Length(diag_h),
    ])
    .split(area);

    draw_report_binding(f, rows[0], app, idx, s, th);
    if app.reports[idx].view == ReportView::Nodes {
        super::report_nodes::draw_report_nodes(f, rows[1], app, idx, s, th);
    } else {
        draw_report_source(f, rows[1], app, idx, s, th);
    }
    draw_report_validation(f, rows[2], app, idx, s, th);
}

/// Draw the pinned Workspace file-tree on the left of a workspace-aware report
/// view. Same filesystem tree the collection Workspace tab shows (`../`,
/// subfolders, collections, reports) but with the open report marked and no
/// inlined requests. Highlights the cursor row; the border is lit only while
/// the tree holds focus (`Tab` moves focus in/out). Selecting a row is handled
/// in the key layer (see [`TuiApp::on_key_report_tree`]).
fn draw_report_workspace_tree(
    f: &mut Frame,
    area: Rect,
    app: &mut TuiApp,
    idx: usize,
    s: &Strings,
    th: &Theme,
) {
    use super::draw::{COLLECTION_CLOSED_ICON, FOLDER_ICON, REPORT_ICON};
    let open_path = app.reports[idx].report.path.clone();
    let Some(ws) = &app.reports[idx].workspace else {
        return;
    };
    let rows = ws.rows(open_path.as_deref());
    let sel = ws.cursor.min(rows.len().saturating_sub(1));
    let items: Vec<ListItem> = rows
        .iter()
        .map(|row| match row {
            ReportTreeRow::Up => ListItem::new(Line::from(Span::styled(
                s.list_up_row.to_string(),
                Style::default().fg(th.dim),
            ))),
            ReportTreeRow::Folder(name) => ListItem::new(Line::from(Span::styled(
                format!("{FOLDER_ICON} {name}/"),
                Style::default().fg(th.accent).add_modifier(Modifier::BOLD),
            ))),
            ReportTreeRow::Collection { name, .. } => ListItem::new(Line::from(Span::styled(
                format!("{COLLECTION_CLOSED_ICON} {name}"),
                Style::default().fg(th.text).add_modifier(Modifier::BOLD),
            ))),
            ReportTreeRow::Report { name, open, .. } => {
                let style = if *open {
                    Style::default().fg(th.ok).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(th.accent)
                };
                ListItem::new(Line::from(Span::styled(
                    format!("{REPORT_ICON} {name}"),
                    style,
                )))
            }
        })
        .collect();
    let focused = app.report_tree_focus;
    let title = format!("{} — {}", s.report_workspace_heading, s.report_hint_tree);
    let mut state = ListState::default();
    if !rows.is_empty() {
        state.select(Some(sel));
    }
    let list = List::new(items)
        .block(panel(title, focused, th))
        .highlight_style(
            Style::default()
                .bg(th.accent)
                .fg(th.bg)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("› ");
    f.render_stateful_widget(list, area, &mut state);
}

/// resolved column names, then one clipped, column-aligned line per produced
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
                // While a run streams, grey the rows that haven't completed yet
                // so the grid doubles as a live progress indicator.
                let filled = rt.run_progress.as_ref().map(|p| p.filled.as_slice());
                let lines = report_grid_lines(result, &header, filled, th);
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
    // Dim the grid's border while the tab bar holds focus (Tab-list stop).
    let block = panel(title, !app.report_tabbar_focus, th);
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
}

/// Build the grid's styled lines: a bold header row of the resolved column
/// headers followed by one line per row, each cell padded to its column's
/// width (capped) so the columns line up under [`WrapMode::Clip`]. Newlines in
/// a cell (e.g. a multi-line response body) are collapsed to a marker so a row
/// stays on one grid line. When `filled` is `Some` (a run is streaming), rows
/// whose flag is `false` are drawn dimmed — the still-pending, placeholder rows
/// of the skeleton — so the grid doubles as a live progress indicator.
fn report_grid_lines(
    result: &ReportResult,
    header: &crate::report::flow::Header,
    filled: Option<&[bool]>,
    th: &Theme,
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

    // Per-column width = widest cell (header or body), capped so one wide cell
    // (a response body) can't push everything else off-screen.
    const MAX_COL: usize = 32;
    let widths: Vec<usize> = (0..columns.len())
        .map(|c| {
            let mut w = headers[c].chars().count();
            for row in &body {
                w = w.max(row[c].chars().count());
            }
            w.clamp(1, MAX_COL)
        })
        .collect();

    let mut lines = Vec::with_capacity(body.len() + 1);
    lines.push(grid_line(
        &headers,
        &widths,
        Style::default().fg(th.accent).add_modifier(Modifier::BOLD),
    ));
    for (i, row) in body.iter().enumerate() {
        // A row still awaiting its result (streaming) is dimmed; completed and
        // static (non-streaming) rows use the normal text colour.
        let pending = filled.map(|f| f.get(i) == Some(&false)).unwrap_or(false);
        let style = if pending {
            Style::default().fg(th.dim)
        } else {
            Style::default().fg(th.text)
        };
        lines.push(grid_line(row, &widths, style));
    }
    lines
}

/// Assemble one grid line: each field padded/truncated to its column width and
/// joined with a two-space gutter, styled uniformly.
fn grid_line(fields: &[String], widths: &[usize], style: Style) -> Line<'static> {
    let mut out = String::new();
    for (i, field) in fields.iter().enumerate() {
        if i > 0 {
            out.push_str("  ");
        }
        let w = widths[i];
        let count = field.chars().count();
        if count > w {
            // Truncate with an ellipsis so a clipped value reads as clipped.
            let take = w.saturating_sub(1);
            let mut s: String = field.chars().take(take).collect();
            s.push('…');
            out.push_str(&s);
        } else {
            out.push_str(field);
            out.extend(std::iter::repeat_n(' ', w - count));
        }
    }
    Line::from(Span::styled(out, style))
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

/// Draw the ghost completion `ghost` as dim text starting at the editor's
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
        format!(
            "{} · {} · {} · {} · {}",
            s.report_hint_edit,
            s.report_hint_run,
            s.report_hint_dry,
            s.report_hint_bind,
            s.report_hint_nodes
        )
    };
    let title = format!("{} — {}", s.report_source_heading, hint);
    // Dim the source panel's border when the tab bar has focus (Tab-list stop),
    // so the focused area is unambiguous; editing always keeps it lit.
    let block = panel(title, editing || !app.report_tabbar_focus, th);
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
}

#[cfg(test)]
mod export_path_tests {
    use super::*;

    #[test]
    fn time_token_name_overrides_a_saved_report_filename_and_stays_in_its_folder() {
        // A saved report whose file is `dfa.report` but whose name carries the
        // `{time}` token: the export must use the expanded name (not `dfa`) and
        // land next to the report file.
        let mut report = Report::from_text("dfa", "# name: run_{time}\n# collection: c.hurl\n");
        report.path = Some(std::path::PathBuf::from("/tmp/reports/dfa.report"));

        let csv = csv_export_path(&report);
        assert_eq!(csv.parent(), Some(std::path::Path::new("/tmp/reports")));
        let file = csv.file_name().unwrap().to_string_lossy().into_owned();
        assert!(file.starts_with("run_"), "expanded name used: {file}");
        assert!(file.ends_with(".csv"));
        assert!(!file.contains("{time}"), "token expanded: {file}");
        assert!(
            !file.starts_with("dfa"),
            "name wins over the file stem: {file}"
        );
    }

    #[test]
    fn without_a_token_a_saved_report_keeps_its_own_stem() {
        let mut report = Report::from_text("dfa", "# name: My Report\n# collection: c.hurl\n");
        report.path = Some(std::path::PathBuf::from("/tmp/reports/dfa.report"));
        assert_eq!(
            csv_export_path(&report),
            std::path::PathBuf::from("/tmp/reports/dfa.csv"),
            "unchanged behaviour when the name has no token"
        );
    }
}
