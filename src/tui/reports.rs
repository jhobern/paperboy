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

use super::app::{Overlay, Pane, TuiApp};
use super::draw::panel;
use super::editor::{Editor, apply_edit_key_full, render_editor_highlighted};
use super::new_request::draw_scrollbar;
use super::theme::Theme;
use crate::i18n::{Status, Strings};
use crate::report::Report;
use crate::report::flow::{FlowNode, Producer};
use crate::report::model::ReportResult;
use crate::report::run::{DryRunner, EntryRunner, LiveRunner, RunContext, run_flow};
use crate::report::validate::{Context, Diagnostic, Severity, validate};
use crate::report::writer::{CsvWriter, ReportWriter};

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
    /// The last run's output, if the report has been run this session. Rendered
    /// as a grid in [`ReportView::Results`] and the source of an `Export CSV`.
    pub(crate) result: Option<ReportResult>,
    /// Selection/scroll panel backing the results grid (clip-wrapped so each
    /// row stays on one line and columns line up, like program output).
    pub(crate) results_panel: MultiSelectPanel,
}

/// Which pane a report tab's body shows.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub(crate) enum ReportView {
    /// The PaperTrail flow source (editable) plus its live validation.
    #[default]
    Source,
    /// The grid of rows produced by the last run.
    Results,
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
            result: None,
            results_panel,
        }
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
        let idx = self.reports.len() - 1;
        self.revalidate_report(idx);
        self.save_state();
    }

    /// Find the loaded collection tab a report is bound to, by resolving the
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
                    let titles: Option<Vec<String>> =
                        self.resolve_bound_collection(&rt.report).map(|ci| {
                            self.collections[ci]
                                .entries
                                .iter()
                                .map(|e| e.title.clone())
                                .collect()
                        });
                    let env_names: Vec<String> =
                        self.global_envs.iter().map(|e| e.name.clone()).collect();
                    let ctx = Context {
                        request_titles: titles.as_deref(),
                        env_names: Some(&env_names),
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
    /// [`Self::report_run_blocker`]).
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

        let ctx = RunContext {
            entries: &self.collections[ci].entries,
            base_vars,
            named_envs,
            root,
            runner,
        };
        Ok(run_flow(&flow, &ctx))
    }

    /// Run the active report against its bound collection and show the results
    /// grid. Runs synchronously for now (the MVP cut); a run that fails to even
    /// start reports why in the status bar and the source view is kept.
    pub(crate) fn run_active_report(&mut self) {
        let Some(idx) = self.active_report_index() else {
            return;
        };
        // The live runner is rooted at the bound collection's directory so
        // relative form-file paths in its requests resolve as they would when
        // the request is sent by hand.
        let file_root = self
            .resolve_bound_collection(&self.reports[idx].report)
            .and_then(|ci| self.collections[ci].path.as_deref())
            .and_then(|p| p.parent())
            .map(std::path::Path::to_path_buf);
        let runner = LiveRunner { file_root };
        self.apply_report_run(idx, &runner);
    }

    /// Run report `idx` via `runner` and fold the outcome into the tab: on
    /// success, store the result, switch to the grid and report the row/error
    /// counts; on a blocked run, keep the source view and report why. Split out
    /// so tests can drive the full store/switch/status path with a fake runner
    /// (no network).
    pub(crate) fn apply_report_run(&mut self, idx: usize, runner: &dyn EntryRunner) {
        match self.run_report_flow(idx, runner) {
            Ok(result) => {
                let rows = result.rows.len();
                let errors = result.errors.len();
                let rt = &mut self.reports[idx];
                rt.result = Some(result);
                rt.view = ReportView::Results;
                rt.results_panel.set_scroll(0);
                self.status = Some(Status::ReportRunDone { rows, errors });
            }
            Err(reason) => self.status = Some(Status::ReportRunBlocked(reason)),
        }
    }

    /// Toggle the active report between the source view and the results grid.
    /// Flipping to the results view is a no-op when there's nothing to show.
    pub(crate) fn toggle_report_view(&mut self) {
        if let Some(idx) = self.active_report_index() {
            let rt = &mut self.reports[idx];
            rt.view = match rt.view {
                ReportView::Source if rt.result.is_some() => ReportView::Results,
                _ => ReportView::Source,
            };
        }
    }

    /// Export the active report's last run to a CSV file next to the report (or
    /// in the current directory for an unsaved scratch report), reporting the
    /// path — or the reason nothing was written — in the status bar.
    pub(crate) fn export_active_report_csv(&mut self) {
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
        let path = csv_export_path(&rt.report);
        match std::fs::write(&path, bytes) {
            Ok(()) => self.status = Some(Status::ReportExported(path.display().to_string())),
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
        // When the source panel has edit focus, keystrokes type into it rather
        // than acting as view shortcuts (Esc leaves edit focus).
        if let Some(idx) = self.active_report_index()
            && self.reports[idx].editor.is_some()
        {
            self.on_key_report_editing(key, idx);
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
            // Run the report against its bound collection and show the grid.
            KeyCode::Char('r') | KeyCode::F(5) => self.run_active_report(),
            // Dry-run: preview the projected rows/bindings without sending HTTP.
            KeyCode::Char('d') => self.open_report_dry_run(),
            // Flip between the source and the last run's results grid.
            KeyCode::Tab | KeyCode::Char('v') => self.toggle_report_view(),
            // Export the last run to CSV next to the report.
            KeyCode::Char('x') => self.export_active_report_csv(),
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
        // A pending ghost completion (only for a plain Right-arrow accept). This
        // is computed *before* the `&mut editor` borrow below, since it needs an
        // immutable borrow of `self` (the bound collection's request names).
        let completion = if key.code == KeyCode::Right && !ctrl && !shift {
            self.report_request_completion(idx)
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
                // Right arrow at end of a `REQUEST` line fills in the completion
                // (auto-quoting the name when it contains spaces).
                KeyCode::Right if completion.is_some() => {
                    accept_request_completion(editor, completion.as_ref().unwrap());
                    Some(editor.text())
                }
                // Everything else goes to the shared multi-line key handler,
                // which reports whether the text changed and any selection the
                // host should copy.
                _ => {
                    let resp = apply_edit_key_full(editor, key);
                    copy = resp.copy;
                    if resp.changed {
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

    /// The ghost suffix to offer while typing a `REQUEST <name>` (or
    /// `REPORT REQUEST <name>`) line in the source editor: the remainder of the
    /// first request title in the bound collection that the partially-typed
    /// The request-name completion to offer while typing a `REQUEST <name>` (or
    /// `REPORT REQUEST <name>`) line in the source editor: the first request
    /// title in the bound collection that the partially-typed name is a prefix
    /// of. `None` unless the cursor is at the end of such a line and a match
    /// exists. The report view can't show the collection's request list (it
    /// takes the whole body), so this keeps request names discoverable and
    /// correct.
    ///
    /// PaperTrail requires a name that contains spaces to be quoted
    /// (`REQUEST "Two Words"`), so completion is quote-aware and always yields a
    /// parseable line:
    /// - a bare fragment matching a space-free title completes bare;
    /// - a bare fragment matching a title *with* spaces auto-quotes it (the
    ///   opening quote is inserted before the fragment on accept, so typing
    ///   `Up` completes to `"Upload document"`);
    /// - inside an opened quote, any title completes and the closing quote is
    ///   appended.
    pub(crate) fn report_request_completion(&self, idx: usize) -> Option<RequestCompletion> {
        let rt = self.reports.get(idx)?;
        let editor = rt.editor.as_ref()?;
        let line = editor.lines.get(editor.row)?;
        // Only complete when the cursor sits at the very end of the line.
        if editor.col != line.chars().count() {
            return None;
        }
        let ci = self.resolve_bound_collection(&rt.report)?;
        let mut titles = self.collections[ci]
            .entries
            .iter()
            .map(|e| e.title.as_str());
        match request_name_partial(line)? {
            // Bare token: match any title (an exact match has nothing to add, so
            // require strictly longer), auto-quoting one that contains spaces.
            NamePartial::Bare(p) => {
                let t = titles.find(|t| t.len() > p.len() && t.starts_with(&p))?;
                let suffix = t[p.len()..].to_string();
                if t.chars().any(char::is_whitespace) {
                    // Show the plain suffix (so the ghost stays visually
                    // balanced); on accept, wrap the whole token in quotes.
                    Some(RequestCompletion {
                        ghost: suffix.clone(),
                        insert: format!("{suffix}\""),
                        wrap_quote: true,
                    })
                } else {
                    Some(RequestCompletion {
                        ghost: suffix.clone(),
                        insert: suffix,
                        wrap_quote: false,
                    })
                }
            }
            // Inside an opened quote: any title is fair game (spaces are fine),
            // and the completion appends the closing quote. An exact match still
            // completes — to just the closing quote.
            NamePartial::Quoted(p) => {
                let t = titles.find(|t| t.len() >= p.len() && t.starts_with(&p))?;
                let ghost = format!("{}\"", &t[p.len()..]);
                Some(RequestCompletion {
                    insert: ghost.clone(),
                    ghost,
                    wrap_quote: false,
                })
            }
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
    /// When set, an opening quote is inserted before the current bare name token
    /// on accept (auto-quoting a spaced name).
    pub(crate) wrap_quote: bool,
}

/// Apply `comp` to `ed`: optionally wrap the current bare name token in an
/// opening quote, then insert the completion text at the cursor.
fn accept_request_completion(ed: &mut Editor, comp: &RequestCompletion) {
    ed.clear_selection();
    if comp.wrap_quote {
        // Find the start of the bare token under the cursor (scan left over
        // non-whitespace) and insert the opening quote there.
        let row = ed.row;
        let chars: Vec<char> = ed.lines[row].chars().collect();
        let mut start = ed.col;
        while start > 0 && !chars[start - 1].is_whitespace() && chars[start - 1] != '"' {
            start -= 1;
        }
        let byte = Editor::byte_idx(&ed.lines[row], start);
        ed.lines[row].insert(byte, '"');
        ed.col += 1;
    }
    ed.insert_str(&comp.insert);
}

/// The partially-typed request name on a `REQUEST`/`REPORT REQUEST` line, and
/// whether the author has opened a quote (so a spaced name is being written).
enum NamePartial {
    /// A bare token with no quote and no internal whitespace.
    Bare(String),
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
    } else if name.is_empty() || name.chars().any(char::is_whitespace) {
        // Empty, or a bare token that already has spaces (unquoted → will fail
        // validation; don't paper over it with a completion).
        None
    } else {
        Some(NamePartial::Bare(name.to_string()))
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
fn csv_export_path(report: &Report) -> std::path::PathBuf {
    if let Some(path) = &report.path {
        return path.with_extension("csv");
    }
    let stem = sanitize_file_stem(&report.name);
    std::path::PathBuf::from(format!("{stem}.csv"))
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
        _ => {}
    }
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

    // The results grid is shown full-height (below the binding row) when the
    // user has flipped to it; otherwise the source + validation split.
    if app.reports[idx].view == ReportView::Results {
        let rows = Layout::vertical([Constraint::Length(3), Constraint::Min(3)]).split(area);
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
        Constraint::Length(3),
        Constraint::Min(3),
        Constraint::Length(diag_h),
    ])
    .split(area);

    draw_report_binding(f, rows[0], app, idx, s, th);
    draw_report_source(f, rows[1], app, idx, s, th);
    draw_report_validation(f, rows[2], app, idx, s, th);
}

/// Draw the results grid for report `idx` from its last run: a header row of
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
                let lines = report_grid_lines(result, &header, th);
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
    let block = panel(title, true, th);
    draw_report_panel(
        f,
        area,
        block,
        &mut app.reports[idx].results_panel,
        &lines,
        th,
    );
}

/// Build the grid's styled lines: a bold header row of the resolved column
/// headers followed by one line per row, each cell padded to its column's
/// width (capped) so the columns line up under [`WrapMode::Clip`]. Newlines in
/// a cell (e.g. a multi-line response body) are collapsed to a marker so a row
/// stays on one grid line.
fn report_grid_lines(
    result: &ReportResult,
    header: &crate::report::flow::Header,
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
    for row in &body {
        lines.push(grid_line(row, &widths, Style::default().fg(th.text)));
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
/// view's panels.
fn draw_report_panel(
    f: &mut Frame,
    area: Rect,
    block: Block<'static>,
    panel: &mut MultiSelectPanel,
    lines: &[Line<'static>],
    th: &Theme,
) {
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    panel.set_styled_content(lines, inner.width as usize);
    panel.clamp_scroll(inner.height);
    let visible = panel.visible_rows(inner.height);
    f.render_widget(
        Paragraph::new(visible).style(Style::default().fg(th.text)),
        inner,
    );
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
    }
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

fn draw_report_binding(
    f: &mut Frame,
    area: Rect,
    app: &TuiApp,
    idx: usize,
    s: &Strings,
    th: &Theme,
) {
    let rt = &app.reports[idx];
    let line = match app.resolve_bound_collection(&rt.report) {
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
    f.render_widget(
        Paragraph::new(line)
            .block(panel(s.report_binding_heading.to_string(), false, th))
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
            "{} · {} · {}",
            s.report_hint_edit, s.report_hint_run, s.report_hint_dry
        )
    };
    let title = format!("{} — {}", s.report_source_heading, hint);
    let block = panel(title, true, th);
    let error_line = app.reports[idx].parse_error_line;

    if editing {
        // Edit focus: render the live editor (with cursor) inside the panel,
        // keeping the same syntax highlighting as the read view.
        let inner = block.inner(area);
        f.render_widget(block, area);
        // A pending `REQUEST`-name completion, drawn dim after the cursor.
        let completion = app.report_request_completion(idx);
        if let Some(editor) = app.reports[idx].editor.as_ref() {
            render_editor_highlighted(f, inner, editor, th, |row, line| {
                super::report_highlight::highlight_row(row, line, error_line, th)
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
        super::report_highlight::highlight_source(trimmed, error_line, th)
    };
    draw_report_panel(
        f,
        area,
        block,
        &mut app.reports[idx].source_panel,
        &lines,
        th,
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
    draw_report_panel(
        f,
        area,
        block,
        &mut app.reports[idx].validation_panel,
        &lines,
        th,
    );
}
