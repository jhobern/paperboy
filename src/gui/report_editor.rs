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
    self, DetachWhich, InsertPos, Modifier, NodeKind, RowKind, attach_modifier, detach_modifier,
    flatten, insert_node, insert_pos_after, move_node, node_at, parse_one_node, remove_node,
    replace_node, request_node, set_request_name,
};
use crate::report::flow::{FlowNode, ReportFlow, ReportStmt, WithItem};
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
    /// When `Some`, a node-configure wizard (request / envs / files) is open as
    /// a modal over the blocks view.
    pub wizard: Option<super::report_wizard::Wizard>,
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
            wizard: None,
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

    /// Replace the node at `path` with `node` (from a configure wizard),
    /// re-select it, and mirror the result back into the session — the wizard
    /// module's single commit path into the editor's private edit machinery.
    pub(super) fn wizard_apply(&mut self, app: &mut GuiApp, path: &[usize], node: FlowNode) {
        self.edit_flow(|flow| {
            replace_node(flow, path, node);
        });
        self.selection = path.to_vec();
        self.reseed_line = true;
        sync_back(self, app);
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

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 * (1.0 - t) + y as f32 * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// A colour for a palette block, by category — matches [`node_chips`] so a
/// palette chip reads the same colour as the block it inserts.
fn kind_color(kind: NodeKind, th: &GuiTheme) -> Color32 {
    match kind {
        NodeKind::Request | NodeKind::ReportRequest => th.ok,
        NodeKind::ReportVar => th.subst,
        NodeKind::Assign => th.accent,
        NodeKind::ForFiles | NodeKind::ForFolders | NodeKind::ForEnvs => th.accent,
        NodeKind::List => th.pending,
    }
}

/// Build the [`FlowNode`] a dropped/clicked palette `kind` inserts. Request
/// kinds pick up a default name from the bound collection (first request, else a
/// `request` placeholder shown amber until the user edits it inline).
fn node_for_kind(kind: NodeKind, titles: &[String]) -> FlowNode {
    if kind.needs_request() {
        let name = titles
            .first()
            .cloned()
            .unwrap_or_else(|| "request".to_string());
        request_node(&name, matches!(kind, NodeKind::ReportRequest))
    } else {
        kind.template()
            .unwrap_or_else(|| request_node("request", false))
    }
}

/// A drag payload for an *existing* in-report item (as opposed to the palette's
/// `NodeKind` / `Modifier` payloads for adding new ones). Dragging a base chip
/// carries its whole row (to reorder it onto a drop strip, or bin it); dragging
/// a modifier chip carries the detach it represents (to bin it off its node).
#[derive(Clone, PartialEq, Eq)]
enum DragItem {
    /// A whole block/row identified by its path — reorder or delete.
    Row(Vec<usize>),
    /// A modifier chip on the node at `path` — detach (bin) it.
    Chip {
        path: Vec<usize>,
        which: DetachWhich,
    },
}

/// One chip in a row's chip cluster (a compositional block is drawn as several
/// chips side by side). Exactly one chip per node is the *base* (the editable
/// subject: click it to select the row for inline editing); the rest are
/// modifier chips, each of which may carry a detach (`×`) action.
struct Chip {
    text: String,
    color: Color32,
    /// The editable subject chip (selects the row + drives the inline editor).
    is_base: bool,
    /// `Some(which)` shows a `×` that detaches this modifier from the node.
    detach: Option<DetachWhich>,
}

impl Chip {
    fn base(text: String, color: Color32) -> Chip {
        Chip {
            text,
            color,
            is_base: true,
            detach: None,
        }
    }
    fn modifier(text: String, color: Color32, which: DetachWhich) -> Chip {
        Chip {
            text,
            color,
            is_base: false,
            detach: Some(which),
        }
    }
    /// A modifier chip that cannot be detached (removing it would break the
    /// node, e.g. the required `AS` name of a computed column).
    fn fixed(text: String, color: Color32) -> Chip {
        Chip {
            text,
            color,
            is_base: false,
            detach: None,
        }
    }
}

/// Decompose a node into its chip cluster: the leading modifier chips, the
/// editable base chip, and any trailing modifier chips (`AS`, `WITH …`).
fn node_chips(node: &FlowNode, req_ok: Option<bool>, th: &GuiTheme) -> Vec<Chip> {
    let req_col = |ok: Option<bool>| match ok {
        Some(true) => th.ok,
        Some(false) => th.pending,
        None => th.ok,
    };
    match node {
        FlowNode::Request { name } => {
            vec![Chip::base(format!("REQUEST {name}"), req_col(req_ok))]
        }
        FlowNode::Report(ReportStmt::Request {
            name,
            alias,
            response_fmt,
            show,
            hide,
            with,
        }) => {
            let mut chips = vec![Chip::modifier(
                "REPORT".into(),
                th.subst,
                DetachWhich::Report,
            )];
            chips.push(Chip::base(format!("REQUEST {name}"), req_col(req_ok)));
            // RESPONSE / SHOW / HIDE are their own detachable chips so a long
            // reported request reads as a row of small, legible clauses rather
            // than one dense line.
            if let Some(fmt) = response_fmt {
                let text = match fmt {
                    crate::report::flow::ResponseFmt::Raw => "RESPONSE RAW",
                    crate::report::flow::ResponseFmt::Pretty => "RESPONSE PRETTY",
                };
                chips.push(Chip::modifier(text.into(), th.subst, DetachWhich::Response));
            }
            if !show.is_empty() {
                chips.push(Chip::modifier(
                    format!("SHOW({})", show.join(", ")),
                    th.subst,
                    DetachWhich::Show,
                ));
            }
            if !hide.is_empty() {
                chips.push(Chip::modifier(
                    format!("HIDE({})", hide.join(", ")),
                    th.subst,
                    DetachWhich::Hide,
                ));
            }
            if let Some(a) = alias {
                chips.push(Chip::modifier(format!("AS {a}"), th.subst, DetachWhich::As));
            }
            for (i, w) in with.iter().enumerate() {
                let text = match w {
                    WithItem::Field { name, .. } => format!("WITH {name}"),
                    WithItem::ResponseFmt(fmt) => format!(
                        "WITH RESPONSE {}",
                        match fmt {
                            crate::report::flow::ResponseFmt::Raw => "RAW",
                            crate::report::flow::ResponseFmt::Pretty => "PRETTY",
                        }
                    ),
                };
                chips.push(Chip::modifier(text, th.subst, DetachWhich::With(i)));
            }
            chips
        }
        FlowNode::Report(ReportStmt::Vars(vars)) => {
            let text = if vars.len() == 1 {
                vars[0].clone()
            } else {
                format!("({})", vars.join(", "))
            };
            vec![
                Chip::modifier("REPORT".into(), th.subst, DetachWhich::Report),
                Chip::base(text, th.subst),
            ]
        }
        FlowNode::Report(ReportStmt::VarAs { var, name, .. }) => vec![
            Chip::modifier("REPORT".into(), th.subst, DetachWhich::Report),
            Chip::base(var.clone(), th.subst),
            Chip::modifier(format!("AS {name}"), th.subst, DetachWhich::As),
        ],
        FlowNode::Report(ReportStmt::Computed { template, name, .. }) => vec![
            Chip::modifier("REPORT".into(), th.subst, DetachWhich::Report),
            Chip::base(format!("\"{template}\""), th.subst),
            // A computed column requires its AS name, so this chip is fixed.
            Chip::fixed(format!("AS {name}"), th.subst),
        ],
        FlowNode::Assign { .. } | FlowNode::ListDecl { .. } => {
            let col = if matches!(node, FlowNode::Assign { .. }) {
                th.accent
            } else {
                th.pending
            };
            vec![Chip::base(node.label(), col)]
        }
        FlowNode::ForEach { parallel, .. } | FlowNode::ForEnvs { parallel, .. } => {
            let mut chips = Vec::new();
            if let Some(spec) = parallel {
                let text = match spec.degree {
                    None => "PARALLEL".to_string(),
                    Some(n) => format!("PARALLEL({n})"),
                };
                // The head label already embeds the PARALLEL prefix; strip it so
                // it is not duplicated by the base chip below.
                chips.push(Chip::modifier(text, th.accent, DetachWhich::Parallel));
            }
            let full = node.label();
            let head = match parallel {
                Some(spec) => {
                    let prefix = match spec.degree {
                        None => "PARALLEL".to_string(),
                        Some(n) => format!("PARALLEL({n})"),
                    };
                    full.strip_prefix(&prefix)
                        .map(|s| s.trim_start().to_string())
                        .unwrap_or(full)
                }
                None => full,
            };
            // For an ENVS comparison loop, split the BASELINE/COMPARISON clause
            // off the head into their own chips so a long compare line reads as
            // legible parts. They are edited through the ENVS wizard, so the
            // chips are fixed (no detach ×) — the base chip becomes just the
            // `FOR … IN ENVS` opener.
            if let FlowNode::ForEnvs {
                clause:
                    crate::report::flow::EnvClause::Roles {
                        baseline,
                        comparisons,
                        ..
                    },
                var,
                ..
            } = node
            {
                chips.push(Chip::base(format!("FOR {var} IN ENVS"), th.accent));
                if !baseline.is_empty() {
                    chips.push(Chip::fixed(
                        format!("BASELINE({})", role_refs_text(baseline)),
                        th.pending,
                    ));
                }
                if !comparisons.is_empty() {
                    chips.push(Chip::fixed(
                        format!("COMPARISON({})", role_refs_text(comparisons)),
                        th.pending,
                    ));
                }
            } else {
                chips.push(Chip::base(head, th.accent));
            }
            chips
        }
    }
}

/// Render a list of environment role refs for a BASELINE/COMPARISON chip: a
/// bare env name, or `FILE("…")` for a saved-snapshot reference.
fn role_refs_text(refs: &[crate::report::flow::RoleRef]) -> String {
    use crate::report::flow::RoleRef;
    refs.iter()
        .map(|r| match r {
            RoleRef::Env(n) => n.clone(),
            RoleRef::File(p) => format!("FILE(\"{p}\")"),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// An action collected while rendering the (borrow-frozen) blocks, applied to
/// the editor afterwards.
enum Act {
    Select(Vec<usize>),
    OpenPalette(InsertPos),
    ClosePalette,
    PickKind(NodeKind),
    InsertRequest {
        report: bool,
        name: String,
    },
    MoveUp,
    MoveDown,
    Delete,
    CommitLine {
        path: Vec<usize>,
        text: String,
    },
    /// A palette block was dragged and dropped at `pos` (from the always-visible
    /// palette list). The node is built at drop time so request kinds pick up a
    /// default name from the bound collection.
    DropNode {
        pos: InsertPos,
        node: FlowNode,
    },
    /// A modifier chip (REPORT / PARALLEL / WITH / AS) was dragged onto the node
    /// at `path`, attaching it (see [`attach_modifier`]).
    AttachMod {
        path: Vec<usize>,
        modifier: Modifier,
    },
    /// A modifier chip's `×` was clicked, detaching it from the node at `path`
    /// (see [`detach_modifier`]).
    DetachMod {
        path: Vec<usize>,
        which: DetachWhich,
    },
    /// The request-name picker chose `name` for the node at `path`, renamed in
    /// place so a report request keeps its `AS` / `WITH` / `RESPONSE` modifiers.
    RenameRequest {
        path: Vec<usize>,
        name: String,
    },
    /// An existing block was dragged from `from` onto a drop strip, relocating
    /// it to `pos` (see [`edit::move_node_to`]).
    MoveNode {
        from: Vec<usize>,
        pos: InsertPos,
    },
    /// A block dragged onto the trash bin: delete the node at `path`.
    DeletePath(Vec<usize>),
    /// Open the configure wizard for the node at `path`.
    OpenWizard(Vec<usize>),
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

    // The node-configure wizard modal (opened by double-clicking a block on the
    // blocks view) floats above whichever view is showing.
    super::report_wizard::show(&mut ed, app, ui.ctx());

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

    // The insert palette popup (the click-based Add flow, with request-name
    // picking) is shown inline when open — complementary to the always-visible
    // drag palette below.
    if ed.palette.is_some() {
        palette_panel(ed, app, ui, &titles, &mut acts);
    }

    // ── Two panes: the always-visible drag palette (left) and the report's
    // stacked blocks (right). Blocks are dropped from the palette onto a row to
    // insert after it (onto a FOR header inserts inside it; onto Begin inserts
    // at the top). Reserve room for the diagnostics panel below.
    let diag_h = (ed.diagnostics.len().max(1) as f32 * 18.0 + 12.0).min(160.0);
    let body_h = (ui.available_height() - diag_h - 12.0).max(120.0);
    ui.allocate_ui(egui::vec2(ui.available_width(), body_h), |ui| {
        ui.horizontal_top(|ui| {
            const PALETTE_W: f32 = 168.0;
            ui.allocate_ui_with_layout(
                egui::vec2(PALETTE_W, body_h),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_min_width(PALETTE_W);
                    ui.set_max_width(PALETTE_W);
                    egui::ScrollArea::vertical()
                        .id_salt("pt_palette")
                        .auto_shrink([false, false])
                        .show(ui, |ui| palette_list(app, ui, &mut acts));
                },
            );
            ui.separator();
            ui.vertical(|ui| {
                egui::ScrollArea::vertical()
                    .id_salt("pt_blocks")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for (i, row) in rows.iter().enumerate() {
                            let selected = row.path == ed.selection
                                && (row.kind != RowKind::LoopEnd || ed.selection.is_empty());
                            let drop_pos = insert_pos_after(&rows, i);
                            block_row(ed, app, ui, row, i, selected, &drop_pos, &titles, &mut acts);
                        }
                    });
            });
        });
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

/// The base blocks the palette offers, in display order. Each drops in as a new
/// statement row. `ReportRequest` is intentionally absent — a reported request
/// is now composed by dropping the `REPORT` modifier onto a `REQUEST`.
const BASE_KINDS: [NodeKind; 7] = [
    NodeKind::Request,
    NodeKind::ReportVar,
    NodeKind::Assign,
    NodeKind::ForFiles,
    NodeKind::ForFolders,
    NodeKind::ForEnvs,
    NodeKind::List,
];

/// The always-visible palette, split into two groups: **Blocks** (base
/// statements, dragged into the gaps between rows to insert a new line) and
/// **Modifiers** (dragged *onto* a row to attach REPORT / PARALLEL / WITH / AS).
/// A trash bin at the foot deletes a block or detaches a modifier dropped on it.
/// Drag-only — the toolbar's "Add block" popup still covers click-based insert.
fn palette_list(app: &GuiApp, ui: &mut egui::Ui, acts: &mut Vec<Act>) {
    let th = app.theme;
    ui.label(
        RichText::new(app.strings.gui_report_palette_blocks)
            .strong()
            .color(th.text),
    );
    ui.colored_label(th.dim, app.strings.gui_report_palette_hint);
    ui.add_space(2.0);
    for (i, kind) in BASE_KINDS.into_iter().enumerate() {
        let base = kind_color(kind, &th);
        let id = ui.id().with(("pt_base_chip", i));
        ui.dnd_drag_source(id, kind, |ui| {
            palette_chip(ui, &th, kind.label(&app.strings), base);
        });
        ui.add_space(4.0);
    }

    ui.add_space(6.0);
    ui.label(
        RichText::new(app.strings.gui_report_palette_mods)
            .strong()
            .color(th.text),
    );
    ui.colored_label(th.dim, app.strings.gui_report_palette_mods_hint);
    ui.add_space(2.0);
    for (i, m) in Modifier::ALL.into_iter().enumerate() {
        let base = modifier_color(m, &th);
        let id = ui.id().with(("pt_mod_chip", i));
        ui.dnd_drag_source(id, m, |ui| {
            palette_chip(ui, &th, m.label(&app.strings), base);
        });
        ui.add_space(4.0);
    }

    ui.add_space(10.0);
    trash_bin(app, ui, acts);
}

/// The trash bin drop target at the foot of the palette. It reacts only to an
/// in-report [`DragItem`]: a dropped row is deleted, a dropped modifier chip is
/// detached from its node. Highlights while such a drag hovers it.
fn trash_bin(app: &GuiApp, ui: &mut egui::Ui, acts: &mut Vec<Act>) {
    let th = app.theme;
    let frame = egui::Frame::NONE
        .fill(mix(th.panel, th.err, 0.14))
        .stroke(egui::Stroke::new(1.0, mix(th.panel, th.err, 0.5)))
        .inner_margin(egui::Margin::symmetric(8, 8))
        .corner_radius(6);
    let resp = frame
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.add(
                    egui::Label::new(
                        RichText::new(format!(
                            "{} {}",
                            super::icons::TRASH,
                            app.strings.gui_report_trash
                        ))
                        .color(th.err),
                    )
                    .selectable(false),
                );
            });
        })
        .response;
    let zone = ui.interact(resp.rect, ui.id().with("pt_trash"), egui::Sense::hover());
    if zone.dnd_hover_payload::<DragItem>().is_some() {
        ui.painter().rect_stroke(
            resp.rect.expand(2.0),
            egui::CornerRadius::same(6),
            egui::Stroke::new(2.0, th.err),
            egui::StrokeKind::Outside,
        );
    }
    if let Some(item) = zone.dnd_release_payload::<DragItem>() {
        match &*item {
            DragItem::Row(path) => acts.push(Act::DeletePath(path.clone())),
            DragItem::Chip { path, which } => acts.push(Act::DetachMod {
                path: path.clone(),
                which: *which,
            }),
        }
    }
}

/// One rounded, category-tinted palette chip.
fn palette_chip(ui: &mut egui::Ui, th: &GuiTheme, text: &str, base: Color32) {
    let frame = egui::Frame::NONE
        .fill(mix(th.panel, base, 0.22))
        .stroke(egui::Stroke::new(1.0, mix(th.panel, base, 0.5)))
        .inner_margin(egui::Margin::symmetric(8, 4))
        .corner_radius(6);
    frame.show(ui, |ui| {
        ui.add(egui::Label::new(RichText::new(text).color(base)).selectable(false));
    });
}

/// The palette colour for a modifier chip.
fn modifier_color(m: Modifier, th: &GuiTheme) -> Color32 {
    match m {
        Modifier::Report | Modifier::With | Modifier::As => th.subst,
        Modifier::Parallel => th.accent,
    }
}

/// Render one flattened row as a horizontal cluster of chips (a compositional
/// block: the base subject plus any attached `REPORT` / `PARALLEL` / `WITH` /
/// `AS` modifier chips). The row is both a **modifier drop zone** (drop a
/// modifier chip onto it to attach) and sits above a **base insert strip** that
/// opens an animated gap when a base block is dragged over it. Leaf and loop
/// heads carry an inline single-line editor when selected.
#[allow(clippy::too_many_arguments)]
fn block_row(
    ed: &mut ReportEditor,
    app: &GuiApp,
    ui: &mut egui::Ui,
    row: &edit::NodeRow,
    row_index: usize,
    selected: bool,
    drop_pos: &InsertPos,
    titles: &[String],
    acts: &mut Vec<Act>,
) {
    let th = app.theme;
    // A cheap owned copy of the node so the chip cluster / applicability checks
    // don't hold a borrow of `ed` across the inline editor (which needs `&mut`).
    let node = ed
        .flow
        .as_ref()
        .and_then(|f| node_at(f, &row.path))
        .cloned();

    let inner = ui.horizontal(|ui| {
        ui.add_space(row.depth as f32 * 16.0);
        match row.kind {
            RowKind::Begin => static_chip(ui, &th, app.strings.report_node_begin, th.accent),
            RowKind::LoopEnd => static_chip(ui, &th, "END", th.accent),
            RowKind::Leaf | RowKind::LoopHead => {
                let chips = node
                    .as_ref()
                    .map(|n| node_chips(n, row.req_ok, &th))
                    .unwrap_or_default();
                for chip in &chips {
                    render_chip(ui, &th, chip, selected, &row.path, acts);
                }
            }
        }
    });
    let cluster = inner.response.rect;

    // ── Modifier drop zone: dropping a modifier chip onto a real node attaches
    // it. Only reacts to `Modifier` payloads, so it never competes with the base
    // insert strip below (which reacts to `NodeKind`).
    if let Some(n) = &node
        && matches!(row.kind, RowKind::Leaf | RowKind::LoopHead)
    {
        let zresp = ui.interact(
            cluster,
            ui.id().with(("pt_modzone", row_index)),
            egui::Sense::hover(),
        );
        if let Some(m) = zresp.dnd_hover_payload::<Modifier>()
            && m.applies_to(n)
        {
            ui.painter().rect_stroke(
                cluster.expand(2.0),
                egui::CornerRadius::same(6),
                egui::Stroke::new(2.0, th.accent),
                egui::StrokeKind::Outside,
            );
        }
        if let Some(m) = zresp.dnd_release_payload::<Modifier>()
            && m.applies_to(n)
        {
            acts.push(Act::AttachMod {
                path: row.path.clone(),
                modifier: *m,
            });
        }
    }

    // ── Base insert strip: a full-width strip over the row that, when a base
    // block is dragged over it, opens an animated gap below the row (the
    // existing blocks slide down to make room) with a dashed placeholder where
    // the new block will land. The strip is sized to include the currently-open
    // gap (read from last frame) so the pointer stays over it as the gap opens —
    // avoiding open/close flicker at the seam.
    const GAP_H: f32 = 30.0;
    let gap_id = ui.id().with(("pt_gap", row_index));
    let prev_gap: f32 = ui.ctx().data(|d| d.get_temp(gap_id)).unwrap_or(0.0);
    let strip = egui::Rect::from_x_y_ranges(
        ui.max_rect().x_range(),
        cluster.top()..=cluster.bottom() + prev_gap,
    );
    let strip_resp = ui.interact(
        strip,
        ui.id().with(("pt_drop", row_index)),
        egui::Sense::hover(),
    );
    // The strip opens its gap for either a palette base block (`NodeKind`) or an
    // existing block being dragged to a new home (`DragItem::Row`) — but not
    // when a row is hovering its own insert point (dropping there is a no-op).
    let hovering_new = strip_resp.dnd_hover_payload::<NodeKind>().is_some();
    let hovering_move = strip_resp
        .dnd_hover_payload::<DragItem>()
        .map(|d| matches!(&*d, DragItem::Row(from) if *from != row.path))
        .unwrap_or(false);
    let hovering_base = hovering_new || hovering_move;
    let gap =
        ui.ctx()
            .animate_value_with_time(gap_id, if hovering_base { GAP_H } else { 0.0 }, 0.12);
    ui.ctx().data_mut(|d| d.insert_temp(gap_id, gap));
    if let Some(kind) = strip_resp.dnd_release_payload::<NodeKind>() {
        acts.push(Act::DropNode {
            pos: drop_pos.clone(),
            node: node_for_kind(*kind, titles),
        });
    } else if let Some(item) = strip_resp.dnd_release_payload::<DragItem>() {
        if let DragItem::Row(from) = &*item
            && *from != row.path
        {
            acts.push(Act::MoveNode {
                from: from.clone(),
                pos: drop_pos.clone(),
            });
        }
    }
    if gap > 0.5 {
        let indent = row.depth as f32 * 16.0;
        let top = cluster.bottom() + 2.0;
        let ph = egui::Rect::from_min_max(
            egui::pos2(strip.left() + indent, top),
            egui::pos2(strip.right() - 8.0, top + gap - 4.0),
        );
        ui.painter().rect(
            ph,
            egui::CornerRadius::same(6),
            mix(th.panel, th.accent, 0.18),
            egui::Stroke::new(1.5, th.accent),
            egui::StrokeKind::Inside,
        );
        ui.add_space(gap);
    }

    // Inline editor for the selected concrete node.
    if selected && matches!(row.kind, RowKind::Leaf | RowKind::LoopHead) {
        ui.horizontal(|ui| {
            ui.add_space(row.depth as f32 * 16.0 + 8.0);
            inline_node_editor(ed, app, ui, row, titles, acts);
        });
    }
}

/// A plain, non-interactive tinted chip (the synthetic `Begin` / `END` rows).
fn static_chip(ui: &mut egui::Ui, th: &GuiTheme, text: &str, color: Color32) {
    egui::Frame::NONE
        .fill(mix(th.panel, color, 0.22))
        .stroke(egui::Stroke::new(1.0, mix(th.panel, color, 0.5)))
        .inner_margin(egui::Margin::symmetric(8, 3))
        .corner_radius(6)
        .show(ui, |ui| {
            ui.add(egui::Label::new(RichText::new(text).color(color)).selectable(false));
        });
}

/// Render one [`Chip`]. The base chip is click-to-select, double-click-to-open
/// the wizard, and a drag source that relocates or bins its whole row; a
/// modifier chip shows a `×` that detaches it and is itself a drag source that
/// bins the modifier.
fn render_chip(
    ui: &mut egui::Ui,
    th: &GuiTheme,
    chip: &Chip,
    selected: bool,
    path: &[usize],
    acts: &mut Vec<Act>,
) {
    let hot = chip.is_base && selected;
    let fill = if hot {
        th.select_bg
    } else {
        mix(th.panel, chip.color, 0.22)
    };
    let stroke = if hot {
        egui::Stroke::new(1.5, th.select_fg)
    } else {
        egui::Stroke::new(1.0, mix(th.panel, chip.color, 0.5))
    };
    let text_col = if hot { th.select_fg } else { chip.color };
    let resp = egui::Frame::NONE
        .fill(fill)
        .stroke(stroke)
        .inner_margin(egui::Margin::symmetric(8, 3))
        .corner_radius(6)
        .show(ui, |ui| {
            ui.add(egui::Label::new(RichText::new(&chip.text).color(text_col)).selectable(false));
            if let Some(which) = chip.detach {
                let x = ui.add(
                    egui::Button::new(RichText::new("×").color(text_col))
                        .small()
                        .frame(false),
                );
                if x.clicked() {
                    acts.push(Act::DetachMod {
                        path: path.to_vec(),
                        which,
                    });
                }
            }
        })
        .response;

    // Every chip is a drag source (base → its whole row; a detachable modifier →
    // itself), so it can be dragged onto a drop strip (reorder) or the trash bin.
    let sensed = resp.interact(egui::Sense::click_and_drag());
    if sensed.dragged() {
        let payload = if chip.is_base {
            DragItem::Row(path.to_vec())
        } else if let Some(which) = chip.detach {
            DragItem::Chip {
                path: path.to_vec(),
                which,
            }
        } else {
            DragItem::Row(path.to_vec())
        };
        sensed.dnd_set_drag_payload(payload);
    }
    if chip.is_base {
        if sensed.double_clicked() {
            acts.push(Act::OpenWizard(path.to_vec()));
        } else if sensed.clicked() {
            acts.push(Act::Select(path.to_vec()));
        }
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
                acts.push(Act::RenameRequest {
                    path: row.path.clone(),
                    name: name.clone(),
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
            Act::DropNode { pos, node } => {
                ed.edit_flow(|flow| insert_node(flow, &pos, node));
                let mut sel = pos.parent.clone();
                sel.push(pos.index);
                ed.selection = sel;
                ed.reseed_line = true;
            }
            Act::AttachMod { path, modifier } => {
                ed.edit_flow(|flow| {
                    attach_modifier(flow, &path, modifier);
                });
                ed.selection = path;
                ed.reseed_line = true;
            }
            Act::DetachMod { path, which } => {
                ed.edit_flow(|flow| {
                    if detach_modifier(flow, &path, which) {
                        remove_node(flow, &path);
                    }
                });
                ed.selection = Vec::new();
                ed.reseed_line = true;
            }
            Act::RenameRequest { path, name } => {
                ed.edit_flow(|flow| {
                    set_request_name(flow, &path, &name);
                });
                ed.selection = path;
                ed.reseed_line = true;
            }
            Act::MoveNode { from, pos } => {
                let mut new_sel = None;
                ed.edit_flow(|flow| {
                    new_sel = edit::move_node_to(flow, &from, &pos);
                });
                if let Some(ns) = new_sel {
                    ed.selection = ns;
                }
                ed.reseed_line = true;
            }
            Act::DeletePath(path) => {
                ed.edit_flow(|flow| {
                    remove_node(flow, &path);
                });
                ed.selection = Vec::new();
                ed.reseed_line = true;
            }
            Act::OpenWizard(path) => super::report_wizard::open(ed, app, &path),
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
