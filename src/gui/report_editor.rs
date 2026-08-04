//! The GUI PaperTrail report editor — a Scratch-style block editor over the
//! shared [`ReportFlow`] AST, plus a raw source view. Both views edit the *same*
//! report: every structural block edit re-serializes the AST back into the
//! report text via [`ReportFlow::to_text`], and the source view parses text back
//! into blocks, so they round-trip. All the heavy lifting — flattening the flow
//! into rows, inserting / moving / removing / replacing nodes, the node-kind
//! palette, and validation against the bound collection — is reused from the
//! front-end-agnostic [`crate::report::edit`] and [`crate::report::context`]
//! modules the terminal UI's node editor is built on.

use eframe::egui::{self, Color32, RichText};

use crate::i18n::Status;
use crate::report::Report;
use crate::report::context;
use crate::report::edit::{
    self, InsertPos, NodeKind, RowKind, flatten, insert_node, insert_pos_after, move_node, node_at,
    parse_one_node, remove_node, replace_node, request_node,
};
use crate::report::flow::{FlowNode, ReportFlow, ReportStmt};
use crate::report::model::ReportResult;
use crate::report::validate::{Diagnostic, Severity};

use super::app::GuiApp;
use super::report_run::{self, RowState, RunHandle, RunProgress};
use super::theme::GuiTheme;

/// Which of the two editor views is shown.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum EditorView {
    /// The Scratch-style stacked, nested, colour-coded blocks.
    Blocks,
    /// The raw `.trail` source text.
    Source,
    /// The results grid from the last (or in-flight) run.
    Results,
}

/// Where the editor's report is saved back to.
pub enum ReportOrigin {
    /// A report tab carried in the session (index into [`crate::session::Session::reports`]).
    Session(usize),
    /// A `.trail` file opened from a Workspace tree, saved to its own path.
    Workspace,
}

/// The GUI report editor's state. Owns a core [`Report`] (text = source of
/// truth) plus the parsed AST cache, current selection and per-view scratch.
pub struct ReportEditor {
    pub origin: ReportOrigin,
    pub report: Report,
    pub view: EditorView,
    /// Parsed AST cache, recomputed from `report.text` after every edit.
    pub flow: Option<ReportFlow>,
    pub parse_error: Option<String>,
    pub diagnostics: Vec<Diagnostic>,
    /// The selected node's path (a sequence of indices into nested loop
    /// bodies). Empty = the synthetic `Begin` root.
    pub selection: Vec<usize>,
    /// When `Some`, the insert palette is open, inserting at this position.
    pub palette: Option<PaletteState>,
    /// The inline "edit as line" buffer for the selected block: its path and the
    /// editable single-line form. Reset whenever the selection changes.
    pub line_buf: Option<(Vec<usize>, String)>,
    /// Snapshots of `report.text` for undo (Ctrl+Z), newest last.
    pub undo: Vec<String>,
    /// Set when the blocks view needs its `line_buf` reseeded (selection moved).
    reseed_line: bool,
    /// The last run's output (skeleton while streaming, finalized at the end),
    /// rendered as a grid in [`EditorView::Results`].
    pub result: Option<ReportResult>,
    /// Live streaming state while a run is in flight; `None` when idle.
    pub progress: Option<RunProgress>,
    /// The in-flight run's handle (cancel flag + update channel); `None` when idle.
    pub run: Option<RunHandle>,
    /// Whether the current `result` has been exported since it was produced.
    pub results_exported: bool,
}

/// The insert-palette popup state: where a new node lands, and whether we're on
/// the second step choosing a request name.
pub struct PaletteState {
    pub pos: InsertPos,
    /// `Some(report)` once a REQUEST / REPORT REQUEST kind is chosen and we're
    /// picking its name (`report` distinguishes the two).
    pub pick_request: Option<bool>,
    /// The free-text request name while picking (seeded from the first bound
    /// request, editable).
    pub request_name: String,
}

impl ReportEditor {
    /// Build an editor for a core [`Report`], parsing its text up front.
    pub fn new(origin: ReportOrigin, report: Report) -> Self {
        let mut ed = Self {
            origin,
            report,
            view: EditorView::Blocks,
            flow: None,
            parse_error: None,
            diagnostics: Vec::new(),
            selection: Vec::new(),
            palette: None,
            line_buf: None,
            undo: Vec::new(),
            reseed_line: true,
            result: None,
            progress: None,
            run: None,
            results_exported: false,
        };
        ed.reparse();
        ed
    }

    /// The report file path this editor is bound to, if any.
    pub fn path(&self) -> Option<&std::path::Path> {
        self.report.path.as_deref()
    }

    /// Whether this editor was opened from a Workspace tree (so it closes when
    /// the user navigates away in that tab).
    pub fn is_workspace(&self) -> bool {
        matches!(self.origin, ReportOrigin::Workspace)
    }

    /// Re-parse `report.text` into the AST cache and parse-error state.
    fn reparse(&mut self) {
        match self.report.flow() {
            Ok(flow) => {
                self.flow = Some(flow);
                self.parse_error = None;
            }
            Err(e) => {
                self.flow = None;
                self.parse_error = Some(e.to_string());
            }
        }
    }

    /// Set the report text (marking it dirty) and re-parse.
    fn set_text(&mut self, text: String) {
        self.report.set_text(text);
        self.reparse();
    }

    /// Push the current text onto the undo stack, then apply a structural edit
    /// to the AST via `f` and re-serialize. Keeps the two views round-tripping.
    fn edit_flow(&mut self, f: impl FnOnce(&mut ReportFlow)) {
        let Some(mut flow) = self.flow.clone() else {
            return;
        };
        self.undo.push(self.report.text.clone());
        f(&mut flow);
        self.set_text(flow.to_text());
    }

    /// Undo the last structural edit (or source change captured on the stack).
    fn undo(&mut self) {
        if let Some(prev) = self.undo.pop() {
            self.report.set_text(prev);
            self.reparse();
            self.reseed_line = true;
        }
    }

    /// Whether a run is currently in flight.
    fn is_running(&self) -> bool {
        self.run.as_ref().is_some_and(|h| !h.finished())
    }

    /// Whether the report can be run right now: it parses and carries no
    /// error-level diagnostics (an unbound collection is itself an error, so
    /// this also gates on binding).
    fn can_run(&self) -> bool {
        self.flow.is_some()
            && self
                .diagnostics
                .iter()
                .all(|d| d.severity != Severity::Error)
    }

    /// Start a background run of the current flow, switching to the Results view.
    /// A blocked run (unbound / no inputs) reports why in the status line.
    fn start_run(&mut self, app: &mut GuiApp) {
        let Some(flow) = self.flow.clone() else {
            return;
        };
        match context::report_run_inputs(
            &app.session.collections,
            &app.session.global_envs,
            app.session.active_env_id,
            &flow,
            self.report.path.as_deref(),
        ) {
            Ok(inputs) => {
                self.result = None;
                self.progress = None;
                self.results_exported = false;
                self.view = EditorView::Results;
                self.run = Some(report_run::spawn(inputs));
                app.session.status = Some(Status::ReportRunning);
            }
            Err(context::RunInputError::Unbound) => {
                app.session.status = Some(Status::ReportRunBlocked(
                    app.strings.report_run_unbound.to_string(),
                ));
            }
        }
    }

    /// Stop an in-flight run, retaining whatever partial grid has streamed in.
    fn stop_run(&mut self, app: &mut GuiApp) {
        if let Some(h) = &self.run {
            h.cancel();
        }
        self.progress = None;
        app.session.status = Some(Status::ReportRunStopped);
    }

    /// Drain the run channel this frame, folding streamed rows into the grid.
    /// Returns `true` while the run is still live (so the caller keeps
    /// repainting). Retires the handle once the run finishes or disconnects.
    fn poll_run(&mut self, app: &mut GuiApp) -> bool {
        let Some(handle) = self.run.as_mut() else {
            return false;
        };
        match report_run::drain(handle, &mut self.result, &mut self.progress) {
            report_run::Drained::Progress { done, total } => {
                app.session.status = Some(Status::ReportRunProgress { done, total });
            }
            report_run::Drained::Done { rows, errors } => {
                app.session.status = Some(Status::ReportRunDone { rows, errors });
            }
            report_run::Drained::Disconnected => {
                self.run = None;
                return false;
            }
            report_run::Drained::Idle => {}
        }
        if handle.finished() {
            self.run = None;
            false
        } else {
            true
        }
    }
}

/// A colour for a node's block, by category, blended toward a readable chip fill.
fn node_color(node: &FlowNode, req_ok: Option<bool>, th: &GuiTheme) -> Color32 {
    if let Some(ok) = req_ok {
        return if ok { th.ok } else { th.pending };
    }
    match node {
        FlowNode::ForEach { .. } | FlowNode::ForEnvs { .. } => th.accent,
        FlowNode::Request { .. } => th.ok,
        FlowNode::Report(ReportStmt::Request { .. }) => th.ok,
        FlowNode::Report(_) => th.subst,
        FlowNode::Assign { .. } => th.accent,
        FlowNode::ListDecl { .. } => th.pending,
    }
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 * (1.0 - t) + y as f32 * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// An action collected while rendering the (borrow-frozen) blocks, applied to
/// the editor afterwards.
enum Act {
    Select(Vec<usize>),
    OpenPalette(InsertPos),
    ClosePalette,
    PickKind(NodeKind),
    InsertRequest { report: bool, name: String },
    MoveUp,
    MoveDown,
    Delete,
    CommitLine { path: Vec<usize>, text: String },
}

pub fn ui(app: &mut GuiApp, ui: &mut egui::Ui) {
    // Take the editor out so we can freely borrow `app.session` alongside it.
    let Some(mut ed) = app.report_editor.take() else {
        return;
    };
    let th = app.theme;
    let mut close = false;

    // Fold any streamed run updates into the grid, and keep repainting while a
    // run is live so the grid fills in real time.
    let running = ed.poll_run(app);
    if running {
        ui.ctx().request_repaint();
    }

    // Recompute diagnostics against the current collections/envs each frame
    // (cheap; keeps the Run gate and panel live as the bound collection changes).
    ed.diagnostics = match &ed.flow {
        Some(flow) => context::report_diagnostics(
            &app.session.collections,
            &app.session.global_envs,
            app.session.active_env_id,
            flow,
            ed.report.path.as_deref(),
        ),
        None => Vec::new(),
    };

    // ── Header: name, dirty marker, Run / Save / Close ─────────────────────
    ui.horizontal(|ui| {
        let mut title = RichText::new(&ed.report.name).strong().color(th.text);
        if ed.report.dirty {
            title = RichText::new(format!("{} •", ed.report.name))
                .strong()
                .color(th.accent);
        }
        ui.label(title);

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .button(format!("{} {}", super::icons::CLOSE, app.strings.gui_close))
                .clicked()
            {
                close = true;
            }
            let save = ui.add_enabled(
                ed.report.dirty,
                egui::Button::new(format!("{} {}", super::icons::SAVE, app.strings.gui_save)),
            );
            if save.clicked() {
                save_report(&mut ed, app);
            }
            // Run toggles to Stop while a run is in flight.
            if ed.is_running() {
                if ui
                    .button(format!(
                        "{} {}",
                        super::icons::STOP,
                        app.strings.gui_report_stop
                    ))
                    .clicked()
                {
                    ed.stop_run(app);
                }
            } else {
                let run = ui.add_enabled(
                    ed.can_run(),
                    egui::Button::new(format!(
                        "{} {}",
                        super::icons::PLAY,
                        app.strings.gui_report_run
                    )),
                );
                if run.clicked() {
                    ed.start_run(app);
                }
            }
        });
    });

    // View toggle (Blocks | Source | Results).
    ui.horizontal(|ui| {
        if super::widgets::selectable(
            ui,
            ed.view == EditorView::Blocks,
            RichText::new(app.strings.gui_report_view_blocks),
        )
        .clicked()
        {
            ed.view = EditorView::Blocks;
        }
        if super::widgets::selectable(
            ui,
            ed.view == EditorView::Source,
            RichText::new(app.strings.gui_report_view_source),
        )
        .clicked()
        {
            ed.view = EditorView::Source;
        }
        if super::widgets::selectable(
            ui,
            ed.view == EditorView::Results,
            RichText::new(app.strings.gui_report_view_results),
        )
        .clicked()
        {
            ed.view = EditorView::Results;
        }
    });
    ui.separator();

    match ed.view {
        EditorView::Source => source_view(&mut ed, app, ui),
        EditorView::Blocks => blocks_view(&mut ed, app, ui),
        EditorView::Results => results_view(&mut ed, app, ui),
    }

    // Global keys: Ctrl+Z undo (both views); Delete on the blocks view is
    // handled inside `blocks_view` so it doesn't fire while typing.
    if ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Z)) {
        ed.undo();
    }

    if !close {
        app.report_editor = Some(ed);
    }
    // (A dropped `ed` cancels any in-flight run via `RunHandle`'s `Drop`.)
}

/// The raw `.trail` source editor + validation panel.
fn source_view(ed: &mut ReportEditor, app: &GuiApp, ui: &mut egui::Ui) {
    // Reserve room for the diagnostics panel at the bottom, then let the editor
    // fill the rest. Keeping the editor above avoids nesting egui panels inside
    // the centre panel (which egui 0.35 disallows in a `panel_frame` closure).
    let diag_h = (ed.diagnostics.len().max(1) as f32 * 18.0 + 12.0).min(160.0);
    let edit_h = (ui.available_height() - diag_h - 8.0).max(80.0);
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .max_height(edit_h)
        .show(ui, |ui| {
            let mut text = ed.report.text.clone();
            let resp = ui.add(
                egui::TextEdit::multiline(&mut text)
                    .code_editor()
                    .desired_width(f32::INFINITY)
                    .desired_rows(20),
            );
            if resp.changed() {
                // Snapshot for undo only when transitioning from a saved
                // baseline, to avoid one entry per keystroke.
                if ed.undo.last().map(String::as_str) != Some(ed.report.text.as_str()) {
                    ed.undo.push(ed.report.text.clone());
                }
                ed.set_text(text);
                ed.reseed_line = true;
            }
        });
    ui.separator();
    diagnostics_panel(ed, app, ui);
}

/// The results grid from the last (or in-flight) run, plus an Export button.
fn results_view(ed: &mut ReportEditor, app: &mut GuiApp, ui: &mut egui::Ui) {
    let th = app.theme;

    ui.horizontal(|ui| {
        if ed.is_running() {
            ui.colored_label(
                th.pending,
                format!(
                    "{} {}",
                    super::icons::RUNNING,
                    app.strings.gui_report_running
                ),
            );
        }
        if let Some(prog) = &ed.progress {
            ui.colored_label(th.dim, format!("{}/{}", prog.done, prog.total));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let has_rows = ed.result.as_ref().is_some_and(|r| !r.rows.is_empty());
            let export = ui.add_enabled(
                has_rows,
                egui::Button::new(format!(
                    "{} {}",
                    super::icons::EXPORT,
                    app.strings.gui_report_export
                )),
            );
            if export.clicked() {
                open_export_dialog(app);
            }
        });
    });
    ui.separator();

    let Some(result) = ed.result.as_ref() else {
        ui.add_space(8.0);
        ui.colored_label(th.dim, app.strings.gui_report_no_results);
        return;
    };

    // Resolve columns via the flow header's `columns:` directive (falling back
    // to the discovered column order when the flow doesn't parse or names none).
    let header = ed
        .flow
        .as_ref()
        .map(|f| f.header.clone())
        .unwrap_or_default();
    let columns = result.resolved_columns(&header);
    if columns.is_empty() {
        ui.add_space(8.0);
        ui.colored_label(th.dim, app.strings.gui_report_no_results);
        return;
    }

    // Run-level errors (unresolved requests, producer problems) above the grid.
    if !result.errors.is_empty() {
        for e in &result.errors {
            ui.colored_label(th.err, format!("{} {e}", super::icons::FAIL));
        }
        ui.add_space(2.0);
    }

    let states = ed.progress.as_ref().map(|p| p.states.as_slice());
    results_grid(app, ui, result, &columns, states);
}

/// Render the results as a scrollable table: a header row, one row per data row
/// (greyed/marked by its streaming [`RowState`]), then any STATISTICS summary
/// rows. Mirrors the TUI's `report_grid_lines` semantics.
fn results_grid(
    app: &GuiApp,
    ui: &mut egui::Ui,
    result: &ReportResult,
    columns: &[crate::report::model::OutputColumn],
    states: Option<&[RowState]>,
) {
    let th = app.theme;
    let show_icons = states.is_some();
    egui::ScrollArea::both()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            egui::Grid::new("report_results_grid")
                .striped(true)
                .spacing(egui::vec2(14.0, 3.0))
                .show(ui, |ui| {
                    // Header row.
                    if show_icons {
                        ui.label(" ");
                    }
                    for col in columns {
                        ui.label(RichText::new(&col.header).strong().color(th.accent));
                    }
                    ui.end_row();

                    // Data rows.
                    for (i, row) in result.rows.iter().enumerate() {
                        let state = states.and_then(|s| s.get(i)).copied();
                        if show_icons {
                            let (glyph, colour) = match state {
                                Some(RowState::Running) => (super::icons::RUNNING, th.pending),
                                Some(RowState::Finished) => (super::icons::PASS, th.ok),
                                _ => (super::icons::ROW_SCHEDULED, th.dim),
                            };
                            ui.colored_label(colour, glyph);
                        }
                        let text_col = match state {
                            Some(RowState::Running) => th.pending,
                            Some(RowState::Scheduled) => th.dim,
                            _ => th.text,
                        };
                        for col in columns {
                            let cell = flatten_cell(&col.value(row, &result.no_match_marker));
                            ui.label(RichText::new(truncate_cell(&cell)).color(text_col))
                                .on_hover_text(cell);
                        }
                        ui.end_row();
                    }

                    // STATISTICS summary rows (a footer, distinguished by italic accent).
                    for srow in result.summary_rows(columns) {
                        if show_icons {
                            ui.label(" ");
                        }
                        for (c, _col) in columns.iter().enumerate() {
                            let cell = flatten_cell(&srow.text_cell(c));
                            ui.label(
                                RichText::new(truncate_cell(&cell))
                                    .italics()
                                    .color(th.accent),
                            )
                            .on_hover_text(cell);
                        }
                        ui.end_row();
                    }
                });
        });
}

/// Collapse a cell's newlines to a single line (a response body can be huge).
fn flatten_cell(value: &str) -> String {
    if value.contains(['\n', '\r']) {
        value.replace("\r\n", "⏎").replace(['\n', '\r'], "⏎")
    } else {
        value.to_string()
    }
}

/// Per-cell display cap so one wide cell can't push the grid off-screen; the
/// full value is available on hover. Mirrors the TUI's `MAX_COL_WIDTH`.
fn truncate_cell(value: &str) -> String {
    const MAX: usize = 48;
    if value.chars().count() > MAX {
        let mut s: String = value.chars().take(MAX - 1).collect();
        s.push('…');
        s
    } else {
        value.to_string()
    }
}

/// Open the Save dialog for exporting the active report's results, defaulting to
/// a `.csv` beside the report (or in the current dir for a scratch report).
fn open_export_dialog(app: &mut GuiApp) {
    let default = app
        .report_editor
        .as_ref()
        .and_then(|e| e.report.path.as_ref())
        .map(|p| p.with_extension("csv"))
        .unwrap_or_else(|| std::path::PathBuf::from("report.csv"));
    app.dialog = Some(super::app::Dialog::SaveFile {
        kind: super::app::SaveKind::ReportResults,
        path: default.to_string_lossy().into_owned(),
        error: None,
    });
}

/// The Scratch-style block view: a toolbar plus the stacked, nested blocks.
fn blocks_view(ed: &mut ReportEditor, app: &mut GuiApp, ui: &mut egui::Ui) {
    let th = app.theme;

    if let Some(err) = &ed.parse_error {
        ui.add_space(6.0);
        ui.colored_label(th.err, app.strings.report_nodes_parse_error);
        ui.colored_label(th.dim, err);
        ui.separator();
        // Fall back to the source view's diagnostics panel so the user can fix it.
        diagnostics_panel(ed, app, ui);
        return;
    }
    let Some(flow) = ed.flow.clone() else {
        return;
    };

    // Which request names resolve in the bound collection (for green/amber).
    let bound = context::resolve_bound_collection(
        &app.session.collections,
        &flow,
        ed.report.path.as_deref(),
    );
    let titles: Vec<String> = bound
        .map(|ci| {
            app.session.collections[ci]
                .entries
                .iter()
                .map(|e| e.title.clone())
                .collect()
        })
        .unwrap_or_default();
    let resolves = |name: &str| titles.iter().any(|t| t == name);
    let rows = flatten(&flow, &resolves);

    // Clamp / reseed the inline line editor when the selection points at a
    // concrete editable node.
    if ed.reseed_line {
        ed.line_buf =
            node_at(&flow, &ed.selection).map(|n| (ed.selection.clone(), n.header_line()));
        ed.reseed_line = false;
    }

    let mut acts: Vec<Act> = Vec::new();

    // Toolbar for the current selection.
    ui.horizontal(|ui| {
        let sel_pos = rows
            .iter()
            .position(|r| r.path == ed.selection && r.kind != RowKind::LoopEnd)
            .or_else(|| rows.iter().position(|r| r.kind == RowKind::Begin))
            .unwrap_or(0);
        if ui
            .button(format!(
                "{} {}",
                super::icons::PLUS,
                app.strings.gui_report_add_block
            ))
            .clicked()
        {
            acts.push(Act::OpenPalette(insert_pos_after(&rows, sel_pos)));
        }
        let on_node = !ed.selection.is_empty();
        if ui
            .add_enabled(
                on_node,
                egui::Button::new(super::icons::CARET_UP.to_string()),
            )
            .on_hover_text(app.strings.gui_report_move_up)
            .clicked()
        {
            acts.push(Act::MoveUp);
        }
        if ui
            .add_enabled(
                on_node,
                egui::Button::new(super::icons::CARET_DOWN.to_string()),
            )
            .on_hover_text(app.strings.gui_report_move_down)
            .clicked()
        {
            acts.push(Act::MoveDown);
        }
        if ui
            .add_enabled(
                on_node,
                egui::Button::new(format!(
                    "{} {}",
                    super::icons::TRASH,
                    app.strings.gui_report_delete_block
                )),
            )
            .clicked()
        {
            acts.push(Act::Delete);
        }
    });
    ui.separator();

    // The insert palette, shown inline when open.
    if ed.palette.is_some() {
        palette_panel(ed, app, ui, &titles, &mut acts);
    }

    // The stacked blocks.
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for row in &rows {
                let selected = row.path == ed.selection
                    && (row.kind != RowKind::LoopEnd || ed.selection.is_empty());
                block_row(ed, app, ui, row, selected, &titles, &mut acts);
            }
        });

    // Delete key removes the selection (but not while a text field has focus).
    let typing = ui.memory(|m| m.focused().is_some());
    if !typing && !ed.selection.is_empty() && ui.input(|i| i.key_pressed(egui::Key::Delete)) {
        acts.push(Act::Delete);
    }

    ui.separator();
    diagnostics_panel(ed, app, ui);

    apply_block_actions(ed, app, acts);
    let _ = th;
}

/// Render one flattened row as a colour-coded, indented block. Leaf and loop
/// heads carry an inline single-line editor when selected.
fn block_row(
    ed: &mut ReportEditor,
    app: &GuiApp,
    ui: &mut egui::Ui,
    row: &edit::NodeRow,
    selected: bool,
    titles: &[String],
    acts: &mut Vec<Act>,
) {
    let th = app.theme;
    ui.horizontal(|ui| {
        ui.add_space(row.depth as f32 * 16.0);

        let (label, base) = match row.kind {
            RowKind::Begin => (app.strings.report_node_begin.to_string(), th.accent),
            RowKind::LoopEnd => ("END".to_string(), th.accent),
            RowKind::LoopHead => (row.label.clone(), th.accent),
            RowKind::Leaf => {
                let node = ed.flow.as_ref().and_then(|f| node_at(f, &row.path));
                let colour = node
                    .map(|n| node_color(n, row.req_ok, &th))
                    .unwrap_or(th.text);
                (row.label.clone(), colour)
            }
        };

        // The coloured chip: a rounded frame tinted by the block's category,
        // outlined in the selection colour when selected.
        let fill = if selected {
            th.select_bg
        } else {
            mix(th.panel, base, 0.22)
        };
        let stroke = if selected {
            egui::Stroke::new(1.5, th.select_fg)
        } else {
            egui::Stroke::new(1.0, mix(th.panel, base, 0.5))
        };
        let text_col = if selected { th.select_fg } else { base };
        let frame = egui::Frame::NONE
            .fill(fill)
            .stroke(stroke)
            .inner_margin(egui::Margin::symmetric(8, 3))
            .corner_radius(6);
        let resp = frame
            .show(ui, |ui| {
                ui.label(RichText::new(&label).color(text_col));
            })
            .response
            .interact(egui::Sense::click());
        if resp.clicked() {
            acts.push(Act::Select(row.path.clone()));
        }
    });

    // Inline editor for the selected concrete node.
    if selected && matches!(row.kind, RowKind::Leaf | RowKind::LoopHead) {
        ui.horizontal(|ui| {
            ui.add_space(row.depth as f32 * 16.0 + 8.0);
            inline_node_editor(ed, app, ui, row, titles, acts);
        });
    }
}

/// The inline editor shown under a selected block: a request-name picker for
/// request nodes, else the universal editable single-line form.
fn inline_node_editor(
    ed: &mut ReportEditor,
    app: &GuiApp,
    ui: &mut egui::Ui,
    row: &edit::NodeRow,
    titles: &[String],
    acts: &mut Vec<Act>,
) {
    let th = app.theme;
    let is_request = ed
        .flow
        .as_ref()
        .and_then(|f| node_at(f, &row.path))
        .map(|n| n.request_name().is_some())
        .unwrap_or(false);

    // A request node also offers a quick name picker seeded from the collection.
    if is_request && !titles.is_empty() {
        ui.label(RichText::new(app.strings.node_pick_request_title).color(th.dim));
        for name in titles {
            if ui.selectable_label(false, name).clicked() {
                let report = matches!(
                    ed.flow.as_ref().and_then(|f| node_at(f, &row.path)),
                    Some(FlowNode::Report(_))
                );
                acts.push(Act::CommitLine {
                    path: row.path.clone(),
                    text: request_node(name, report).header_line(),
                });
            }
        }
    }

    // The universal single-line editor (full grammar coverage).
    if let Some((path, buf)) = ed.line_buf.as_mut()
        && *path == row.path
    {
        let resp = ui.add(
            egui::TextEdit::singleline(buf)
                .desired_width(f32::INFINITY)
                .font(egui::TextStyle::Monospace),
        );
        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            acts.push(Act::CommitLine {
                path: row.path.clone(),
                text: buf.clone(),
            });
        }
    }
}

/// The insert palette: pick a node kind, then (for request kinds) a name.
fn palette_panel(
    ed: &mut ReportEditor,
    app: &GuiApp,
    ui: &mut egui::Ui,
    titles: &[String],
    acts: &mut Vec<Act>,
) {
    let th = app.theme;
    let s = &app.strings;
    egui::Frame::NONE
        .fill(th.raised())
        .stroke(egui::Stroke::new(1.0, th.accent))
        .inner_margin(8)
        .corner_radius(6)
        .show(ui, |ui| {
            let pick_request = ed.palette.as_ref().and_then(|p| p.pick_request);
            ui.horizontal(|ui| {
                let title = if pick_request.is_some() {
                    s.node_pick_request_title
                } else {
                    s.gui_report_add_block
                };
                ui.label(RichText::new(title).strong().color(th.text));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(super::icons::CLOSE.to_string()).clicked() {
                        acts.push(Act::ClosePalette);
                    }
                });
            });
            ui.separator();

            match pick_request {
                None => {
                    for kind in NodeKind::ALL {
                        if ui.selectable_label(false, kind.label(s)).clicked() {
                            acts.push(Act::PickKind(kind));
                        }
                    }
                }
                Some(report) => {
                    if titles.is_empty() {
                        ui.colored_label(th.dim, s.node_pick_request_none);
                    }
                    if let Some(p) = ed.palette.as_mut() {
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut p.request_name)
                                    .hint_text(s.node_pick_request_title)
                                    .desired_width(200.0),
                            );
                            if ui.button(s.gui_ok).clicked() && !p.request_name.trim().is_empty() {
                                acts.push(Act::InsertRequest {
                                    report,
                                    name: p.request_name.trim().to_string(),
                                });
                            }
                        });
                    }
                    for name in titles {
                        if ui.selectable_label(false, name).clicked() {
                            acts.push(Act::InsertRequest {
                                report,
                                name: name.clone(),
                            });
                        }
                    }
                }
            }
        });
}

/// The diagnostics panel: parse error line, then validation warnings/errors.
fn diagnostics_panel(ed: &ReportEditor, app: &GuiApp, ui: &mut egui::Ui) {
    let th = app.theme;
    ui.add_space(4.0);
    ui.label(
        RichText::new(app.strings.report_validation_heading)
            .strong()
            .color(th.text),
    );
    egui::ScrollArea::vertical()
        .id_salt("report_diags")
        .max_height(120.0)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if ed.diagnostics.is_empty() && ed.parse_error.is_none() {
                ui.colored_label(th.ok, app.strings.report_no_diagnostics);
                return;
            }
            if let Some(err) = &ed.parse_error {
                ui.colored_label(th.err, format!("{} {err}", super::icons::FAIL));
            }
            for d in &ed.diagnostics {
                let (icon, colour) = match d.severity {
                    Severity::Error => (super::icons::FAIL, th.err),
                    Severity::Warning => (super::icons::WARNING, th.pending),
                };
                ui.colored_label(colour, format!("{icon} {}", d.message));
            }
        });
}

/// Apply the collected block actions to the editor and session.
fn apply_block_actions(ed: &mut ReportEditor, app: &mut GuiApp, acts: Vec<Act>) {
    for act in acts {
        match act {
            Act::Select(path) => {
                ed.selection = path;
                ed.reseed_line = true;
            }
            Act::OpenPalette(pos) => {
                ed.palette = Some(PaletteState {
                    pos,
                    pick_request: None,
                    request_name: String::new(),
                });
            }
            Act::ClosePalette => ed.palette = None,
            Act::PickKind(kind) => {
                if kind.needs_request() {
                    if let Some(p) = ed.palette.as_mut() {
                        p.pick_request = Some(matches!(kind, NodeKind::ReportRequest));
                    }
                } else if let Some(node) = kind.template() {
                    insert_at_palette(ed, node);
                }
            }
            Act::InsertRequest { report, name } => {
                insert_at_palette(ed, request_node(&name, report));
            }
            Act::MoveUp | Act::MoveDown => {
                let up = matches!(act, Act::MoveUp);
                let path = ed.selection.clone();
                let mut new_sel = None;
                ed.edit_flow(|flow| {
                    new_sel = move_node(flow, &path, up);
                });
                if let Some(ns) = new_sel {
                    ed.selection = ns;
                }
                ed.reseed_line = true;
            }
            Act::Delete => {
                let path = ed.selection.clone();
                ed.edit_flow(|flow| {
                    remove_node(flow, &path);
                });
                ed.selection = Vec::new();
                ed.reseed_line = true;
            }
            Act::CommitLine { path, text } => {
                let prefer_loop = ed
                    .flow
                    .as_ref()
                    .and_then(|f| node_at(f, &path))
                    .map(FlowNode::is_loop)
                    .unwrap_or(false);
                if let Some(node) = parse_one_node(&text, prefer_loop) {
                    ed.edit_flow(|flow| {
                        replace_node(flow, &path, node);
                    });
                }
                ed.reseed_line = true;
            }
        }
    }
    sync_back(ed, app);
}

/// Insert `node` at the open palette's position, select it, and close the palette.
fn insert_at_palette(ed: &mut ReportEditor, node: FlowNode) {
    let Some(p) = ed.palette.take() else {
        return;
    };
    let pos = p.pos.clone();
    ed.edit_flow(|flow| insert_node(flow, &pos, node));
    // Select the newly inserted node.
    let mut sel = pos.parent.clone();
    sel.push(pos.index);
    ed.selection = sel;
    ed.reseed_line = true;
}

/// Mirror a Session-origin editor's text back into `session.reports` on every
/// edit so nothing is lost if the view is closed without an explicit Save.
fn sync_back(ed: &ReportEditor, app: &mut GuiApp) {
    if let ReportOrigin::Session(i) = ed.origin
        && let Some(r) = app.session.reports.get_mut(i)
    {
        r.text = ed.report.text.clone();
        r.name = ed.report.name.clone();
    }
}

/// Save the report: to disk when it has a path, and always back into the session.
fn save_report(ed: &mut ReportEditor, app: &mut GuiApp) {
    if let Some(path) = ed.report.path.clone() {
        if let Err(e) = ed.report.save_local(&path) {
            app.session.status = Some(crate::i18n::Status::Error(e));
            return;
        }
    } else {
        ed.report.dirty = false;
    }
    sync_back(ed, app);
    app.session.save();
    app.session.status = Some(crate::i18n::Status::Saved);
}
