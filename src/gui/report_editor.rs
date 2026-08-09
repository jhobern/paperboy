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
    self, CarriedMod, DetachWhich, HEADER_PLACEHOLDER, HeaderKind, HeaderSpec, InsertPos, Modifier,
    NodeKind, RowKind, attach_modifier, attach_to_node, carry_modifier, detach_modifier, flatten,
    header_specs, insert_node, insert_pos_after, move_node, node_at, remove_node, replace_node,
    report_assignment, request_node, set_request_name, transfer_modifier,
};
use crate::report::flow::{FlowNode, ReportFlow, ReportStmt, WithItem};
use crate::report::indent::{
    INDENT_UNIT, ReformatError, indent_for_new_line, is_end_line, matching_opener_indent,
};
use crate::report::model::ReportResult;
use crate::report::validate::{Diagnostic, Severity};

use crate::tui::report_highlight::{self, HlCtx};

use super::app::GuiApp;
use super::report_run::{self, ParkedRun, RowState, RunHandle, RunKey, RunProgress};
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
    /// 1-based line the parser rejected, so the Source view can mark exactly
    /// that line the way the terminal UI does.
    pub parse_error_line: Option<usize>,
    pub diagnostics: Vec<Diagnostic>,
    /// The [`context::diagnostics_fingerprint`] the current `diagnostics` were
    /// computed from. Validation is re-run only when this changes, so it happens
    /// on an edit rather than on every frame — see that function for why.
    diag_key: Option<u64>,
    /// The selected node's path (a sequence of indices into nested loop
    /// bodies). Empty = the synthetic `Begin` root.
    pub selection: Vec<usize>,
    /// When `Some`, the insert palette is open, inserting at this position.
    pub palette: Option<PaletteState>,
    /// Snapshots of `report.text` for undo (Ctrl+Z), newest last.
    pub undo: Vec<String>,
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
    /// Height (px) reserved for the validation panel at the bottom of the
    /// Blocks / Source views. User-adjustable by dragging the splitter above it
    /// (the GUI's stand-in for a fixed panel), so a report with many validation
    /// errors can be given as much room as needed.
    pub diag_h: f32,
    /// Width (px) of the palette column in the Blocks view. User-adjustable by
    /// dragging the divider between the palette and the block stack.
    pub palette_w: f32,
    /// When `Some`, the results cell inspector window is open, showing one
    /// cell's full (pretty-printed) value — the GUI stand-in for the TUI's
    /// result-cell popup, so a long/truncated cell can be read in full.
    pub inspector: Option<CellInspector>,
    /// When `Some`, the Results view is showing a dry run — what the flow
    /// *would* emit, worked out without sending a request — instead of the last
    /// real result.
    ///
    /// Held apart from [`Self::result`] rather than written into it, so a
    /// preview never destroys the results you actually ran for (and can't be
    /// exported as though it were them). Dismissing the preview brings them
    /// straight back.
    pub dry_run: Option<Box<crate::report::dry_run::DryRunReport>>,
    /// A toolbar button pressed this frame, run *after* the body has been drawn
    /// (see [`ToolbarAct`]).
    pending_toolbar: Option<ToolbarAct>,
}

/// A toolbar button press, deferred to the end of the frame that pressed it.
///
/// The header strip is drawn before the view below it, and the inline chip
/// fields in that view commit what has been typed into them when they lose
/// focus — which is the same frame the button click takes focus away. Acting
/// immediately therefore acted on the *pre-edit* report: Run ran the old flow
/// (and switched to Results, so the field was never redrawn and the typing was
/// simply dropped), and Save wrote the old text. Deferring to the end of the
/// frame lets the field commit first, so a button always acts on what is on
/// screen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ToolbarAct {
    Run,
    DryRun,
    Save,
    Close,
}

/// One results cell opened in the inspector window: the column header (title)
/// and the cell's full, unflattened (JSON-pretty-printed when applicable) value.
pub struct CellInspector {
    pub title: String,
    pub content: String,
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
            parse_error_line: None,
            diagnostics: Vec::new(),
            diag_key: None,
            selection: Vec::new(),
            palette: None,
            undo: Vec::new(),
            result: None,
            progress: None,
            run: None,
            results_exported: false,
            wizard: None,
            diag_h: 132.0,
            palette_w: 168.0,
            inspector: None,
            dry_run: None,
            pending_toolbar: None,
        };
        ed.reparse();
        ed
    }

    /// What this editor's run is filed under while the editor isn't on screen.
    pub fn run_key(&self) -> RunKey {
        RunKey::of(&self.report)
    }

    /// Lift the run out of the editor so it can outlive it.
    ///
    /// Called when the editor is closed or replaced. Everything about a *run*
    /// moves out; everything about the *view* (selection, palette, undo) goes
    /// with the editor, because it is rebuilt from the report text anyway.
    pub fn park_run(&mut self) -> ParkedRun {
        ParkedRun {
            result: self.result.take(),
            progress: self.progress.take(),
            run: self.run.take(),
            results_exported: self.results_exported,
        }
    }

    /// Take back a run parked earlier, so reopening a report shows the rows it
    /// collected while you were elsewhere — and keeps streaming if it is still
    /// going.
    pub fn adopt_run(&mut self, parked: ParkedRun) {
        self.result = parked.result;
        self.progress = parked.progress;
        self.run = parked.run;
        self.results_exported = parked.results_exported;
        if self.result.is_some() {
            self.view = EditorView::Results;
        }
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
                self.parse_error_line = None;
            }
            Err(e) => {
                self.flow = None;
                self.parse_error = Some(e.to_string());
                self.parse_error_line = Some(e.line);
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
    /// A no-op edit (one whose re-serialized text is unchanged) is dropped
    /// entirely — it neither marks the report dirty nor pushes an undo entry, so
    /// inline commits (e.g. blurring an `AS` field without changing it) are free.
    fn edit_flow(&mut self, f: impl FnOnce(&mut ReportFlow)) {
        let Some(mut flow) = self.flow.clone() else {
            return;
        };
        f(&mut flow);
        let new_text = flow.to_text();
        if new_text != self.report.text {
            self.undo.push(self.report.text.clone());
            self.set_text(new_text);
        }
    }

    /// Re-indent the source to its true block depth, as one undo step.
    ///
    /// Deliberately *not* routed through [`Self::edit_flow`]: that re-serializes
    /// the AST, which has nowhere to keep a body comment, so it would quietly
    /// delete them. [`indent::reformat`] only moves leading whitespace.
    /// `Ok(true)` when the text moved, `Ok(false)` when it was already tidy.
    /// The caller turns the outcome into a status so the message can be
    /// translated against the live language.
    fn reformat(&mut self) -> Result<bool, ReformatError> {
        match crate::report::indent::reformat(&self.report.text)? {
            Some(text) => {
                self.undo.push(self.report.text.clone());
                self.set_text(text);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Undo the last structural edit (or source change captured on the stack).
    fn undo(&mut self) {
        if let Some(prev) = self.undo.pop() {
            self.report.set_text(prev);
            self.reparse();
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
        sync_back(self, app);
    }

    /// Commit an arbitrary in-place flow mutation (undo-tracked) and mirror it
    /// back into the session — the wizard module's path for edits that tweak
    /// *part* of a node (e.g. a single `WITH` field) rather than replacing it
    /// wholesale like [`Self::wizard_apply`].
    pub(super) fn commit_edit(&mut self, app: &mut GuiApp, f: impl FnOnce(&mut ReportFlow)) {
        self.edit_flow(f);
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
                // A real run supersedes any preview of it.
                self.dry_run = None;
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

    /// Hand a finished preview to the Results view and switch to it.
    ///
    /// The Dry run button sits on the toolbar above *every* view, but the
    /// preview is rendered by the Results view — so without this the button
    /// does nothing visible when pressed from Blocks or Source, which is
    /// exactly where it is pressed from.
    fn show_preview(&mut self, preview: Box<crate::report::dry_run::DryRunReport>) {
        self.dry_run = Some(preview);
        self.view = EditorView::Results;
    }

    /// Expand the flow with no HTTP and show the preview in the Results view.
    ///
    /// Runs on the calling thread rather than through
    /// [`report_run::spawn`]'s worker: a dry run sends nothing, so it finishes
    /// in the time it takes to walk the flow, and making the user wait on a
    /// background thread for that would only add a frame of latency and a
    /// second run-state to reason about.
    fn start_dry_run(&mut self, app: &mut GuiApp) {
        use crate::report::run::{DryRunner, RunContext, run_flow_raw};

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
                let ctx = RunContext {
                    entries: &inputs.entries,
                    base_vars: inputs.base_vars.clone(),
                    named_envs: inputs.named_envs.clone(),
                    root: inputs.root.clone(),
                    runner: &DryRunner,
                    sink: None,
                };
                let result = run_flow_raw(&inputs.flow, &ctx);
                // The non-blocking variable-availability warnings are worth
                // showing here even though they never stop a run: a preview is
                // exactly when you want to hear about a `{{VAR}}` that might
                // not be set by the time its request goes out.
                let var_warnings: Vec<String> = self
                    .diagnostics
                    .iter()
                    .filter(|d| d.severity == Severity::Warning)
                    .map(|d| d.message.clone())
                    .collect();
                self.show_preview(Box::new(crate::report::dry_run::DryRunReport::from_result(
                    result,
                    flow.header.clone(),
                    var_warnings,
                )));
            }
            Err(context::RunInputError::Unbound) => {
                app.session.status = Some(Status::ReportRunBlocked(
                    app.strings.report_run_unbound.to_string(),
                ));
            }
        }
    }

    /// Stop an in-flight run, retaining whatever partial grid has streamed in.
    ///
    /// The handle is retired *now* — cancelled and dropped, taking our end of
    /// the channel with it — rather than left in place until the worker's `Done`
    /// arrives. Cancelling only stops new requests being fired; an in-flight one
    /// still has to finish, and a `PARALLEL` batch can take a while to wind
    /// down, during which `is_running()` would keep reporting the run as live
    /// and the Run button would stay a Stop button. Retiring immediately means
    /// the very next click starts a fresh run. The detached worker keeps
    /// draining in the background; its remaining messages land on a dropped
    /// receiver and are ignored. Mirrors the TUI's `prepare_report_run`.
    fn stop_run(&mut self, app: &mut GuiApp) {
        if let Some(h) = self.run.take() {
            h.cancel();
        }
        // Clear streaming progress so no row is left rendering as "running".
        // The partial grid in `self.result` is intentionally kept.
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
        NodeKind::ReportVar | NodeKind::ReportComputed => th.subst,
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
/// subject: click it to select the row, double-click to open its wizard); the
/// rest are modifier chips, each of which may carry a detach (`×`) action.
struct Chip {
    text: String,
    color: Color32,
    /// The editable subject chip (selects the row; its wizard opens on
    /// double-click; it is the row's drag handle for reordering).
    is_base: bool,
    /// `Some(which)` shows a `×` that detaches this modifier from the node.
    detach: Option<DetachWhich>,
    /// An optional inline dropdown replacing part of the chip label with a
    /// picker (e.g. the request name, or a BASELINE/COMPARISON environment).
    edit: ChipEdit,
    /// Hover help: what this chip is, and what its editable parts do. Empty for
    /// a chip whose meaning is already spelled out by its own label.
    help: &'static str,
    /// This chip qualifies the one immediately before it rather than the node
    /// as a whole, so it is drawn tucked against it and joined to it by a
    /// bracket. Without that, `BASELINE(prod) SHOW(Time) COMPARISON(stage)`
    /// reads as three peers in a row and the SHOW looks like it governs all of
    /// them.
    tethered: bool,
    /// Whether this chip is drawn merged with its neighbour into one segmented
    /// pill: the joined edge is squared off so the pair shares a single
    /// outline. Derived from [`Chip::tethered`] by [`link_tethers`] rather than
    /// set at construction, because it depends on the neighbours — and it is
    /// cleared again wherever the pair is being pulled apart, so a chip is
    /// never drawn joined to something that has left its slot.
    join_prev: bool,
    join_next: bool,
}

impl Chip {
    /// Mark this chip as belonging to the one before it (see
    /// [`Chip::tethered`]).
    fn tether(mut self) -> Chip {
        self.tethered = true;
        self
    }
}

impl Chip {
    /// Attach hover help, so the chip constructors stay short at their call
    /// sites (`Chip::modifier(…).with_help(s.chip_help_report)`).
    fn with_help(mut self, help: &'static str) -> Chip {
        self.help = help;
        self
    }
}

/// The inline picker a chip hosts, if any. The chip still shows a keyword prefix
/// (`REQUEST`, `BASELINE`, …) as a drag/select handle; the enumerable part
/// becomes a combo box the user picks from.
#[derive(Clone)]
enum ChipEdit {
    None,
    /// A request-name dropdown: the prefix is `REQUEST`, the combo lists the
    /// bound collection's request titles. Carries the current name.
    Request {
        name: String,
    },
    /// A `BASELINE`/`COMPARISON` environment dropdown for the role ref at
    /// `index` (an `Env(…)` ref); the combo lists the loaded environments.
    EnvRole {
        baseline: bool,
        index: usize,
        name: String,
    },
    /// An inline editable `AS <alias>` field: the prefix is `AS`, followed by a
    /// text box that commits the alias/name on blur.
    Alias {
        text: String,
    },
    /// The `PARALLEL` modifier's optional max-concurrency: the prefix is
    /// `PARALLEL`, followed by a small numeric box. Empty means "no explicit
    /// limit" (plain `PARALLEL`), which the runner resolves from the prelude's
    /// `MAX_PARALLEL`; `degree` carries the current value.
    Parallel {
        degree: Option<u32>,
    },
    /// A `FOR` loop head, broken into the parts of it that are worth editing in
    /// place: the loop variable, and — for the producers that draw from one
    /// path — that path, with a picker beside it, plus a `FILES` loop's `MATCH`
    /// glob.
    ///
    /// The point is legibility as much as convenience: a loop head rendered as
    /// one long label gave no hint that the folder or the variable name were
    /// things you could change, so both were only ever found through the
    /// wizard. Boxes look editable.
    Loop(LoopEdit),
}

/// The editable parts of a `FOR` loop head (see [`ChipEdit::Loop`]).
#[derive(Clone)]
struct LoopEdit {
    /// The loop variable, when the pattern binds exactly one name. `None` for a
    /// destructuring pattern (`FOR (A, B) IN …`) or a `_` discard, where one
    /// box could not say which binder it meant — those keep to the wizard.
    var: Option<String>,
    /// The keyword between the variable and the source: `IN FILES`, `IN ENVS`,
    /// `IN TUPLES FROM`, or plain `IN`.
    keyword: String,
    /// The single folder/file the loop draws from, and whether it is a file
    /// (`TUPLES FROM`) rather than a folder, which decides which picker opens.
    dir: Option<(String, bool)>,
    /// A `FILES` loop's `MATCH` glob. `Some("")` when the loop is a `FILES` one
    /// with no glob yet (so the box is offered); `None` when the producer has
    /// no glob at all.
    glob: Option<String>,
    /// Whatever the producer is when it can't be broken up — a list literal, a
    /// `ZIP`/`CONCAT`, or a named `LIST`. Shown as a plain label after the
    /// keyword.
    tail: String,
}

impl Chip {
    /// The chip's label and the extra width its inline editor occupies, for
    /// drawing a same-sized placeholder of it.
    ///
    /// Several chips keep their keyword in [`ChipEdit`] rather than in `text`
    /// (a `PARALLEL` chip's `text` is empty — its label and numeric box are both
    /// drawn by `parallel_chip`), so a ghost built from `text` alone came out a
    /// thin sliver rather than the size of the block that will land there.
    fn ghost_shape(&self) -> (String, f32) {
        match &self.edit {
            ChipEdit::None => (self.text.clone(), 0.0),
            ChipEdit::Request { name } => (format!("{} {name}", self.text), COMBO_CHIP_WIDTH),
            ChipEdit::EnvRole { name, .. } => (format!("{} {name}", self.text), COMBO_CHIP_WIDTH),
            ChipEdit::Alias { text } => (format!("AS {text}"), ALIAS_FIELD_WIDTH),
            ChipEdit::Parallel { degree } => (
                degree
                    .map(|n| format!("PARALLEL({n})"))
                    .unwrap_or_else(|| "PARALLEL".to_string()),
                PARALLEL_FIELD_WIDTH,
            ),
            ChipEdit::Loop(l) => {
                let mut label = "FOR".to_string();
                let mut extra = 0.0;
                if let Some(v) = &l.var {
                    label.push(' ');
                    label.push_str(v);
                    extra += LOOP_VAR_FIELD_WIDTH;
                }
                label.push(' ');
                label.push_str(&l.keyword);
                if let Some((dir, _)) = &l.dir {
                    label.push(' ');
                    label.push_str(dir);
                    // The box and the picker button beside it.
                    extra += LOOP_PATH_FIELD_WIDTH + PICKER_BUTTON_WIDTH;
                }
                if let Some(g) = &l.glob {
                    label.push_str(" MATCH ");
                    label.push_str(g);
                    extra += LOOP_GLOB_FIELD_WIDTH;
                }
                if !l.tail.is_empty() {
                    label.push(' ');
                    label.push_str(&l.tail);
                }
                (label, extra)
            }
        }
    }

    fn base(text: String, color: Color32) -> Chip {
        Chip {
            text,
            color,
            is_base: true,
            detach: None,
            edit: ChipEdit::None,
            help: "",
            tethered: false,
            join_prev: false,
            join_next: false,
        }
    }
    fn modifier(text: String, color: Color32, which: DetachWhich) -> Chip {
        Chip {
            text,
            color,
            is_base: false,
            detach: Some(which),
            edit: ChipEdit::None,
            help: "",
            tethered: false,
            join_prev: false,
            join_next: false,
        }
    }
    /// The request base chip, whose name is picked from an inline dropdown.
    fn request(name: &str, color: Color32) -> Chip {
        Chip {
            text: format!("REQUEST {name}"),
            color,
            is_base: true,
            detach: None,
            edit: ChipEdit::Request {
                name: name.to_string(),
            },
            help: "",
            tethered: false,
            join_prev: false,
            join_next: false,
        }
    }
    /// A `BASELINE`/`COMPARISON` chip carrying a single-environment dropdown.
    /// Detachable: dragging it out (or clicking its `×`) drops that role, which
    /// leaves the loop iterating whatever environments remain.
    fn env_role(baseline: bool, index: usize, name: &str, color: Color32) -> Chip {
        let kw = if baseline { "BASELINE" } else { "COMPARISON" };
        Chip {
            text: format!("{kw}({name})"),
            color,
            is_base: false,
            detach: Some(DetachWhich::Role { baseline, index }),
            edit: ChipEdit::EnvRole {
                baseline,
                index,
                name: name.to_string(),
            },
            help: "",
            tethered: false,
            join_prev: false,
            join_next: false,
        }
    }
    /// An `AS <alias>` chip whose alias is edited inline. `detach` is `Some(As)`
    /// for an optional alias (a report request / reported variable) and `None`
    /// for a required one (a computed column, whose `AS` name can't be removed).
    fn alias(text: &str, color: Color32, detach: Option<DetachWhich>) -> Chip {
        Chip {
            text: String::new(),
            color,
            is_base: false,
            detach,
            edit: ChipEdit::Alias {
                text: text.to_string(),
            },
            help: "",
            tethered: false,
            join_prev: false,
            join_next: false,
        }
    }
    /// A `FOR` loop head whose variable, folder and glob are edited inline.
    /// Still the row's base chip, so it stays the drag/select handle.
    fn loop_head(edit: LoopEdit, color: Color32) -> Chip {
        Chip {
            text: String::new(),
            color,
            is_base: true,
            detach: None,
            edit: ChipEdit::Loop(edit),
            help: "",
            tethered: false,
            join_prev: false,
            join_next: false,
        }
    }
    /// A `PARALLEL` chip whose concurrency limit is edited inline.
    fn parallel(degree: Option<u32>, color: Color32) -> Chip {
        Chip {
            text: String::new(),
            color,
            is_base: false,
            detach: Some(DetachWhich::Parallel),
            edit: ChipEdit::Parallel { degree },
            help: "",
            tethered: false,
            join_prev: false,
            join_next: false,
        }
    }
}

/// Break a `FOR` loop head into the parts the chip edits in place.
///
/// `envs` marks the `FOR … IN ENVS` form, whose source is a clause rendered as
/// its own `BASELINE`/`COMPARISON` chips rather than a path, so it contributes
/// only the variable and the keyword.
fn loop_edit_parts(node: &FlowNode, envs: bool) -> LoopEdit {
    use crate::report::flow::{Binder, Producer};

    // Only a pattern binding exactly one name can be edited through one box.
    let var = match node {
        FlowNode::ForEnvs { var, .. } => Some(var.clone()),
        FlowNode::ForEach { pattern, .. } => match (pattern.rest, pattern.binders.as_slice()) {
            (false, [Binder::Named(n)]) => Some(n.clone()),
            _ => None,
        },
        _ => None,
    };
    if envs {
        // A `Roles` clause is drawn as its own BASELINE/COMPARISON chips beside
        // this one, so the head carries nothing more. A `Plain` list of
        // environment names has no chips of its own and would otherwise vanish,
        // so it is shown as the tail.
        let tail = match node {
            FlowNode::ForEnvs {
                clause: crate::report::flow::EnvClause::Plain(names),
                ..
            } => names
                .iter()
                .map(|n| format!("\"{n}\""))
                .collect::<Vec<_>>()
                .join(", "),
            _ => String::new(),
        };
        return LoopEdit {
            var,
            keyword: "IN ENVS".to_string(),
            dir: None,
            glob: None,
            tail,
        };
    }
    let FlowNode::ForEach { producer, .. } = node else {
        return LoopEdit {
            var,
            keyword: "IN".to_string(),
            dir: None,
            glob: None,
            tail: String::new(),
        };
    };
    match producer {
        // `FILES` always offers the glob box, even when the loop has no MATCH
        // yet: an empty box invites the glob, where nothing at all hides that
        // the loop can be narrowed.
        Producer::Files { dir, glob } => LoopEdit {
            var,
            keyword: "IN FILES".to_string(),
            dir: Some((dir.clone(), false)),
            glob: Some(glob.clone().unwrap_or_default()),
            tail: String::new(),
        },
        // `FOLDERS` takes the same `MATCH` glob as `FILES` (matching the folder
        // name, and recursing when it contains `**`), so it gets the same box.
        // Its `WITH role="glob"` list is several named globs rather than one, so
        // that part stays in the wizard and is shown as the tail.
        Producer::Folders { dir, glob, roles } => LoopEdit {
            var,
            keyword: "IN FOLDERS".to_string(),
            dir: Some((dir.clone(), false)),
            glob: Some(glob.clone().unwrap_or_default()),
            tail: if roles.is_empty() {
                String::new()
            } else {
                let rs: Vec<String> = roles.iter().map(role_label).collect();
                format!("WITH {}", rs.join(", "))
            },
        },
        Producer::Tuples { path } => LoopEdit {
            var,
            keyword: "IN TUPLES FROM".to_string(),
            // A file, so the picker opens a file dialog rather than a folder one.
            dir: Some((path.clone(), true)),
            glob: None,
            tail: String::new(),
        },
        // No single path to point at: shown whole, as before.
        other => LoopEdit {
            var,
            keyword: "IN".to_string(),
            dir: None,
            glob: None,
            tail: producer_label(other),
        },
    }
}

/// A producer with no single path, rendered as the label the loop head used to
/// carry. Mirrors `flow::producer_text`, which is private to the model.
/// A single `WITH` role binding as it is written in the source, so the chip's
/// tail reads back exactly what the file says — including the `?` that marks a
/// role as optional.
fn role_label(r: &crate::report::flow::RoleBinding) -> String {
    let opt = if r.optional { "?" } else { "" };
    format!("{}=\"{}\"{opt}", r.name, r.glob)
}

fn producer_label(p: &crate::report::flow::Producer) -> String {
    use crate::report::flow::{Element, Producer};
    fn element(e: &Element) -> String {
        match e {
            Element::Scalar(s) => format!("\"{s}\""),
            Element::Tuple(parts) => {
                let items: Vec<String> = parts.iter().map(|s| format!("\"{s}\"")).collect();
                format!("({})", items.join(", "))
            }
        }
    }
    match p {
        Producer::List(elems) => {
            let items: Vec<String> = elems.iter().map(element).collect();
            format!("[{}]", items.join(", "))
        }
        Producer::Zip(ps) => {
            let items: Vec<String> = ps.iter().map(producer_label).collect();
            format!("ZIP({})", items.join(", "))
        }
        Producer::Concat(ps) => {
            let items: Vec<String> = ps.iter().map(producer_label).collect();
            format!("CONCAT({})", items.join(", "))
        }
        Producer::Named(n) => n.clone(),
        Producer::Files { dir, glob } => match glob {
            Some(g) => format!("FILES \"{dir}\" MATCH \"{g}\""),
            None => format!("FILES \"{dir}\""),
        },
        Producer::Folders { dir, glob, .. } => match glob {
            Some(g) => format!("FOLDERS \"{dir}\" MATCH \"{g}\""),
            None => format!("FOLDERS \"{dir}\""),
        },
        Producer::Tuples { path } => format!("TUPLES FROM \"{path}\""),
    }
}

/// Decompose a node into its chip cluster: the leading modifier chips, the
/// editable base chip, and any trailing modifier chips (`AS`, `WITH …`).
fn node_chips(
    node: &FlowNode,
    req_ok: Option<bool>,
    th: &GuiTheme,
    s: &crate::i18n::Strings,
) -> Vec<Chip> {
    let req_col = |ok: Option<bool>| match ok {
        Some(true) => th.ok,
        Some(false) => th.pending,
        None => th.ok,
    };
    let mut chips = build_node_chips(node, req_col(req_ok), th, s);
    // The rule for whether a chip can be pulled out of a line on its own: only
    // if the statement still stands without it. `REPORT` on a reported *column*
    // is the case that matters — take it away and there is no statement left at
    // all — so grabbing that chip has to move the whole row instead of
    // half-deleting it. Applied centrally rather than at each construction site
    // so it can't be forgotten for a chip added later.
    for chip in &mut chips {
        if let Some(which) = chip.detach
            && !crate::report::edit::detach_leaves_statement(node, which)
        {
            chip.detach = None;
        }
    }
    chips
}

/// The chips a node is drawn as, before the load-bearing rule in [`node_chips`]
/// is applied. `req_col` is the colour a request chip takes (its last run's
/// outcome), already resolved by the caller.
fn build_node_chips(
    node: &FlowNode,
    req_col: Color32,
    th: &GuiTheme,
    s: &crate::i18n::Strings,
) -> Vec<Chip> {
    match node {
        FlowNode::Request { name } => {
            vec![Chip::request(name, req_col).with_help(s.chip_help_request)]
        }
        FlowNode::Report(ReportStmt::Request {
            name,
            alias,
            response_fmt,
            show,
            hide,
            with,
        }) => {
            let mut chips = vec![
                Chip::modifier("REPORT".into(), th.subst, DetachWhich::Report)
                    .with_help(s.chip_help_report),
            ];
            chips.push(Chip::request(name, req_col).with_help(s.chip_help_request));
            // RESPONSE / SHOW / HIDE are their own detachable chips so a long
            // reported request reads as a row of small, legible clauses rather
            // than one dense line.
            if let Some(fmt) = response_fmt {
                let text = match fmt {
                    crate::report::flow::ResponseFmt::Raw => "RESPONSE RAW",
                    crate::report::flow::ResponseFmt::Pretty => "RESPONSE PRETTY",
                };
                chips.push(
                    Chip::modifier(text.into(), th.accent, DetachWhich::Response)
                        .with_help(s.chip_help_response),
                );
            }
            if !show.is_empty() {
                chips.push(
                    Chip::modifier(
                        format!("SHOW({})", show.join(", ")),
                        th.ok,
                        DetachWhich::Show,
                    )
                    .with_help(s.chip_help_show),
                );
            }
            if !hide.is_empty() {
                chips.push(
                    Chip::modifier(
                        format!("HIDE({})", hide.join(", ")),
                        th.dim,
                        DetachWhich::Hide,
                    )
                    .with_help(s.chip_help_hide),
                );
            }
            if let Some(a) = alias {
                chips.push(
                    Chip::alias(a, th.pending, Some(DetachWhich::As)).with_help(s.chip_help_alias),
                );
            }
            // The `WITH … END` fields are rendered as a *nested block* under the
            // request line (see `with_block` in `block_row`); the line itself
            // only carries the opening `WITH` keyword so it reads like the
            // textual form (`… SHOW(Time) WITH`).
            if !with.is_empty() {
                chips.push(
                    Chip::modifier("WITH".into(), th.accent, DetachWhich::WithBlock)
                        .with_help(s.chip_help_with),
                );
            }
            chips
        }
        FlowNode::Report(ReportStmt::Vars(vars)) => {
            let text = if vars.len() == 1 {
                vars[0].clone()
            } else {
                format!("({})", vars.join(", "))
            };
            // The reported value is a plain identifier (a loop var / raw name),
            // so it reads in the neutral text colour — matching the terminal UI
            // and keeping it distinct from the `REPORT` keyword's own colour.
            vec![
                Chip::modifier("REPORT".into(), th.subst, DetachWhich::Report)
                    .with_help(s.chip_help_report),
                Chip::base(text, th.text).with_help(s.chip_help_var),
            ]
        }
        FlowNode::Report(ReportStmt::VarAs {
            var, name, stats, ..
        }) => {
            let mut chips = vec![
                Chip::modifier("REPORT".into(), th.subst, DetachWhich::Report)
                    .with_help(s.chip_help_report),
                Chip::base(var.clone(), th.text).with_help(s.chip_help_var),
                Chip::alias(name, th.pending, Some(DetachWhich::As)).with_help(s.chip_help_alias),
            ];
            chips.extend(stats_chip(stats, th, s));
            chips
        }
        FlowNode::Report(ReportStmt::Computed {
            template,
            name,
            stats,
            ..
        }) => {
            let mut chips = vec![
                Chip::modifier("REPORT".into(), th.subst, DetachWhich::Report)
                    .with_help(s.chip_help_report),
                Chip::base(format!("\"{template}\""), th.text).with_help(s.chip_help_computed),
                // A computed column requires its AS name, so this chip is
                // inline-editable but never detachable.
                Chip::alias(name, th.pending, None).with_help(s.chip_help_alias_required),
            ];
            chips.extend(stats_chip(stats, th, s));
            chips
        }
        FlowNode::Assign { .. } | FlowNode::ListDecl { .. } => {
            let (col, help) = if matches!(node, FlowNode::Assign { .. }) {
                (th.accent, s.chip_help_assign)
            } else {
                (th.pending, s.chip_help_list)
            };
            vec![Chip::base(node.label(), col).with_help(help)]
        }
        FlowNode::ForEach { parallel, .. } | FlowNode::ForEnvs { parallel, .. } => {
            let mut chips = Vec::new();
            if let Some(spec) = parallel {
                // PARALLEL is its own chip, and uses the theme's *error* hue so
                // it stands apart from the blue loop/set chips it sits beside
                // (`PARALLEL(8) FOR …`). The loop head below is built from the
                // node rather than from its label, so the prefix is never
                // duplicated and there is nothing to strip back off.
                chips.push(Chip::parallel(spec.degree, th.err).with_help(s.chip_help_parallel));
            }
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
                        baseline_show,
                    },
                ..
            } = node
            {
                chips.push(
                    Chip::loop_head(loop_edit_parts(node, true), th.accent)
                        .with_help(s.chip_help_for_envs),
                );
                // A single live-environment role becomes an inline dropdown; any
                // other shape (multiple refs, or a FILE snapshot) stays a fixed
                // chip edited through the ENVS wizard.
                use crate::report::flow::RoleRef;
                if let [RoleRef::Env(name)] = baseline.as_slice() {
                    chips.push(
                        Chip::env_role(true, 0, name, th.pending).with_help(s.chip_help_baseline),
                    );
                } else if !baseline.is_empty() {
                    chips.push(
                        Chip::modifier(
                            format!("BASELINE({})", role_refs_text(baseline)),
                            th.pending,
                            DetachWhich::Role {
                                baseline: true,
                                index: 0,
                            },
                        )
                        .with_help(s.chip_help_roles_fixed),
                    );
                }
                // The SHOW belongs to the BASELINE, so it follows it directly —
                // before the COMPARISON, mirroring the source order — and is
                // *tethered* to it, which merges the two into one segmented pill
                // (see `link_tethers`). It keeps SHOW's own colour: the pill
                // already says which chip it qualifies, so the hue is free to go
                // on saying what kind of clause it is.
                if !baseline_show.is_empty() {
                    chips.push(
                        Chip::modifier(
                            format!("SHOW({})", baseline_show.join(", ")),
                            th.ok,
                            DetachWhich::BaselineShow,
                        )
                        .with_help(s.chip_help_baseline_show)
                        .tether(),
                    );
                }
                if let [RoleRef::Env(name)] = comparisons.as_slice() {
                    chips.push(
                        Chip::env_role(false, 0, name, th.pending)
                            .with_help(s.chip_help_comparison),
                    );
                } else if !comparisons.is_empty() {
                    chips.push(
                        Chip::modifier(
                            format!("COMPARISON({})", role_refs_text(comparisons)),
                            th.pending,
                            DetachWhich::Role {
                                baseline: false,
                                index: 0,
                            },
                        )
                        .with_help(s.chip_help_roles_fixed),
                    );
                }
            } else {
                let envs = matches!(node, FlowNode::ForEnvs { .. });
                let help = if envs {
                    s.chip_help_for_envs
                } else {
                    s.chip_help_for
                };
                chips.push(Chip::loop_head(loop_edit_parts(node, envs), th.accent).with_help(help));
            }
            chips
        }
    }
}

/// The `STATISTICS(…)` chip for a named report column, or nothing when the
/// column has no statistics. Tethered, because the statistics belong to the
/// column named immediately before them rather than to the statement as a
/// whole — so the two are drawn as one segmented pill (see [`link_tethers`]).
fn stats_chip(
    stats: &[crate::report::model::StatKind],
    th: &GuiTheme,
    s: &crate::i18n::Strings,
) -> Option<Chip> {
    if stats.is_empty() {
        return None;
    }
    let list = stats
        .iter()
        .map(|k| k.keyword())
        .collect::<Vec<_>>()
        .join(", ");
    Some(
        Chip::modifier(
            format!("STATISTICS({list})"),
            th.subst,
            DetachWhich::Statistics,
        )
        .with_help(s.chip_help_statistics)
        .tether(),
    )
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
    /// Move the block at `path` up (`up`) or down among its siblings. The path
    /// is captured when the button is pressed rather than read back from the
    /// selection later, because a value commit applied first re-selects the
    /// field it wrote — the move has to act on what was highlighted on screen.
    Move {
        path: Vec<usize>,
        up: bool,
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
    /// A clause pulled off one line was dropped on another: move it there, or
    /// with `copy` (Shift held at the drop) leave the original in place and
    /// graft a duplicate (see [`transfer_modifier`]).
    MoveMod {
        from: Vec<usize>,
        which: DetachWhich,
        to: Vec<usize>,
        copy: bool,
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
    /// A `BASELINE`/`COMPARISON` env dropdown picked `name` for the role ref at
    /// `index` of the ENVS loop at `path` (see [`edit::set_env_role`]).
    SetEnvRole {
        path: Vec<usize>,
        baseline: bool,
        index: usize,
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
    /// The inline `AS` field committed a new alias/name for the node at `path`
    /// (see [`edit::set_report_alias`]).
    SetAlias {
        path: Vec<usize>,
        text: String,
    },
    /// One of the loop chip's inline boxes committed: the loop variable, the
    /// folder/file it draws from, or a `FILES` loop's `MATCH` glob.
    SetLoopVar {
        path: Vec<usize>,
        text: String,
    },
    SetLoopDir {
        path: Vec<usize>,
        text: String,
    },
    SetLoopGlob {
        path: Vec<usize>,
        text: String,
    },
    /// The folder/file picker beside a loop's path box. `file` picks a file
    /// (`TUPLES FROM`) rather than a folder.
    PickLoopDir {
        path: Vec<usize>,
        file: bool,
    },
    /// The inline `PARALLEL` box committed a new max-concurrency for the loop at
    /// `path`; `None` clears it back to the prelude-driven default.
    SetParallelDegree {
        path: Vec<usize>,
        degree: Option<u32>,
    },
    /// The nested `WITH` block's "add field" affordance: open the WITH-field
    /// wizard for a new field on the report request at `path`.
    AddWith {
        path: Vec<usize>,
    },
    /// Edit the existing `WITH` field at `index` of the report request at `path`
    /// (open its wizard).
    EditWith {
        path: Vec<usize>,
        index: usize,
    },
    /// Remove the `WITH` item at `index` of the report request at `path`.
    RemoveWith {
        path: Vec<usize>,
        index: usize,
    },
    /// Attach `STATISTICS(…)` to the `WITH` field at `index` of the report
    /// request at `path` — the drop a `WITH` field row accepts. Its own action
    /// rather than an [`Act::AttachMod`] because a field is addressed by
    /// (path, index) and isn't a `FlowNode` a modifier can be attached to.
    AttachWithStats {
        path: Vec<usize>,
        index: usize,
    },
    /// A header-strip chip committed a `# key: value` directive; `None` removes
    /// the directive (see [`edit::set_header`]).
    SetHeader {
        key: &'static str,
        value: Option<String>,
    },
    /// Browse for the file a path-valued header directive should point at.
    PickHeaderFile {
        key: &'static str,
    },
}

impl Act {
    /// Whether this action only writes a value the user typed into an inline
    /// field, leaving the shape of the flow alone. These are applied first (see
    /// [`apply_block_actions`]) because their paths were resolved against the
    /// tree as drawn.
    fn is_value_commit(&self) -> bool {
        matches!(
            self,
            Act::SetAlias { .. }
                | Act::SetLoopVar { .. }
                | Act::SetLoopDir { .. }
                | Act::SetLoopGlob { .. }
                | Act::SetParallelDegree { .. }
                | Act::SetHeader { .. }
        )
    }
}

pub fn ui(app: &mut GuiApp, ui: &mut egui::Ui) {
    // Take the editor out so we can freely borrow `app.session` alongside it.
    let Some(mut ed) = app.report_editor.take() else {
        return;
    };
    let th = app.theme;
    let mut close = false;
    ed.pending_toolbar = None;

    // Fold any streamed run updates into the grid, and keep repainting while a
    // run is live so the grid fills in real time.
    let running = ed.poll_run(app);
    if running {
        ui.ctx().request_repaint();
    }

    // Revalidate against the current collections/envs, but only when one of the
    // inputs has actually changed. Doing it per frame re-ran a deep-cloning walk
    // dozens of times a second and — because parts of it are driven by hash-set
    // iteration — reshuffled the panel each time, so the warnings visibly
    // flickered whenever the mouse moved (see `diagnostics_fingerprint`).
    match &ed.flow {
        Some(flow) => {
            let key = context::diagnostics_fingerprint(
                &app.session.collections,
                &app.session.global_envs,
                app.session.active_env_id,
                flow,
                ed.report.path.as_deref(),
                &app.strings,
            );
            if ed.diag_key != Some(key) {
                ed.diagnostics = context::report_diagnostics(
                    &app.session.collections,
                    &app.session.global_envs,
                    app.session.active_env_id,
                    flow,
                    ed.report.path.as_deref(),
                    &app.strings,
                );
                ed.diag_key = Some(key);
            }
        }
        None => {
            ed.diagnostics.clear();
            ed.diag_key = None;
        }
    }

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
                ed.pending_toolbar = Some(ToolbarAct::Close);
            }
            let save = ui.add_enabled(
                ed.report.dirty,
                egui::Button::new(format!("{} {}", super::icons::SAVE, app.strings.gui_save)),
            );
            if save.clicked() {
                ed.pending_toolbar = Some(ToolbarAct::Save);
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
                    ed.pending_toolbar = Some(ToolbarAct::Run);
                }
                // Dry run sits beside Run, disabled on the same terms: it
                // expands the flow for real, so a flow with errors can't be
                // previewed any more than it can be run.
                let dry = ui
                    .add_enabled(
                        ed.can_run(),
                        egui::Button::new(format!(
                            "{} {}",
                            super::icons::PREVIEW,
                            app.strings.gui_report_dry_run
                        )),
                    )
                    .on_hover_text(app.strings.gui_report_dry_run_tooltip);
                if dry.clicked() {
                    ed.pending_toolbar = Some(ToolbarAct::DryRun);
                }
            }
        });
    });

    // View toggle (Blocks | Source | Results) plus the reindent button.
    let mut reindent = false;
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
        // Re-indent the source to its real block depth. Lives beside the view
        // toggle rather than inside the source view because it is about the
        // document, not about one way of looking at it.
        ui.separator();
        if ui
            .button(app.strings.gui_report_reindent)
            .on_hover_text(app.strings.gui_report_reindent_help)
            .clicked()
        {
            reindent = true;
        }
    });
    ui.separator();

    if reindent {
        app.session.status = Some(match ed.reformat() {
            Ok(true) => Status::ReportReformatted,
            Ok(false) => Status::ReportAlreadyTidy,
            Err(ReformatError::Unparseable(msg)) => Status::ReportReformatFailed(msg),
            Err(ReformatError::WouldChangeMeaning) => {
                Status::ReportReformatFailed(app.strings.report_reformat_unsafe.to_string())
            }
        });
    }

    match ed.view {
        EditorView::Source => source_view(&mut ed, app, ui),
        EditorView::Blocks => blocks_view(&mut ed, app, ui),
        EditorView::Results => results_view(&mut ed, app, ui),
    }

    // The node-configure wizard modal (opened by double-clicking a block on the
    // blocks view) floats above whichever view is showing.
    super::report_wizard::show(&mut ed, app, ui.ctx());

    // The results cell inspector: a floating window showing one cell's full
    // (JSON-pretty-printed) value, so a long/truncated cell can be read and
    // copied in full. The GUI stand-in for the TUI's result-cell popup.
    if ed.inspector.is_some() {
        let mut open = true;
        let esc = ui.ctx().input(|i| i.key_pressed(egui::Key::Escape));
        let ins = ed.inspector.as_ref().unwrap();
        egui::Window::new(RichText::new(&ins.title).strong().color(th.text))
            .id(egui::Id::new("pt_cell_inspector"))
            .collapsible(false)
            .resizable(true)
            .default_size([520.0, 340.0])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ui.ctx(), |ui| {
                ui.horizontal(|ui| {
                    if ui.button(app.strings.gui_report_cell_copy_full).clicked() {
                        ui.ctx().copy_text(ins.content.clone());
                        app.session.status = Some(crate::i18n::Status::Copied);
                    }
                });
                ui.separator();
                egui::ScrollArea::both()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        egui::Frame::new()
                            .fill(th.sunken())
                            .inner_margin(6.0)
                            .show(ui, |ui| {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(&ins.content).monospace().color(th.text),
                                    )
                                    .selectable(true)
                                    .wrap_mode(egui::TextWrapMode::Extend),
                                );
                            });
                    });
            });
        if !open || esc {
            ed.inspector = None;
        }
    }

    // Global keys: Ctrl+Z undo (both views); Delete on the blocks view is
    // handled inside `blocks_view` so it doesn't fire while typing.
    if ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Z)) {
        ed.undo();
    }

    // The header's buttons act here, at the end of the frame that pressed them,
    // so any inline field that committed on losing focus above has already been
    // folded into the report (see [`ToolbarAct`]).
    match ed.pending_toolbar.take() {
        Some(ToolbarAct::Run) => ed.start_run(app),
        Some(ToolbarAct::DryRun) => ed.start_dry_run(app),
        Some(ToolbarAct::Save) => save_report(&mut ed, app),
        Some(ToolbarAct::Close) => close = true,
        None => {}
    }

    if close {
        // The editor goes, the run stays: closing the view must not cancel a
        // report mid-flight or discard the rows it has already collected.
        let key = ed.run_key();
        let parked = ed.park_run();
        if parked.is_worth_keeping() {
            app.report_runs.insert(key, parked);
        }
    } else {
        app.report_editor = Some(ed);
    }
}

/// The dry-run preview's contents: a notice that nothing was sent, the
/// projected row count, the grid the run would produce, and the problems the
/// expansion turned up.
///
/// Reuses [`results_grid`], so the preview looks exactly like the result it is
/// predicting — including the clickable cells, since a projected value can be
/// just as long as a real one.
fn dry_run_body(
    app: &GuiApp,
    ui: &mut egui::Ui,
    preview: &crate::report::dry_run::DryRunReport,
) -> Option<CellInspector> {
    let th = app.theme;
    let s = &app.strings;
    let mut opened = None;

    ui.colored_label(th.dim, s.report_dry_run_preview_notice);
    ui.add_space(4.0);
    ui.label(
        RichText::new(format!("{} {}", s.report_dry_run_rows, preview.rows))
            .strong()
            .color(th.accent),
    );
    ui.add_space(4.0);

    // Problems first here, unlike the terminal UI's bottom-of-the-scroll
    // placement: a window this size can hide them below the fold, and an
    // unresolved request is the whole reason to have asked for a preview.
    if preview.var_warnings.is_empty() && preview.errors.is_empty() {
        ui.colored_label(th.accent, s.report_dry_run_no_problems);
    } else {
        if !preview.var_warnings.is_empty() {
            ui.label(
                RichText::new(s.report_dry_run_warnings_heading)
                    .strong()
                    .color(th.pending),
            );
            for w in &preview.var_warnings {
                ui.colored_label(th.pending, format!("! {w}"));
            }
        }
        if !preview.errors.is_empty() {
            ui.label(
                RichText::new(s.report_dry_run_problems_heading)
                    .strong()
                    .color(th.err),
            );
            for e in &preview.errors {
                ui.colored_label(th.err, format!("• {e}"));
            }
        }
    }
    ui.separator();

    if preview.rows == 0 {
        ui.colored_label(th.dim, s.report_dry_run_no_rows);
        return None;
    }
    let columns = preview.result.resolved_columns(&preview.header);
    if columns.is_empty() {
        ui.colored_label(th.dim, app.strings.gui_report_no_results);
        return None;
    }
    // `None` states: a dry run has no streaming progress, so the grid draws
    // without status icons, exactly like a finished run.
    if let Some(ins) = results_grid(&th, ui, &preview.result, &columns, None) {
        opened = Some(ins);
    }
    opened
}

/// Build the highlighter context for `ed`: which line the parser rejected, and
/// which of the report's references currently resolve.
///
/// The colours only *mean* something with this context — a `# collection:` or
/// `ENVS` name reads green when it binds to something loaded and amber when it
/// doesn't — so the Source view answers "is this report wired up?" at a glance,
/// exactly as the terminal UI's does.
fn highlight_ctx(ed: &ReportEditor, app: &GuiApp) -> HlCtx {
    let bound = ed.flow.as_ref().and_then(|flow| {
        context::resolve_bound_collection(&app.session.collections, flow, ed.report.path.as_deref())
    });
    HlCtx {
        error_line: ed.parse_error_line,
        collection_resolves: bound.is_some(),
        loaded_envs: app
            .session
            .global_envs
            .iter()
            .map(|e| e.name.clone())
            .collect(),
        request_names: bound
            .map(|ci| {
                app.session.collections[ci]
                    .entries
                    .iter()
                    .map(|e| e.title.clone())
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// Lay `text` out as a syntax-highlighted [`egui::text::LayoutJob`], reusing the
/// terminal UI's PaperTrail highlighter so both front-ends colour a script
/// identically (see [`crate::tui::report_highlight`]).
///
/// The highlighter works a line at a time and drops the line breaks, so the
/// newlines are re-inserted here as their own sections — otherwise the whole
/// script would lay out as one run-on line.
fn highlight_job(
    text: &str,
    ctx: &HlCtx,
    spec: &crate::theme::ThemeSpec,
    th: &GuiTheme,
    font: egui::FontId,
    wrap_width: f32,
) -> egui::text::LayoutJob {
    use egui::text::{LayoutJob, TextFormat};

    let theme = spec.to_theme();
    let mut job = LayoutJob {
        wrap: egui::text::TextWrapping {
            max_width: wrap_width,
            ..Default::default()
        },
        ..Default::default()
    };
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            job.append("\n", 0.0, TextFormat::simple(font.clone(), th.text));
        }
        // `highlight_row` is 0-based over the visible rows; it maps that onto
        // the highlighter's 1-based `error_line` itself.
        for span in report_highlight::highlight_row(i, line, ctx, &theme) {
            let style = span.style;
            let mut fmt = TextFormat::simple(
                font.clone(),
                style
                    .fg
                    .map_or(th.text, |c| super::theme::from_ratatui(c, th.text)),
            );
            fmt.underline = if style
                .add_modifier
                .contains(ratatui::style::Modifier::UNDERLINED)
            {
                egui::Stroke::new(1.0, fmt.color)
            } else {
                egui::Stroke::NONE
            };
            job.append(&span.content, 0.0, fmt);
        }
    }
    job
}

/// [`highlight_job`], reused between frames while nothing it depends on has
/// changed.
///
/// `TextEdit` asks its layouter for a job on every frame, and building one
/// re-tokenises the whole document — around 270µs for a 200-line report, on
/// every frame, whether or not a key was pressed. Cloning a cached job costs a
/// fraction of that, and handing it to `layout_job` as before leaves egui's own
/// galley cache in charge of the actual text layout (which is what keeps this
/// correct across a font or scale change).
#[allow(clippy::too_many_arguments)]
fn cached_highlight_job(
    ui: &egui::Ui,
    id: egui::Id,
    text: &str,
    ctx: &HlCtx,
    spec: &crate::theme::ThemeSpec,
    th: &GuiTheme,
    font: egui::FontId,
    wrap_width: f32,
) -> egui::text::LayoutJob {
    let key = highlight_key(text, ctx, spec, &font, wrap_width);
    if let Some((cached_key, job)) = ui.data(|d| d.get_temp::<(u64, egui::text::LayoutJob)>(id))
        && cached_key == key
    {
        return job;
    }
    let job = highlight_job(text, ctx, spec, th, font, wrap_width);
    ui.data_mut(|d| d.insert_temp(id, (key, job.clone())));
    job
}

/// Everything [`highlight_job`] reads, in one number.
///
/// **Maintenance:** anything the highlighter starts colouring by has to be
/// added here, or the colours will stop following it.
fn highlight_key(
    text: &str,
    ctx: &HlCtx,
    spec: &crate::theme::ThemeSpec,
    font: &egui::FontId,
    wrap_width: f32,
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    // The document itself dominates, so it is hashed with the cheap mixer
    // rather than a SipHash pass over every byte.
    fnv1a(text.as_bytes(), FNV_OFFSET).hash(&mut h);
    ctx.error_line.hash(&mut h);
    ctx.collection_resolves.hash(&mut h);
    // Both name sets are hash sets, so their iteration order is not stable:
    // combine each name's hash with `^` so the key depends on the membership
    // and not on the order it comes out in.
    let mut names = 0u64;
    for e in &ctx.loaded_envs {
        names ^= fnv1a(e.as_bytes(), FNV_OFFSET);
    }
    for r in &ctx.request_names {
        names ^= fnv1a(r.as_bytes(), 0x9e37_79b9_7f4a_7c15);
    }
    names.hash(&mut h);
    spec.hash(&mut h);
    font.size.to_bits().hash(&mut h);
    wrap_width.to_bits().hash(&mut h);
    h.finish()
}

/// The byte offset of char index `at` in `text` (clamped to the end).
fn byte_at(text: &str, at: usize) -> usize {
    text.char_indices()
        .nth(at)
        .map(|(b, _)| b)
        .unwrap_or(text.len())
}

/// The (row, column) — both counted in chars — of char index `at`.
fn row_col_at(text: &str, at: usize) -> (usize, usize) {
    let (mut row, mut col) = (0usize, 0usize);
    for ch in text.chars().take(at) {
        if ch == '\n' {
            row += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    (row, col)
}

/// The char index at which `row` starts (clamped to the end of `text`).
fn row_start(text: &str, row: usize) -> usize {
    let mut idx = 0usize;
    for (n, line) in text.split('\n').enumerate() {
        if n == row {
            return idx;
        }
        idx += line.chars().count() + 1; // + the newline itself
    }
    text.chars().count()
}

/// The text and new cursor position after Enter replaces the char range `sel`,
/// with PaperTrail's auto-indent applied.
///
/// `egui`'s own Enter would insert a bare `\n`, dumping the caret in column 0
/// of a nested block; this inherits the current line's indentation instead (and
/// adds a level after a block opener), matching the terminal editor. Like that
/// editor, a *mid-line* split stays a plain newline — the rule is for the
/// ordinary "type to the end of the line, press Enter" flow, where anything else
/// would be a surprise. What counts as "the end" is measured at `sel.end`, since
/// that is where the caret lands once the selection is replaced, while the
/// indent is taken from the line the selection *starts* on.
fn newline_with_indent(text: &str, sel: std::ops::Range<usize>) -> (String, usize) {
    let lines: Vec<&str> = text.split('\n').collect();
    let (row, _) = row_col_at(text, sel.start);
    let (end_row, end_col) = row_col_at(text, sel.end);
    let at_line_end = end_col == lines.get(end_row).map_or(0, |l| l.chars().count());
    let indent = if at_line_end {
        indent_for_new_line(lines.get(row).copied().unwrap_or(""))
    } else {
        String::new()
    };
    let (from, to) = (byte_at(text, sel.start), byte_at(text, sel.end));
    let mut out = String::with_capacity(text.len() + 1 + indent.len());
    out.push_str(&text[..from]);
    out.push('\n');
    out.push_str(&indent);
    out.push_str(&text[to..]);
    (out, sel.start + 1 + indent.chars().count())
}

/// The text and new cursor position after Tab indents one level over the char
/// range `sel` (replacing any selection, as typing a character would).
fn indent_at(text: &str, sel: std::ops::Range<usize>) -> (String, usize) {
    let (from, to) = (byte_at(text, sel.start), byte_at(text, sel.end));
    let mut out = String::with_capacity(text.len() + INDENT_UNIT.len());
    out.push_str(&text[..from]);
    out.push_str(INDENT_UNIT);
    out.push_str(&text[to..]);
    (out, sel.start + INDENT_UNIT.chars().count())
}

/// How many chars a de-indent at char index `at` should delete: back to the
/// previous four-column stop within the run of spaces ending at the caret, so
/// one press clears a whole level rather than a single space. `None` when the
/// caret isn't preceded by a space, in which case the key means what it always
/// did. Mirrors the terminal editor's Tab/Backspace rule.
fn dedent_span(text: &str, at: usize) -> Option<usize> {
    let (row, col) = row_col_at(text, at);
    let chars: Vec<char> = text.split('\n').nth(row)?.chars().collect();
    if col == 0 || chars.get(col - 1) != Some(&' ') {
        return None;
    }
    let mut run_start = col;
    while run_start > 0 && chars[run_start - 1] == ' ' {
        run_start -= 1;
    }
    Some((col - run_start - 1) % INDENT_UNIT.len() + 1)
}

/// The text and new cursor position after deleting `n` chars before `at`.
fn delete_before(text: &str, at: usize, n: usize) -> (String, usize) {
    let start = at.saturating_sub(n);
    let (from, to) = (byte_at(text, start), byte_at(text, at));
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..from]);
    out.push_str(&text[to..]);
    (out, start)
}

/// Handle the source editor's code-editing keys, which `egui`'s text area would
/// otherwise take literally: Enter (auto-indent), Tab / Shift+Tab (indent by one
/// four-space level rather than inserting a literal `\t`) and Backspace over
/// indentation (clear a whole level). Returns the replacement text and caret
/// position, or `None` to let the widget handle the key itself.
///
/// Tab stays in the field either way — `code_editor()` locks focus — so
/// **Escape** is the way out of the editor (egui releases focus on it, and the
/// focus filter deliberately lets it through).
fn source_edit_key(
    ui: &egui::Ui,
    text: &str,
    sel: std::ops::Range<usize>,
) -> Option<(String, usize)> {
    use egui::{Key, Modifiers};
    if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
        return Some(newline_with_indent(text, sel));
    }
    if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Tab)) {
        return Some(indent_at(text, sel));
    }
    // Shift+Tab is always swallowed, even where there is no indentation to
    // remove: with focus locked it can't mean "previous widget", and letting the
    // widget see it would insert egui's own tab-flavoured indentation instead.
    if ui.input_mut(|i| i.consume_key(Modifiers::SHIFT, Key::Tab)) {
        return dedent_span(text, sel.start).map(|n| delete_before(text, sel.start, n));
    }
    // Backspace is only taken when it really is deleting indentation, so an
    // ordinary character delete (and every selection-aware case) stays egui's.
    if sel.is_empty()
        && let Some(n) = dedent_span(text, sel.start)
        && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Backspace))
    {
        return Some(delete_before(text, sel.start, n));
    }
    None
}

/// If the line holding char index `at` now reads exactly `END`, snap it to its
/// opener's indentation and return the corrected text with the cursor parked at
/// the line end — so finishing a block dedents one level, as it does in the
/// terminal editor. `None` when nothing needs changing (already aligned, not an
/// `END`, or the nesting above is unbalanced, in which case guessing would be
/// worse than leaving the typed text alone).
fn snap_end_line(text: &str, at: usize) -> Option<(String, usize)> {
    let lines: Vec<&str> = text.split('\n').collect();
    let (row, _) = row_col_at(text, at);
    let line = *lines.get(row)?;
    if !is_end_line(line) {
        return None;
    }
    let indent = matching_opener_indent(&lines[..row])?;
    let fixed = format!("{indent}{}", line.trim_start());
    if fixed == line {
        return None;
    }
    let start = row_start(text, row);
    let cursor = start + fixed.chars().count();
    let mut out: Vec<&str> = lines;
    out[row] = &fixed;
    Some((out.join("\n"), cursor))
}

/// The raw `.trail` source editor + validation panel.
fn source_view(ed: &mut ReportEditor, app: &GuiApp, ui: &mut egui::Ui) {
    // Reserve room for the diagnostics panel at the bottom, then let the editor
    // fill the rest. Keeping the editor above avoids nesting egui panels inside
    // the centre panel (which egui 0.35 disallows in a `panel_frame` closure).
    let avail = ui.available_height();
    let diag_h = ed.diag_h.clamp(48.0, (avail - 100.0).max(48.0));
    let edit_h = (avail - diag_h - 8.0).max(80.0);
    let hl = highlight_ctx(ed, app);
    let spec = app.session.active_theme_spec();
    let th = app.theme;
    // `TextEdit` asks for a job every frame; re-tokenising the whole document
    // that often is the expensive part, so it is cached and egui's galley cache
    // still handles the layout itself.
    let job_id = egui::Id::new("report_source_highlight");
    let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap_width: f32| {
        let font = egui::TextStyle::Monospace.resolve(ui.style());
        let job = cached_highlight_job(ui, job_id, buf.as_str(), &hl, &spec, &th, font, wrap_width);
        ui.ctx().fonts_mut(|f| f.layout_job(job))
    };
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .max_height(edit_h)
        .show(ui, |ui| {
            let mut text = ed.report.text.clone();
            // The code-editing keys are intercepted *before* the widget sees
            // them: egui would insert a bare newline or a literal tab, so the
            // only way to indent PaperTrail's way is to consume the key and
            // perform the edit (caret included) ourselves. Modified variants are
            // left alone — Ctrl/Cmd+Enter runs the report, and Shift+Enter stays
            // a plain-newline escape hatch.
            let te_id = ui.id().with("trail_source");
            // Only intercept when there is a live cursor to work from: with no
            // stored state we have no idea where the newline goes, so the key
            // is left for egui rather than consumed and dropped on the floor.
            let cursor = ui
                .memory(|m| m.has_focus(te_id))
                .then(|| egui::TextEdit::load_state(ui.ctx(), te_id))
                .flatten()
                .and_then(|s| s.cursor.char_range())
                .map(|r| {
                    let r = r.as_sorted_char_range();
                    r.start.0..r.end.0
                });
            let mut new_cursor: Option<usize> = None;
            let mut edited = false;
            if let Some(range) = cursor
                && let Some((next, caret)) = source_edit_key(ui, &text, range)
            {
                text = next;
                new_cursor = Some(caret);
                edited = true;
            }
            let resp = ui.add(
                egui::TextEdit::multiline(&mut text)
                    .id(te_id)
                    // Also locks focus, so Tab stays in the field (Escape is the
                    // way out) — we just intercept it above to indent in spaces
                    // rather than the literal `\t` egui would insert.
                    .code_editor()
                    .desired_width(f32::INFINITY)
                    .desired_rows(20)
                    .layouter(&mut layouter),
            );
            // A line that has just become `END` dedents to its opener. Done
            // after the widget so it also catches the `END` the user typed by
            // hand, not just the one a newline left behind.
            if (edited || resp.changed())
                && let Some(state) = egui::TextEdit::load_state(ui.ctx(), te_id)
            {
                let at = new_cursor.unwrap_or_else(|| {
                    state
                        .cursor
                        .char_range()
                        .map(|r| r.primary.index.0)
                        .unwrap_or(0)
                });
                if let Some((next, caret)) = snap_end_line(&text, at) {
                    text = next;
                    new_cursor = Some(caret);
                    edited = true;
                }
            }
            if let Some(caret) = new_cursor
                && let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), te_id)
            {
                state
                    .cursor
                    .set_char_range(Some(egui::text_selection::CCursorRange::one(
                        egui::text::CCursor::new(egui::text::CharIndex(caret)),
                    )));
                egui::TextEdit::store_state(ui.ctx(), te_id, state);
            }
            if edited || resp.changed() {
                // Snapshot for undo only when transitioning from a saved
                // baseline, to avoid one entry per keystroke.
                if ed.undo.last().map(String::as_str) != Some(ed.report.text.as_str()) {
                    ed.undo.push(ed.report.text.clone());
                }
                ed.set_text(text);
            }
        });
    diag_splitter(ed, ui);
    diagnostics_panel(ed, app, ui);
}

/// The results grid from the last (or in-flight) run, plus an Export button.
fn results_view(ed: &mut ReportEditor, app: &mut GuiApp, ui: &mut egui::Ui) {
    let th = app.theme;

    // A dry run is a result like any other, so it is shown in this view rather
    // than in a window of its own — same table, same cell viewer, nothing
    // floating over the top of anything. Its banner is what says it isn't real.
    if let Some(preview) = ed.dry_run.take() {
        let mut keep = true;
        ui.horizontal(|ui| {
            ui.colored_label(th.pending, app.strings.report_dry_run_preview_notice);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button(format!(
                        "{} {}",
                        super::icons::CLOSE,
                        app.strings.gui_report_dry_run_close
                    ))
                    .on_hover_text(app.strings.gui_report_dry_run_close_tooltip)
                    .clicked()
                {
                    keep = false;
                }
            });
        });
        ui.separator();
        if let Some(ins) = dry_run_body(app, ui, &preview) {
            ed.inspector = Some(ins);
        }
        if keep {
            ed.dry_run = Some(preview);
        }
        return;
    }

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
            // Saving the run as a snapshot is a different thing from exporting
            // it for reading, so it gets its own button rather than hiding as a
            // format in the export dialog: the file it writes is an input to a
            // later run, not a report anyone opens.
            let baseline = ui
                .add_enabled(
                    has_rows,
                    egui::Button::new(format!(
                        "{} {}",
                        super::icons::SAVE,
                        app.strings.gui_report_save_baseline
                    )),
                )
                .on_hover_text(app.strings.help_report_baseline)
                .on_disabled_hover_text(app.strings.report_baseline_no_result);
            if baseline.clicked() {
                super::menu::save_via_picker(app, super::app::SaveKind::ReportBaseline);
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
    ui.colored_label(th.dim, app.strings.gui_report_cell_hint);
    ui.add_space(2.0);
    if let Some(ins) = results_grid(&th, ui, result, &columns, states) {
        ed.inspector = Some(ins);
    }
}

/// The narrowest a column is allowed to get when the table is squeezed, in
/// pixels — roughly four characters plus the ellipsis. Below this a column
/// shows nothing useful, so it is better to stop shrinking and let the table
/// scroll sideways instead.
const MIN_COL_W: f32 = 46.0;

/// Share `avail` pixels out between columns that would naturally like
/// `natural` pixels each.
///
/// Two jobs in one, because they are the same sum from opposite sides:
///
/// * **Too much room** — the widths are grown in proportion so the table fills
///   the window rather than huddling at the left edge.
/// * **Not enough room** — the widths are *water-filled*: a level is found such
///   that every column wider than it is cut down to it and every column
///   narrower than it is left alone. Shrinking proportionally instead would
///   punish a 3-character `Status` column just as hard as a sprawling body
///   column, which is exactly backwards; capping the greedy columns first is
///   what keeps everything on screen and still legible.
///
/// Columns are never squeezed below [`MIN_COL_W`]. If even that doesn't fit,
/// the returned widths deliberately overflow `avail` — the caller's horizontal
/// scroll bar is the honest answer at that point, and the cell viewer is there
/// for whatever still gets clipped.
fn fit_column_widths(natural: &[f32], avail: f32, spacing: f32) -> Vec<f32> {
    if natural.is_empty() {
        return Vec::new();
    }
    let gaps = spacing * (natural.len() as f32 - 1.0);
    let budget = (avail - gaps).max(0.0);
    let total: f32 = natural.iter().sum();

    if total <= 0.0 {
        return vec![(budget / natural.len() as f32).max(MIN_COL_W); natural.len()];
    }
    if total <= budget {
        // Grow in proportion to what each column asked for, so the extra room
        // goes to the columns most likely to be truncating.
        let scale = budget / total;
        return natural.iter().map(|w| w * scale).collect();
    }

    // Water-fill: walk the columns narrowest-first, handing each the smaller of
    // what it wants and an even share of what is left.
    let mut order: Vec<usize> = (0..natural.len()).collect();
    order.sort_by(|&a, &b| natural[a].total_cmp(&natural[b]));
    let mut out = vec![0.0f32; natural.len()];
    let mut left = budget;
    for (i, &c) in order.iter().enumerate() {
        let share = left / (order.len() - i) as f32;
        if natural[c] <= share {
            out[c] = natural[c];
            left -= natural[c];
        } else {
            out[c] = share;
            left -= share;
        }
    }
    out.iter().map(|w| w.max(MIN_COL_W)).collect()
}

/// Render the results as a scrollable table: a header row, one row per data row
/// (greyed/marked by its streaming [`RowState`]), then any STATISTICS summary
/// rows. Mirrors the TUI's `report_grid_lines` semantics.
fn results_grid(
    th: &GuiTheme,
    ui: &mut egui::Ui,
    result: &ReportResult,
    columns: &[crate::report::model::OutputColumn],
    states: Option<&[RowState]>,
) -> Option<CellInspector> {
    let show_icons = states.is_some();
    let mut opened: Option<CellInspector> = None;
    let widths = fitted_column_widths(ui, result, columns, show_icons);
    let row_h = ui.text_style_height(&egui::TextStyle::Body);

    egui::ScrollArea::both()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            egui::Grid::new("report_results_grid")
                .striped(true)
                .spacing(egui::vec2(SPACING_X, 3.0))
                .show(ui, |ui| {
                    // Header row.
                    if show_icons {
                        ui.label(" ");
                    }
                    for (c, col) in columns.iter().enumerate() {
                        let w = widths.get(c).copied().unwrap_or(MIN_COL_W);
                        cell_slot(ui, w, row_h, |ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&col.header).strong().color(th.accent),
                                )
                                .truncate(),
                            )
                            .on_hover_text(&col.header);
                        });
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
                        for (c, col) in columns.iter().enumerate() {
                            let full = col.value(row, &result.no_match_marker);
                            let w = widths.get(c).copied().unwrap_or(MIN_COL_W);
                            cell_slot(ui, w, row_h, |ui| {
                                if let Some(ins) = result_cell(ui, text_col, &col.header, &full) {
                                    opened = Some(ins);
                                }
                            });
                        }
                        ui.end_row();
                    }

                    // STATISTICS summary rows (a footer, distinguished by italic accent).
                    for srow in result.summary_rows(columns) {
                        if show_icons {
                            ui.label(" ");
                        }
                        for (c, col) in columns.iter().enumerate() {
                            let full = srow.text_cell(c);
                            let cell = flatten_cell(&full);
                            let w = widths.get(c).copied().unwrap_or(MIN_COL_W);
                            cell_slot(ui, w, row_h, |ui| {
                                let resp = ui
                                    .add(
                                        egui::Label::new(
                                            RichText::new(truncate_cell(&cell))
                                                .italics()
                                                .color(th.accent),
                                        )
                                        .truncate()
                                        .sense(egui::Sense::click()),
                                    )
                                    .on_hover_text(&cell);
                                if resp.clicked() {
                                    opened = Some(CellInspector {
                                        title: col.header.clone(),
                                        content: pretty_json_cell(&full),
                                    });
                                }
                            });
                        }
                        ui.end_row();
                    }
                });
        });
    opened
}

/// The gap left between a tethered chip and the chip it qualifies: none, so the
/// two sit flush and their touching borders form the divider of a single
/// segmented pill (see [`link_tethers`]).
///
/// Applied while laying out the *anchor*, not the tethered chip. egui advances
/// the cursor — spacing included — as each widget is allocated, so the gap
/// between two chips is the one in effect when the **first** of them was added;
/// narrowing it just before the second has no effect on the space already left
/// behind it.
const TETHER_GAP: f32 = 0.0;

/// The corner radius of a chip.
///
/// Small on purpose. A large radius reads as a soft, toy-like pill; the shallow
/// one here still says "this is a discrete object you can pick up" without the
/// bubbliness. It is the single number most responsible for how playful or how
/// businesslike the flow looks, so it is worth keeping deliberately low.
const CHIP_RADIUS: u8 = 3;

/// Fully rounded corners, for a chip that never joins a neighbour.
const ROUND_CHIP: egui::CornerRadius = egui::CornerRadius::same(CHIP_RADIUS);

/// The corner radii for a chip, squaring off whichever edge it shares with a
/// tethered neighbour so the pair is drawn as one pill split into segments
/// rather than as two separate chips that happen to be adjacent.
fn chip_corners(chip: &Chip) -> egui::CornerRadius {
    let joined = |yes: bool| if yes { 0 } else { CHIP_RADIUS };
    egui::CornerRadius {
        nw: joined(chip.join_prev),
        sw: joined(chip.join_prev),
        ne: joined(chip.join_next),
        se: joined(chip.join_next),
    }
}

/// Work out how each tethered chip joins the chip it qualifies.
///
/// A tethered chip is drawn as the right-hand segment of a single pill: the
/// pair sits flush, the anchor keeps its rounded left corners and squares off
/// its right, and the hanger mirrors it, so one outline encloses both and their
/// touching borders read as a divider. Segmented controls and breadcrumbs
/// already teach that shape as "one control in two parts". The bracket this
/// replaces was drawn *beside* the chips, and a thin line in a muted grey read
/// as decoration — it never said which of the two owned the other.
///
/// Only the shape changes: each chip keeps the colour its own kind always has,
/// so a `SHOW` is recognisable as a `SHOW` wherever it appears. Restating the
/// ownership in the hue as well would trade that away for something the pill
/// already says.
fn link_tethers(chips: &mut [Chip]) {
    for i in 1..chips.len() {
        if !chips[i].tethered {
            continue;
        }
        chips[i - 1].join_next = true;
        chips[i].join_prev = true;
    }
}

/// Undo the join between `chips[i - 1]` and `chips[i]`, restoring both the
/// rounded corners and the normal inter-chip gap.
///
/// Used where the pair is about to stop being adjacent: either half is in hand,
/// or a hovering clause is about to be dropped between them. Merging across
/// that gap would say "these two are one thing" at exactly the moment the user
/// is separating them, and would leave a squared-off edge facing an empty slot.
fn split_tether(chips: &mut [Chip], i: usize) {
    chips[i - 1].join_next = false;
    chips[i].join_prev = false;
    chips[i].tethered = false;
}

/// Highlight both halves of a tethered pair while the pointer is over either of
/// them: a single outline around the two, which answers "what does this SHOW
/// belong to?" the moment the user goes looking, without adding anything to the
/// resting state of an already-busy line.
fn paint_tether_hover(ui: &egui::Ui, th: &GuiTheme, anchor: egui::Rect, hanger: egui::Rect) {
    let pair = anchor.union(hanger);
    // Suppressed mid-drag: the pointer is then carrying something, and an
    // outline under it reads as a drop target rather than as an explanation.
    if ui.ctx().dragged_id().is_some() || !ui.rect_contains_pointer(pair) {
        return;
    }
    ui.painter().rect_stroke(
        pair.expand(2.0),
        egui::CornerRadius::same(CHIP_RADIUS + 2),
        egui::Stroke::new(1.0, mix(th.panel, th.text, 0.75)),
        egui::StrokeKind::Outside,
    );
}

/// Horizontal spacing between the grid's columns.
const SPACING_X: f32 = 14.0;

/// The width to give each data column of `result` in the space `ui` has left:
/// what the column would like, fitted to the window by [`fit_column_widths`].
fn fitted_column_widths(
    ui: &egui::Ui,
    result: &ReportResult,
    columns: &[crate::report::model::OutputColumn],
    show_icons: bool,
) -> Vec<f32> {
    // The status-glyph column is a fixed narrow gutter, not a data column, so
    // it is taken off the top of the budget rather than shared in it.
    let icon_w = if show_icons { 18.0 + SPACING_X } else { 0.0 };
    let natural = cached_natural_widths(ui, result, columns);
    let avail = (ui.available_width() - icon_w).max(0.0);
    // Fitting is pure arithmetic over the naturals, so it stays uncached and
    // the grid still follows the window as it is dragged.
    fit_column_widths(&natural, avail, SPACING_X)
}

/// [`natural_column_widths`], reused between frames while nothing it depends on
/// has changed.
///
/// Measuring is not cheap — a `String` per cell, a text layout per column, and
/// a full pass over the summary rows — and the grid is redrawn on every frame,
/// including the ones where all that happened was the mouse moving. The widths
/// only depend on the values, the columns and the font, so they are kept in
/// egui's own per-frame memory (rather than on `ReportEditor`) because the grid
/// is drawn by free functions that are also called from the dry-run preview and
/// from tests.
fn cached_natural_widths(
    ui: &egui::Ui,
    result: &ReportResult,
    columns: &[crate::report::model::OutputColumn],
) -> Vec<f32> {
    let key = widths_fingerprint(ui, result, columns);
    let id = egui::Id::new("report_grid_widths");
    if let Some((cached_key, widths)) = ui.data(|d| d.get_temp::<(u64, Vec<f32>)>(id))
        && cached_key == key
    {
        return widths;
    }
    let widths = natural_column_widths(ui, result, columns);
    ui.data_mut(|d| d.insert_temp(id, (key, widths.clone())));
    widths
}

/// FNV-1a's starting value.
pub(super) const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

/// FNV-1a over `bytes`, continuing from `seed`.
///
/// Used instead of a `DefaultHasher` per cell in [`widths_fingerprint`]: the
/// fingerprint runs over every cell of the table on every frame, and building a
/// SipHash state for each of tens of thousands of cells costs more than hashing
/// their bytes does. This is not a hash anyone attacks — it guards a cache of
/// column widths — so the cheap mixing function is the right one.
pub(super) fn fnv1a(bytes: &[u8], seed: u64) -> u64 {
    let mut h = seed;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Everything [`natural_column_widths`] reads, in one number.
///
/// This hashes the cell *text* rather than something cheaper like the row
/// count, because a streaming run replaces rows in place: the grid is built as
/// a skeleton of empty rows and each one is overwritten as its result arrives,
/// so a key that only counted rows would pin the columns at the width of an
/// empty table for the whole run. Hashing the text is still far less work than
/// laying it out — no allocation, one pass — which is what makes the cache
/// worth having rather than a second copy of the same cost.
///
/// **Maintenance:** anything `natural_column_widths` starts reading has to be
/// added here too, or the widths will stop following it.
fn widths_fingerprint(
    ui: &egui::Ui,
    result: &ReportResult,
    columns: &[crate::report::model::OutputColumn],
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    // The font decides how wide any of it renders.
    let font = egui::TextStyle::Body.resolve(ui.style());
    font.size.to_bits().hash(&mut h);
    format!("{:?}", font.family).hash(&mut h);
    result.no_match_marker.hash(&mut h);
    for col in columns {
        col.header.hash(&mut h);
        // A column's own definition decides what it pulls out of each row, and
        // its statistics decide whether there are summary rows to measure.
        format!("{:?}", col.stats).hash(&mut h);
        format!("{:?}", col.sources).hash(&mut h);
    }
    result.rows.len().hash(&mut h);
    for row in &result.rows {
        // `cells` and `vars` are hash maps, so their iteration order changes
        // from run to run. Combining each entry's hash with `^` makes the
        // fingerprint depend on the contents and not on the order they come
        // out in — the same trap that made the validation panel flicker.
        let mut cells = 0u64;
        for (k, v) in row.cells.iter().chain(&row.vars) {
            cells ^= fnv1a(k.as_bytes(), fnv1a(v.as_bytes(), FNV_OFFSET));
        }
        cells.hash(&mut h);
        row.target.hash(&mut h);
    }
    h.finish()
}

/// Lay a cell's content out in a slot exactly `w` wide.
///
/// `egui::Grid` sizes a column to its widest cell, so pinning every cell in a
/// column to the same width is what makes the column that width — and it keeps
/// the grid's striping and row alignment, which hand-rolling the rows would
/// throw away. `set_min_size` is the part that matters: without it a short
/// label would allocate only its own width and the column would collapse.
fn cell_slot(ui: &mut egui::Ui, w: f32, h: f32, add: impl FnOnce(&mut egui::Ui)) {
    ui.allocate_ui_with_layout(
        egui::vec2(w, h),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_min_size(egui::vec2(w, h));
            add(ui);
        },
    );
}

/// How wide each column would like to be: the width of its header or of its
/// longest value, whichever is greater.
///
/// Only the *longest-by-character-count* value in each column is actually
/// measured. Laying out every cell of a thousand-row report each frame to find
/// the widest pixel width would be far more work than the answer is worth, and
/// in a proportional font the longest string is almost always the widest one.
/// Where it isn't, the loser is truncated by a character or two — which the
/// cell viewer already covers.
fn natural_column_widths(
    ui: &egui::Ui,
    result: &ReportResult,
    columns: &[crate::report::model::OutputColumn],
) -> Vec<f32> {
    let font = egui::TextStyle::Body.resolve(ui.style());
    // Computed once, not once per column: `summary_rows` walks every row of
    // every column, so calling it inside the per-column loop below made finding
    // the widths quadratic in the number of columns.
    let summary = result.summary_rows(columns);
    // Leave room for the cell's own padding so text isn't flush against the
    // next column.
    let pad = 6.0;
    let measure = |text: &str| {
        ui.painter()
            .layout_no_wrap(text.to_string(), font.clone(), egui::Color32::WHITE)
            .size()
            .x
            + pad
    };

    columns
        .iter()
        .enumerate()
        .map(|(c, col)| {
            let mut longest = String::new();
            let mut longest_len = 0usize;
            let mut consider = |text: String| {
                let n = text.chars().count();
                if n > longest_len {
                    longest_len = n;
                    longest = text;
                }
            };
            for row in &result.rows {
                consider(truncate_cell(&flatten_cell(
                    &col.value(row, &result.no_match_marker),
                )));
            }
            for srow in &summary {
                consider(truncate_cell(&flatten_cell(&srow.text_cell(c))));
            }
            measure(&col.header).max(measure(&longest))
        })
        .collect()
}

/// A single clickable results cell: shows the truncated one-line value, the full
/// value on hover, and — when clicked — returns a [`CellInspector`] carrying the
/// cell's full (JSON-pretty-printed) value so a long cell can be read in full.
fn result_cell(
    ui: &mut egui::Ui,
    text_col: Color32,
    header: &str,
    full: &str,
) -> Option<CellInspector> {
    let cell = flatten_cell(full);
    let resp = ui
        .add(
            egui::Label::new(RichText::new(truncate_cell(&cell)).color(text_col))
                .truncate()
                .sense(egui::Sense::click()),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(cell);
    resp.clicked().then(|| CellInspector {
        title: header.to_string(),
        content: pretty_json_cell(full),
    })
}

/// Pretty-print a cell whose whole trimmed value is a single JSON object/array
/// (e.g. a captured response body) so the inspector shows an indented,
/// one-field-per-line view; anything else is returned unchanged. Mirrors the
/// TUI's `pretty_print_json_cell`.
fn pretty_json_cell(raw: &str) -> String {
    let trimmed = raw.trim();
    if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
        return raw.to_string();
    }
    match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(value) => serde_json::to_string_pretty(&value).unwrap_or_else(|_| raw.to_string()),
        Err(_) => raw.to_string(),
    }
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

/// Open the native save picker for exporting the active report's results,
/// defaulting to a `.csv` beside the report (or in the current dir for a scratch
/// report).
fn open_export_dialog(app: &mut GuiApp) {
    super::menu::save_via_picker(app, super::app::SaveKind::ReportResults);
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
            acts.push(Act::Move {
                path: ed.selection.clone(),
                up: true,
            });
        }
        if ui
            .add_enabled(
                on_node,
                egui::Button::new(super::icons::CARET_DOWN.to_string()),
            )
            .on_hover_text(app.strings.gui_report_move_down)
            .clicked()
        {
            acts.push(Act::Move {
                path: ed.selection.clone(),
                up: false,
            });
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
            acts.push(Act::DeletePath(ed.selection.clone()));
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
    let avail = ui.available_height();
    let diag_h = ed.diag_h.clamp(48.0, (avail - 120.0).max(48.0));
    let body_h = (avail - diag_h - 12.0).max(120.0);
    ui.allocate_ui(egui::vec2(ui.available_width(), body_h), |ui| {
        ui.horizontal_top(|ui| {
            // The palette column width is user-adjustable via the divider drawn
            // just after it (mirrors the diagnostics splitter's drag model).
            let palette_w = ed
                .palette_w
                .clamp(96.0, (ui.available_width() - 160.0).max(96.0));
            ui.allocate_ui_with_layout(
                egui::vec2(palette_w, body_h),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_min_width(palette_w);
                    ui.set_max_width(palette_w);
                    egui::ScrollArea::vertical()
                        .id_salt("pt_palette")
                        .auto_shrink([false, false])
                        .show(ui, |ui| palette_list(app, ui));
                },
            );
            palette_splitter(ed, ui, body_h);
            ui.vertical(|ui| {
                egui::ScrollArea::vertical()
                    .id_salt("pt_blocks")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        // A click on empty space in the blocks pane deselects.
                        // This background sense is registered *before* the rows
                        // so the chips (added on top, each sensing its own
                        // click) win clicks that land on them; the background
                        // only fires where no chip caught the click.
                        let bg = ui.interact(
                            ui.max_rect(),
                            ui.id().with("pt_bg_deselect"),
                            egui::Sense::click(),
                        );
                        // Whole-report settings, pinned above BEGIN (see
                        // `header_strip`) — deliberately outside the row loop so
                        // no drop target, drag lift or selection ever reaches
                        // them.
                        header_strip(ed, app, ui, &mut acts);
                        ui.add_space(4.0);
                        let mut lift = DragLift::default();
                        let mut hovers: Vec<RowHover> = Vec::new();
                        for (i, row) in rows.iter().enumerate() {
                            let selected = row.path == ed.selection
                                && (row.kind != RowKind::LoopEnd || ed.selection.is_empty());
                            let drop_pos = insert_pos_after(&rows, i);
                            if let Some(h) = block_row(
                                ed, app, ui, row, i, selected, &drop_pos, &titles, &mut lift,
                                &mut acts,
                            ) {
                                hovers.push(h);
                            }
                        }
                        // Now that every row has been measured, light the block
                        // under the pointer and everything a drag would take
                        // with it (see `paint_hover_group`).
                        paint_hover_group(ui, &th, &hovers);
                        // A report with no steps is where every new report
                        // starts, and an empty gap between BEGIN and END says
                        // nothing about what to do next — the palette's own
                        // hint is over in the other column, easy to skim past.
                        if rows.is_empty() {
                            empty_flow_hint(ui, &th, &app.strings);
                        }
                        // Every lifted row is now in the drag layer, so the
                        // whole picked-up subtree can be moved under the pointer
                        // in one go (see `DragLift`).
                        lift.follow_pointer(ui.ctx());
                        // Close the flow with an `END` matching the `BEGIN` at
                        // the top, so the whole report reads as one bracketed
                        // block the way each FOR loop does. It is drawn here
                        // rather than emitted by the shared `flatten` because it
                        // is pure punctuation: there is no node to select, move
                        // or drop onto, and adding a row would shift every index
                        // the terminal UI's node editor navigates by.
                        flow_end_row(ui, &th, &app.strings);
                        // The empty space under the last row is itself a drop
                        // target: dropping a base block (or an existing row)
                        // anywhere below the report appends it as the last
                        // top-level line.
                        tail_drop_zone(ui, &th, flow.nodes.len(), &titles, &mut acts);
                        if bg.clicked() && !ed.selection.is_empty() {
                            acts.push(Act::Select(Vec::new()));
                        }
                    });
            });
        });
    });

    // Delete key removes the selection (but not while a text field has focus).
    let typing = ui.memory(|m| m.focused().is_some());
    if !typing && !ed.selection.is_empty() && ui.input(|i| i.key_pressed(egui::Key::Delete)) {
        acts.push(Act::DeletePath(ed.selection.clone()));
    }

    // The delete drop target lives here — a distinct full-width bar that only
    // appears while a block/chip is being dragged, so it never reads as just
    // another palette block.
    if egui::DragAndDrop::has_payload_of_type::<DragItem>(ui.ctx()) {
        ui.add_space(4.0);
        trash_bar(app, ui, &mut acts);
    }

    diag_splitter(ed, ui);
    diagnostics_panel(ed, app, ui);

    apply_block_actions(ed, app, acts);
    let _ = th;
}

/// The base blocks the palette offers, in display order. Each drops in as a new
/// statement row.
///
/// `ReportRequest` is the one kind deliberately absent: a reported request is
/// composed by dropping the `REPORT` modifier onto a `REQUEST`, so offering both
/// would be two routes to the same block. `ReportVar` is *not* such a case —
/// dropping `REPORT` on a `VARIABLE` only reports a variable this flow sets
/// right there, which leaves no way at all to report a captured value or a loop
/// variable — so it is offered as a block of its own.
const BASE_KINDS: [NodeKind; 8] = [
    NodeKind::Request,
    NodeKind::ReportVar,
    NodeKind::ReportComputed,
    NodeKind::Assign,
    NodeKind::List,
    NodeKind::ForFiles,
    NodeKind::ForFolders,
    NodeKind::ForEnvs,
];

/// The always-visible palette, split into two groups: **Blocks** (base
/// statements, dragged into the gaps between rows to insert a new line) and
/// **Modifiers** (dragged *onto* a row to attach REPORT / PARALLEL / WITH / AS).
/// Drag-only — the toolbar's "Add block" popup still covers click-based insert.
/// (The delete drop target is a separate bar shown at the foot of the editor
/// while dragging — see [`trash_bar`].)
fn palette_list(app: &GuiApp, ui: &mut egui::Ui) {
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
        let src = ui.dnd_drag_source(id, kind, |ui| {
            palette_chip(ui, &th, kind.label(&app.strings), base);
        });
        // Remember how big the chip in hand is, so the drop markers down in the
        // flow can be drawn at that size rather than a full-width bar (see
        // `dragged_block_size`).
        if ui.ctx().is_being_dragged(id) {
            let size = src.response.rect.size();
            ui.ctx()
                .data_mut(|d| d.insert_temp(palette_drag_size_id(), size));
        }
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
}

/// The delete drop target: a distinct full-width bar shown at the foot of the
/// editor *only while a block/chip is being dragged*, so it never reads as just
/// another palette block. It reacts only to an in-report [`DragItem`]: a dropped
/// row is deleted, a dropped modifier chip is detached from its node.
fn trash_bar(app: &GuiApp, ui: &mut egui::Ui, acts: &mut Vec<Act>) {
    let th = app.theme;
    let frame = egui::Frame::NONE
        .fill(mix(th.panel, th.err, 0.14))
        .stroke(egui::Stroke::new(1.0, mix(th.panel, th.err, 0.5)))
        .inner_margin(egui::Margin::symmetric(8, 8))
        .corner_radius(BLOCK_RADIUS as u8);
    let resp = frame
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.vertical_centered(|ui| {
                ui.add(
                    egui::Label::new(
                        RichText::new(format!(
                            "{}  {}",
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
    if let Some(item) = release_payload::<DragItem>(&zone) {
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
///
/// Drawn through [`chip_shell`] rather than its own frame so a palette entry and
/// the chip it drops into the flow are the same object visually — same rule,
/// same tint, same corners. They differ only in that the palette chip is inert.
fn palette_chip(ui: &mut egui::Ui, th: &GuiTheme, text: &str, base: Color32) {
    let tint = chip_tint(th, base);
    chip_shell(ui, &tint, true, ROUND_CHIP, |ui| {
        ui.add(egui::Label::new(RichText::new(text).color(tint.text)).selectable(false));
    });
}

/// The palette colour for a modifier chip. Kept in step with [`node_chips`] so
/// a palette modifier reads the same colour as the chip it drops in.
fn modifier_color(m: Modifier, th: &GuiTheme) -> Color32 {
    match m {
        Modifier::Report => th.subst,
        Modifier::With => th.accent,
        Modifier::As => th.pending,
        Modifier::Parallel => th.err,
        // RESPONSE / SHOW / HIDE reuse the same hues their attached chips carry
        // (see `node_chips`) so the palette entry reads as the chip it creates.
        Modifier::Response => th.accent,
        Modifier::Show => th.ok,
        Modifier::Hide => th.dim,
        Modifier::Statistics => th.subst,
    }
}

/// Render one flattened row as a horizontal cluster of chips (a compositional
/// block: the base subject plus any attached `REPORT` / `PARALLEL` / `WITH` /
/// `AS` modifier chips). The row is both a **modifier drop zone** (drop a
/// modifier chip onto it to attach) and sits above a **base insert strip** that
/// opens an animated gap when a base block is dragged over it. Leaf and loop
/// heads carry an inline single-line editor when selected.
#[allow(clippy::too_many_arguments)]
/// Consume a drag-and-drop payload of type `T` on the release frame — but only
/// when the payload in flight is *actually* a `T`.
///
/// egui's [`egui::Response::dnd_release_payload`] is destructive in a subtle
/// way: it `take()`s the stored payload out of the DnD plugin *before* it checks
/// its type, so asking it for a type that doesn't match still throws the payload
/// away — silently starving any later reader on the same frame. Several drop
/// zones overlap each report row (the modifier zone reads [`Modifier`], the
/// insert strip reads [`NodeKind`] *and* [`DragItem`]), so an unguarded
/// `dnd_release_payload::<NodeKind>()` would eat a [`DragItem::Row`] before the
/// reorder branch ever ran (which is exactly why drops "opened a gap" but never
/// moved anything). Peeking non-destructively with `dnd_hover_payload` first and
/// only consuming a matching payload lets the zones coexist.
fn release_payload<T: std::any::Any + Send + Sync>(
    resp: &egui::Response,
) -> Option<std::sync::Arc<T>> {
    resp.dnd_hover_payload::<T>()?;
    resp.dnd_release_payload::<T>()
}

/// Minimum widths of the inline editors embedded in a chip. Each field grows
/// past this to fit what is actually in it (see [`fitted_field_width`]) and
/// stops at [`FIELD_MAX_WIDTH`]; these are the resting sizes an empty box takes,
/// and what the same-sized placeholder a pending drop draws uses (see
/// [`Chip::ghost_shape`]) so the two can't drift apart.
const ALIAS_FIELD_WIDTH: f32 = 96.0;
const PARALLEL_FIELD_WIDTH: f32 = 44.0;
/// A loop variable is an identifier, and short ones are the convention.
const LOOP_VAR_FIELD_WIDTH: f32 = 72.0;
/// A folder path is the longest thing on a loop head, and the one most worth
/// being able to read without scrolling the row.
const LOOP_PATH_FIELD_WIDTH: f32 = 150.0;
const LOOP_GLOB_FIELD_WIDTH: f32 = 84.0;
/// How wide any inline field is allowed to grow to fit its contents.
///
/// A deep folder path would otherwise push a loop head past the width of the
/// pane and put the rest of the statement out of sight. Past this the box
/// scrolls internally, which is the old behaviour — but now only for the
/// genuinely long values rather than for almost all of them.
const FIELD_MAX_WIDTH: f32 = 320.0;
/// The folder/file picker button beside a loop's path box.
///
/// Wider than the glyph needs because the button now carries a frame: it looked
/// like decoration when drawn flat, and users didn't discover it was a button.
const PICKER_BUTTON_WIDTH: f32 = 28.0;
/// A combo chip's dropdown grows to fit its text, which the ghost already spells
/// out; this is just the arrow and its padding.
const COMBO_CHIP_WIDTH: f32 = 24.0;

/// Horizontal indent applied per nesting level in the block editor, so a
/// statement inside a `FOR`/`PARALLEL`/`WITH` block sits clearly further right
/// than its parent. Used for both the chip clusters and the drop-placeholder /
/// nested-field indents so they all line up at the same depth.
const INDENT_STEP: f32 = 24.0;

/// The report path of the block currently being dragged for reordering (an
/// active `DragItem::Row` payload), if any. Read at the start of a row's render
/// from the payload set last frame, so the dragged block can be lifted out of
/// its slot and floated under the cursor (see [`block_row`]).
fn dragged_row_path(ctx: &egui::Context) -> Option<Vec<usize>> {
    egui::DragAndDrop::payload::<DragItem>(ctx).and_then(|d| match &*d {
        DragItem::Row(p) => Some(p.clone()),
        _ => None,
    })
}

/// The chip currently picked up on its own (an active [`DragItem::Chip`]
/// payload), if any. Read at the start of a row's render from the payload set
/// last frame — exactly like [`dragged_row_path`] — so the chip can be lifted
/// out of its slot and floated under the cursor.
fn dragged_chip(ctx: &egui::Context) -> Option<(Vec<usize>, DetachWhich)> {
    egui::DragAndDrop::payload::<DragItem>(ctx).and_then(|d| match &*d {
        DragItem::Chip { path, which } => Some((path.clone(), *which)),
        _ => None,
    })
}

/// Whether `row_path` is part of the subtree currently being dragged: the
/// dragged path itself, or any descendant of it (a `FOR` loop's body rows and
/// its synthetic `END`, all of which carry the loop's path as a prefix). A leaf
/// therefore lifts only itself; a loop lifts its whole body in one piece.
fn row_is_lifted(dragged: &[usize], row_path: &[usize]) -> bool {
    row_path.starts_with(dragged)
}

/// How strongly a row is highlighted while the pointer rests on a block.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum HoverTier {
    /// The hovered block itself. A `FOR` loop is *two* rows at this tier — its
    /// header and its `END` — because those are the two halves of one block, and
    /// lighting only the half under the pointer made a loop look like it ended
    /// somewhere it doesn't.
    Block,
    /// A row that isn't the block but travels with it: the body of a hovered
    /// loop. Softer, because it answers a different question ("and this comes
    /// too") from the block under the pointer.
    CarriedAlong,
}

/// The highlight `row_path` earns while `hovered` is the block under the
/// pointer, or `None` if it is unaffected.
///
/// Deliberately the same subtree rule as [`row_is_lifted`] — the highlight's
/// whole promise is "this is what a drag would pick up", so if the two ever
/// disagreed the preview would be a lie. A leaf therefore lights only itself; a
/// loop lights its body and its `END` as well.
fn hover_tier(hovered: &[usize], row_path: &[usize]) -> Option<HoverTier> {
    if !row_is_lifted(hovered, row_path) {
        None
    } else if row_path.len() == hovered.len() {
        Some(HoverTier::Block)
    } else {
        Some(HoverTier::CarriedAlong)
    }
}

/// `rect` with the row's indent trimmed off its left edge: the block's own
/// bounds. Never collapses the rect, however deeply nested the row is.
fn indented_content(rect: egui::Rect, depth: usize) -> egui::Rect {
    let indent = (depth as f32 * INDENT_STEP).min((rect.width() - 1.0).max(0.0));
    egui::Rect::from_min_max(
        egui::pos2(rect.left() + indent, rect.top()),
        rect.right_bottom(),
    )
}

/// Where the measured *silhouette* of the picked-up subtree is stashed between
/// frames, so the drop markers can take its shape (see [`DragLift`] and
/// [`dragged_block_shape`]). One drag is in flight at a time, so a single
/// global id is enough.
///
/// A subtree is not a rectangle: a `FOR` loop is a short head, an indented body
/// of varying widths and a short `END`. Storing one rect per row — each
/// positioned relative to the subtree's own top-left corner — lets the marker
/// show that outline instead of the bounding box, which for a loop over three
/// requests promised a large solid slab nothing like the block in hand.
fn lifted_shape_id() -> egui::Id {
    egui::Id::new("pt_lifted_block_shape")
}

/// Where the size of the palette chip currently being dragged is stashed.
///
/// A palette block has no laid-out block to measure — it doesn't exist in the
/// flow yet — but the thing physically in the user's hand *is* the palette
/// chip's floating preview, so matching the drop ghost to that is both the
/// honest answer and the one that looks right.
fn palette_drag_size_id() -> egui::Id {
    egui::Id::new("pt_palette_drag_size")
}

/// The floating ("picked up") subtree, accumulated as the block list renders.
///
/// Every lifted row paints into one shared layer so the whole block moves as a
/// single unit, but the translation that puts it under the pointer can only be
/// applied **after the last of those rows has been painted**:
/// [`egui::Context::transform_layer_shapes`] moves the shapes *already in* the
/// layer, so applying it from inside the head row — which always renders before
/// its body — moved the head alone and left a `FOR` loop's body and its `END`
/// sitting at their layout positions. That is what made the parts of a dragged
/// loop appear to move at different speeds.
#[derive(Default)]
struct DragLift {
    /// The shared layer every lifted row painted into.
    layer: Option<egui::LayerId>,
    /// The head row's laid-out rect — the anchor centred on the pointer, so the
    /// rest of the subtree hangs off it at its natural offsets.
    head: Option<egui::Rect>,
    /// Union of every lifted row's rect: the true size of the block in hand.
    bounds: Option<egui::Rect>,
    /// Every lifted row's rect, in layout order — the block's silhouette.
    rows: Vec<egui::Rect>,
}

impl DragLift {
    fn add(&mut self, layer: egui::LayerId, rect: egui::Rect, is_head: bool) {
        self.layer = Some(layer);
        if is_head {
            self.head = Some(rect);
        }
        self.bounds = Some(match self.bounds {
            Some(bounds) => bounds.union(rect),
            None => rect,
        });
        self.rows.push(rect);
    }

    /// Move the whole picked-up subtree under the pointer in one transform, and
    /// remember its measured height for the drop ghosts. Call once per frame,
    /// after every row has been rendered.
    fn follow_pointer(self, ctx: &egui::Context) {
        let (Some(layer), Some(head), Some(bounds)) = (self.layer, self.head, self.bounds) else {
            // Nothing in hand this frame — forget the last drag's measurement so
            // the next drag's first frame doesn't briefly shape its marker like
            // the previous (possibly much bigger) block.
            ctx.data_mut(|d| d.remove::<Vec<egui::Rect>>(lifted_shape_id()));
            return;
        };
        // Normalised to the subtree's own top-left so the marker can simply be
        // translated to wherever the block would land.
        let shape: Vec<egui::Rect> = self
            .rows
            .iter()
            .map(|r| egui::Rect::from_min_size(r.min - bounds.min.to_vec2(), r.size()))
            .collect();
        ctx.data_mut(|d| d.insert_temp(lifted_shape_id(), shape));
        if let Some(pointer) = ctx.pointer_interact_pos() {
            ctx.transform_layer_shapes(
                layer,
                egui::emath::TSTransform::from_translation(pointer - head.center()),
            );
        }
    }
}

/// The silhouette of the block currently in hand — one rect per row it will
/// occupy, positioned relative to the block's own top-left corner — so a drop
/// marker can be drawn in the *shape* of what will land there rather than a
/// rectangle covering its bounding box.
///
///   * A palette block ([`NodeKind`]) has nothing laid out in the flow to
///     measure. Its rows come from what it will flatten to — a `FOR` inserts a
///     head plus its `END`, everything else a single row — and its width from
///     the palette chip being dragged (see [`palette_drag_size_id`]), which is
///     literally the preview in the user's hand.
///   * An existing block ([`DragItem::Row`]) is measured from the floating
///     subtree itself by [`DragLift`], so a `FOR` loop's stepped outline (short
///     head, indented body, short `END`) is reproduced exactly. That
///     measurement is necessarily the previous frame's — the lifted rows are
///     painted interleaved with the very drop strips that need it — which is
///     invisible mid-drag; a drag's first frame falls back to a single block.
fn dragged_block_shape(ui: &egui::Ui) -> Vec<egui::Rect> {
    let ctx = ui.ctx();
    let one = chip_h(ui) + 10.0;
    // A width to fall back on when nothing has been measured yet. Wide enough
    // to read as a block, narrow enough that it never looks like the old
    // full-width bar.
    let default_w = 160.0;
    let stack = |w: f32, rows: usize| -> Vec<egui::Rect> {
        (0..rows)
            .map(|i| egui::Rect::from_min_size(egui::pos2(0.0, i as f32 * one), egui::vec2(w, one)))
            .collect()
    };
    if let Some(kind) = egui::DragAndDrop::payload::<NodeKind>(ctx) {
        let rows = match *kind {
            NodeKind::ForFiles | NodeKind::ForFolders | NodeKind::ForEnvs => 2,
            _ => 1,
        };
        let w = ctx
            .data(|d| d.get_temp::<egui::Vec2>(palette_drag_size_id()))
            .map(|s| s.x)
            .filter(|w| *w >= 1.0)
            .unwrap_or(default_w);
        return stack(w, rows);
    }
    let dragging_row = egui::DragAndDrop::payload::<DragItem>(ctx)
        .is_some_and(|d| matches!(&*d, DragItem::Row(_)));
    if dragging_row
        && let Some(shape) = ctx.data(|d| d.get_temp::<Vec<egui::Rect>>(lifted_shape_id()))
        && !shape.is_empty()
        && shape.iter().all(|r| r.width() >= 1.0 && r.height() >= 1.0)
    {
        // A subtree measured as shorter than a single row is a stale or
        // degenerate reading; a marker that small would be invisible.
        let h: f32 = shape
            .iter()
            .fold(f32::NEG_INFINITY, |acc, r| acc.max(r.bottom()));
        if h >= one {
            return shape;
        }
    }
    stack(default_w, 1)
}

/// The bounding size of [`dragged_block_shape`] — what the insert strips
/// animate a gap open to.
fn dragged_block_size(ui: &egui::Ui) -> egui::Vec2 {
    dragged_block_shape(ui)
        .into_iter()
        .reduce(egui::Rect::union)
        .map_or(egui::Vec2::ZERO, |r| r.max.to_vec2())
}

/// Just the height of [`dragged_block_size`] — the dimension the insert strips
/// animate open.
fn dragged_block_h(ui: &egui::Ui) -> f32 {
    dragged_block_size(ui).y
}

/// Paint the block that will land here as its own silhouette: one rounded,
/// accent-tinted rect per row, laid out from `origin` and clipped to `clip`.
///
/// Clipping (rather than scaling) is what lets the marker animate open — the
/// gap grows from nothing to the block's full height, revealing more of the
/// same fixed shape, so the block never appears to stretch. The clip also
/// bounds a very wide block to the editor.
fn paint_drop_silhouette(
    ui: &egui::Ui,
    origin: egui::Pos2,
    shape: &[egui::Rect],
    clip: egui::Rect,
    th: &GuiTheme,
) {
    if clip.width() < 1.0 || clip.height() < 1.0 {
        return;
    }
    let painter = ui.painter().with_clip_rect(clip);
    for r in shape {
        let rect = egui::Rect::from_min_size(origin + r.min.to_vec2(), r.size());
        if rect.width() < 1.0 || rect.height() < 1.0 {
            continue;
        }
        painter.rect(
            // A hair of inset between stacked rows so a loop's head, body and
            // `END` read as separate blocks rather than one column.
            rect.shrink2(egui::vec2(0.0, 2.0)),
            egui::CornerRadius::same(BLOCK_RADIUS as u8),
            mix(th.panel, th.accent, 0.18),
            egui::Stroke::new(1.5, th.accent),
            egui::StrokeKind::Inside,
        );
    }
}

/// The corner radius every block, chip and ghost shares, so an outline drawn
/// around a block traces the same silhouette the block itself has.
const BLOCK_RADIUS: f32 = 3.0;

/// A closed polyline tracing `rect` with rounded corners, for dashing along.
///
/// `egui` can dash an arbitrary path but only knows how to *fill* a rounded
/// rectangle, so a dashed rounded outline has to be approximated by hand. Eight
/// segments per quarter-turn is indistinguishable from a curve at the radii
/// blocks use, while staying cheap enough to rebuild every frame of a drag.
fn rounded_rect_path(rect: egui::Rect, radius: f32) -> Vec<egui::Pos2> {
    // A radius can never exceed half the shorter side, or the corners overlap
    // and the path folds back on itself.
    let r = radius
        .min(rect.width() * 0.5)
        .min(rect.height() * 0.5)
        .max(0.0);
    const SEGMENTS: usize = 8;
    let mut path = Vec::with_capacity(SEGMENTS * 4 + 5);
    // Corner centres and the sweep each one starts at, walking clockwise from
    // the top-left corner in screen coordinates (y grows downwards).
    let corners = [
        (
            egui::pos2(rect.left() + r, rect.top() + r),
            std::f32::consts::PI,
        ),
        (
            egui::pos2(rect.right() - r, rect.top() + r),
            1.5 * std::f32::consts::PI,
        ),
        (egui::pos2(rect.right() - r, rect.bottom() - r), 0.0),
        (
            egui::pos2(rect.left() + r, rect.bottom() - r),
            0.5 * std::f32::consts::PI,
        ),
    ];
    for (centre, start) in corners {
        for i in 0..=SEGMENTS {
            let a = start + (i as f32 / SEGMENTS as f32) * 0.5 * std::f32::consts::PI;
            path.push(egui::pos2(centre.x + r * a.cos(), centre.y + r * a.sin()));
        }
    }
    // Close the loop so the final corner joins the first.
    if let Some(&first) = path.first() {
        path.push(first);
    }
    path
}

/// Paint a faint dashed "ghost" of a block into the current (base) layer,
/// marking the slot a dragged block was lifted from. So if a block was picked
/// up by accident, there's an obvious outline showing where it came from and
/// where dropping it back would return it.
///
/// `rect` is the block's *own* rect — its indent already stripped off the left
/// edge by the caller — so the outline traces exactly where the block sat
/// rather than starting at the far left of the editor, which made a nested
/// block's ghost look like it belonged to the whole flow.
fn paint_origin_ghost(painter: &egui::Painter, rect: egui::Rect, th: &GuiTheme) {
    if rect.width() < 1.0 || rect.height() < 1.0 {
        return;
    }
    let r = rect.expand(1.0);
    let radius = BLOCK_RADIUS;
    painter.rect_filled(
        r,
        egui::CornerRadius::same(radius as u8),
        mix(th.panel, th.dim, 0.12),
    );
    // A dashed outline reads as "empty slot / drop back here" rather than a
    // solid block; dashing round a rounded path (not the four straight edges)
    // keeps its corners as round as the block's own.
    for shape in egui::Shape::dashed_line(
        &rounded_rect_path(r, radius),
        egui::Stroke::new(1.0, th.dim),
        4.0,
        3.0,
    ) {
        painter.add(shape);
    }
}

/// A modifier drop that is currently *pending* over a line — either a fresh
/// clause dragged in from the palette, or one pulled off another line and
/// carried here with its contents (see [`CarriedMod`]).
///
/// Both are answered by the same three questions — will it go here, why not,
/// and what will it look like — so the drop zone handles one type rather than
/// branching on the payload everywhere.
#[derive(Clone)]
enum PendingMod {
    New(Modifier),
    Moved(CarriedMod),
}

impl PendingMod {
    fn reject_reason(&self, node: &FlowNode, s: &crate::i18n::Strings) -> Option<&'static str> {
        match self {
            PendingMod::New(m) => m.reject_reason(node, s),
            PendingMod::Moved(carried) => carried.reject_reason(node, s),
        }
    }

    /// Perform the drop on `node`. Used on a throwaway clone to work out what
    /// the result would look like, so the preview and the drop can't disagree.
    fn apply(&self, node: &mut FlowNode) -> bool {
        match self {
            PendingMod::New(m) => attach_to_node(node, *m),
            PendingMod::Moved(carried) => carried.attach_to(node),
        }
    }
}

/// The chip a pending drop would add to `node`, and where in the chip cluster it
/// would sit.
///
/// Worked out by *rehearsing the drop* on a throwaway clone and diffing the
/// resulting chips against the current ones, so the preview can never drift from
/// what the drop actually does — the same trick
/// [`edit::detach_leaves_statement`] uses. `None` when the drop rewrites the
/// line without adding a chip of its own (outlining the whole row is the honest
/// preview then).
fn preview_chip(
    node: &FlowNode,
    pending: &PendingMod,
    req_ok: Option<bool>,
    th: &GuiTheme,
    s: &crate::i18n::Strings,
) -> Option<(usize, String, f32)> {
    let mut probe = node.clone();
    if !pending.apply(&mut probe) {
        return None;
    }
    let before = node_chips(node, req_ok, th, s);
    let after = node_chips(&probe, req_ok, th, s);
    if after.len() <= before.len() {
        return None;
    }
    let idx = before
        .iter()
        .zip(after.iter())
        .position(|(b, a)| b.ghost_shape() != a.ghost_shape())
        .unwrap_or(before.len());
    let (text, extra) = after.get(idx)?.ghost_shape();
    Some((idx, text, extra))
}

/// The ctx-data key a row parks its drop preview under. The preview is computed
/// by the drop zone, which is only laid out *after* the chip cluster it needs to
/// open a gap in, so it is handed to the next frame rather than to this one — a
/// lag of a single frame in the middle of a drag that lasts hundreds.
fn mod_ghost_id(row_index: usize) -> egui::Id {
    egui::Id::new(("pt_modghost", row_index))
}

/// A dashed, chip-shaped placeholder standing in for the block a pending drop
/// would add, drawn inline in the chip cluster at the position it will occupy —
/// so the line visibly opens up to make room and the user can see how the drop
/// changes the statement *before* letting go.
///
/// Laid out with exactly the frame a real chip uses (same margins, same minimum
/// height) and showing the chip's own text, which is what guarantees the gap is
/// the size of the block that will fill it.
fn ghost_chip(ui: &mut egui::Ui, th: &GuiTheme, text: &str, extra_width: f32) {
    let h = chip_h(ui);
    let rect = egui::Frame::NONE
        .inner_margin(egui::Margin::symmetric(8, 3))
        .corner_radius(ROUND_CHIP)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.set_min_height(h);
                ui.add(
                    egui::Label::new(RichText::new(text).color(th.dim))
                        .selectable(false)
                        .truncate(),
                );
                // The chip that lands here carries an inline editor (a name
                // field, a concurrency box, a dropdown) that has no text of its
                // own yet — reserve its width too, or the gap is narrower than
                // the block about to fill it.
                ui.add_space(extra_width);
            });
        })
        .response
        .rect;
    ui.painter().rect_filled(
        rect,
        egui::CornerRadius::same(6),
        mix(th.panel, th.accent, 0.10),
    );
    for shape in egui::Shape::dashed_line(
        &rounded_rect_path(rect, 6.0),
        egui::Stroke::new(1.0, th.accent),
        4.0,
        3.0,
    ) {
        ui.painter().add(shape);
    }
}

/// A row's geometry and its reserved background slot, collected as the rows are
/// laid out so the hover highlight can be painted once the whole pane has been
/// walked and the block under the pointer is known.
///
/// The slot matters: the highlight has to sit *behind* the row's chips, but
/// which rows light up isn't decided until every row has been measured (hovering
/// a loop header lights rows that haven't been laid out yet). Reserving a shape
/// index up-front and filling it in at the end gets both — a background that is
/// genuinely in the background, with no frame of lag.
struct RowHover {
    path: Vec<usize>,
    kind: RowKind,
    /// The block's own bounds, with the indent trimmed off (see
    /// [`indented_content`]) so the highlight hugs the block rather than
    /// starting at the far-left margin.
    rect: egui::Rect,
    bg: egui::layers::ShapeIdx,
    pointer_inside: bool,
}

fn block_row(
    ed: &mut ReportEditor,
    app: &GuiApp,
    ui: &mut egui::Ui,
    row: &edit::NodeRow,
    row_index: usize,
    selected: bool,
    drop_pos: &InsertPos,
    titles: &[String],
    lift: &mut DragLift,
    acts: &mut Vec<Act>,
) -> Option<RowHover> {
    let th = app.theme;
    let s = &app.strings;
    // Reserved *before* any of the row's own content, so whatever the hover
    // highlight later puts here is painted underneath the chips rather than over
    // them. Filled in by `paint_hover_group` once every row has been laid out.
    let bg_slot = ui.painter().add(egui::Shape::Noop);
    // Loaded environment names — the choices a BASELINE/COMPARISON dropdown
    // offers. Cheap to gather; only consulted by env-role chips.
    let env_choices: Vec<String> = app
        .session
        .global_envs
        .iter()
        .map(|e| e.name.clone())
        .collect();
    // A cheap owned copy of the node so the chip cluster / applicability checks
    // don't hold a borrow of `ed` across the inline editor (which needs `&mut`).
    let node = ed
        .flow
        .as_ref()
        .and_then(|f| node_at(f, &row.path))
        .cloned();
    // The report-request's `WITH … END` fields, rendered as a nested block under
    // the line (GUI-only — the shared `flatten` never emits WITH rows).
    let with_items: Vec<WithItem> = match &node {
        Some(FlowNode::Report(ReportStmt::Request { with, .. })) => with.clone(),
        _ => Vec::new(),
    };

    // The whole block is the request line plus any nested WITH block; capture
    // both its first-line rect (for the modifier drop zone) and its full rect
    // (for the insert strip) so a drop lands *after the whole block*.
    //
    // When this row belongs to the picked-up subtree, lift it out of its slot
    // and paint it into a floating (Tooltip-order) layer that follows the
    // pointer — so it reads as physically picked up, leaving an empty slot
    // where it was. A leaf lifts just itself; a FOR loop lifts its head, its
    // whole body and the matching `END` (every row whose path is prefixed by
    // the dragged path), all into one shared layer so they float together as a
    // single unit. This mirrors egui's own `dnd_drag_source`, which we can't
    // use directly here because the drag handle is an inner chip, not the row.
    let drag_path = dragged_row_path(ui.ctx());
    let lifted = drag_path
        .as_deref()
        .is_some_and(|d| row_is_lifted(d, &row.path));
    let is_drag_head = drag_path.as_deref() == Some(row.path.as_slice());
    // A chip dragged out on its own (rather than Ctrl-dragged to move the whole
    // line) is lifted the same way a row is: it paints into a floating layer
    // that follows the pointer and leaves a dashed ghost in the slot it came
    // from. Without that, pulling a clause out of a line looked like nothing was
    // happening at all.
    let lifted_chip = dragged_chip(ui.ctx());
    let chip_in_this_row = |which: DetachWhich| {
        lifted_chip
            .as_ref()
            .is_some_and(|(p, w)| p.as_slice() == row.path.as_slice() && *w == which)
    };
    // What the chip in hand is actually carrying (`SHOW(Time, Status)`, not just
    // "a SHOW"), read once here so every row's drop zone can ask whether that
    // clause would fit — and so dropping it on another line re-creates it with
    // its contents rather than as a fresh placeholder.
    let carried_chip: Option<(Vec<usize>, DetachWhich, CarriedMod)> =
        lifted_chip.as_ref().and_then(|(p, w)| {
            let node = ed.flow.as_ref().and_then(|f| node_at(f, p))?;
            Some((p.clone(), *w, carry_modifier(node, *w)?))
        });
    // The gap this row opened for a hovering modifier, decided by last frame's
    // drop zone (see `mod_ghost_id`).
    let mod_ghost: Option<(usize, String, f32)> = ui
        .ctx()
        .data(|d| d.get_temp(mod_ghost_id(row_index)))
        .unwrap_or(None);
    let block_body = |ui: &mut egui::Ui| -> egui::Rect {
        // Where the picked-up chip's floating layer ended up, so it can be moved
        // under the pointer once everything lifted with it has been painted.
        let mut chip_lift: Option<(egui::LayerId, egui::Rect)> = None;
        // Top-align the chip cluster (rather than the default centre alignment):
        // every chip is the same height, so top-alignment keeps them level while
        // avoiding egui's horizontal-centre re-centring, which otherwise drifts
        // each successive chip on the line progressively lower.
        let inner = ui.horizontal_top(|ui| {
            ui.add_space(row.depth as f32 * INDENT_STEP);
            match row.kind {
                // Drawn in `dim`, not `accent`: this row is the one thing in the
                // flow that is *not* in the file. It exists so there is
                // somewhere to drop a first statement and somewhere for the
                // top of the report to be. Giving it a keyword's colour made it
                // look like syntax — users reasonably asked whether `BEGIN` was
                // a word they had to write, and it isn't (the grammar has `END`,
                // but no opening keyword at all). So it stays sentence case,
                // stays translated, and reads as a caption.
                RowKind::Begin => static_chip(
                    ui,
                    &th,
                    app.strings.report_node_begin,
                    th.dim,
                    s.chip_help_begin,
                ),
                // Deliberately the literal, untranslated "END": unlike the
                // `Begin` row above, this one is not an editor invention. It
                // renders a real `END` keyword that the parser requires and
                // that the Source view shows verbatim, so translating or
                // title-casing it would make the two views disagree about what
                // is actually in the file.
                RowKind::LoopEnd => static_chip(ui, &th, "END", th.accent, s.chip_help_end),
                RowKind::Leaf | RowKind::LoopHead => {
                    let mut chips = node
                        .as_ref()
                        .map(|n| node_chips(n, row.req_ok, &th, s))
                        .unwrap_or_default();
                    // Merge each tethered chip into one pill with the chip it
                    // qualifies (see `link_tethers`) — except where the two are
                    // about to stop being neighbours, because either half is in
                    // hand or a hovering clause is about to be dropped between
                    // them.
                    link_tethers(&mut chips);
                    for i in (1..chips.len()).rev() {
                        let parted = [i - 1, i]
                            .iter()
                            .any(|&j| chips[j].detach.is_some_and(&chip_in_this_row))
                            || mod_ghost.as_ref().is_some_and(|(gi, _, _)| *gi == i);
                        if chips[i].join_prev && parted {
                            split_tether(&mut chips, i);
                        }
                    }
                    // A tethered chip is pulled flush against the chip it
                    // qualifies (see `TETHER_GAP`, which is why this keys off
                    // the anchor's `join_next` rather than the tethered chip's
                    // own flag); everything else keeps the normal inter-chip
                    // gap.
                    let gap = ui.spacing().item_spacing.x;
                    let mut prev: Option<egui::Rect> = None;
                    let mut tethers: Vec<(egui::Rect, egui::Rect)> = Vec::new();
                    for (ci, chip) in chips.iter().enumerate() {
                        // Open the gap *before* the chip the drop would land in
                        // front of, so the rest of the line slides right and the
                        // placeholder sits exactly where the new block will.
                        if let Some((gi, text, extra)) = &mod_ghost
                            && *gi == ci
                        {
                            ghost_chip(ui, &th, text, *extra);
                        }
                        if chip.join_next {
                            ui.spacing_mut().item_spacing.x = TETHER_GAP;
                        }
                        let in_hand = chip.detach.is_some_and(&chip_in_this_row);
                        let rect = if in_hand {
                            let (layer, slot) = lift_chip(
                                ui,
                                &th,
                                s,
                                chip,
                                selected,
                                &row.path,
                                row_index,
                                titles,
                                &env_choices,
                                acts,
                            );
                            chip_lift = Some((layer, slot));
                            slot
                        } else {
                            render_chip(
                                ui,
                                &th,
                                s,
                                chip,
                                selected,
                                &row.path,
                                titles,
                                &env_choices,
                                acts,
                            )
                        };
                        ui.spacing_mut().item_spacing.x = gap;
                        // Remember the pair so it can be highlighted together
                        // on hover. `split_tether` has already cleared the flag
                        // for a pair that is being pulled apart, so there is no
                        // in-hand case left to exclude here.
                        if chip.join_prev
                            && let Some(anchor) = prev
                        {
                            tethers.push((anchor, rect));
                        }
                        prev = Some(rect);
                    }
                    // A clause that appends to the end of the line gets its gap
                    // after the last chip.
                    if let Some((gi, text, extra)) = &mod_ghost
                        && *gi >= chips.len()
                    {
                        ghost_chip(ui, &th, text, *extra);
                    }
                    for (anchor, hanger) in tethers {
                        paint_tether_hover(ui, &th, anchor, hanger);
                    }
                }
            }
        });
        if !with_items.is_empty() {
            let cluster = inner.response.rect;
            // Dragging the `WITH` chip detaches the *whole* block, fields and
            // all, so the fields have to travel with it. Painting them into the
            // chip's own floating layer keeps chip and fields rigidly together
            // (one transform moves both) and leaves a ghost over the space they
            // vacated, so what is being pulled out is what you see moving.
            let lifting_with = chip_in_this_row(DetachWhich::WithBlock);
            let with_rect = match chip_lift {
                Some((layer, _)) if lifting_with => {
                    let rect = ui
                        .scope_builder(
                            egui::UiBuilder::new()
                                .layer_id(layer)
                                .layout(egui::Layout::top_down(egui::Align::Min)),
                            |ui| with_block(ui, &th, s, &row.path, row.depth, &with_items, acts),
                        )
                        .inner;
                    paint_origin_ghost(ui.painter(), rect, &th);
                    rect
                }
                _ => with_block(ui, &th, s, &row.path, row.depth, &with_items, acts),
            };
            // Enclose the request line and its WITH fields in one subtle border
            // so the block reads as a single unit — you drop *around* it, never
            // into the middle of its WITH statements. The border hugs from the
            // request line's indent down past the `END` footer. Suppressed while
            // the WITH block is being pulled off: a border drawn around a
            // half-empty unit says "still one thing" at exactly the wrong moment.
            if !lifting_with {
                let indent = row.depth as f32 * INDENT_STEP;
                let unit = egui::Rect::from_min_max(
                    egui::pos2(cluster.left() + indent, cluster.top()),
                    egui::pos2(cluster.right().max(with_rect.right()), with_rect.bottom()),
                )
                .expand(3.0);
                ui.painter().rect_stroke(
                    unit,
                    egui::CornerRadius::same(6),
                    egui::Stroke::new(1.0, mix(th.panel, th.subst, 0.55)),
                    egui::StrokeKind::Outside,
                );
            }
        }
        // Everything that belongs to the picked-up chip's layer is painted, so
        // it is finally safe to move the layer under the pointer.
        if let Some((layer, slot)) = chip_lift {
            follow_pointer(ui.ctx(), layer, slot);
        }
        inner.response.rect
    };
    if lifted {
        // All lifted rows share one layer (keyed by the dragged path) so a
        // single transform moves the whole subtree together, keeping each row's
        // relative offset. Their slots stay blank (the `scope_builder` still
        // reserves the space), and we skip the drop targets below — you can't
        // drop onto a row that's currently in your hand.
        let layer_id = egui::LayerId::new(
            egui::Order::Tooltip,
            ui.id()
                .with(("pt_drag_subtree", drag_path.as_ref().unwrap())),
        );
        let ir = ui.scope_builder(
            egui::UiBuilder::new()
                .layer_id(layer_id)
                .layout(egui::Layout::top_down(egui::Align::Min)),
            block_body,
        );
        // The row's laid-out rect starts at the editor's left margin because the
        // indent is `add_space`d *inside* the layout; strip it back off so both
        // the origin ghost and the lift's measurement describe the block itself,
        // not the block plus a stripe of empty indent.
        let content = indented_content(ir.response.rect, row.depth);
        // Leave a dashed ghost in the (now blank) origin slot so the lift is
        // obviously reversible — dropping the block back here restores it.
        paint_origin_ghost(ui.painter(), content, &th);
        // Hand the row to the shared lift; the single transform that puts the
        // whole subtree under the pointer is applied once every row has been
        // painted (see `DragLift`), never from here.
        lift.add(layer_id, content, is_drag_head);
        return None;
    }
    let block = ui.vertical(block_body);
    let cluster = block.inner;
    let block_rect = block.response.rect;
    // The block's own bounds (indent stripped), and whether the pointer is over
    // them. `rect_contains_pointer` rather than an `interact` so this purely
    // observational read never competes with the row's chips or its drop zones
    // for the click.
    let hover_rect = indented_content(block_rect, row.depth);
    let hover = RowHover {
        path: row.path.clone(),
        kind: row.kind,
        rect: hover_rect,
        bg: bg_slot,
        pointer_inside: ui.rect_contains_pointer(hover_rect),
    };

    // ── Modifier drop zone: dropping a modifier chip onto a real node attaches
    // it. Only reacts to `Modifier` payloads, so it never competes with the base
    // insert strip below (which reacts to `NodeKind`).
    if let Some(n) = &node
        && matches!(row.kind, RowKind::Leaf | RowKind::LoopHead)
    {
        // The modifier drop zone spans the whole first line to the right of the
        // base chip (not just the chips), so a modifier can be dropped anywhere
        // on the row rather than having to hit the small cluster exactly.
        let zone_rect =
            egui::Rect::from_x_y_ranges(cluster.left()..=ui.max_rect().right(), cluster.y_range());
        let zresp = ui.interact(
            zone_rect,
            ui.id().with(("pt_modzone", row_index)),
            egui::Sense::hover(),
        );
        // The zone takes a fresh clause from the palette *or* one pulled off
        // another line — a `SHOW` lifted from one reported request drops onto
        // the next, bringing the columns it was carrying with it.
        let pending: Option<PendingMod> = if let Some(m) = zresp.dnd_hover_payload::<Modifier>() {
            Some(PendingMod::New(*m))
        } else if zresp.dnd_hover_payload::<DragItem>().is_some() {
            carried_chip
                .as_ref()
                .filter(|(from, _, _)| from.as_slice() != row.path.as_slice())
                .map(|(_, _, carried)| PendingMod::Moved(carried.clone()))
        } else {
            None
        };

        // Park the gap this row should open for the hovering clause. Computed
        // here (where the hover is known) and consumed by the chip cluster on
        // the next frame — see `mod_ghost_id`.
        let ghost = match &pending {
            Some(p) if p.reject_reason(n, s).is_none() => preview_chip(n, p, row.req_ok, &th, s),
            _ => None,
        };
        ui.ctx()
            .data_mut(|d| d.insert_temp(mod_ghost_id(row_index), ghost.clone()));

        if let Some(p) = &pending {
            match p.reject_reason(n, s) {
                None => {
                    // A row that has already opened a gap showing exactly where
                    // the clause lands needs no outline as well — the gap *is*
                    // the highlight, and the box round the whole line only
                    // competed with it.
                    if ghost.is_none() {
                        ui.painter().rect_stroke(
                            zone_rect.expand(2.0),
                            egui::CornerRadius::same(6),
                            egui::Stroke::new(2.0, th.accent),
                            egui::StrokeKind::Outside,
                        );
                    }
                }
                // A refusal gets its own (error-coloured) outline plus the
                // reason at the pointer, so the chip springing back reads as
                // "not here, because…" rather than as a missed drop.
                Some(why) => {
                    ui.painter().rect_stroke(
                        zone_rect.expand(2.0),
                        egui::CornerRadius::same(6),
                        egui::Stroke::new(2.0, th.err),
                        egui::StrokeKind::Outside,
                    );
                    egui::Tooltip::always_open(
                        ui.ctx().clone(),
                        ui.layer_id(),
                        ui.id().with(("pt_modwhy", row_index)),
                        egui::PopupAnchor::Pointer,
                    )
                    .show(|ui| {
                        ui.colored_label(th.err, why);
                    });
                }
            }
        }
        if let Some(m) = release_payload::<Modifier>(&zresp)
            && m.applies_to(n)
        {
            acts.push(Act::AttachMod {
                path: row.path.clone(),
                modifier: *m,
            });
        }
        // Shift is read at the *drop*, not at the pick-up, so the user can
        // change their mind mid-drag — and so the decision is made at the
        // moment they can see where the clause is about to land.
        let copy = ui.input(|i| i.modifiers.shift);
        // Every guard is checked *before* taking the payload: releasing it is
        // destructive, and swallowing a `DragItem::Row` here (or a clause this
        // line won't take) would silently cancel a block reorder that the
        // insert strip below is about to handle.
        if let Some((from, which, carried)) = &carried_chip
            && from.as_slice() != row.path.as_slice()
            && carried.applies_to(n)
            && release_payload::<DragItem>(&zresp).is_some()
        {
            acts.push(Act::MoveMod {
                from: from.clone(),
                which: *which,
                to: row.path.clone(),
                copy,
            });
        }
    }

    // ── Base insert strip: a full-width strip over the block that, when a base
    // block is dragged over it, opens an animated gap below the block (the
    // existing blocks slide down to make room) with a dashed placeholder where
    // the new block will land. The strip is sized to include the currently-open
    // gap (read from last frame) so the pointer stays over it as the gap opens —
    // avoiding open/close flicker at the seam. The gap is sized to the block
    // actually in hand (see `dragged_block_h`), so dragging a whole `FOR` loop
    // or a request with `WITH` fields opens a gap the size of that whole block
    // rather than a one-line sliver it obviously won't fit into.
    let gap_h = dragged_block_h(ui);
    let gap_id = ui.id().with(("pt_gap", row_index));
    let prev_gap: f32 = ui.ctx().data(|d| d.get_temp(gap_id)).unwrap_or(0.0);
    let strip = egui::Rect::from_x_y_ranges(
        ui.max_rect().x_range(),
        block_rect.top()..=block_rect.bottom() + prev_gap,
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
            .animate_value_with_time(gap_id, if hovering_base { gap_h } else { 0.0 }, 0.12);
    ui.ctx().data_mut(|d| d.insert_temp(gap_id, gap));
    if let Some(kind) = release_payload::<NodeKind>(&strip_resp) {
        acts.push(Act::DropNode {
            pos: drop_pos.clone(),
            node: node_for_kind(*kind, titles),
        });
    } else if let Some(item) = release_payload::<DragItem>(&strip_resp) {
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
        // Indent the silhouette to the depth the block will *land* at, not the
        // depth of the row it is hovering. They differ wherever a drop steps
        // inward: after `BEGIN` (a synthetic depth-0 row whose statements are
        // depth 1) and after a `FOR` header (which inserts into the loop body).
        // A drop path's depth is one per body it nests inside, plus one for the
        // top level, so it is read off the insert position itself and can't
        // drift from where the block really goes.
        let indent = (drop_pos.parent.len() + 1) as f32 * INDENT_STEP;
        let top = block_rect.bottom() + 2.0;
        let origin = egui::pos2(strip.left() + indent, top);
        // The marker is a preview of the block: its own outline, at its own
        // indent, revealed as the gap animates open.
        let clip =
            egui::Rect::from_min_max(origin, egui::pos2(strip.right() - 8.0, top + gap - 4.0));
        paint_drop_silhouette(ui, origin, &dragged_block_shape(ui), clip, &th);
        ui.add_space(gap);
    }
    Some(hover)
}

/// The block under the pointer, if any: the *last* matching row, so a nested row
/// wins over the loop whose rect happens to reach it.
///
/// The synthetic `Begin` row is never a candidate. Its path is empty, which
/// every other path starts with, so treating it as hoverable would light the
/// entire report the moment the pointer crossed the top of the pane — and
/// `Begin` isn't a block you can select or move anyway, so the highlight would
/// be promising something that can't happen.
fn hovered_block(rows: &[RowHover]) -> Option<&[usize]> {
    rows.iter()
        .rev()
        .find(|r| r.pointer_inside && r.kind != RowKind::Begin)
        .map(|r| r.path.as_slice())
}

/// Paint the hover highlight: a filled panel behind the block under the pointer,
/// and a softer one behind everything that would travel with it.
///
/// Skipped entirely while something is being dragged. A drag already says what
/// is in hand — the lifted subtree floats under the pointer and leaves dashed
/// ghosts behind — and adding a second, differently-shaped highlight to that
/// only muddled which of the two to read.
fn paint_hover_group(ui: &egui::Ui, th: &GuiTheme, rows: &[RowHover]) {
    if drag_in_flight(ui.ctx()) {
        return;
    }
    let Some(hovered) = hovered_block(rows) else {
        return;
    };
    // Derived from the panel colour rather than fixed, so the highlight stays a
    // gentle lift off the background on a light theme as well as a dark one.
    let block = mix(th.panel, th.accent, 0.28);
    let carried = mix(th.panel, th.accent, 0.12);
    for row in rows {
        let Some(tier) = hover_tier(hovered, &row.path) else {
            continue;
        };
        let fill = match tier {
            HoverTier::Block => block,
            HoverTier::CarriedAlong => carried,
        };
        ui.painter().set(
            row.bg,
            egui::Shape::rect_filled(
                row.rect.expand2(egui::vec2(4.0, 1.0)),
                egui::CornerRadius::same(4),
                fill,
            ),
        );
    }
}

/// Whether anything at all is currently being dragged in the block editor — a
/// palette block, a modifier clause or an existing row. Each travels as its own
/// payload type, so all three have to be asked.
fn drag_in_flight(ctx: &egui::Context) -> bool {
    egui::DragAndDrop::has_payload_of_type::<DragItem>(ctx)
        || egui::DragAndDrop::has_payload_of_type::<NodeKind>(ctx)
        || egui::DragAndDrop::has_payload_of_type::<Modifier>(ctx)
}

/// Render the `WITH … END` fields of a report-request as a nested block under
/// its line (indented one level): each field is an editable `name: query` row
/// with a `×` to remove it, followed by an "add field" affordance and an `END`
/// footer aligned to the request line. This is a **GUI-only** view over
/// [`ReportStmt::Request::with`] — the shared [`flatten`] deliberately doesn't
/// emit WITH rows, so this never affects the TUI or the flow's drop-index maths.
fn with_block(
    ui: &mut egui::Ui,
    th: &GuiTheme,
    s: &crate::i18n::Strings,
    path: &[usize],
    depth: usize,
    items: &[WithItem],
    acts: &mut Vec<Act>,
) -> egui::Rect {
    let field_indent = (depth as f32 + 1.0) * INDENT_STEP;
    let tint = chip_tint(th, th.subst);
    ui.vertical(|ui| {
        for (i, item) in items.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.add_space(field_indent);
                let text = match item {
                    // The field's own STATISTICS(…) belongs on its row: leaving
                    // it out made a clause that is plainly there in the source
                    // invisible in the block editor.
                    WithItem::Field {
                        name, query, stats, ..
                    } => {
                        let mut t = format!("{name}: {query}");
                        if !stats.is_empty() {
                            t.push_str(&format!(
                                " STATISTICS({})",
                                stats
                                    .iter()
                                    .map(|k| k.keyword())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ));
                        }
                        t
                    }
                    WithItem::ResponseFmt(fmt) => format!(
                        "RESPONSE {}",
                        match fmt {
                            crate::report::flow::ResponseFmt::Raw => "RAW",
                            crate::report::flow::ResponseFmt::Pretty => "PRETTY",
                        }
                    ),
                };
                let lbl = chip_shell(ui, &tint, true, ROUND_CHIP, |ui| {
                    let lbl = ui.add(
                        egui::Label::new(RichText::new(&text).color(tint.text))
                            .selectable(false)
                            .sense(egui::Sense::click()),
                    );
                    if detach_x(ui, tint.text) {
                        acts.push(Act::RemoveWith {
                            path: path.to_vec(),
                            index: i,
                        });
                    }
                    lbl
                });
                // Only `name: query` fields have a wizard; a bare `WITH RESPONSE`
                // item is edited/removed via its `×` only.
                if lbl.clicked() && matches!(item, WithItem::Field { .. }) {
                    acts.push(Act::EditWith {
                        path: path.to_vec(),
                        index: i,
                    });
                }
                // A `WITH` field is a report column with a name of its own, so
                // it is what a `STATISTICS` clause attaches to — the request
                // line above has no single column to summarise. The row is
                // therefore its own drop target: without one the clause bounced
                // off every part of a `WITH` block, and there was no way at all
                // to add a summary to a field by dragging.
                let zone = egui::Rect::from_x_y_ranges(
                    lbl.rect.left()..=ui.max_rect().right(),
                    lbl.rect.y_range(),
                );
                let zresp = ui.interact(
                    zone,
                    ui.id().with(("pt_withstats", path, i)),
                    egui::Sense::hover(),
                );
                if let Some(m) = zresp.dnd_hover_payload::<Modifier>() {
                    let ok = *m == Modifier::Statistics && edit::with_stats_applies(items, i);
                    ui.painter().rect_stroke(
                        zone.expand(2.0),
                        egui::CornerRadius::same(6),
                        egui::Stroke::new(2.0, if ok { th.accent } else { th.err }),
                        egui::StrokeKind::Outside,
                    );
                    // The refusals a field row can give differ from a node's, so
                    // they are spelled out here rather than through
                    // `Modifier::reject_reason` (which only speaks about nodes).
                    if !ok {
                        let why = if *m != Modifier::Statistics {
                            s.mod_reject_with_field
                        } else {
                            s.mod_reject_present
                        };
                        egui::Tooltip::always_open(
                            ui.ctx().clone(),
                            ui.layer_id(),
                            ui.id().with(("pt_withwhy", path, i)),
                            egui::PopupAnchor::Pointer,
                        )
                        .show(|ui| {
                            ui.colored_label(th.err, why);
                        });
                    }
                }
                if let Some(m) = release_payload::<Modifier>(&zresp)
                    && *m == Modifier::Statistics
                    && edit::with_stats_applies(items, i)
                {
                    acts.push(Act::AttachWithStats {
                        path: path.to_vec(),
                        index: i,
                    });
                }
            });
        }
        ui.horizontal(|ui| {
            ui.add_space(field_indent);
            if ui
                .add(
                    egui::Button::new(
                        RichText::new(format!("{} {}", super::icons::PLUS, s.gui_report_with_add))
                            .color(th.subst),
                    )
                    .small(),
                )
                .clicked()
            {
                acts.push(Act::AddWith {
                    path: path.to_vec(),
                });
            }
        });
        ui.horizontal(|ui| {
            ui.add_space(depth as f32 * INDENT_STEP);
            static_chip(ui, th, "END", th.accent, "");
        });
    })
    .response
    .rect
}

/// A catch-all drop target filling the empty space beneath the last row: a base
/// block dropped here is appended as the last top-level line, and an existing
/// row dragged here is moved to the end. `top_len` is the current number of
/// top-level nodes (the append index). A no-op when there is no spare vertical
/// space (the report already fills / overflows the viewport — the last row's own
/// insert strip covers appending in that case).

/// One option in the `collection:` dropdown.
///
/// The label and the stored value are deliberately different things. What gets
/// written into the directive has to be a *path*, because that is literally
/// what the runner opens (`report_cli` does a `read_to_string` on it); but a
/// path is a poor thing to pick from a list, so the user sees the collection's
/// name and the path only as secondary detail.
#[derive(Clone, Debug, PartialEq)]
struct CollectionChoice {
    /// Written into `# collection:` — relative to the report when it can be,
    /// so a report and its collection can be moved together.
    value: String,
    /// The collection's name, which is what the user actually recognises.
    label: String,
    /// Where it is, shown under the name to tell two same-named collections
    /// apart: relative to the workspace root for a workspace file, otherwise
    /// the path as stored.
    detail: String,
    /// Whether it lives inside this report's workspace. Those are listed first
    /// and shown by default; anything else is a deliberate reach outside.
    in_workspace: bool,
}

/// The collections a report can bind to: every collection file in its
/// workspace, plus any collection open in a tab.
///
/// The workspace is scanned rather than read from the open tabs because a
/// workspace usually holds far more collections than are open at any moment,
/// and those are exactly the ones a report living in that workspace is likely
/// to want. Open tabs are still offered (a report doesn't have to live in a
/// workspace at all), but only when they aren't already in the scan.
fn collection_choices(
    root: Option<&std::path::Path>,
    report_path: Option<&std::path::Path>,
    open: &[crate::collection::Collection],
    unsaved_label: &str,
) -> Vec<CollectionChoice> {
    let mut out: Vec<CollectionChoice> = Vec::new();
    let root_scope = root;

    if let Some(root) = root {
        for e in crate::workspace::scan_workspace(root, true) {
            if e.is_dir || !is_collection_file(&e.path) {
                continue;
            }
            out.push(CollectionChoice {
                value: portable_ref(&e.path, report_path, root_scope),
                label: collection_label(&e.path),
                detail: e
                    .path
                    .strip_prefix(root)
                    .unwrap_or(&e.path)
                    .to_string_lossy()
                    .into_owned(),
                in_workspace: true,
            });
        }
    }

    for c in open {
        match c.path.as_deref() {
            Some(p) => {
                // Already offered by the scan above — listing it twice would
                // only invite picking the "wrong" identical one.
                let value = portable_ref(p, report_path, root_scope);
                if out.iter().any(|ch| ch.value == value) {
                    continue;
                }
                out.push(CollectionChoice {
                    value,
                    label: c.name.clone(),
                    detail: p.to_string_lossy().into_owned(),
                    in_workspace: root.is_some_and(|r| p.starts_with(r)),
                });
            }
            // An unsaved collection has no path to write, so it can only be
            // referenced by name — which is the fallback
            // `resolve_bound_collection` keeps for exactly this case. It won't
            // resolve for the headless runner, hence the explicit label.
            None => out.push(CollectionChoice {
                value: c.name.clone(),
                label: c.name.clone(),
                detail: unsaved_label.to_string(),
                in_workspace: false,
            }),
        }
    }

    // Workspace files first, then alphabetically, so the list is stable no
    // matter what order the tabs happen to be in.
    out.sort_by(|a, b| {
        b.in_workspace
            .cmp(&a.in_workspace)
            .then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase()))
            .then_with(|| a.value.cmp(&b.value))
    });
    out
}

/// Whether `path` is a collection, as opposed to the reports and environments
/// that share a workspace with it.
fn is_collection_file(path: &std::path::Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("hurl") | Some("json")
    )
}

/// The name to show for a collection file: its filename without the extension.
/// `# collection:` stores a path, but a path is not what the user named the
/// thing, so the list shows this and keeps the path as detail.
fn collection_label(path: &std::path::Path) -> String {
    path.file_stem()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// The body of the `collection:` dropdown: the workspace's own collections
/// under a heading, and anything else behind an opt-in toggle.
///
/// Collections outside the report's workspace are hidden by default because
/// they are almost always the wrong answer — binding to one writes a path that
/// escapes the workspace, so the report stops travelling with it. They stay one
/// click away rather than being removed, since a report needn't live in a
/// workspace at all (and when it doesn't, there is nothing to hide behind and
/// everything is shown).
fn collection_menu(
    ui: &mut egui::Ui,
    th: &GuiTheme,
    s: &crate::i18n::Strings,
    choices: &[CollectionChoice],
    current: &str,
    picked: &mut Option<String>,
    browse: &mut bool,
) {
    let (mine, others): (Vec<&CollectionChoice>, Vec<&CollectionChoice>) =
        choices.iter().partition(|c| c.in_workspace);

    let show_all_id = ui.make_persistent_id("pt_hdr_collection_show_all");
    // With no workspace to scope to there is nothing to reveal, so the toggle
    // would only be a switch that does nothing.
    let mut show_all = mine.is_empty()
        || ui
            .ctx()
            .data(|d| d.get_temp::<bool>(show_all_id))
            .unwrap_or(false);

    if !mine.is_empty() {
        ui.label(
            RichText::new(s.gui_report_ws_collections)
                .color(th.dim)
                .small(),
        );
        for c in &mine {
            collection_item(ui, th, c, current, picked);
        }
    }

    if !others.is_empty() {
        if mine.is_empty() {
            show_all = true;
        } else {
            ui.separator();
            if ui
                .checkbox(&mut show_all, s.gui_report_show_all_collections)
                .changed()
            {
                ui.ctx().data_mut(|d| d.insert_temp(show_all_id, show_all));
            }
        }
        if show_all {
            if !mine.is_empty() {
                ui.label(
                    RichText::new(s.gui_report_other_collections)
                        .color(th.dim)
                        .small(),
                );
            }
            for c in &others {
                collection_item(ui, th, c, current, picked);
            }
        }
    }

    // A report outside a workspace, opened before any collection, has nothing
    // to list at all — and `collection:` is the one setting a report can't run
    // without. An empty menu would be a dead end, so say so and offer the way
    // out. Browse is always available; the list is a shortcut, not the only way.
    if choices.is_empty() {
        ui.colored_label(th.dim, s.gui_report_no_collections);
    }
    if !choices.is_empty() {
        ui.separator();
    }
    if ui.button(s.gui_report_browse).clicked() {
        *browse = true;
        ui.close();
    }
}

/// One row of the collection dropdown: the name, with where it lives beneath it
/// so two collections that share a name can still be told apart.
fn collection_item(
    ui: &mut egui::Ui,
    th: &GuiTheme,
    c: &CollectionChoice,
    current: &str,
    picked: &mut Option<String>,
) {
    if ui
        .selectable_label(c.value == current, &c.label)
        .on_hover_text(&c.detail)
        .clicked()
    {
        *picked = Some(c.value.clone());
    }
    ui.label(RichText::new(&c.detail).color(th.dim).small());
}

/// The workspace a report belongs to, if any: the deepest open workspace root
/// that contains it. Deepest, because workspaces can be nested and the closest
/// one is the one whose collections are actually relevant.
fn report_workspace_root(app: &GuiApp, ed: &ReportEditor) -> Option<std::path::PathBuf> {
    let path = ed.report.path.as_deref()?;
    app.session
        .collections
        .iter()
        .filter_map(|c| c.workspace_root.as_ref())
        .filter(|root| path.starts_with(root))
        .max_by_key(|root| root.components().count())
        .cloned()
}

/// The panel the report settings are drawn in.
///
/// Enclosing them says what a column of chips above `BEGIN` could not: these
/// are settings *for* the report, not the first steps *of* it. Unframed they
/// read as blocks that merely happened not to be draggable. The muted fill and
/// quiet border mark the boundary without competing with the flow for
/// attention, and the frame's own left edge lines up with the blocks below so
/// the two still read as one column.
fn settings_frame(th: &GuiTheme) -> egui::Frame {
    egui::Frame::NONE
        .fill(mix(th.panel, th.dim, 0.10))
        .stroke(egui::Stroke::new(1.0, mix(th.panel, th.dim, 0.35)))
        .inner_margin(egui::Margin::symmetric(8, 6))
        .corner_radius(BLOCK_RADIUS as u8)
}

/// The report's `# key: value` header directives, drawn as a pinned strip above
/// `BEGIN`.
///
/// These are settings for the report as a *whole* — which collection it runs
/// against, where its results go — not steps in the flow, so they are
/// deliberately not blocks: there is nothing meaningful about reordering them,
/// dropping one inside a loop, or binning them onto the trash bar. Keeping them
/// in a fixed strip above `BEGIN` says exactly that, while still making them
/// discoverable and editable, which they previously weren't from the GUI at all.
///
/// The two directives a report can't run without are always shown (as an unset
/// prompt when empty); the rest appear once set, or on demand from the `+`.
fn header_strip(ed: &ReportEditor, app: &GuiApp, ui: &mut egui::Ui, acts: &mut Vec<Act>) {
    let th = app.theme;
    let s = &app.strings;
    let Some(flow) = ed.flow.as_ref() else {
        return;
    };
    // Built on demand rather than every frame: it scans the workspace off disk,
    // and the list is only ever looked at while the dropdown is open.
    let ws_root = report_workspace_root(app, ed);
    let collections = || {
        collection_choices(
            ws_root.as_deref(),
            ed.report.path.as_deref(),
            &app.session.collections,
            s.gui_report_collection_unsaved,
        )
    };
    let envs: Vec<String> = app
        .session
        .global_envs
        .iter()
        .map(|e| e.name.clone())
        .collect();

    let formats: Vec<String> = crate::report::writer::OUTPUT_EXTENSIONS
        .iter()
        .map(|e| e.to_string())
        .collect();

    settings_panel(
        ui,
        &th,
        s,
        &|key| flow.header.get(key).unwrap_or_default().to_string(),
        &collections,
        &envs,
        &formats,
        acts,
    );
}

/// The settings panel itself, given only the values it shows.
///
/// Split from `header_strip` so the layout can be exercised without standing up
/// a whole `GuiApp`: the caller resolves each directive's current value and the
/// choices its dropdowns offer, and this decides what is drawn and in what
/// order.
#[allow(clippy::too_many_arguments)]
fn settings_panel(
    ui: &mut egui::Ui,
    th: &GuiTheme,
    s: &crate::i18n::Strings,
    value_of: &dyn Fn(&str) -> String,
    collections: &dyn Fn() -> Vec<CollectionChoice>,
    envs: &Vec<String>,
    formats: &Vec<String>,
    acts: &mut Vec<Act>,
) {
    let specs = header_specs();
    settings_frame(th).show(ui, |ui| {
        // One setting per line, each starting at the same left edge as `BEGIN`
        // below. Laying them out in a row instead left them ragged — a combo
        // chip and a text chip are different widths and sit differently in a
        // wrapped line.
        ui.set_width(settings_width(ui));
        for spec in &specs {
            let value = value_of(spec.key);
            if value.is_empty() && !spec.always_shown {
                continue;
            }
            let choices = match spec.kind {
                HeaderKind::Environment => Some(envs),
                HeaderKind::Format => Some(formats),
                _ => None,
            };
            ui.horizontal(|ui| header_chip(ui, th, s, spec, &value, choices, collections, acts));
        }

        // Optional directives that aren't set yet are offered from the button
        // rather than shown as a row of empty prompts, which would bury the two
        // that matter. It sits below the settings it appends to, where a list's
        // "add another" belongs. When every setting is already present there is
        // nothing to add, so it isn't drawn at all.
        let missing: Vec<&HeaderSpec> = specs
            .iter()
            .filter(|sp| !sp.always_shown && value_of(sp.key).is_empty())
            .collect();
        if !missing.is_empty() {
            header_add_menu(ui, s, &missing, acts);
        }
    });
}

/// How wide the settings panel is drawn.
///
/// Deliberately fixed rather than shrink-wrapped: the contents change width
/// constantly (picking a longer collection name, adding a setting, clearing
/// one) and a panel that resized with them would make the whole view twitch
/// every time a dropdown was used. It still yields to a narrow editor pane so
/// the panel can never overflow its column.
///
/// Wide enough for a realistic collection or baseline name to sit in its
/// dropdown unabbreviated: these are file names, and truncating them to
/// `billing-servi…` defeats the point of showing names instead of paths.
fn settings_width(ui: &egui::Ui) -> f32 {
    const SETTINGS_W: f32 = 460.0;
    SETTINGS_W.min(ui.available_width())
}

/// The menu offering the optional directives that aren't set yet.
///
/// Spelled out rather than a bare `+`: on its own, a plus above the flow gives
/// no hint whether it adds a *block* (which is what everything else in this
/// view does) or a report-wide setting. The button also carries the explanation
/// of what report settings are, which previously hung off a decorative icon
/// beside it that did nothing else.
fn header_add_menu(
    ui: &mut egui::Ui,
    s: &crate::i18n::Strings,
    missing: &[&HeaderSpec],
    acts: &mut Vec<Act>,
) {
    let label = format!("{}  {}", super::icons::PLUS, s.report_add_setting);
    ui.menu_button(label, |ui| {
        for spec in missing {
            if ui
                .button(spec.key.to_uppercase())
                .on_hover_text(edit::header_help(spec.key, s))
                .clicked()
            {
                // Seed with a placeholder so the chip appears; the user
                // then types or picks the real value. An empty string
                // would immediately be dropped again by `set_header`.
                acts.push(Act::SetHeader {
                    key: spec.key,
                    value: Some(HEADER_PLACEHOLDER.to_string()),
                });
                ui.close();
            }
        }
    })
    .response
    .on_hover_text(s.report_settings_help);
}

/// One chip in the header strip: an uppercase key label plus its editor.
fn header_chip(
    ui: &mut egui::Ui,
    th: &GuiTheme,
    s: &crate::i18n::Strings,
    spec: &HeaderSpec,
    value: &str,
    choices: Option<&Vec<String>>,
    collections: &dyn Fn() -> Vec<CollectionChoice>,
    acts: &mut Vec<Act>,
) {
    // An unset required directive is drawn in the error colour: it is the one
    // thing standing between the report and a run, so it should look like it.
    let unset = value.is_empty() || value == "?";
    let color = if unset && spec.required {
        th.err
    } else {
        th.dim
    };
    let mut tint = chip_tint(th, color);
    if !(unset && spec.required) {
        // A settings row is chrome, not a category: its rule would otherwise be
        // a grey bar saying nothing. An *unset required* one keeps its red rule,
        // where the bar is doing real work — it marks the row blocking the run.
        tint.rule = None;
    }
    tint.text = if unset && spec.required {
        th.err
    } else {
        th.text
    };
    let key = spec.key;
    let text_col = tint.text;

    let combo = matches!(
        spec.kind,
        HeaderKind::Collection | HeaderKind::Environment | HeaderKind::Format
    );

    let scope = ui.scope(|ui| {
        // A combo box sets its own (tallest) height; everything else has to be
        // grown to match, exactly as in the flow's chips.
        chip_shell(ui, &tint, !combo, ROUND_CHIP, |ui| {
            // The key label goes on the left, but a combo box is taller than a
            // label and only sets the row height once it has been added — a
            // label placed first would be "centred" in a still-short row and end
            // up sitting above the combo's own text. So reserve its slot now and
            // paint it after, centred against the finished row. (The flow's
            // request chips do exactly this, for exactly this reason.)
            let font = egui::TextStyle::Button.resolve(ui.style());
            let galley = ui.painter().layout_no_wrap(key.to_uppercase(), font, color);
            let gsize = galley.size();
            let (label_rect, _) = ui.allocate_exact_size(gsize, egui::Sense::hover());
            match spec.kind {
                HeaderKind::Collection | HeaderKind::Environment | HeaderKind::Format => {
                    // A collection is stored as a path but shown by name, so
                    // the closed text has to be derived rather than echoed.
                    let shown = if unset {
                        s.report_setting_unset.to_string()
                    } else if matches!(spec.kind, HeaderKind::Collection) {
                        collection_label(std::path::Path::new(value))
                    } else {
                        value.to_string()
                    };
                    let mut picked = None;
                    let mut browse = false;
                    egui::ComboBox::from_id_salt(("pt_hdr", key))
                        .selected_text(RichText::new(shown).color(text_col))
                        .show_ui(ui, |ui| {
                            if matches!(spec.kind, HeaderKind::Collection) {
                                collection_menu(
                                    ui,
                                    th,
                                    s,
                                    &collections(),
                                    value,
                                    &mut picked,
                                    &mut browse,
                                );
                            } else {
                                for c in choices.map(Vec::as_slice).unwrap_or_default() {
                                    if ui.selectable_label(c == value, c).clicked() {
                                        picked = Some(c.clone());
                                    }
                                }
                            }
                        });
                    if browse {
                        acts.push(Act::PickHeaderFile { key });
                    }
                    if let Some(v) = picked
                        && v != value
                    {
                        acts.push(Act::SetHeader {
                            key,
                            value: Some(v),
                        });
                    }
                }
                HeaderKind::Folder | HeaderKind::File | HeaderKind::Text => {
                    let current = if value == "?" { "" } else { value };
                    let id = ui.make_persistent_id(("pt_hdr_text", key));
                    if let Some(text) = inline_text_edit(
                        ui,
                        id,
                        current,
                        s.report_setting_unset,
                        "",
                        150.0,
                        FIELD_MAX_WIDTH,
                    ) && text != current
                    {
                        acts.push(Act::SetHeader {
                            key,
                            value: Some(text),
                        });
                    }
                    if spec.kind.is_path()
                        && ui
                            .small_button(super::icons::FOLDER)
                            .on_hover_text(s.gui_report_browse)
                            .clicked()
                    {
                        acts.push(Act::PickHeaderFile { key });
                    }
                }
            }
            // Anything actually set can be cleared, `collection:` included —
            // an always-shown setting simply falls back to its unset prompt
            // rather than disappearing, and the rest return to the add menu.
            //
            // An *optional* setting keeps its `×` even while unset: it was put
            // here from the add menu and starts life showing the unset prompt,
            // so without one there would be no way to take it off again.
            if (!unset || !spec.always_shown) && detach_x(ui, color) {
                acts.push(Act::SetHeader { key, value: None });
            }
            let cy = ui.min_rect().center().y;
            ui.painter().galley(
                egui::pos2(label_rect.left(), cy - gsize.y / 2.0),
                galley,
                color,
            );
        });
    });
    if ui.ctx().dragged_id().is_none() {
        scope.response.on_hover_text(edit::header_help(key, s));
    }
}

/// Browse for the file (or folder, for `root:`) a path-valued header directive
/// should point at, and store it relative to the report when that is shorter —
/// a report and its data usually travel together, so an absolute path would
/// break the moment the pair moved.
fn pick_header_file(ed: &mut ReportEditor, app: &mut GuiApp, key: &'static str) {
    let seed = ed
        .flow
        .as_ref()
        .and_then(|f| f.header.get(key))
        .and_then(super::filepick::seed_dir)
        .or_else(|| {
            ed.report
                .path
                .as_deref()
                .and_then(|p| p.parent())
                .map(std::path::Path::to_path_buf)
        });
    let title = edit::header_help(key, &app.strings);
    // Only `root:` and `baseline:` are paths — `output:` names a format and is
    // picked from a list, so it never reaches here.
    let picked = match key {
        "root" => super::filepick::pick_folder(title, seed.as_deref()),
        // `collection:` is normally chosen from the dropdown, but that list can
        // be empty (a report outside a workspace, opened before any collection),
        // so Browse is offered there too and lands here.
        "collection" => super::filepick::pick_file(
            title,
            seed.as_deref(),
            &[("hurl", &["hurl"]), ("*", &["*"])],
        ),
        _ => super::filepick::pick_file(
            title,
            seed.as_deref(),
            &[("baseline", &["baseline", "json"]), ("*", &["*"])],
        ),
    };
    let Some(path) = picked else {
        return;
    };
    // A browsed collection is relativised the same way a picked one is — the
    // dropdown writes a portable `../`-walking ref, and Browse must not quietly
    // bake in an absolute path instead.
    let text = if key == "collection" {
        portable_ref(
            &path,
            ed.report.path.as_deref(),
            report_workspace_root(app, ed).as_deref(),
        )
    } else {
        relative_to_report(&path, ed.report.path.as_deref())
    };
    ed.edit_flow(|flow| {
        edit::set_header(flow, key, Some(&text));
    });
}

/// The folder/file picker beside a loop's path box.
///
/// Seeded from the path the loop already names, resolved against the report's
/// own folder, so browsing starts where the loop is looking rather than at the
/// process working directory. What comes back is written relative to the report
/// wherever it can be: a loop that hard-codes an absolute path stops working
/// the moment the report is shared or moved, and picking a folder is exactly
/// when that would otherwise happen without the user noticing.
fn pick_loop_dir(ed: &mut ReportEditor, app: &mut GuiApp, path: &[usize], file: bool) {
    let report_dir = ed
        .report
        .path
        .as_deref()
        .and_then(|p| p.parent())
        .map(std::path::Path::to_path_buf);
    let current = ed
        .flow
        .as_ref()
        .and_then(|f| edit::loop_dir(f, path))
        .unwrap_or_default();
    // Resolve the loop's own (usually relative) path against the report so the
    // dialog opens on the folder it names.
    let seed = if current.is_empty() {
        report_dir.clone()
    } else {
        let joined = match &report_dir {
            Some(dir) => dir.join(&current),
            None => std::path::PathBuf::from(&current),
        };
        super::filepick::seed_dir(joined.to_string_lossy().as_ref()).or(report_dir.clone())
    };

    let title = if file {
        app.strings.gui_pick_loop_file
    } else {
        app.strings.gui_pick_loop_folder
    };
    let picked = if file {
        super::filepick::pick_file(title, seed.as_deref(), &[("*", &["*"])])
    } else {
        super::filepick::pick_folder(title, seed.as_deref())
    };
    let Some(picked) = picked else {
        return;
    };
    let text = relative_to_report(&picked, ed.report.path.as_deref());
    ed.edit_flow(|flow| {
        edit::set_loop_dir(flow, path, &text);
    });
    ed.selection = path.to_vec();
}

/// `path` expressed relative to the report's own folder when it lives under it,
/// else the absolute path.
fn relative_to_report(path: &std::path::Path, report: Option<&std::path::Path>) -> String {
    report
        .and_then(|r| r.parent())
        .and_then(|dir| path.strip_prefix(dir).ok())
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

/// `path` expressed relative to the report, walking *up* out of the report's
/// folder where it has to (`../apis/billing.hurl`), so long as it stays inside
/// `scope`.
///
/// This is what makes a workspace portable. A report in `reports/` binding to a
/// collection in `apis/` shares no prefix with it, so a plain `strip_prefix`
/// gives up and writes an absolute path — which is a path on *this* machine,
/// and breaks the moment the workspace is handed to someone else. Both files
/// are in the same workspace and will be copied together, so the way to say
/// where the collection is, is by where it sits relative to the report.
///
/// `scope` bounds how far up we're willing to walk: outside it the two files
/// aren't travelling together anyway, so a relative path would be a lie and the
/// absolute one is the honest answer.
fn portable_ref(
    path: &std::path::Path,
    report: Option<&std::path::Path>,
    scope: Option<&std::path::Path>,
) -> String {
    let plain = relative_to_report(path, report);
    if !std::path::Path::new(&plain).is_absolute() {
        return plain;
    }
    let (Some(dir), Some(scope)) = (report.and_then(std::path::Path::parent), scope) else {
        return plain;
    };
    if !path.starts_with(scope) || !dir.starts_with(scope) {
        return plain;
    }

    // Drop the shared leading components, then step up once per component of
    // the report's folder that remains.
    let mut from = dir.components().peekable();
    let mut to = path.components().peekable();
    while let (Some(a), Some(b)) = (from.peek(), to.peek()) {
        if a != b {
            break;
        }
        from.next();
        to.next();
    }
    let mut rel = std::path::PathBuf::new();
    for _ in from {
        rel.push("..");
    }
    rel.extend(to);
    if rel.as_os_str().is_empty() {
        return plain;
    }
    // Forward slashes even on Windows: the directive is written into a file
    // that is meant to be read on someone else's machine, and every path API
    // involved accepts them.
    rel.to_string_lossy().replace('\\', "/")
}

/// The `END` closing the whole flow, mirroring the `BEGIN` chip at the top.
///
/// Laid out exactly like a depth-0 block row (the same vertical/horizontal
/// nesting) so it lines up with `BEGIN` rather than sitting at a different
/// indent.
/// The closing caption of the whole flow, matching the synthetic `Begin` row at
/// the top.
///
/// Dim, sentence case and translated, exactly like its opening partner and
/// deliberately *unlike* the uppercase `END` that closes a `FOR` or a `WITH`.
/// Those are real keywords the parser requires and the Source view prints
/// verbatim; this one is punctuation the editor draws so the report reads as one
/// bracketed block, and there is no node behind it to select, move or drop onto.
/// Two things that look identical but behave completely differently is the worst
/// of both, so the pair that isn't in the file is the pair that looks like a
/// caption.
fn flow_end_row(ui: &mut egui::Ui, th: &GuiTheme, s: &crate::i18n::Strings) {
    ui.vertical(|ui| {
        ui.horizontal_top(|ui| {
            static_chip(ui, th, s.report_node_end, th.dim, s.chip_help_flow_end);
        });
    });
}

/// The "nothing here yet" line drawn between `BEGIN` and `END` when the report
/// has no steps. Indented to sit where the first block will, and dimmed so it
/// reads as guidance rather than as a block of its own.
fn empty_flow_hint(ui: &mut egui::Ui, th: &GuiTheme, s: &crate::i18n::Strings) {
    ui.horizontal(|ui| {
        ui.add_space(INDENT_STEP);
        ui.colored_label(th.dim, s.gui_report_empty_flow);
    });
    ui.add_space(2.0);
}

fn tail_drop_zone(
    ui: &mut egui::Ui,
    th: &GuiTheme,
    top_len: usize,
    titles: &[String],
    acts: &mut Vec<Act>,
) {
    let remaining = ui.available_size_before_wrap();
    if remaining.y < 6.0 {
        return;
    }
    let (rect, _) = ui.allocate_exact_size(remaining, egui::Sense::hover());
    let resp = ui.interact(rect, ui.id().with("pt_tail_drop"), egui::Sense::hover());
    let end = InsertPos {
        parent: Vec::new(),
        index: top_len,
    };
    let hovering = resp.dnd_hover_payload::<NodeKind>().is_some()
        || resp
            .dnd_hover_payload::<DragItem>()
            .is_some_and(|d| matches!(&*d, DragItem::Row(_)));
    if hovering {
        // Match the insert-strip placeholder's fully-open height (the block
        // actually in hand, less the same 4px inset) so the tail ghost is the
        // size of the block being dropped — including a whole `FOR` loop — not
        // a fixed sliver.
        let size = dragged_block_size(ui);
        let clip = egui::Rect::from_min_size(
            rect.left_top(),
            egui::vec2(
                (rect.width() - 8.0).max(1.0),
                (size.y - 4.0).min(rect.height()).max(6.0),
            ),
        );
        paint_drop_silhouette(ui, rect.left_top(), &dragged_block_shape(ui), clip, th);
    }
    if let Some(kind) = release_payload::<NodeKind>(&resp) {
        acts.push(Act::DropNode {
            pos: end,
            node: node_for_kind(*kind, titles),
        });
    } else if let Some(item) = release_payload::<DragItem>(&resp) {
        // Moving the row that is *already* last to the end is a no-op — skip it
        // so it doesn't push a redundant undo entry / mark the report dirty.
        if let DragItem::Row(from) = &*item
            && from.as_slice() != [top_len.saturating_sub(1)]
        {
            acts.push(Act::MoveNode {
                from: from.clone(),
                pos: end,
            });
        }
    }
}

/// The uniform content height every chip reserves — the natural height of the
/// tallest inline control a chip can host, a combo box (`interact_size.y` grown
/// by the app's larger `button_padding`, rounded up to a whole pixel). Shorter
/// controls are lifted to this height so a plain-label chip is exactly as tall
/// as one hosting a combo box or a text field.
fn chip_h(ui: &egui::Ui) -> f32 {
    let sp = ui.spacing();
    let row = ui.text_style_height(&egui::TextStyle::Body);
    (sp.interact_size.y.max(row + 2.0 * sp.button_padding.y)).ceil()
}

/// Lay out a chip's content inside a rounded, tinted frame of uniform height.
///
/// A combo box grows to fill whatever vertical space its row offers, so it
/// already renders at the tallest chip height on its own — pass `grow = false`
/// for a chip that hosts one. Every *other* control (a plain label, a text
/// field) is shorter, so `grow = true` reserves a [`chip_h`]-tall row and
/// centres the content in it, lifting those chips to exactly the same height as
/// a combo-box chip. Returns the content closure's value.
/// Lay out a chip's label as two distinct kinds of word rather than one
/// undifferentiated run: the editor's own keyword in the UI face, and any
/// identifier the user supplied in the monospace face.
///
/// This is most of what makes a flow skimmable. `BASELINE(staging)` is two
/// different things joined by punctuation — one is vocabulary the editor
/// defines, the other is the user's own data — and setting them identically
/// makes the reader parse the brackets to tell which is which. A face change
/// does that pre-attentively, and it is the convention every code editor
/// already teaches, so it needs no explaining.
///
/// egui has no bold variant of the default proportional font (only Phosphor is
/// registered alongside it), so weight is not available as a channel here; face
/// and the theme's brighter `strong` colour carry the emphasis instead.
fn chip_label_job(ui: &egui::Ui, text: &str, color: Color32) -> egui::text::LayoutJob {
    let base = egui::TextStyle::Button.resolve(ui.style());
    let mono = egui::FontId::new(base.size, egui::FontFamily::Monospace);

    // The keyword runs up to the first bracket or space; everything after it is
    // the user's. A label with neither is all keyword (`GET`, `PARALLEL`).
    let split = text.find(['(', ' ']).unwrap_or(text.len());
    let (keyword, rest) = text.split_at(split);

    let mut job = egui::text::LayoutJob::default();
    let mut push = |s: &str, font: egui::FontId| {
        if s.is_empty() {
            return;
        }
        job.append(
            s,
            0.0,
            egui::TextFormat {
                font_id: font,
                color,
                ..Default::default()
            },
        );
    };
    push(keyword, base.clone());
    push(rest, mono);
    job
}

/// A chip label, laid out by [`chip_label_job`] and sensing clicks and drags.
fn chip_label(ui: &mut egui::Ui, text: &str, color: Color32) -> egui::Response {
    let job = chip_label_job(ui, text, color);
    ui.add(
        egui::Label::new(job)
            .selectable(false)
            .sense(egui::Sense::click_and_drag()),
    )
}

fn chip_shell<R>(
    ui: &mut egui::Ui,
    tint: &ChipTint,
    grow: bool,
    corners: egui::CornerRadius,
    content: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let h = chip_h(ui);
    let framed = egui::Frame::NONE
        .fill(tint.fill)
        .stroke(tint.stroke)
        .inner_margin(egui::Margin::symmetric(8, 3))
        .corner_radius(corners)
        .show(ui, |ui| {
            // egui sizes a combo box (and a button) to
            // `max(interact_size.y, content + 2 * button_padding.y)`, which is
            // the same formula `chip_h` uses — so the two *coincided* until the
            // app scaled its text and they landed a fraction of a pixel apart,
            // enough to round to a visibly uneven bottom edge next to a label
            // chip. Pinning `interact_size.y` to the chip height makes the combo
            // *derive* its height from `chip_h` instead of agreeing with it by
            // arithmetic: the max can only resolve to `h`, because `h` is by
            // definition at least the content plus its padding.
            ui.spacing_mut().interact_size.y = h;
            ui.horizontal(|ui| {
                if grow {
                    ui.set_min_height(h);
                }
                content(ui)
            })
            .inner
        });
    // The category bar, painted after the frame so it sits over the border it
    // replaces along that edge. It mirrors the chip's own left corner radii and
    // squares off on the right, so on a tethered pill it stops cleanly at the
    // seam instead of bulging past it.
    if let Some(rule) = tint.rule {
        let rect = framed.response.rect;
        ui.painter().rect_filled(
            egui::Rect::from_min_max(
                rect.min,
                egui::pos2(rect.left() + CHIP_RULE_W, rect.bottom()),
            ),
            egui::CornerRadius {
                nw: corners.nw,
                sw: corners.sw,
                ne: 0,
                se: 0,
            },
            rule,
        );
    }
    framed.inner
}

/// How strongly a chip's category colour is allowed to show, as blend factors
/// against the panel colour: the background wash, the border, and how far the
/// label text is pulled from the ordinary text colour toward the category hue.
///
/// These are deliberately one set of constants rather than a literal repeated at
/// each chip site. A block flow puts five categories on screen at once, and when
/// the fill, the border *and* the text were all near-full chroma the result read
/// as coloured building blocks — several professional users called the editor
/// childish on the strength of it. Quietening the surface while keeping the hue
/// identifiable is a single judgement about the whole editor, so it is made in
/// one place and every chip reads from it.
///
/// The *hue* per category is untouched (see [`kind_color`]): a `FOR` is the same
/// colour it always was, just no longer shouted.
///
/// The fill and border are now barely tinted at all, because the category is
/// carried by the rule down the chip's leading edge instead (see [`ChipTint`]).
/// Spending the whole chip surface on the hue and spending a 3px bar on it say
/// the same thing, but only one of them fills the window with colour.
const CHIP_FILL_MIX: f32 = 0.05;
const CHIP_STROKE_MIX: f32 = 0.16;
const CHIP_TEXT_MIX: f32 = 0.45;

/// The width of the category rule down a chip's leading edge.
///
/// It fits inside [`chip_shell`]'s existing 8px left inner margin, so turning
/// the rule on cost no reflow: the label sits exactly where it always did, with
/// 5px of clear space beside the bar.
const CHIP_RULE_W: f32 = 3.0;

/// How a chip is coloured: a near-neutral surface plus a full-strength category
/// bar down its leading edge.
///
/// The bar is the reason the surface can be neutral. Colour used as a *fill*
/// scales with the size of the thing filled, so a flow of blocks became a wall
/// of colour; the same hue used as a rule is just as identifiable — the eye
/// scans a column of coloured edges very well — while leaving the chip itself a
/// quiet card. This is the pattern editors and issue trackers already use for
/// exactly this reason.
struct ChipTint {
    fill: Color32,
    stroke: egui::Stroke,
    text: Color32,
    /// The category bar, or `None` for a chip that shouldn't show one: a
    /// selected chip (whose fill already carries the selection colour) and the
    /// hanger of a tethered pair (whose leading edge is *inside* the pill, where
    /// a bar would read as a divider rather than as a category).
    rule: Option<Color32>,
}

/// The colours for a chip of category colour `color`.
///
/// Text is blended *from the theme's text colour toward* the category hue rather
/// than being the hue itself: full-chroma text on a tint of the same hue is both
/// loud and low-contrast, and the label has to stay comfortably readable — it is
/// the part that actually says what the block does.
fn chip_tint(th: &GuiTheme, color: Color32) -> ChipTint {
    ChipTint {
        fill: mix(th.panel, color, CHIP_FILL_MIX),
        stroke: egui::Stroke::new(1.0, mix(th.panel, color, CHIP_STROKE_MIX)),
        text: mix(th.text, color, CHIP_TEXT_MIX),
        rule: Some(color),
    }
}

/// The colours for a chip, honouring the selected-base highlight.
fn chip_colors(th: &GuiTheme, chip: &Chip, selected: bool) -> ChipTint {
    let mut tint = if chip.is_base && selected {
        // Keep the stroke *width* identical to the unselected state (only the
        // colour and fill change): a thicker stroke would expand the frame by a
        // pixel and shift the chip and its neighbours, so selecting a block
        // would visibly nudge it. Selection must recolour in place, never
        // resize or move.
        ChipTint {
            fill: th.select_bg,
            stroke: egui::Stroke::new(1.0, th.select_fg),
            text: th.select_fg,
            rule: None,
        }
    } else {
        chip_tint(th, chip.color)
    };
    if chip.join_prev {
        tint.rule = None;
    }
    tint
}

/// A small frameless `×` button. Returns whether it was clicked. Kept separate
/// from the chip's drag handle so its click is never stolen by a frame-wide
/// drag interaction (the bug where the detach `×` did nothing).
fn detach_x(ui: &mut egui::Ui, col: Color32) -> bool {
    ui.add(
        egui::Button::new(RichText::new("×").color(col))
            .small()
            .frame(false),
    )
    .clicked()
}

/// The width to give an inline field so its current value actually fits.
///
/// The fields used to be fixed-width, which meant anything longer than the
/// guess — most real folder paths, and any alias longer than a word — was
/// silently cut off, with no indication that the box held more than it showed.
/// Measuring instead means a field is as wide as what is in it, clamped so a
/// long path can't push the rest of the row off screen (past `max` the box
/// scrolls, as before, but now only in the genuinely long cases).
fn fitted_field_width(ui: &egui::Ui, text: &str, hint: &str, min: f32, max: f32) -> f32 {
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    // Measure the hint too, so an *empty* box is still wide enough to show its
    // placeholder rather than clipping it.
    let measure = |t: &str| {
        ui.painter()
            .layout_no_wrap(t.to_string(), font.clone(), egui::Color32::PLACEHOLDER)
            .size()
            .x
    };
    // Room for the text edit's own margin plus the caret sitting after the last
    // character — without it the caret lands on the border when the box is full.
    let padding = 12.0;
    (measure(text).max(measure(hint)) + padding).clamp(min, max)
}

/// An inline single-line text field that commits on blur. The in-progress buffer
/// lives in egui temp memory keyed by `id` (so it survives across frames while
/// focused) and is dropped once committed / idle, keeping the field synced to
/// the AST. Returns `Some(trimmed value)` on the frame focus is lost.
///
/// `hint` is the placeholder shown inside the empty box and must stay short
/// enough to fit it; `help` is the full explanation, shown on hover. These were
/// once the same string, so a field's entire explanation was crammed into a
/// box a few characters wide and read as "Only files ...".
fn inline_text_edit(
    ui: &mut egui::Ui,
    id: egui::Id,
    current: &str,
    hint: &str,
    help: &str,
    min_width: f32,
    max_width: f32,
) -> Option<String> {
    let mut buf = ui
        .data(|d| d.get_temp::<String>(id))
        .unwrap_or_else(|| current.to_string());
    let width = fitted_field_width(ui, &buf, hint, min_width, max_width);
    let resp = ui.add(
        egui::TextEdit::singleline(&mut buf)
            .hint_text(hint)
            // Match the fill a combo-box chip (BASELINE/COMPARISON/REQUEST) uses
            // for its button, so an inline field (the AS alias) doesn't read as
            // a darker, sunken box beside them.
            .background_color(ui.visuals().widgets.inactive.weak_bg_fill)
            // Monospace for the same reason the identifier half of a chip label
            // is (see `chip_label_job`): what goes in here is the user's own
            // name for something, not the editor's vocabulary, and the two
            // should not look alike. It also stops an alias and the keyword
            // beside it from reading as one phrase.
            .font(egui::TextStyle::Monospace)
            .desired_width(width),
    );
    // Only when not being typed into: a tooltip popping up under the cursor
    // while the user is editing is in the way of the thing they're editing.
    if !help.is_empty() && !resp.has_focus() {
        resp.clone().on_hover_text(help);
    }
    if resp.lost_focus() {
        ui.data_mut(|d| d.remove::<String>(id));
        Some(buf.trim().to_string())
    } else if resp.has_focus() {
        ui.data_mut(|d| d.insert_temp(id, buf));
        None
    } else {
        ui.data_mut(|d| d.remove::<String>(id));
        None
    }
}

/// A plain, non-interactive tinted chip (the synthetic `Begin` / `END` rows).
fn static_chip(ui: &mut egui::Ui, th: &GuiTheme, text: &str, color: Color32, help: &str) {
    let tint = chip_tint(th, color);
    let text_col = tint.text;
    let scope = ui.scope(|ui| {
        chip_shell(ui, &tint, true, ROUND_CHIP, |ui| {
            let job = chip_label_job(ui, text, text_col);
            ui.add(egui::Label::new(job).selectable(false));
        });
    });
    if !help.is_empty() && ui.ctx().dragged_id().is_none() {
        scope.response.on_hover_text(help);
    }
}

/// Render one [`Chip`]. The base chip is click-to-select, double-click-to-open
/// the wizard, and a drag source that relocates or bins its whole row; a
/// modifier chip shows a `×` that detaches it and is itself a drag source that
/// bins the modifier.
/// Render a chip that has been picked up on its own: into a floating layer that
/// follows the pointer, with a dashed ghost left behind in its slot.
///
/// The chip still allocates its space in the row (so the line doesn't reflow
/// under the pointer) and is still rendered by [`render_chip`], so it keeps its
/// widget id and the drag stays alive.
///
/// Returns the floating layer and the *slot* rect. The caller applies the
/// transform that puts the layer under the pointer, and only once it has
/// finished adding to that layer: a `WITH` chip carries its whole field block
/// with it, and transforming from here would move the chip while leaving the
/// fields behind — the same "parts move at different speeds" bug [`DragLift`]
/// exists to avoid.
#[allow(clippy::too_many_arguments)]
fn lift_chip(
    ui: &mut egui::Ui,
    th: &GuiTheme,
    s: &crate::i18n::Strings,
    chip: &Chip,
    selected: bool,
    path: &[usize],
    row_index: usize,
    titles: &[String],
    env_choices: &[String],
    acts: &mut Vec<Act>,
) -> (egui::LayerId, egui::Rect) {
    // One lifted chip per row at most, so the row index is a sufficient key.
    let layer_id = egui::LayerId::new(
        egui::Order::Tooltip,
        ui.id().with(("pt_drag_chip", row_index)),
    );
    let slot = ui
        .scope_builder(
            egui::UiBuilder::new()
                .layer_id(layer_id)
                .layout(egui::Layout::left_to_right(egui::Align::Min)),
            |ui| render_chip(ui, th, s, chip, selected, path, titles, env_choices, acts),
        )
        .inner;
    // Painted after the scope, into the *base* layer: the chip itself has moved
    // to the floating layer, so the slot underneath is empty.
    paint_origin_ghost(ui.painter(), slot, th);
    (layer_id, slot)
}

/// Put a chip's floating layer under the pointer, anchored so the chip itself
/// sits on the cursor and anything lifted with it hangs off at its natural
/// offset. Call once, after everything that belongs to the layer is painted.
fn follow_pointer(ctx: &egui::Context, layer_id: egui::LayerId, anchor: egui::Rect) {
    if let Some(pointer) = ctx.pointer_interact_pos() {
        ctx.transform_layer_shapes(
            layer_id,
            egui::emath::TSTransform::from_translation(pointer - anchor.center()),
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn render_chip(
    ui: &mut egui::Ui,
    th: &GuiTheme,
    s: &crate::i18n::Strings,
    chip: &Chip,
    selected: bool,
    path: &[usize],
    titles: &[String],
    env_choices: &[String],
    acts: &mut Vec<Act>,
) -> egui::Rect {
    // The hover help is attached to a wrapper covering the whole chip rather
    // than to any one widget inside it, so it shows over the label, the `×` and
    // the inline combo/text field alike. It is suppressed mid-drag, where a
    // tooltip trailing the pointer would just obscure the drop targets.
    let scope = ui.scope(|ui| {
        render_chip_body(ui, th, s, chip, selected, path, titles, env_choices, acts);
    });
    let rect = scope.response.rect;
    // A detachable chip also spells out the drag gesture. It is not otherwise
    // discoverable that a plain drag pulls one chip out of a line while Ctrl
    // takes the whole line, and the two are easy to trigger by accident.
    if ui.ctx().dragged_id().is_none() {
        let help = match (chip.help.is_empty(), chip.detach.is_some()) {
            (true, false) => String::new(),
            (false, false) => chip.help.to_string(),
            (true, true) => s.chip_help_drag_gesture.to_string(),
            (false, true) => format!("{}\n\n{}", chip.help, s.chip_help_drag_gesture),
        };
        if !help.is_empty() {
            scope.response.on_hover_text(help);
        }
    }
    rect
}

#[allow(clippy::too_many_arguments)]
fn render_chip_body(
    ui: &mut egui::Ui,
    th: &GuiTheme,
    s: &crate::i18n::Strings,
    chip: &Chip,
    selected: bool,
    path: &[usize],
    titles: &[String],
    env_choices: &[String],
    acts: &mut Vec<Act>,
) {
    // Chips that host an inline control draw a keyword prefix (the drag/select
    // handle) plus a combo box / text field; they only fall back to a plain
    // label when there is nothing to pick from.
    match &chip.edit {
        ChipEdit::Request { name } if !titles.is_empty() => {
            combo_chip(
                ui,
                th,
                s,
                chip,
                selected,
                path,
                "REQUEST",
                name,
                titles,
                true,
                acts,
                |picked| Act::RenameRequest {
                    path: path.to_vec(),
                    name: picked,
                },
            );
            return;
        }
        ChipEdit::EnvRole {
            baseline,
            index,
            name,
        } if !env_choices.is_empty() => {
            let kw = if *baseline { "BASELINE" } else { "COMPARISON" };
            let (baseline, index) = (*baseline, *index);
            combo_chip(
                ui,
                th,
                s,
                chip,
                selected,
                path,
                kw,
                name,
                env_choices,
                false,
                acts,
                move |picked| Act::SetEnvRole {
                    path: path.to_vec(),
                    baseline,
                    index,
                    name: picked,
                },
            );
            return;
        }
        ChipEdit::Alias { text } => {
            alias_chip(ui, th, s, chip, path, text, acts);
            return;
        }
        ChipEdit::Parallel { degree } => {
            parallel_chip(ui, th, s, chip, path, *degree, acts);
            return;
        }
        ChipEdit::Loop(l) => {
            let l = l.clone();
            loop_chip(ui, th, s, chip, path, &l, acts);
            return;
        }
        _ => {}
    }

    let tint = chip_colors(th, chip, selected);
    let text_col = tint.text;
    let handle = chip_shell(ui, &tint, true, chip_corners(chip), |ui| {
        // The label is the drag/select handle, kept separate from the `×`
        // button so the button's click is never stolen by the drag sense.
        let handle = chip_label(ui, &chip.text, text_col);
        if let Some(which) = chip.detach
            && detach_x(ui, text_col)
        {
            acts.push(Act::DetachMod {
                path: path.to_vec(),
                which,
            });
        }
        handle
    });

    if handle.dragged() {
        handle.dnd_set_drag_payload(chip_drag_payload(ui, chip, path));
    }
    if chip.is_base {
        if handle.double_clicked() {
            acts.push(Act::OpenWizard(path.to_vec()));
        } else if handle.clicked() {
            acts.push(Act::Select(path.to_vec()));
        }
    } else if chip_opens_wizard_on_click(chip) && handle.clicked() {
        // SHOW / HIDE / RESPONSE are edited through the request wizard's field
        // pickers, so a plain click on one of these chips opens that wizard
        // (dragging it still detaches it — see `chip_drag_payload`).
        acts.push(Act::OpenWizard(path.to_vec()));
    }
}

/// Choose the drag payload for a chip. Holding Ctrl/Cmd forces moving the whole
/// line/subtree from *any* chip (`DragItem::Row`); otherwise a detachable
/// modifier chip is picked up on its own to detach it (`DragItem::Chip`), and
/// every other chip (the editable base, the request handle, fixed keywords)
/// moves the line. This is the "plain-drag detaches a chip, Ctrl-drag moves the
/// line" behaviour, applied uniformly across every draggable chip.
fn chip_drag_payload(ui: &egui::Ui, chip: &Chip, path: &[usize]) -> DragItem {
    let force_row = ui.input(|i| i.modifiers.command);
    match (force_row, chip.is_base, chip.detach) {
        (false, false, Some(which)) => DragItem::Chip {
            path: path.to_vec(),
            which,
        },
        _ => DragItem::Row(path.to_vec()),
    }
}

/// Whether a plain click on this (non-base) chip should open a wizard to edit
/// it. True for the `SHOW` / `HIDE` / `RESPONSE` clauses, whose fields are
/// picked in the request wizard rather than typed inline, and for an ENVS
/// loop's `BASELINE … SHOW(…)`, whose checklist lives in the ENVS wizard — the
/// wizard opened is the one belonging to the chip's own node, so both land in
/// the right place.
fn chip_opens_wizard_on_click(chip: &Chip) -> bool {
    matches!(
        chip.detach,
        Some(
            DetachWhich::Show
                | DetachWhich::Hide
                | DetachWhich::Response
                | DetachWhich::BaselineShow
        )
    )
}

/// Render an `AS <alias>` chip: an `AS` prefix (the drag/detach handle) followed
/// by an inline text field that commits the alias/name on blur.
fn alias_chip(
    ui: &mut egui::Ui,
    th: &GuiTheme,
    s: &crate::i18n::Strings,
    chip: &Chip,
    path: &[usize],
    current: &str,
    acts: &mut Vec<Act>,
) {
    let tint = chip_colors(th, chip, false);
    let text_col = tint.text;
    let handle = chip_shell(ui, &tint, true, chip_corners(chip), |ui| {
        let handle = ui.add(
            egui::Label::new(RichText::new("AS").color(text_col))
                .selectable(false)
                .sense(egui::Sense::click_and_drag()),
        );
        let id = ui.make_persistent_id(("pt_alias", path));
        if let Some(text) = inline_text_edit(
            ui,
            id,
            current,
            s.gui_report_alias_hint,
            "",
            ALIAS_FIELD_WIDTH,
            FIELD_MAX_WIDTH,
        ) && text != current
        {
            acts.push(Act::SetAlias {
                path: path.to_vec(),
                text,
            });
        }
        if let Some(which) = chip.detach
            && detach_x(ui, text_col)
        {
            acts.push(Act::DetachMod {
                path: path.to_vec(),
                which,
            });
        }
        handle
    });

    if handle.dragged() {
        handle.dnd_set_drag_payload(chip_drag_payload(ui, chip, path));
    }
}

/// The `PARALLEL` chip: a `PARALLEL` drag handle plus a small box for the
/// optional max-concurrency. Leaving the box empty is meaningful — it is the
/// plain `PARALLEL` form, where the limit comes from the prelude's
/// `MAX_PARALLEL` — so a blank commits `None` rather than being ignored. Text
/// that isn't a positive number is discarded on blur, which keeps the flow from
/// ever holding the `PARALLEL(0)` the parser would reject on reload.
/// Render a `FOR` loop head with its editable parts inline: `FOR` (the drag
/// handle) followed by a box for the loop variable, the source keyword, a box
/// and picker for the folder/file, and a `FILES` loop's `MATCH` glob.
///
/// The parts that can't be edited in one box — a destructuring pattern, a list
/// literal, a `FOLDERS … WITH` role list — are drawn as plain labels, exactly as
/// the whole head used to be, and are still reached through the wizard.
fn loop_chip(
    ui: &mut egui::Ui,
    th: &GuiTheme,
    s: &crate::i18n::Strings,
    chip: &Chip,
    path: &[usize],
    l: &LoopEdit,
    acts: &mut Vec<Act>,
) {
    let tint = chip_colors(th, chip, false);
    let text_col = tint.text;
    let handle = chip_shell(ui, &tint, true, chip_corners(chip), |ui| {
        // `FOR` is the drag/select handle, kept apart from the boxes beside it
        // so a click meant for a field never starts a drag of the row.
        let handle = ui.add(
            egui::Label::new(RichText::new("FOR").color(text_col))
                .selectable(false)
                .sense(egui::Sense::click_and_drag()),
        );
        if let Some(var) = &l.var {
            let id = ui.make_persistent_id(("pt_loop_var", path));
            let resp = inline_text_edit(
                ui,
                id,
                var,
                s.gui_report_loop_var_hint,
                s.chip_help_loop_var,
                LOOP_VAR_FIELD_WIDTH,
                FIELD_MAX_WIDTH,
            );
            if let Some(text) = resp
                && &text != var
            {
                acts.push(Act::SetLoopVar {
                    path: path.to_vec(),
                    text,
                });
            }
        }
        ui.add(
            egui::Label::new(RichText::new(&l.keyword).color(text_col))
                .selectable(false)
                .sense(egui::Sense::hover()),
        );
        if let Some((dir, is_file)) = &l.dir {
            let id = ui.make_persistent_id(("pt_loop_dir", path));
            let resp = inline_text_edit(
                ui,
                id,
                dir,
                s.gui_report_loop_dir_hint,
                s.chip_help_loop_dir,
                LOOP_PATH_FIELD_WIDTH,
                FIELD_MAX_WIDTH,
            );
            if let Some(text) = resp
                && &text != dir
            {
                acts.push(Act::SetLoopDir {
                    path: path.to_vec(),
                    text,
                });
            }
            // Framed, not flat. Drawn without a frame this read as an icon
            // printed on the chip rather than as something to press, and users
            // reported not realising a folder picker was there at all. The
            // frame gives it egui's normal hover fill, and the pointing-hand
            // cursor confirms it before the click.
            let pick = ui
                .add(
                    egui::Button::new(RichText::new(super::icons::FOLDER).color(text_col))
                        .min_size(egui::vec2(PICKER_BUTTON_WIDTH, 0.0)),
                )
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text(if *is_file {
                    s.chip_help_loop_pick_file
                } else {
                    s.chip_help_loop_pick_folder
                });
            if pick.clicked() {
                acts.push(Act::PickLoopDir {
                    path: path.to_vec(),
                    file: *is_file,
                });
            }
        }
        if let Some(glob) = &l.glob {
            // The keyword carries the same explanation as the box beside it:
            // "MATCH" alone says nothing to someone meeting it for the first
            // time, and it is the word their eye lands on first.
            ui.add(
                egui::Label::new(RichText::new("MATCH").color(text_col))
                    .selectable(false)
                    .sense(egui::Sense::hover()),
            )
            .on_hover_text(s.chip_help_loop_glob);
            let id = ui.make_persistent_id(("pt_loop_glob", path));
            let resp = inline_text_edit(
                ui,
                id,
                glob,
                s.gui_report_loop_glob_hint,
                s.chip_help_loop_glob,
                LOOP_GLOB_FIELD_WIDTH,
                FIELD_MAX_WIDTH,
            );
            if let Some(text) = resp
                && &text != glob
            {
                acts.push(Act::SetLoopGlob {
                    path: path.to_vec(),
                    text,
                });
            }
        }
        if !l.tail.is_empty() {
            ui.add(
                egui::Label::new(RichText::new(&l.tail).color(text_col))
                    .selectable(false)
                    .sense(egui::Sense::hover()),
            );
        }
        handle
    });

    if handle.dragged() {
        handle.dnd_set_drag_payload(chip_drag_payload(ui, chip, path));
    }
}

fn parallel_chip(
    ui: &mut egui::Ui,
    th: &GuiTheme,
    s: &crate::i18n::Strings,
    chip: &Chip,
    path: &[usize],
    current: Option<u32>,
    acts: &mut Vec<Act>,
) {
    let tint = chip_colors(th, chip, false);
    let text_col = tint.text;
    let shown = current.map(|n| n.to_string()).unwrap_or_default();
    let handle = chip_shell(ui, &tint, true, chip_corners(chip), |ui| {
        let handle = ui.add(
            egui::Label::new(RichText::new("PARALLEL").color(text_col))
                .selectable(false)
                .sense(egui::Sense::click_and_drag()),
        );
        let id = ui.make_persistent_id(("pt_parallel", path));
        if let Some(text) = inline_text_edit(
            ui,
            id,
            &shown,
            s.node_form_parallel_degree,
            "",
            PARALLEL_FIELD_WIDTH,
            FIELD_MAX_WIDTH,
        ) && text != shown
        {
            let degree = match text.trim() {
                "" => Some(None),
                t => t.parse::<u32>().ok().filter(|n| *n > 0).map(Some),
            };
            if let Some(degree) = degree {
                acts.push(Act::SetParallelDegree {
                    path: path.to_vec(),
                    degree,
                });
            }
        }
        if let Some(which) = chip.detach
            && detach_x(ui, text_col)
        {
            acts.push(Act::DetachMod {
                path: path.to_vec(),
                which,
            });
        }
        handle
    });

    if handle.dragged() {
        handle.dnd_set_drag_payload(chip_drag_payload(ui, chip, path));
    }
}

/// Render a chip whose enumerable part is an inline dropdown. The keyword
/// `prefix` (e.g. `REQUEST`) is the drag/select handle so the combo beside it
/// stays free to open without starting a drag; picking a new value emits the
/// action `make_act(value)`. When `filter` is set the dropdown gains a search
/// box that narrows the choices as you type (the TUI-style request picker).
#[allow(clippy::too_many_arguments)]
fn combo_chip(
    ui: &mut egui::Ui,
    th: &GuiTheme,
    s: &crate::i18n::Strings,
    chip: &Chip,
    selected: bool,
    path: &[usize],
    prefix: &str,
    current: &str,
    choices: &[String],
    filter: bool,
    acts: &mut Vec<Act>,
    make_act: impl FnOnce(String) -> Act,
) {
    let tint = chip_colors(th, chip, selected);
    let text_col = tint.text;
    let mut picked: Option<String> = None;
    let mut detached: Option<DetachWhich> = None;
    // A combo box already renders at the tallest chip height, so this chip does
    // not grow its row (which would only inflate the combo box further).
    let handle = chip_shell(ui, &tint, false, chip_corners(chip), |ui| {
        // The keyword prefix is the interactive handle (drag to reorder, click
        // to select, double-click to open the wizard). It must be added *before*
        // the combo (so it sits on the left), but the combo is taller and only
        // sets the row height once it is added — a plain label placed first
        // would therefore be "centred" in a still-short row and end up sitting
        // above the combo's text. So we reserve the prefix's slot now (as the
        // drag handle) but defer painting the text until the combo has set the
        // row height, then paint it vertically centred against the combo.
        let font = egui::TextStyle::Button.resolve(ui.style());
        let galley = ui
            .painter()
            .layout_no_wrap(prefix.to_string(), font, text_col);
        let gsize = galley.size();
        let (label_rect, handle) = ui.allocate_exact_size(gsize, egui::Sense::click_and_drag());
        egui::ComboBox::from_id_salt((path, prefix))
            .selected_text(RichText::new(current).color(text_col))
            .show_ui(ui, |ui| {
                if filter {
                    filtered_choices(ui, s, path, prefix, current, choices, &mut picked);
                } else {
                    for c in choices {
                        if ui.selectable_label(c == current, c).clicked() {
                            picked = Some(c.clone());
                        }
                    }
                }
            });
        // A detachable combo chip (a BASELINE/COMPARISON role) gets the same
        // `×` as every other detachable chip, so the dropdown isn't the only
        // thing you can do to it.
        if let Some(which) = chip.detach
            && detach_x(ui, text_col)
        {
            detached = Some(which);
        }
        let cy = ui.min_rect().center().y;
        ui.painter().galley(
            egui::pos2(label_rect.left(), cy - gsize.y / 2.0),
            galley,
            text_col,
        );
        handle
    });
    if let Some(which) = detached {
        acts.push(Act::DetachMod {
            path: path.to_vec(),
            which,
        });
    }

    // Same rule as every other chip: plain-drag pulls a detachable chip out of
    // the line, Ctrl/Cmd-drag moves the whole line (see `chip_drag_payload`).
    if handle.dragged() {
        handle.dnd_set_drag_payload(chip_drag_payload(ui, chip, path));
    }
    if handle.double_clicked() {
        acts.push(Act::OpenWizard(path.to_vec()));
    } else if handle.clicked() {
        acts.push(Act::Select(path.to_vec()));
    }
    if let Some(name) = picked
        && name != current
    {
        acts.push(make_act(name));
    }
}

/// The filtered body of a request dropdown: an auto-focused search field at the
/// top, then the choices narrowed (case-insensitively) by what's typed —
/// mirroring the terminal UI's type-to-filter request picker. Sets `*picked`
/// when a match is clicked.
fn filtered_choices(
    ui: &mut egui::Ui,
    s: &crate::i18n::Strings,
    path: &[usize],
    prefix: &str,
    current: &str,
    choices: &[String],
    picked: &mut Option<String>,
) {
    let filt_id = ui.make_persistent_id(("pt_chip_filter", path, prefix));
    let mut q = ui
        .data(|d| d.get_temp::<String>(filt_id))
        .unwrap_or_default();
    let te = ui.add(
        egui::TextEdit::singleline(&mut q)
            .hint_text(s.gui_report_filter_hint)
            .desired_width(200.0),
    );
    // Focus the filter the frame the dropdown opens (nothing else is focused
    // yet) so the user can just start typing, like the TUI picker.
    if q.is_empty() && ui.memory(|m| m.focused().is_none()) {
        te.request_focus();
    }
    ui.data_mut(|d| d.insert_temp(filt_id, q.clone()));
    ui.separator();
    let needle = q.to_lowercase();
    egui::ScrollArea::vertical()
        .max_height(220.0)
        .show(ui, |ui| {
            for c in choices
                .iter()
                .filter(|c| needle.is_empty() || c.to_lowercase().contains(&needle))
            {
                if ui.selectable_label(c == current, c).clicked() {
                    *picked = Some(c.clone());
                    ui.data_mut(|d| d.remove::<String>(filt_id));
                }
            }
        });
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
        .corner_radius(BLOCK_RADIUS as u8)
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
                    for kind in BASE_KINDS {
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
/// A thin draggable handle above the validation panel. Dragging it up/down
/// grows or shrinks the panel (`ed.diag_h`), the GUI's replacement for a fixed
/// panel height: a report with many validation errors can be given as much room
/// as the user wants. Drag *up* (negative Δy) enlarges the panel.
fn diag_splitter(ed: &mut ReportEditor, ui: &mut egui::Ui) {
    ui.add_space(2.0);
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 6.0), egui::Sense::drag());
    if resp.dragged() {
        ed.diag_h = (ed.diag_h - resp.drag_delta().y).clamp(48.0, 600.0);
    }
    if resp.hovered() || resp.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
    }
    // A short grab bar centred in the strip, brightened while hovered/dragged so
    // the resize affordance reads as interactive.
    let active = resp.hovered() || resp.dragged();
    let visuals = ui.visuals();
    let colour = if active {
        visuals.widgets.active.fg_stroke.color
    } else {
        visuals.widgets.noninteractive.bg_stroke.color
    };
    let w = (rect.width() * 0.25).clamp(40.0, 160.0);
    let x0 = rect.center().x - w / 2.0;
    ui.painter()
        .hline(x0..=x0 + w, rect.center().y, egui::Stroke::new(2.0, colour));
}

/// A thin draggable handle between the palette and the block stack. Dragging it
/// left/right grows or shrinks the palette column (`ed.palette_w`), the GUI's
/// click-and-drag replacement for the TUI's fixed panel widths.
fn palette_splitter(ed: &mut ReportEditor, ui: &mut egui::Ui, body_h: f32) {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(8.0, body_h), egui::Sense::drag());
    if resp.dragged() {
        ed.palette_w = (ed.palette_w + resp.drag_delta().x).clamp(96.0, 480.0);
    }
    if resp.hovered() || resp.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
    // A short grab bar centred in the strip, brightened while hovered/dragged so
    // the resize affordance reads as interactive.
    let active = resp.hovered() || resp.dragged();
    let visuals = ui.visuals();
    let colour = if active {
        visuals.widgets.active.fg_stroke.color
    } else {
        visuals.widgets.noninteractive.bg_stroke.color
    };
    let h = (rect.height() * 0.25).clamp(24.0, 120.0);
    let y0 = rect.center().y - h / 2.0;
    ui.painter()
        .vline(rect.center().x, y0..=y0 + h, egui::Stroke::new(2.0, colour));
}

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
///
/// Value commits are applied before anything else. An inline chip field commits
/// what has been typed into it when it loses focus — which is the same click
/// that presses a button on the toolbar drawn *above* it, so both land in the
/// same batch, with the toolbar's action first (it was drawn first). Both carry
/// paths describing the tree as it was on screen, but a structural action
/// rewrites those paths out from under the commit: applied in collection order,
/// deleting one block would land the edit on whichever block shuffled up into
/// its slot — renaming a block the user never touched, and losing the edit they
/// did make. Sorting the commits to the front keeps every path meaning what it
/// meant when it was drawn. The partition is stable, so commits keep their own
/// relative order, and so does everything else.
fn apply_block_actions(ed: &mut ReportEditor, app: &mut GuiApp, acts: Vec<Act>) {
    let (commits, rest): (Vec<Act>, Vec<Act>) = acts.into_iter().partition(Act::is_value_commit);
    for act in commits.into_iter().chain(rest) {
        match act {
            Act::Select(path) => {
                ed.selection = path;
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
                    if let Some(sel) = insert_at_palette(ed, node) {
                        super::report_wizard::open(ed, app, &sel);
                    }
                }
            }
            Act::InsertRequest { report, name } => {
                if let Some(sel) = insert_at_palette(ed, request_node(&name, report)) {
                    super::report_wizard::open(ed, app, &sel);
                }
            }
            Act::Move { path, up } => {
                let mut new_sel = None;
                ed.edit_flow(|flow| {
                    new_sel = move_node(flow, &path, up);
                });
                if let Some(ns) = new_sel {
                    ed.selection = ns;
                }
            }
            Act::DropNode { pos, node } => {
                ed.edit_flow(|flow| insert_node(flow, &pos, node));
                let mut sel = pos.parent.clone();
                sel.push(pos.index);
                ed.selection = sel.clone();
                // Placing a new block opens its configure wizard straight away
                // so the user can fill it in (pick a request, set VAR=VALUE, …)
                // without a second click.
                super::report_wizard::open(ed, app, &sel);
            }
            Act::AttachMod { path, modifier } => {
                // Dropping WITH opens the WITH-field wizard directly (append on
                // OK) rather than attaching a placeholder field, so a cancelled
                // drop leaves no empty `field: HttpStatus` behind.
                if modifier == Modifier::With {
                    ed.selection = path.clone();
                    super::report_wizard::open_with_field(ed, &path, None);
                    continue;
                }
                // REPORT on a VARIABLE (`Assign`) doesn't transform it (the
                // assignment must stay to define the variable) — it inserts a
                // sibling `REPORT (VAR)` line right after it. Every other
                // modifier transforms its node in place.
                let assign_report = modifier == Modifier::Report
                    && matches!(
                        ed.flow.as_ref().and_then(|f| node_at(f, &path)),
                        Some(FlowNode::Assign { .. })
                    );
                if assign_report {
                    let mut new_sel = None;
                    ed.edit_flow(|flow| {
                        new_sel = report_assignment(flow, &path);
                    });
                    ed.selection = new_sel.unwrap_or(path);
                } else {
                    ed.edit_flow(|flow| {
                        attach_modifier(flow, &path, modifier);
                    });
                    ed.selection = path;
                }
                // Dropping a modifier chip (AS/REPORT/PARALLEL) opens the
                // affected node's wizard so the new clause can be filled in
                // immediately.
                let sel = ed.selection.clone();
                super::report_wizard::open(ed, app, &sel);
            }
            Act::MoveMod {
                from,
                which,
                to,
                copy,
            } => {
                let mut moved = false;
                ed.edit_flow(|flow| {
                    moved = transfer_modifier(flow, &from, which, &to, copy);
                });
                // Select the line the clause landed on, so the result of the
                // drop is what's highlighted. A refused transfer leaves the
                // selection (and the flow) exactly as it was.
                if moved {
                    ed.selection = to;
                }
            }
            Act::DetachMod { path, which } => {
                ed.edit_flow(|flow| {
                    if detach_modifier(flow, &path, which) {
                        remove_node(flow, &path);
                    }
                });
                ed.selection = Vec::new();
            }
            Act::RenameRequest { path, name } => {
                ed.edit_flow(|flow| {
                    set_request_name(flow, &path, &name);
                });
                ed.selection = path;
            }
            Act::SetEnvRole {
                path,
                baseline,
                index,
                name,
            } => {
                ed.edit_flow(|flow| {
                    edit::set_env_role(flow, &path, baseline, index, &name);
                });
                ed.selection = path;
            }
            Act::MoveNode { from, pos } => {
                let mut new_sel = None;
                ed.edit_flow(|flow| {
                    new_sel = edit::move_node_to(flow, &from, &pos);
                });
                if let Some(ns) = new_sel {
                    ed.selection = ns;
                }
            }
            Act::DeletePath(path) => {
                ed.edit_flow(|flow| {
                    remove_node(flow, &path);
                });
                ed.selection = Vec::new();
            }
            Act::OpenWizard(path) => super::report_wizard::open(ed, app, &path),
            Act::SetAlias { path, text } => {
                ed.edit_flow(|flow| {
                    edit::set_report_alias(flow, &path, &text);
                });
                ed.selection = path;
            }
            Act::SetLoopVar { path, text } => {
                ed.edit_flow(|flow| {
                    edit::set_loop_var(flow, &path, &text);
                });
                ed.selection = path;
            }
            Act::SetLoopDir { path, text } => {
                ed.edit_flow(|flow| {
                    edit::set_loop_dir(flow, &path, &text);
                });
                ed.selection = path;
            }
            Act::SetLoopGlob { path, text } => {
                ed.edit_flow(|flow| {
                    edit::set_loop_glob(flow, &path, &text);
                });
                ed.selection = path;
            }
            Act::PickLoopDir { path, file } => {
                pick_loop_dir(ed, app, &path, file);
            }
            Act::SetParallelDegree { path, degree } => {
                ed.edit_flow(|flow| {
                    edit::set_parallel_degree(flow, &path, degree);
                });
                ed.selection = path;
            }
            Act::SetHeader { key, value } => {
                ed.edit_flow(|flow| {
                    edit::set_header(flow, key, value.as_deref());
                });
            }
            Act::PickHeaderFile { key } => {
                pick_header_file(ed, app, key);
            }
            Act::AddWith { path } => {
                ed.selection = path.clone();
                super::report_wizard::open_with_field(ed, &path, None);
            }
            Act::EditWith { path, index } => {
                ed.selection = path.clone();
                super::report_wizard::open_with_field(ed, &path, Some(index));
            }
            Act::RemoveWith { path, index } => {
                ed.edit_flow(|flow| {
                    detach_modifier(flow, &path, DetachWhich::With(index));
                });
                ed.selection = path;
            }
            Act::AttachWithStats { path, index } => {
                ed.edit_flow(|flow| {
                    edit::attach_with_stats(flow, &path, index);
                });
                ed.selection = path;
            }
        }
    }
    sync_back(ed, app);
}

/// Insert `node` at the open palette's position, select it, close the palette,
/// and return the new node's path (so the caller can open its wizard). `None`
/// when no palette was open.
fn insert_at_palette(ed: &mut ReportEditor, node: FlowNode) -> Option<Vec<usize>> {
    let p = ed.palette.take()?;
    let pos = p.pos.clone();
    ed.edit_flow(|flow| insert_node(flow, &pos, node));
    // Select the newly inserted node.
    let mut sel = pos.parent.clone();
    sel.push(pos.index);
    ed.selection = sel.clone();
    Some(sel)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{Language, Strings};

    /// Enter at the end of an indented line carries that indentation over, so a
    /// statement typed inside a loop body stays inside it visually.
    #[test]
    fn enter_inherits_the_current_lines_indent() {
        let text = "FOR F IN [\"a\"]\n    REQUEST r";
        let at = text.chars().count();
        let (out, caret) = newline_with_indent(text, at..at);
        assert_eq!(out, "FOR F IN [\"a\"]\n    REQUEST r\n    ");
        assert_eq!(caret, out.chars().count());
    }

    /// A block opener indents its body one further level.
    #[test]
    fn enter_after_an_opener_adds_a_level() {
        let text = "FOR F IN [\"a\"]";
        let at = text.chars().count();
        let (out, caret) = newline_with_indent(text, at..at);
        assert_eq!(out, "FOR F IN [\"a\"]\n    ");
        assert_eq!(caret, out.chars().count());

        let text = "    REPORT REQUEST p WITH";
        let at = text.chars().count();
        let (out, _) = newline_with_indent(text, at..at);
        assert_eq!(out, "    REPORT REQUEST p WITH\n        ");
    }

    /// Splitting a line in the middle stays a plain newline: the text after the
    /// caret moves down as-is, and silently inserting spaces in front of it
    /// would corrupt what the user was splitting.
    #[test]
    fn a_mid_line_split_is_not_indented() {
        let text = "    REQUEST rs";
        let at = text.chars().count() - 1;
        let (out, caret) = newline_with_indent(text, at..at);
        assert_eq!(out, "    REQUEST r\ns");
        assert_eq!(caret, at + 1);
    }

    /// Enter with a selection replaces it, like any typed character.
    #[test]
    fn enter_replaces_the_selection() {
        let text = "FOR F IN [\"a\"]\n    REQUEST rXXX";
        let start = text.chars().count() - 3;
        let (out, caret) = newline_with_indent(text, start..text.chars().count());
        assert_eq!(out, "FOR F IN [\"a\"]\n    REQUEST r\n    ");
        assert_eq!(caret, out.chars().count());
    }

    /// A line that has become `END` snaps back to its opener's indentation.
    #[test]
    fn end_dedents_to_its_opener() {
        let text = "FOR F IN [\"a\"]\n    REQUEST r\n    END";
        let at = text.chars().count();
        let (out, caret) = snap_end_line(text, at).expect("END should snap");
        assert_eq!(out, "FOR F IN [\"a\"]\n    REQUEST r\nEND");
        assert_eq!(caret, out.chars().count());
    }

    /// Snapping is idempotent, and leaves alone anything it can't resolve — a
    /// stray `END` is the user's to fix, not ours to guess at.
    #[test]
    fn end_snapping_leaves_aligned_or_unresolvable_lines_alone() {
        let aligned = "FOR F IN [\"a\"]\n    REQUEST r\nEND";
        assert!(snap_end_line(aligned, aligned.chars().count()).is_none());

        let stray = "REQUEST r\n    END";
        assert!(snap_end_line(stray, stray.chars().count()).is_none());

        let not_an_end = "FOR F IN [\"a\"]\n    ENDPOINT = x";
        assert!(snap_end_line(not_an_end, not_an_end.chars().count()).is_none());
    }

    /// Tab indents one four-space level, replacing any selection.
    #[test]
    fn tab_indents_one_level() {
        let (out, caret) = indent_at("REQUEST r", 0..0);
        assert_eq!(out, "    REQUEST r");
        assert_eq!(caret, 4);

        let (out, caret) = indent_at("REQUEST rXX", 9..11);
        assert_eq!(out, "REQUEST r    ");
        assert_eq!(caret, 13);
    }

    /// A de-indent deletes back to the previous four-column stop within the run
    /// of spaces at the caret, so one press clears a level rather than a space.
    #[test]
    fn dedent_walks_back_to_the_previous_four_stop() {
        assert_eq!(dedent_span("        END", 8), Some(4));
        assert_eq!(dedent_span("        END", 4), Some(4));
        // A partial level is cleared on its own, landing on the stop below.
        assert_eq!(dedent_span("      END", 6), Some(2));
        assert_eq!(dedent_span("     END", 5), Some(1));

        let (out, caret) = delete_before("        END", 8, 4);
        assert_eq!(out, "    END");
        assert_eq!(caret, 4);
    }

    /// Away from indentation the key keeps its ordinary meaning, so the widget
    /// keeps its own (selection-aware) handling.
    #[test]
    fn dedent_declines_anywhere_but_a_run_of_spaces() {
        assert_eq!(dedent_span("END", 3), None); // no space before the caret
        assert_eq!(dedent_span("    END", 0), None); // start of the line
        assert_eq!(dedent_span("\tEND", 1), None); // a tab is not our indent
    }

    /// Padding after content de-indents too — that's what Tab leaves behind
    /// when it lands mid-line.
    #[test]
    fn dedent_also_clears_trailing_padding() {
        assert_eq!(dedent_span("END    ", 7), Some(4));
        let (out, caret) = delete_before("END    ", 7, 4);
        assert_eq!(out, "END");
        assert_eq!(caret, 3);
    }

    /// Both helpers count in chars, so multi-byte text can't split a character
    /// or land the caret mid-codepoint.
    #[test]
    fn indent_helpers_are_char_indexed_not_byte_indexed() {
        let text = "    REQUEST \u{e9}\u{e9}\u{e9}";
        let at = text.chars().count();
        let (out, caret) = newline_with_indent(text, at..at);
        assert_eq!(out, "    REQUEST \u{e9}\u{e9}\u{e9}\n    ");
        assert_eq!(caret, out.chars().count());
        assert_eq!(row_col_at(text, at), (0, 15));
        assert_eq!(row_start("\u{e9}\u{e9}\n x", 1), 3);
    }

    /// The relative luminance of a colour, per WCAG 2.1.
    fn luminance(c: Color32) -> f64 {
        let ch = |v: u8| {
            let v = v as f64 / 255.0;
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * ch(c.r()) + 0.7152 * ch(c.g()) + 0.0722 * ch(c.b())
    }

    /// The WCAG contrast ratio between two colours (1.0 = identical, 21.0 = the
    /// most any pair can manage).
    fn contrast(a: Color32, b: Color32) -> f64 {
        let (hi, lo) = {
            let (x, y) = (luminance(a), luminance(b));
            if x > y { (x, y) } else { (y, x) }
        };
        (hi + 0.05) / (lo + 0.05)
    }

    /// A chip's label must stay comfortably readable on its own tint, in every
    /// theme.
    ///
    /// This guards the change that quietened the blocks: the label colour is now
    /// blended from the theme text colour toward the category hue rather than
    /// being the hue itself, which is what makes the tinting safe to keep. Full
    /// chroma text on a tint of the same hue used to fall as low as 2.3:1 for
    /// `accent` chips on the English preset — text a user with ordinary eyesight
    /// has to lean in to read. Anyone tempted to turn the colour back up will
    /// trip this rather than find out from a customer.
    ///
    /// 4.5:1 is WCAG AA for body text. Checked against every built-in preset,
    /// since a preset is a set of arbitrary RGB triples and a new one could
    /// easily be picked that reads well in isolation but not behind a chip.
    #[test]
    fn every_chip_categorys_label_is_readable_on_its_own_tint_in_every_theme() {
        for spec in crate::theme::builtin_presets() {
            let th = GuiTheme::from_spec(&spec);
            for kind in [
                NodeKind::Request,
                NodeKind::ReportRequest,
                NodeKind::ReportVar,
                NodeKind::ReportComputed,
                NodeKind::Assign,
                NodeKind::ForFiles,
                NodeKind::ForFolders,
                NodeKind::ForEnvs,
                NodeKind::List,
            ] {
                let color = kind_color(kind, &th);
                let tint = chip_tint(&th, color);
                let ratio = contrast(tint.text, tint.fill);
                assert!(
                    ratio >= 4.5,
                    "{} chip {kind:?}: label contrast {ratio:.2} is below WCAG AA",
                    spec.name
                );
            }
            // The error colour never comes from `kind_color` but is used for
            // unset required settings — the one thing standing between the
            // report and a run, so of all of them it must be legible.
            let tint = chip_tint(&th, th.err);
            assert!(
                contrast(tint.text, tint.fill) >= 4.5,
                "{} error chip",
                spec.name
            );
        }
    }

    /// Categories are still told apart by colour, so quietening the chips must
    /// not have quietened them into each other.
    ///
    /// Checked on the *rule* rather than the fill, because the rule is now where
    /// the category lives: the surface is deliberately near-neutral, so fills
    /// that were once far apart are now all within a few points of the panel
    /// colour and would make this assertion meaningless.
    #[test]
    fn the_chip_categories_remain_visually_distinct_from_one_another() {
        let th = GuiTheme::from_spec(&crate::theme::preset_for_language(&Language::English));
        let kinds = [
            NodeKind::Request,
            NodeKind::ReportVar,
            NodeKind::Assign,
            NodeKind::List,
        ];
        let rules: Vec<Color32> = kinds
            .into_iter()
            .map(|k| {
                chip_tint(&th, kind_color(k, &th))
                    .rule
                    .expect("a category chip shows a rule")
            })
            .collect();
        let labels: Vec<Color32> = kinds
            .into_iter()
            .map(|k| chip_tint(&th, kind_color(k, &th)).text)
            .collect();
        for channel in [&rules, &labels] {
            for (i, a) in channel.iter().enumerate() {
                for b in &channel[i + 1..] {
                    let dist = (a.r() as i32 - b.r() as i32).abs()
                        + (a.g() as i32 - b.g() as i32).abs()
                        + (a.b() as i32 - b.b() as i32).abs();
                    assert!(
                        dist > 40,
                        "two categories came out too close to tell apart: {a:?} vs {b:?}"
                    );
                }
            }
        }
    }

    /// The category rule is the full-strength hue, not a tint of it.
    ///
    /// The whole trade in moving the category from the fill to the rule is that
    /// a *small* area can afford *full* chroma. Tinting the bar as well would
    /// give up the identifiability the neutral surface was bought with, and
    /// would do it invisibly — the editor would just slowly become grey.
    #[test]
    fn the_category_rule_carries_the_hue_at_full_strength() {
        let th = GuiTheme::from_spec(&crate::theme::default_preset());
        for kind in [NodeKind::Request, NodeKind::Assign, NodeKind::List] {
            let color = kind_color(kind, &th);
            let tint = chip_tint(&th, color);
            assert_eq!(tint.rule, Some(color), "{kind:?} rule is the category hue");
            // ...while the surface it sits on stays close to the panel.
            let from_panel = (tint.fill.r() as i32 - th.panel.r() as i32).abs()
                + (tint.fill.g() as i32 - th.panel.g() as i32).abs()
                + (tint.fill.b() as i32 - th.panel.b() as i32).abs();
            assert!(
                from_panel < 60,
                "{kind:?} chip fill drifted away from the panel colour ({from_panel})"
            );
        }
    }

    /// A selected chip and the hanger of a tethered pair show no rule.
    ///
    /// The first because its fill already carries the selection colour, and a
    /// category bar inside it competes with that; the second because the
    /// hanger's leading edge is the *seam* of a two-part pill, where a coloured
    /// bar reads as a divider rather than as a category.
    #[test]
    fn chips_that_should_not_show_a_rule_do_not() {
        let th = GuiTheme::from_spec(&crate::theme::default_preset());

        let mut chip = Chip::base("GET".to_string(), th.accent);
        assert!(
            chip_colors(&th, &chip, false).rule.is_some(),
            "an ordinary base chip shows its category"
        );
        assert!(
            chip_colors(&th, &chip, true).rule.is_none(),
            "a selected chip drops the rule"
        );

        chip.join_prev = true;
        assert!(
            chip_colors(&th, &chip, false).rule.is_none(),
            "the hanger of a tethered pair drops the rule"
        );
    }

    /// The text of one laid-out section, for the label-face test below.
    fn section_text(job: &egui::text::LayoutJob, i: usize) -> &str {
        let r = &job.sections[i].byte_range;
        &job.text[usize::from(r.start)..usize::from(r.end)]
    }

    /// A chip label is set as two runs — the editor's keyword and the user's
    /// identifier — in two different faces.
    ///
    /// The split is what stops `BASELINE(staging)` reading as one word, so it is
    /// worth pinning: a future label built with a different separator would
    /// otherwise silently fall back to one undifferentiated run.
    #[test]
    fn a_chip_label_sets_the_keyword_and_the_identifier_in_different_faces() {
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            {
                let col = Color32::WHITE;

                let job = chip_label_job(ui, "BASELINE(staging)", col);
                assert_eq!(job.sections.len(), 2, "keyword and identifier");
                assert_eq!(section_text(&job, 0), "BASELINE");
                assert_eq!(section_text(&job, 1), "(staging)");
                assert_eq!(
                    job.sections[1].format.font_id.family,
                    egui::FontFamily::Monospace,
                    "the user's own name is set in the monospace face"
                );
                assert_ne!(
                    job.sections[0].format.font_id.family,
                    egui::FontFamily::Monospace,
                    "the keyword stays in the UI face"
                );

                // `REQUEST orders` splits on the space just the same.
                let job = chip_label_job(ui, "REQUEST orders", col);
                assert_eq!(section_text(&job, 0), "REQUEST");
                assert_eq!(
                    job.sections[1].format.font_id.family,
                    egui::FontFamily::Monospace
                );

                // A bare keyword is a single run and stays in the UI face.
                let job = chip_label_job(ui, "PARALLEL", col);
                assert_eq!(job.sections.len(), 1);
                assert_ne!(
                    job.sections[0].format.font_id.family,
                    egui::FontFamily::Monospace
                );
            }
        });
    }

    /// The two synthetic sentinels are drawn as a matched pair, and neither is
    /// dressed as a keyword.
    ///
    /// `Begin` and the flow's closing `End` are editor punctuation: no node
    /// backs them, and nothing in the report file corresponds to them. The
    /// uppercase `END` that closes a `FOR` or a `WITH` *is* in the file. Users
    /// asked whether `BEGIN` was a word they had to write — it isn't — so the
    /// two kinds must not look alike.
    #[test]
    fn the_synthetic_sentinels_are_captions_not_keywords() {
        let th = GuiTheme::from_spec(&crate::theme::default_preset());
        let s = Strings::for_language(&Language::English);

        // Sentence case and translated, so they can never be mistaken for the
        // literal keyword.
        assert_ne!(s.report_node_begin, "BEGIN");
        assert_ne!(s.report_node_end, "END");
        for lang in [Language::English, Language::French, Language::Danish] {
            let s = Strings::for_language(&lang);
            assert_ne!(
                s.report_node_end, "END",
                "the synthetic end must not collide with the real keyword"
            );
        }

        // ...and drawn in the dim colour rather than the keyword accent.
        assert_ne!(
            th.dim, th.accent,
            "the test below is only meaningful if the two colours differ"
        );
    }

    /// An inline field is as wide as what is in it, within bounds.
    ///
    /// Fixed widths were the reason aliases and folder paths appeared cut off
    /// with nothing to say there was more text: the box simply ended. The clamp
    /// matters as much as the growth — an unbounded field would let one deep
    /// path push the rest of a loop head off the side of the pane.
    #[test]
    fn an_inline_field_grows_to_fit_its_value_but_not_without_limit() {
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            let min = LOOP_PATH_FIELD_WIDTH;

            let empty = fitted_field_width(ui, "", "", min, FIELD_MAX_WIDTH);
            assert_eq!(empty, min, "an empty field rests at its minimum");

            let short = fitted_field_width(ui, "envs", "", min, FIELD_MAX_WIDTH);
            assert_eq!(short, min, "a value that already fits doesn't shrink it");

            let long = fitted_field_width(
                ui,
                "reports/2026/quarterly/regional-breakdowns",
                "",
                min,
                FIELD_MAX_WIDTH,
            );
            assert!(
                long > min,
                "a value wider than the box widens it ({long} vs {min})"
            );

            let absurd = fitted_field_width(ui, &"x".repeat(500), "", min, FIELD_MAX_WIDTH);
            assert_eq!(
                absurd, FIELD_MAX_WIDTH,
                "growth stops at the cap rather than running off the pane"
            );

            // An empty box still has to show its own placeholder.
            let hinted = fitted_field_width(ui, "", "*.json", 10.0, FIELD_MAX_WIDTH);
            assert!(hinted > 10.0, "an empty field makes room for its hint");
        });
    }

    /// The boxes' placeholders are short enough to actually fit them, and the
    /// long explanations are kept separate.
    ///
    /// These were once the same string, so a field a few characters wide was
    /// given a full sentence as its placeholder and rendered it as "Only files
    /// ..." — which is what prompted this. Checked in every language, since a
    /// translation is exactly where a "short" label quietly stops being short.
    #[test]
    fn a_fields_placeholder_is_short_and_its_explanation_is_not_the_same_string() {
        for lang in [Language::English, Language::French, Language::Danish] {
            let s = Strings::for_language(&lang);
            for hint in [
                s.gui_report_loop_var_hint,
                s.gui_report_loop_dir_hint,
                s.gui_report_loop_glob_hint,
                s.gui_report_alias_hint,
            ] {
                assert!(
                    hint.chars().count() <= 12,
                    "{lang:?}: placeholder {hint:?} is too long to fit its box"
                );
            }
            assert_ne!(
                s.gui_report_loop_glob_hint, s.chip_help_loop_glob,
                "{lang:?}: the placeholder must not be the explanation"
            );
            // The explanation, by contrast, has room to actually explain.
            assert!(
                s.chip_help_loop_glob.chars().count() > 40,
                "{lang:?}: the MATCH explanation should say what a pattern is"
            );
            assert!(
                s.chip_help_loop_pick_folder != s.chip_help_loop_dir,
                "{lang:?}: the picker button explains itself, not the box"
            );
        }
    }

    /// The Source view's colouring is the terminal UI's, span for span.
    ///
    /// Rather than restate the highlighting rules (which would let the two
    /// front-ends drift the moment one side gained a keyword), this walks the
    /// laid-out job and checks every byte carries the colour the shared
    /// highlighter assigned it for the same line.
    #[test]
    fn the_source_view_colours_match_the_terminal_uis_exactly() {
        let spec = crate::theme::preset_for_language(&Language::English);
        let th = GuiTheme::from_spec(&spec);
        let theme = spec.to_theme();
        let ctx = HlCtx::default();
        let text = "# collection: api.hurl\nFOR f IN FILES \"*.json\"\n  REQUEST Health\nEND\n";

        let job = highlight_job(
            text,
            &ctx,
            &spec,
            &th,
            egui::FontId::monospace(12.0),
            f32::INFINITY,
        );

        // Rebuild the expected colour for every byte from the ratatui spans.
        let mut expected: Vec<egui::Color32> = Vec::new();
        for (i, line) in text.split('\n').enumerate() {
            if i > 0 {
                expected.push(th.text);
            }
            for span in report_highlight::highlight_row(i, line, &ctx, &theme) {
                let c = span
                    .style
                    .fg
                    .map_or(th.text, |c| super::super::theme::from_ratatui(c, th.text));
                expected.extend(std::iter::repeat_n(c, span.content.len()));
            }
        }
        assert_eq!(
            expected.len(),
            job.text.len(),
            "every byte of the source is covered exactly once"
        );

        for section in &job.sections {
            let (start, end) = byte_span(section);
            for (b, want) in expected.iter().enumerate().take(end).skip(start) {
                assert_eq!(
                    section.format.color, *want,
                    "byte {b} of {text:?} is coloured like the terminal UI"
                );
            }
        }

        // Sanity: the keywords really are picked out, so a highlighter that
        // silently returned one plain span couldn't pass the check above.
        let distinct: std::collections::HashSet<_> =
            job.sections.iter().map(|s| s.format.color).collect();
        assert!(
            distinct.len() > 2,
            "the source is multi-coloured, not flat: {distinct:?}"
        );
    }

    /// A script the parser rejects underlines exactly the offending line, which
    /// is what makes a typo findable without reading the validation panel.
    #[test]
    fn the_line_the_parser_rejected_is_underlined() {
        let spec = crate::theme::preset_for_language(&Language::English);
        let th = GuiTheme::from_spec(&spec);
        let text = "REQUEST Health\nOOPS not papertrail\nREQUEST Other\n";
        let ctx = HlCtx {
            error_line: Some(2),
            ..Default::default()
        };

        let job = highlight_job(
            text,
            &ctx,
            &spec,
            &th,
            egui::FontId::monospace(12.0),
            f32::INFINITY,
        );

        let bad_start = text.find("OOPS").unwrap();
        let bad_end = bad_start + "OOPS not papertrail".len();
        for section in &job.sections {
            let underlined = section.format.underline != egui::Stroke::NONE;
            let (start, end) = byte_span(section);
            let overlaps = start < bad_end && bad_start < end;
            assert_eq!(
                underlined, overlaps,
                "only the rejected line is underlined (bytes {start}..{end})"
            );
        }
    }

    /// A layout section's byte range as plain `usize`s (egui wraps them in a
    /// newtype).
    fn byte_span(section: &egui::text::LayoutSection) -> (usize, usize) {
        (
            usize::from(section.byte_range.start),
            usize::from(section.byte_range.end),
        )
    }

    /// Apply the styling `GuiApp::new` applies, so a measurement taken here is
    /// the one the app renders at.
    ///
    /// The text scaling matters as much as the padding: chip heights agreed at
    /// egui's default sizes and drifted a fraction of a pixel apart once the
    /// text grew, which is precisely the bug this harness missed by measuring an
    /// unscaled context.
    fn style_like_the_app(ctx: &egui::Context) {
        ctx.all_styles_mut(|s| s.spacing.button_padding = egui::vec2(8.0, 4.0));
        ctx.all_styles_mut(|s| {
            for (_, font) in s.text_styles.iter_mut() {
                font.size *= 1.08;
            }
        });
    }

    /// Render a single chip in isolation and return the height of the frame it
    /// draws. Runs a few frames so egui's sizing settles.
    fn chip_height(build: impl Fn(&mut egui::Ui, &GuiTheme, &Strings, &mut Vec<Act>)) -> f32 {
        let ctx = egui::Context::default();
        style_like_the_app(&ctx);
        let th = GuiTheme::from_spec(&crate::theme::preset_for_language(&Language::English));
        let s = crate::i18n::Strings::for_language(&Language::English);
        let mut h = 0.0;
        for _ in 0..3 {
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
                ui.horizontal(|ui| {
                    let mut acts = Vec::new();
                    let r = ui.scope(|ui| build(ui, &th, &s, &mut acts));
                    h = r.response.rect.height();
                });
            });
        }
        h
    }

    /// Lay two chips out the way `block_row` lays out a line — closing the gap
    /// in front of a tethered one — and return the rect each rendered into.
    fn pair_rects(chips: &[Chip]) -> Vec<egui::Rect> {
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|s| s.spacing.button_padding = egui::vec2(8.0, 4.0));
        let th = GuiTheme::from_spec(&crate::theme::preset_for_language(&Language::English));
        let s = crate::i18n::Strings::for_language(&Language::English);
        let mut rects = Vec::new();
        // A few frames, so egui's sizing settles before the rects are read.
        for _ in 0..3 {
            rects.clear();
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
                ui.horizontal_top(|ui| {
                    let gap = ui.spacing().item_spacing.x;
                    let mut acts = Vec::new();
                    for chip in chips {
                        if chip.join_next {
                            ui.spacing_mut().item_spacing.x = TETHER_GAP;
                        }
                        rects.push(render_chip(
                            ui,
                            &th,
                            &s,
                            chip,
                            false,
                            &[0],
                            &[],
                            &[],
                            &mut acts,
                        ));
                        ui.spacing_mut().item_spacing.x = gap;
                    }
                });
            });
        }
        rects
    }

    /// The pill only reads as one control if the two halves actually meet: a
    /// gap between them would leave the squared-off corners looking like a
    /// rendering fault rather than a divider.
    #[test]
    fn the_halves_of_a_tethered_pill_are_rendered_touching() {
        let pair = |tether: bool| {
            let show = Chip::modifier(
                "SHOW(Time)".into(),
                Color32::GREEN,
                DetachWhich::BaselineShow,
            );
            let mut chips = vec![
                Chip::modifier(
                    "BASELINE(prod)".into(),
                    Color32::RED,
                    DetachWhich::Statistics,
                ),
                if tether { show.tether() } else { show },
            ];
            link_tethers(&mut chips);
            let r = pair_rects(&chips);
            r[1].left() - r[0].right()
        };

        assert_eq!(
            pair(true),
            0.0,
            "the tethered half sits flush against the chip it qualifies, so their \
             borders meet as the pill's divider"
        );
        assert!(
            pair(false) > 0.0,
            "while an untethered neighbour keeps the normal gap — otherwise every \
             chip on the line would look joined"
        );
    }

    #[test]
    fn all_chip_kinds_render_at_the_same_height() {
        // A plain label chip.
        let label = chip_height(|ui, th, s, acts| {
            let chip = Chip::base("REPORT".into(), th.subst);
            render_chip(ui, th, s, &chip, false, &[0], &[], &[], acts);
        });
        // A chip hosting a combo box (the request picker).
        let combo = chip_height(|ui, th, s, acts| {
            let chip = Chip::request("oauth2", th.subst);
            let titles = vec!["oauth2".to_string()];
            render_chip(ui, th, s, &chip, false, &[0], &titles, &[], acts);
        });
        // A chip hosting an inline text field (the AS alias).
        let alias = chip_height(|ui, th, s, acts| {
            let chip = Chip::alias("Env", th.subst, Some(DetachWhich::As));
            render_chip(ui, th, s, &chip, false, &[0], &[], &[], acts);
        });

        assert!(label > 0.0 && combo > 0.0 && alias > 0.0);
        // Exactly equal, not merely close: a chip that is a fraction of a pixel
        // short still rounds to a visibly uneven bottom edge beside its
        // neighbour, which is how the combo chips came to sit a pixel high.
        assert_eq!(
            label, combo,
            "a combo chip must be exactly as tall as a label chip"
        );
        // The PARALLEL chip, which hosts a small numeric field.
        let parallel = chip_height(|ui, th, s, acts| {
            let chip = Chip::parallel(Some(4), th.err);
            render_chip(ui, th, s, &chip, false, &[0], &[], &[], acts);
        });

        assert!(parallel > 0.0);
        assert_eq!(
            label, alias,
            "an inline text field must be exactly as tall as a label chip"
        );
        assert_eq!(
            label, parallel,
            "the PARALLEL field must be exactly as tall as a label chip"
        );

        // The flow's closing `END`, which brackets the whole report against the
        // `BEGIN` at the top. It sits in the block column like any other row, so
        // it has to line up with them.
        let flow_end = chip_height(|ui, th, s, _acts| flow_end_row(ui, th, s));
        assert_eq!(
            label, flow_end,
            "the closing END must be exactly as tall as a label chip"
        );
    }

    /// `BEGIN` and the flow's `END` are drawn from the same static-chip helper,
    /// carry their own (distinct) hover help, and read as a matching pair.
    #[test]
    fn the_flow_is_bracketed_by_begin_and_a_matching_end() {
        let s = Strings::for_language(&Language::English);
        assert_ne!(
            s.chip_help_flow_end, s.chip_help_end,
            "the flow's END explains itself, not the loop END"
        );
        assert!(
            s.chip_help_flow_end.contains("END"),
            "the help names the block it describes"
        );

        // Both chips paint (a static chip with no text would silently vanish).
        let begin = chip_height(|ui, th, s, _acts| {
            static_chip(ui, th, s.report_node_begin, th.accent, s.chip_help_begin)
        });
        let end = chip_height(|ui, th, s, _acts| flow_end_row(ui, th, s));
        assert!(begin > 0.0 && end > 0.0);
        assert!(
            (begin - end).abs() < 0.5,
            "BEGIN {begin} and END {end} are the same size"
        );
    }

    #[test]
    fn the_settings_strip_covers_every_header_directive_the_language_has() {
        // The parser exposes exactly these six directives; if one is added there
        // and not here, the GUI would quietly be unable to set it.
        let flow = crate::report::parse_flow(
            "# collection: c.hurl\n# output: o.csv\n# environment: dev\n# root: /r\n\
             # baseline: b.baseline\n# columns: a,b\nREQUEST login\n",
        )
        .expect("fixture parses");
        let specs = header_specs();
        for spec in &specs {
            assert!(
                flow.header.get(spec.key).is_some(),
                "{} is a real directive",
                spec.key
            );
        }
        assert_eq!(
            specs
                .iter()
                .filter(|sp| sp.always_shown)
                .map(|sp| sp.key)
                .collect::<Vec<_>>(),
            ["collection", "output"],
            "only collection and output are always shown; the rest are opt-in"
        );
        // Being shown and being mandatory are different things: an absent
        // `output:` still runs (the language defaults it to CSV), so only
        // `collection:` earns the error-coloured unset prompt.
        assert_eq!(
            specs
                .iter()
                .filter(|sp| sp.required)
                .map(|sp| sp.key)
                .collect::<Vec<_>>(),
            ["collection"],
            "only a missing collection actually stops the report running"
        );

        // Each directive explains itself in its own words.
        let s = Strings::for_language(&Language::English);
        let helps: Vec<&str> = specs
            .iter()
            .map(|sp| crate::report::edit::header_help(sp.key, &s))
            .collect();
        for (i, h) in helps.iter().enumerate() {
            assert!(!h.is_empty(), "{} has help", specs[i].key);
            assert!(
                helps.iter().filter(|o| o == &h).count() == 1,
                "{} has help of its own",
                specs[i].key
            );
        }
    }

    /// `set_header` drops an empty value, so an empty placeholder made picking a
    /// setting from the add menu silently do nothing — which is how `columns:`
    /// became unaddable.
    #[test]
    fn every_addable_setting_starts_at_a_value_that_survives_being_set() {
        assert!(
            !crate::report::edit::HEADER_PLACEHOLDER.is_empty(),
            "an added setting would be dropped again the moment it was added"
        );
        // And there is something to add in the first place.
        assert!(header_specs().iter().any(|sp| !sp.always_shown));
    }

    /// `# output:` names a format from a closed list — the runner derives the
    /// filename and rejects anything else — so it must be picked, not typed at
    /// with a file browser beside it.
    #[test]
    fn the_output_setting_offers_only_the_formats_the_runner_can_write() {
        let spec = header_specs()
            .into_iter()
            .find(|sp| sp.key == "output")
            .expect("output is a setting");
        assert!(
            matches!(spec.kind, HeaderKind::Format),
            "output is chosen from a list, not typed or browsed for"
        );

        // Every offered format really is one the runner can write, and every
        // format the runner can write is offered.
        for ext in crate::report::writer::OUTPUT_EXTENSIONS {
            assert!(
                crate::report::writer::writer_for_extension(ext).is_some(),
                "{ext} has a writer"
            );
            let flow = crate::report::parse_flow(&format!(
                "# collection: c.hurl\n# output: {ext}\nREQUEST a\n"
            ))
            .expect("parses");
            assert!(
                crate::report::validate::validate(
                    &flow,
                    &crate::report::validate::Context::default(),
                )
                .iter()
                .all(|d| !d.message.contains("unsupported output")),
                "{ext} is accepted by the validator"
            );
        }

        // No path-valued setting claims `output`, so the browse button is gone.
        let paths: Vec<&str> = header_specs()
            .iter()
            .filter(|sp| sp.kind.is_path())
            .map(|sp| sp.key)
            .collect();
        assert_eq!(paths, ["root", "baseline"]);
    }

    /// The button that adds a report setting says what it adds. A bare `+`
    /// above the flow gives no hint whether it adds a *block* — which is what
    /// everything else in this view does — or a report-wide setting.
    #[test]
    fn the_add_setting_button_says_what_it_adds() {
        let ctx = egui::Context::default();
        let s = Strings::for_language(&Language::English);
        let specs = header_specs();
        let missing: Vec<&HeaderSpec> = specs.iter().filter(|sp| !sp.always_shown).collect();

        let out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let mut acts = Vec::new();
            header_add_menu(ui, &s, &missing, &mut acts);
        });
        let painted: String = out
            .shapes
            .iter()
            .filter_map(|c| match &c.shape {
                egui::Shape::Text(t) => Some(t.galley.text().to_string()),
                _ => None,
            })
            .collect();
        assert!(
            painted.contains(s.report_add_setting),
            "the button is labelled, not a bare glyph (painted: {painted:?})"
        );

        // The label names the thing being added, in every language, so it can't
        // drift back to something as vague as "Add".
        for lang in [Language::English, Language::French, Language::Danish] {
            let s = Strings::for_language(&lang);
            assert!(
                s.report_add_setting.split_whitespace().count() >= 3,
                "{lang:?} label {:?} names what it adds",
                s.report_add_setting
            );
        }
    }

    /// The settings sit in a column above `BEGIN`, each starting at the same
    /// left edge as the flow — laid out in a row they were ragged against each
    /// other, and the leading icon pushed the first one out of line with the
    /// blocks below.
    #[test]
    fn the_settings_are_stacked_and_flush_with_the_flow() {
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|s| s.spacing.button_padding = egui::vec2(8.0, 4.0));
        let th = GuiTheme::from_spec(&crate::theme::preset_for_language(&Language::English));
        let s = Strings::for_language(&Language::English);
        let specs = header_specs();
        let choices = vec!["dev".to_string()];

        let mut rows: Vec<egui::Rect> = Vec::new();
        let mut frame_rect = egui::Rect::NOTHING;
        let mut begin_rect = egui::Rect::NOTHING;
        for _ in 0..3 {
            rows.clear();
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
                ui.vertical(|ui| {
                    frame_rect = settings_frame(&th)
                        .show(ui, |ui| {
                            for spec in &specs {
                                let r = ui.horizontal(|ui| {
                                    let mut acts = Vec::new();
                                    header_chip(
                                        ui,
                                        &th,
                                        &s,
                                        spec,
                                        "dev",
                                        Some(&choices),
                                        &collection_choices_fixture,
                                        &mut acts,
                                    );
                                });
                                rows.push(r.response.rect);
                            }
                        })
                        .response
                        .rect;
                    // The flow's first block, for comparison.
                    begin_rect = ui
                        .horizontal_top(|ui| {
                            static_chip(ui, &th, s.report_node_begin, th.accent, s.chip_help_begin)
                        })
                        .response
                        .rect;
                });
            });
        }

        // Every setting starts at the same left edge as every other.
        let left = rows[0].left();
        for (i, r) in rows.iter().enumerate() {
            assert!(
                (r.left() - left).abs() < 0.5,
                "row {i} starts at {} but the first starts at {left}",
                r.left()
            );
        }
        // Stacked, not side by side: each row sits below the one before it.
        for pair in rows.windows(2) {
            assert!(
                pair[1].top() >= pair[0].bottom() - 0.5,
                "rows overlap: {:?} then {:?}",
                pair[0],
                pair[1]
            );
        }
        // The panel around them lines up with the flow below, so the settings
        // and the blocks still read as one column.
        assert!(
            (frame_rect.left() - begin_rect.left()).abs() < 0.5,
            "the settings panel starts at {} but BEGIN starts at {}",
            frame_rect.left(),
            begin_rect.left()
        );
        assert!(
            frame_rect.bottom() <= begin_rect.top() + 0.5,
            "the settings panel overlaps the flow"
        );
    }

    /// The dropdown shows collections by name, but what it *stores* has to
    /// stay a path — `report_cli` opens the directive's value straight off
    /// disk, so a bare name would leave the report unrunnable headless.
    #[test]
    fn the_collection_dropdown_shows_names_but_stores_relative_paths() {
        let root = std::env::temp_dir().join(format!("paperboy_cc_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("apis")).expect("scratch workspace");
        std::fs::write(
            root.join("apis/billing.hurl"),
            "GET https://x
",
        )
        .expect("collection");
        std::fs::write(
            root.join("smoke.hurl"),
            "GET https://x
",
        )
        .expect("collection");
        // Neither of these is a collection, so neither may be offered as one.
        std::fs::write(
            root.join("nightly.trail"),
            "# collection: smoke.hurl
",
        )
        .expect("report");
        std::fs::write(
            root.join("dev.vars"),
            "K=v
",
        )
        .expect("env");

        let report = root.join("nightly.trail");
        let choices = collection_choices(Some(&root), Some(&report), &[], "unsaved");

        let labels: Vec<&str> = choices.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(
            labels,
            ["billing", "smoke"],
            "collections are listed by name, and only collections are listed"
        );
        let values: Vec<&str> = choices.iter().map(|c| c.value.as_str()).collect();
        assert_eq!(
            values,
            [
                std::path::Path::new("apis/billing.hurl")
                    .to_string_lossy()
                    .as_ref(),
                "smoke.hurl"
            ],
            "the stored value stays a path, relative to the report so the pair stays portable"
        );
        assert!(
            choices.iter().all(|c| c.in_workspace),
            "everything found by scanning the workspace is in the workspace"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A workspace has to survive being handed to someone else, so a report
    /// bound to a collection in a sibling folder must say `../apis/…` rather
    /// than an absolute path that only exists on the machine it was made on.
    #[test]
    fn a_collection_in_a_sibling_folder_is_referenced_relatively_so_it_stays_portable() {
        let root = std::path::Path::new("/w");
        let report = std::path::Path::new("/w/reports/nightly.trail");
        let target = std::path::Path::new("/w/apis/billing.hurl");

        assert_eq!(
            portable_ref(target, Some(report), Some(root)),
            "../apis/billing.hurl",
            "it walks up out of reports/ and back down into apis/"
        );
        // Below the report, no walking up is needed.
        assert_eq!(
            portable_ref(
                std::path::Path::new("/w/reports/sub/c.hurl"),
                Some(report),
                Some(root)
            ),
            "sub/c.hurl",
            "a collection under the report is named directly"
        );
        // Outside the workspace the two files aren't travelling together, so a
        // relative path would be a lie.
        assert_eq!(
            portable_ref(
                std::path::Path::new("/elsewhere/legacy.hurl"),
                Some(report),
                Some(root)
            ),
            "/elsewhere/legacy.hurl",
            "nothing outside the workspace is made to look relative to it"
        );
        // With no workspace to bound the walk, the old behaviour stands.
        assert_eq!(
            portable_ref(target, Some(report), None),
            "/w/apis/billing.hurl",
            "without a workspace there is no scope to stay inside"
        );

        // And what it writes is what the runners resolve: both front-ends send
        // a relative ref back through the report's own directory.
        assert_eq!(
            crate::report::context::resolve_ref_path(Some(report), "../apis/billing.hurl"),
            std::path::PathBuf::from("/w/reports/../apis/billing.hurl"),
            "the relative ref resolves against the report's folder"
        );
    }

    /// Collections open in a tab are still offered, marked as being from
    /// outside the workspace so they can be told apart — and never listed
    /// twice when the workspace scan already found them.
    #[test]
    fn open_collections_outside_the_workspace_are_offered_but_marked_as_such() {
        let root = std::env::temp_dir().join(format!("paperboy_cc2_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("scratch workspace");
        std::fs::write(
            root.join("smoke.hurl"),
            "GET https://x
",
        )
        .expect("collection");
        let report = root.join("nightly.trail");

        let mut inside = crate::collection::Collection::new("smoke".to_string(), Vec::new());
        inside.path = Some(root.join("smoke.hurl"));
        let mut outside = crate::collection::Collection::new("legacy".to_string(), Vec::new());
        outside.path = Some(std::path::PathBuf::from("/elsewhere/legacy.hurl"));
        let scratch = crate::collection::Collection::new("Untitled".to_string(), Vec::new());

        let choices = collection_choices(
            Some(&root),
            Some(&report),
            &[inside, outside, scratch],
            "unsaved",
        );

        assert_eq!(
            choices.iter().filter(|c| c.label == "smoke").count(),
            1,
            "a collection both open and in the workspace is offered once, not twice"
        );
        let legacy = choices
            .iter()
            .find(|c| c.label == "legacy")
            .expect("an open collection outside the workspace is still offered");
        assert!(!legacy.in_workspace, "it is not in the workspace");
        assert_eq!(
            legacy.value, "/elsewhere/legacy.hurl",
            "with nowhere shorter to be relative to, the path stays absolute"
        );
        let scratch = choices
            .iter()
            .find(|c| c.label == "Untitled")
            .expect("an unsaved collection is offered");
        assert_eq!(
            (scratch.value.as_str(), scratch.detail.as_str()),
            ("Untitled", "unsaved"),
            "with no path to write it can only be referenced by name, and says so"
        );
        // Workspace collections sort first: they are the likely answer.
        assert!(
            choices[0].in_workspace,
            "the workspace's own collections are offered first"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A stand-in collection list for the layout tests, which are about
    /// geometry rather than about what happens to be on disk.
    fn collection_choices_fixture() -> Vec<CollectionChoice> {
        vec![CollectionChoice {
            value: "c.hurl".to_string(),
            label: "c".to_string(),
            detail: "c.hurl".to_string(),
            in_workspace: true,
        }]
    }

    /// Renders the settings panel over a fixed set of values and returns the
    /// panel's rect plus the rect of every text run painted inside it.
    fn run_settings_panel(
        ctx: &egui::Context,
        set: &[(&str, &str)],
        avail: f32,
    ) -> (egui::Rect, Vec<(String, egui::Rect)>) {
        let th = GuiTheme::from_spec(&crate::theme::preset_for_language(&Language::English));
        let s = Strings::for_language(&Language::English);
        let envs = vec!["dev".to_string()];
        let formats: Vec<String> = crate::report::writer::OUTPUT_EXTENSIONS
            .iter()
            .map(|e| e.to_string())
            .collect();
        let owned: Vec<(String, String)> = set
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        let value_of = move |key: &str| {
            owned
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
                .unwrap_or_default()
        };

        let mut rect = egui::Rect::NOTHING;
        let mut out = None;
        // Three passes: combo boxes and galley measurements only settle once
        // the layout has been seen at least once.
        for _ in 0..3 {
            let full = ctx.run_ui(egui::RawInput::default(), |ui| {
                ui.set_max_width(avail);
                let mut acts = Vec::new();
                rect = ui
                    .scope(|ui| {
                        settings_panel(
                            ui,
                            &th,
                            &s,
                            &value_of,
                            &collection_choices_fixture,
                            &envs,
                            &formats,
                            &mut acts,
                        );
                    })
                    .response
                    .rect;
            });
            out = Some(full);
        }
        let mut texts = Vec::new();
        fn walk(sh: &egui::Shape, out: &mut Vec<(String, egui::Rect)>) {
            match sh {
                egui::Shape::Text(t) => {
                    out.push((
                        t.galley.text().to_string(),
                        egui::Rect::from_min_size(t.pos, t.galley.size()),
                    ));
                }
                egui::Shape::Vec(v) => v.iter().for_each(|sh| walk(sh, out)),
                _ => {}
            }
        }
        for c in &out.expect("rendered").shapes {
            walk(&c.shape, &mut texts);
        }
        (rect, texts)
    }

    /// "Add a report setting" appends to the list, so it belongs underneath it —
    /// above the settings it read as a heading for them.
    #[test]
    fn the_add_setting_button_sits_below_the_settings_it_adds_to() {
        let ctx = egui::Context::default();
        let s = Strings::for_language(&Language::English);
        let (_, texts) = run_settings_panel(&ctx, &[("collection", "c.hurl")], 600.0);

        let button = texts
            .iter()
            .find(|(t, _)| t.contains(s.report_add_setting))
            .map(|(_, r)| *r)
            .expect("the add button is drawn while settings are still missing");
        let collection = texts
            .iter()
            .find(|(t, _)| t == "COLLECTION")
            .map(|(_, r)| *r)
            .expect("the collection setting is drawn");
        assert!(
            button.top() >= collection.bottom(),
            "the add button at {} should sit below the settings ending at {}",
            button.top(),
            collection.bottom()
        );

        // With nothing left to add there is no button at all.
        let (_, full) = run_settings_panel(
            &ctx,
            &[
                ("collection", "c.hurl"),
                ("output", "csv"),
                ("environment", "dev"),
                ("root", "/r"),
                ("baseline", "b.baseline"),
                ("columns", "a,b"),
            ],
            600.0,
        );
        assert!(
            !full.iter().any(|(t, _)| t.contains(s.report_add_setting)),
            "nothing is missing, so nothing offers to add it"
        );
    }

    /// The panel keeps its width whatever it happens to contain: a box that
    /// grew and shrank as dropdowns were used would make the view twitch.
    #[test]
    fn the_settings_panel_keeps_its_width_whatever_it_holds() {
        let ctx = egui::Context::default();
        let (bare, _) = run_settings_panel(&ctx, &[("collection", "c.hurl")], 600.0);
        let (full, _) = run_settings_panel(
            &ctx,
            &[
                ("collection", "c.hurl"),
                ("output", "xlsx"),
                ("environment", "dev"),
                (
                    "root",
                    "/a/very/long/root/path/that/would/otherwise/stretch/things",
                ),
                ("baseline", "/another/rather/long/baseline/path.baseline"),
                ("columns", "name,status,duration,size,assertions"),
            ],
            600.0,
        );
        assert!(
            (bare.width() - full.width()).abs() < 0.5,
            "one setting gives width {} but six give {}",
            bare.width(),
            full.width()
        );

        // It still yields to a column narrower than its fixed width, so the
        // panel never forces a wide editor pane. It can only give back what its
        // contents don't need: a dropdown has a minimum width of its own, which
        // is the floor here, so the test asserts it shrank rather than naming a
        // width the chips can't actually reach.
        let (narrow, _) = run_settings_panel(&ctx, &[("collection", "c.hurl")], 120.0);
        assert!(
            narrow.width() < bare.width(),
            "a 120px column still gave the panel {} (it takes {} when there is room)",
            narrow.width(),
            bare.width()
        );
    }

    /// The key label in a combo-box setting is centred against the combo, not
    /// left floating above it — a label added before the (taller) combo is
    /// otherwise centred in a row that has not grown yet.
    #[test]
    fn a_settings_key_label_is_centred_against_its_dropdown() {
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|s| s.spacing.button_padding = egui::vec2(8.0, 4.0));
        let th = GuiTheme::from_spec(&crate::theme::preset_for_language(&Language::English));
        let strings = Strings::for_language(&Language::English);
        let choices = vec!["dev".to_string()];

        for spec in &header_specs() {
            let mut label = egui::Rect::NOTHING;
            let mut chip = egui::Rect::NOTHING;
            for _ in 0..3 {
                let out = ctx.run_ui(egui::RawInput::default(), |ui| {
                    let mut acts = Vec::new();
                    chip = ui
                        .horizontal(|ui| {
                            header_chip(
                                ui,
                                &th,
                                &strings,
                                spec,
                                "dev",
                                Some(&choices),
                                &collection_choices_fixture,
                                &mut acts,
                            );
                        })
                        .response
                        .rect;
                });
                // The key is painted as a raw galley (see `header_chip`), so it
                // is found among the text shapes rather than as a widget.
                let want = spec.key.to_uppercase();
                label = out
                    .shapes
                    .iter()
                    .find_map(|c| match &c.shape {
                        egui::Shape::Text(t) if t.galley.text() == want => {
                            Some(egui::Rect::from_min_size(t.pos, t.galley.size()))
                        }
                        _ => None,
                    })
                    .unwrap_or_else(|| panic!("{} label was not painted", spec.key));
            }
            assert!(
                (label.center().y - chip.center().y).abs() < 2.0,
                "{}: label centred at {} but the chip at {}",
                spec.key,
                label.center().y,
                chip.center().y
            );
        }
    }

    #[test]
    fn settings_chips_render_at_the_same_height_as_flow_chips() {
        let specs = header_specs();
        let baseline = chip_height(|ui, th, s, acts| {
            let chip = Chip::base("REPORT".into(), th.subst);
            render_chip(ui, th, s, &chip, false, &[0], &[], &[], acts);
        });
        for spec in &specs {
            let choices = vec!["dev".to_string()];
            let h = chip_height(|ui, th, s, acts| {
                header_chip(
                    ui,
                    th,
                    s,
                    spec,
                    "dev",
                    Some(&choices),
                    &collection_choices_fixture,
                    acts,
                );
            });
            assert!(
                (h - baseline).abs() < 1.0,
                "{} chip is {h}, flow chips are {baseline}",
                spec.key
            );
        }
    }

    #[test]
    fn a_picked_path_is_stored_relative_to_the_report_when_it_can_be() {
        let report = std::path::Path::new("/w/reports/daily.paper");
        assert_eq!(
            relative_to_report(std::path::Path::new("/w/reports/out.csv"), Some(report)),
            "out.csv",
            "a sibling file travels with the report"
        );
        assert_eq!(
            relative_to_report(std::path::Path::new("/elsewhere/out.csv"), Some(report)),
            "/elsewhere/out.csv",
            "anything outside the report's folder keeps its absolute path"
        );
        assert_eq!(
            relative_to_report(std::path::Path::new("/w/out.csv"), None),
            "/w/out.csv",
            "an unsaved report has nothing to be relative to"
        );
    }

    #[test]
    fn every_chip_a_block_can_show_carries_hover_help() {
        let th = GuiTheme::from_spec(&crate::theme::preset_for_language(&Language::English));
        let s = Strings::for_language(&Language::English);
        // One source line per block shape the palette can produce, so a chip
        // added later without help text fails here rather than shipping bare.
        let sources = [
            "REQUEST login",
            "REPORT REQUEST login AS Login RESPONSE PRETTY SHOW(Time) HIDE(Error)",
            "REPORT userId",
            "REPORT userId AS Id",
            "REPORT \"{{a}}-{{b}}\" AS Combined",
            "TOKEN = abc",
            "LIST NAMES = [\"a\", \"b\"]",
            "PARALLEL(3) FOR F IN FILES \"/d\"",
            "FOR T IN ENVS BASELINE(\"dev\"), COMPARISON(\"prod\")",
            "FOR T IN ENVS BASELINE(FILE(\"a.baseline\")), COMPARISON(\"prod\", \"uat\")",
        ];
        for src in sources {
            let node = crate::report::edit::parse_one_node(src, true)
                .unwrap_or_else(|| panic!("could not parse {src:?}"));
            for chip in node_chips(&node, None, &th, &s) {
                assert!(
                    !chip.help.is_empty(),
                    "chip {:?} of {src:?} has no hover help",
                    chip.text
                );
            }
        }
    }

    #[test]
    fn a_parallel_loop_shows_an_editable_degree_chip() {
        let th = GuiTheme::from_spec(&crate::theme::preset_for_language(&Language::English));
        let s = Strings::for_language(&Language::English);

        // With an explicit degree the chip carries it; the base chip beside it
        // must not repeat the "PARALLEL(4)" prefix that the head label embeds.
        let node =
            crate::report::edit::parse_one_node("PARALLEL(4) FOR F IN FILES \"/d\"", true).unwrap();
        let chips = node_chips(&node, None, &th, &s);
        assert!(matches!(
            chips[0].edit,
            ChipEdit::Parallel { degree: Some(4) }
        ));
        assert!(
            !chips[1].text.contains("PARALLEL"),
            "base chip repeated the PARALLEL prefix: {:?}",
            chips[1].text
        );

        // A plain PARALLEL has no explicit limit, so the box shows blank rather
        // than inventing the runner's default.
        let node =
            crate::report::edit::parse_one_node("PARALLEL FOR F IN FILES \"/d\"", true).unwrap();
        let chips = node_chips(&node, None, &th, &s);
        assert!(matches!(chips[0].edit, ChipEdit::Parallel { degree: None }));

        // A serial loop gets no PARALLEL chip at all.
        let node = crate::report::edit::parse_one_node("FOR F IN FILES \"/d\"", true).unwrap();
        let chips = node_chips(&node, None, &th, &s);
        assert!(
            !chips
                .iter()
                .any(|c| matches!(c.edit, ChipEdit::Parallel { .. }))
        );
    }

    #[test]
    fn chips_on_a_line_stay_vertically_aligned() {
        // Reproduce `block_row`'s chip cluster (a top-aligned horizontal row of
        // mixed label / combo / text-field chips) and confirm every chip sits at
        // the same vertical position — i.e. the line does not "cascade" each
        // successive chip lower, which the default centre-aligned row does.
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|s| s.spacing.button_padding = egui::vec2(8.0, 4.0));
        let th = GuiTheme::from_spec(&crate::theme::preset_for_language(&Language::English));
        let s = crate::i18n::Strings::for_language(&Language::English);
        let mut tops: Vec<f32> = Vec::new();
        for _ in 0..3 {
            tops.clear();
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
                // The same layout `block_row` uses for the chip cluster.
                ui.horizontal_top(|ui| {
                    let mut acts = Vec::new();
                    let staging = vec!["eapi_staging".to_string()];
                    let dev = vec!["eapi_dev".to_string()];
                    let dfa = vec!["dfa_result".to_string()];
                    let chips: Vec<(Chip, &[String])> = vec![
                        (Chip::base("FOR".into(), th.subst), &[]),
                        (Chip::env_role(true, 0, "eapi_staging", th.subst), &staging),
                        (Chip::env_role(false, 1, "eapi_dev", th.subst), &dev),
                        (
                            Chip::modifier(
                                "RESPONSE PRETTY".into(),
                                th.subst,
                                DetachWhich::Response,
                            ),
                            &[],
                        ),
                        (Chip::request("dfa_result", th.subst), &dfa),
                        (
                            Chip::alias("Environment", th.subst, Some(DetachWhich::As)),
                            &[],
                        ),
                    ];
                    for (chip, envs) in &chips {
                        let r = ui.scope(|ui| {
                            render_chip(ui, &th, &s, chip, false, &[0], envs, envs, &mut acts)
                        });
                        tops.push(r.response.rect.top());
                    }
                });
            });
        }
        let first = tops[0];
        for (i, t) in tops.iter().enumerate() {
            assert!(
                (t - first).abs() < 0.5,
                "chip {i} top {t} drifted from first chip top {first}: {tops:?}"
            );
        }
    }

    #[test]
    fn selecting_a_block_keeps_its_size_and_position() {
        // Selecting a base chip must only recolour it, never resize or move it
        // (a thicker selection stroke used to expand the frame by a pixel and
        // nudge the chip and its neighbours).
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|s| s.spacing.button_padding = egui::vec2(8.0, 4.0));
        let th = GuiTheme::from_spec(&crate::theme::preset_for_language(&Language::English));
        let s = crate::i18n::Strings::for_language(&Language::English);
        let mut unsel = egui::Rect::ZERO;
        let mut sel = egui::Rect::ZERO;
        for _ in 0..3 {
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
                let mut acts = Vec::new();
                let chip = Chip::base("REQUEST".into(), th.subst);
                unsel = ui
                    .horizontal_top(|ui| {
                        render_chip(ui, &th, &s, &chip, false, &[0], &[], &[], &mut acts)
                    })
                    .response
                    .rect;
                sel = ui
                    .horizontal_top(|ui| {
                        render_chip(ui, &th, &s, &chip, true, &[0], &[], &[], &mut acts)
                    })
                    .response
                    .rect;
            });
        }
        assert!(
            (unsel.width() - sel.width()).abs() < 0.5,
            "selected width {} vs unselected {}",
            sel.width(),
            unsel.width()
        );
        assert!(
            (unsel.height() - sel.height()).abs() < 0.5,
            "selected height {} vs unselected {}",
            sel.height(),
            unsel.height()
        );
    }

    #[test]
    fn pretty_json_cell_reflows_json_documents_only() {
        // A JSON object/array is indented one-field-per-line.
        let pretty = pretty_json_cell(r#"{"a":1,"b":[2,3]}"#);
        assert!(pretty.contains('\n'), "object should be reflowed: {pretty}");
        assert!(pretty.contains("\"a\": 1"));
        // A bare scalar / plain string is left exactly as-is.
        assert_eq!(pretty_json_cell("42"), "42");
        assert_eq!(pretty_json_cell("just text"), "just text");
        // Invalid JSON that merely starts like an object is returned unchanged.
        assert_eq!(pretty_json_cell("{not json"), "{not json");
    }

    /// The floating-drag lift only applies to a whole-row (`DragItem::Row`)
    /// drag, and reports exactly the path being dragged; a modifier-chip drag
    /// (`DragItem::Chip`) must not lift a row.
    #[test]
    fn dragged_row_path_tracks_only_row_drags() {
        let ctx = egui::Context::default();
        assert_eq!(dragged_row_path(&ctx), None, "no drag → no lifted row");

        egui::DragAndDrop::set_payload(&ctx, DragItem::Row(vec![1, 2]));
        assert_eq!(dragged_row_path(&ctx), Some(vec![1, 2]));

        egui::DragAndDrop::set_payload(
            &ctx,
            DragItem::Chip {
                path: vec![0],
                which: DetachWhich::Report,
            },
        );
        assert_eq!(
            dragged_row_path(&ctx),
            None,
            "a modifier-chip drag must not lift a row"
        );
    }

    /// The mirror of the above: a chip drag lifts exactly that chip (so it can
    /// float under the pointer and leave a ghost) and a row drag lifts none, so
    /// the two lifts can never both fire for one drag.
    /// A statistics clause present in the source has to be visible as a chip,
    /// and the load-bearing rule has to leave the row's REPORT chip undraggable
    /// on its own.
    #[test]
    fn a_named_column_shows_its_statistics_and_keeps_report_attached() {
        let th = GuiTheme::from_spec(&crate::theme::preset_for_language(&Language::English));
        let s = crate::i18n::Strings::english();
        let flow = crate::report::parse_flow("REPORT TIER AS Plan STATISTICS(MEAN)\n")
            .expect("fixture parses");
        let chips = node_chips(&flow.nodes[0], None, &th, s);
        assert!(
            chips.iter().any(|c| c.text.contains("STATISTICS(MEAN)")),
            "the statistics clause is drawn: {:?}",
            chips.iter().map(|c| &c.text).collect::<Vec<_>>()
        );
        let report = chips
            .iter()
            .find(|c| c.text == "REPORT")
            .expect("the REPORT chip is drawn");
        assert!(
            report.detach.is_none(),
            "REPORT is load-bearing here, so grabbing it moves the whole row"
        );
    }

    /// The drop preview is computed by rehearsing the drop, so the ghost has to
    /// be the very chip that appears — same text, same slot — once it lands.
    #[test]
    fn the_drop_preview_is_exactly_the_chip_the_drop_will_add() {
        let th = GuiTheme::from_spec(&crate::theme::preset_for_language(&Language::English));
        let s = crate::i18n::Strings::english();
        let flow = crate::report::parse_flow("REPORT REQUEST A\n").expect("fixture parses");
        let node = &flow.nodes[0];

        let pending = PendingMod::New(Modifier::Show);
        let (idx, text, _) =
            preview_chip(node, &pending, Some(true), &th, s).expect("SHOW adds a chip");

        let before = node_chips(node, Some(true), &th, s);
        assert_eq!(idx, before.len(), "SHOW appends to the end of the line");

        let mut after_node = node.clone();
        assert!(pending.apply(&mut after_node));
        assert_eq!(
            node_chips(&after_node, Some(true), &th, s)[idx]
                .ghost_shape()
                .0,
            text,
            "the ghost's label is the landed chip's label, so the gap is its size"
        );
    }

    /// Every block a user cannot compose out of something else has to be in the
    /// palette, or the GUI simply can't write that statement.
    #[test]
    fn the_palette_offers_every_block_that_cannot_be_composed() {
        for kind in NodeKind::ALL {
            // A reported request is `REQUEST` plus the `REPORT` modifier, so it
            // is the one kind the palette deliberately leaves out.
            if matches!(kind, NodeKind::ReportRequest) {
                assert!(
                    !BASE_KINDS.contains(&kind),
                    "{kind:?} is composed from REQUEST + REPORT, not offered directly"
                );
                continue;
            }
            assert!(
                BASE_KINDS.contains(&kind),
                "{kind:?} has no other route into a report, so the palette must offer it"
            );
        }
    }

    /// A chip that keeps its keyword in its inline editor rather than in `text`
    /// still has to produce a full-sized ghost — `PARALLEL` came out a sliver.
    #[test]
    fn a_ghost_is_sized_from_what_the_chip_really_draws() {
        let th = GuiTheme::from_spec(&crate::theme::preset_for_language(&Language::English));
        let s = crate::i18n::Strings::english();
        let flow = crate::report::parse_flow("FOR X IN FILES \"/a\"\n    REQUEST A\nEND\n")
            .expect("fixture parses");

        let (_, text, extra) = preview_chip(
            &flow.nodes[0],
            &PendingMod::New(Modifier::Parallel),
            None,
            &th,
            s,
        )
        .expect("PARALLEL adds a chip");
        assert_eq!(
            text, "PARALLEL",
            "the keyword is in the ghost, not an empty box"
        );
        assert!(
            extra >= PARALLEL_FIELD_WIDTH,
            "the concurrency box's width is reserved too, got {extra}"
        );
    }

    /// A clause the line won't take gets no gap — the row must not open space
    /// for a drop that is about to be refused.
    #[test]
    fn a_refused_drop_previews_nothing() {
        let th = GuiTheme::from_spec(&crate::theme::preset_for_language(&Language::English));
        let s = crate::i18n::Strings::english();
        let flow =
            crate::report::parse_flow("REPORT REQUEST A SHOW(Time)\n").expect("fixture parses");
        let carried = crate::report::edit::carry_modifier(&flow.nodes[0], DetachWhich::Show)
            .expect("the SHOW is there to pick up");
        assert!(
            preview_chip(
                &flow.nodes[0],
                &PendingMod::Moved(carried),
                Some(true),
                &th,
                s
            )
            .is_none(),
            "a request that already has SHOW opens no gap for another"
        );
    }

    #[test]
    fn dragged_chip_tracks_only_chip_drags() {
        let ctx = egui::Context::default();
        assert_eq!(dragged_chip(&ctx), None, "no drag → no lifted chip");

        egui::DragAndDrop::set_payload(
            &ctx,
            DragItem::Chip {
                path: vec![0, 3],
                which: DetachWhich::BaselineShow,
            },
        );
        assert_eq!(
            dragged_chip(&ctx),
            Some((vec![0, 3], DetachWhich::BaselineShow)),
            "the picked-up chip is identified by its row and its clause"
        );

        egui::DragAndDrop::set_payload(&ctx, DragItem::Row(vec![0, 3]));
        assert_eq!(
            dragged_chip(&ctx),
            None,
            "a whole-line drag must not also lift a chip out of that line"
        );
    }

    #[test]
    fn chip_drag_payload_detaches_plainly_and_moves_the_line_with_ctrl() {
        let ctx = egui::Context::default();
        let base = Chip::base("REQUEST x".into(), egui::Color32::WHITE);
        let modi = Chip::modifier("SHOW(Time)".into(), egui::Color32::WHITE, DetachWhich::Show);
        let path = vec![1usize];

        // Plain drag: the base chip moves the whole line, a modifier chip is
        // picked up on its own to detach it.
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            assert!(matches!(
                chip_drag_payload(ui, &base, &path),
                DragItem::Row(p) if p == path
            ));
            assert!(matches!(
                chip_drag_payload(ui, &modi, &path),
                DragItem::Chip {
                    which: DetachWhich::Show,
                    ..
                }
            ));
        });

        // Ctrl/Cmd held: *any* chip — even a detachable modifier — moves the
        // whole line/subtree instead of detaching.
        let ctrl = egui::RawInput {
            modifiers: egui::Modifiers {
                ctrl: true,
                command: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let _ = ctx.run_ui(ctrl, |ui| {
            assert!(matches!(
                chip_drag_payload(ui, &modi, &path),
                DragItem::Row(p) if p == path
            ));
            assert!(matches!(
                chip_drag_payload(ui, &base, &path),
                DragItem::Row(p) if p == path
            ));
        });
    }

    #[test]
    fn clause_chips_open_the_wizard_on_click() {
        assert!(chip_opens_wizard_on_click(&Chip::modifier(
            "SHOW".into(),
            egui::Color32::WHITE,
            DetachWhich::Show
        )));
        assert!(chip_opens_wizard_on_click(&Chip::modifier(
            "HIDE".into(),
            egui::Color32::WHITE,
            DetachWhich::Hide
        )));
        assert!(chip_opens_wizard_on_click(&Chip::modifier(
            "RESPONSE".into(),
            egui::Color32::WHITE,
            DetachWhich::Response
        )));
        // An ENVS loop's BASELINE SHOW is picked the same way, in the ENVS
        // wizard — its chip has to open too, or its checklist is unreachable.
        assert!(chip_opens_wizard_on_click(&Chip::modifier(
            "SHOW(Time)".into(),
            egui::Color32::WHITE,
            DetachWhich::BaselineShow
        )));
        // REPORT / WITH / plain base chips are not opened this way.
        assert!(!chip_opens_wizard_on_click(&Chip::modifier(
            "REPORT".into(),
            egui::Color32::WHITE,
            DetachWhich::Report
        )));
        assert!(!chip_opens_wizard_on_click(&Chip::base(
            "x".into(),
            egui::Color32::WHITE
        )));
    }

    #[test]
    fn lifted_subtree_covers_a_loop_body_but_not_siblings() {
        // Dragging a leaf lifts only that leaf.
        assert!(row_is_lifted(&[2], &[2]));
        assert!(!row_is_lifted(&[2], &[1]));
        assert!(!row_is_lifted(&[2], &[3]));

        // Dragging a FOR loop at [2] lifts its head ([2]), every body row
        // ([2,0], [2,1], …) and its synthetic END (path == [2]) — but never a
        // sibling ([3]) or a cousin under a different loop ([1,0]).
        assert!(row_is_lifted(&[2], &[2, 0]), "loop body row is lifted");
        assert!(row_is_lifted(&[2], &[2, 1, 0]), "nested body row is lifted");
        assert!(!row_is_lifted(&[2], &[3]), "the next sibling stays put");
        assert!(
            !row_is_lifted(&[2], &[1, 0]),
            "another loop's body stays put"
        );
        // A path that merely shares a leading digit is not a descendant.
        assert!(
            !row_is_lifted(&[2], &[20]),
            "prefix is index-wise, not textual"
        );
    }

    /// The hover highlight must promise exactly what a drag delivers, so it is
    /// keyed off the same subtree rule — but split into two tiers so "this
    /// block" and "and this comes with it" read differently.
    #[test]
    fn hover_lights_the_block_strongly_and_its_body_softly() {
        // A leaf lights only itself.
        assert_eq!(hover_tier(&[2], &[2]), Some(HoverTier::Block));
        assert_eq!(hover_tier(&[2], &[1]), None);
        assert_eq!(hover_tier(&[2], &[3]), None);

        // A loop's body and its nested rows come along, one tier down.
        assert_eq!(hover_tier(&[2], &[2, 0]), Some(HoverTier::CarriedAlong));
        assert_eq!(hover_tier(&[2], &[2, 1, 0]), Some(HoverTier::CarriedAlong));
        assert_eq!(hover_tier(&[2], &[1, 0]), None);
        assert_eq!(hover_tier(&[2], &[20]), None);

        // Hovering a row *inside* a loop lights that row, not the loop around
        // it: dragging it out takes only itself.
        assert_eq!(hover_tier(&[2, 1], &[2, 1]), Some(HoverTier::Block));
        assert_eq!(hover_tier(&[2, 1], &[2]), None);
        assert_eq!(hover_tier(&[2, 1], &[2, 0]), None);

        // Every row the highlight touches is a row the drag would lift, and
        // vice versa — the two must never disagree.
        for path in [vec![2], vec![2, 0], vec![2, 1, 0], vec![3], vec![1, 0]] {
            assert_eq!(
                hover_tier(&[2], &path).is_some(),
                row_is_lifted(&[2], &path),
                "hover and lift disagree about {path:?}"
            );
        }
    }

    /// The block under the pointer is the innermost one: a `FOR` header and a
    /// row in its body can both contain the pointer, and the body row is the one
    /// the user is pointing at.
    #[test]
    fn hovered_block_prefers_the_innermost_row_and_ignores_begin() {
        fn row(path: &[usize], kind: RowKind, inside: bool) -> RowHover {
            RowHover {
                path: path.to_vec(),
                kind,
                rect: egui::Rect::ZERO,
                bg: egui::layers::ShapeIdx(0),
                pointer_inside: inside,
            }
        }
        let rows = vec![
            row(&[], RowKind::Begin, true),
            row(&[0], RowKind::LoopHead, true),
            row(&[0, 0], RowKind::Leaf, true),
            row(&[0], RowKind::LoopEnd, false),
        ];
        assert_eq!(hovered_block(&rows), Some([0, 0].as_slice()));

        // Begin is never the answer, even when it is the only row the pointer is
        // over — its empty path would light the whole report.
        let only_begin = vec![
            row(&[], RowKind::Begin, true),
            row(&[0], RowKind::Leaf, false),
        ];
        assert_eq!(hovered_block(&only_begin), None);

        // A loop's END stands in for the loop, so pointing at it lights the
        // whole block.
        let on_end = vec![
            row(&[0], RowKind::LoopHead, false),
            row(&[0, 0], RowKind::Leaf, false),
            row(&[0], RowKind::LoopEnd, true),
        ];
        assert_eq!(hovered_block(&on_end), Some([0].as_slice()));
        assert_eq!(hover_tier(&[0], &[0]), Some(HoverTier::Block));
    }

    #[test]
    fn origin_ghost_paints_without_panicking() {
        let ctx = egui::Context::default();
        let th = GuiTheme::from_spec(&crate::theme::preset_for_language(&Language::English));
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            // A normal slot draws a ghost; a degenerate rect is a safe no-op.
            paint_origin_ghost(
                ui.painter(),
                egui::Rect::from_min_size(egui::pos2(4.0, 4.0), egui::vec2(80.0, 22.0)),
                &th,
            );
            paint_origin_ghost(ui.painter(), egui::Rect::ZERO, &th);
        });
    }

    /// The dashed outline marking where a block was lifted from has to trace the
    /// block, not the whole editor width: a nested block sits in from the left by
    /// its indent, and the outline has to start there too.
    #[test]
    fn the_origin_outline_starts_at_the_blocks_own_indent() {
        // A row's laid-out rect always begins at the editor's left margin,
        // because the indent is `add_space`d inside the row's own layout.
        let row = egui::Rect::from_min_size(egui::pos2(4.0, 40.0), egui::vec2(300.0, 22.0));

        let top = indented_content(row, 0);
        assert_eq!(top, row, "an unindented block is its own rect");

        let nested = indented_content(row, 2);
        assert_eq!(
            nested.left(),
            row.left() + 2.0 * INDENT_STEP,
            "the outline starts where the nested block does"
        );
        assert_eq!(nested.right(), row.right(), "the right edge is untouched");
        assert_eq!(nested.y_range(), row.y_range(), "the height is untouched");

        // A row narrower than its own indent (only reachable from a degenerate
        // layout pass) must not invert into a negative-width rect.
        let tiny = indented_content(
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(10.0, 22.0)),
            8,
        );
        assert!(tiny.width() > 0.0, "the outline never collapses: {tiny:?}");
    }

    /// The outline is as round as the block it replaces — dashed along a rounded
    /// path rather than round the four straight edges, which left square corners
    /// inside a rounded fill.
    #[test]
    fn the_origin_outline_is_as_rounded_as_a_block() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 40.0));
        let path = rounded_rect_path(rect, BLOCK_RADIUS);
        assert!(path.len() > 8, "a rounded path is more than four corners");
        assert_eq!(path.first(), path.last(), "the path closes back on itself");
        for p in &path {
            assert!(
                rect.expand(0.01).contains(*p),
                "{p:?} escapes the rect it traces"
            );
        }
        // The true corners are cut off: nothing sits within the radius square of
        // a corner except along its arc, so no point lands *on* the corner.
        for corner in [
            rect.left_top(),
            rect.right_top(),
            rect.right_bottom(),
            rect.left_bottom(),
        ] {
            let nearest = path
                .iter()
                .map(|p| (*p - corner).length())
                .fold(f32::MAX, f32::min);
            assert!(
                nearest > BLOCK_RADIUS * 0.3,
                "the path reaches the square corner {corner:?} (nearest {nearest})"
            );
        }
        // A radius larger than the box can hold is clamped rather than folding
        // the path inside out.
        let thin = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 4.0));
        for p in rounded_rect_path(thin, BLOCK_RADIUS) {
            assert!(thin.expand(0.01).contains(p), "{p:?} escapes a thin rect");
        }
    }

    /// The drop marker is a preview of the block that will land, so it has the
    /// block's width — not a bar running to the right-hand edge of the editor.
    #[test]
    fn the_drop_marker_is_as_wide_as_the_block_being_dropped() {
        let measure = |setup: &dyn Fn(&egui::Context)| -> egui::Vec2 {
            let ctx = egui::Context::default();
            ctx.all_styles_mut(|s| s.spacing.button_padding = egui::vec2(8.0, 4.0));
            let mut size = egui::Vec2::ZERO;
            for _ in 0..2 {
                setup(&ctx);
                let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
                    size = dragged_block_size(ui)
                });
            }
            size
        };

        // An existing block reports the width the lift measured.
        let moved = measure(&|ctx| {
            egui::DragAndDrop::set_payload(ctx, DragItem::Row(vec![0]));
            ctx.data_mut(|d| {
                d.insert_temp(
                    lifted_shape_id(),
                    vec![egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(214.0, 60.0),
                    )],
                )
            });
        });
        assert_eq!(moved, egui::vec2(214.0, 60.0));

        // A palette block has nothing laid out in the flow to measure, so it
        // reports the width of the chip actually in hand.
        let from_palette = measure(&|ctx| {
            egui::DragAndDrop::set_payload(ctx, NodeKind::Request);
            ctx.data_mut(|d| d.insert_temp(palette_drag_size_id(), egui::vec2(96.0, 24.0)));
        });
        assert_eq!(from_palette.x, 96.0, "the palette chip's own width");

        // With nothing measured yet (a drag's very first frame) the marker still
        // has a sane block-like width rather than zero or the full editor.
        let unmeasured = measure(&|ctx| egui::DragAndDrop::set_payload(ctx, NodeKind::Request));
        assert!(
            unmeasured.x > 40.0 && unmeasured.x < 400.0,
            "unmeasured width {} is block-like",
            unmeasured.x
        );
    }

    /// A dragged `FOR` loop is not a rectangle — short head, indented body,
    /// short `END` — and the drop marker has to say so, otherwise it promises a
    /// solid slab nothing like the block in hand.
    #[test]
    fn the_drop_marker_takes_the_shape_of_the_block_being_dropped() {
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|s| s.spacing.button_padding = egui::vec2(8.0, 4.0));
        let th = GuiTheme::from_spec(&crate::theme::preset_for_language(&Language::English));
        // A loop's silhouette, relative to its own top-left.
        let shape = vec![
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(140.0, 24.0)),
            egui::Rect::from_min_size(egui::pos2(24.0, 24.0), egui::vec2(180.0, 24.0)),
            egui::Rect::from_min_size(egui::pos2(0.0, 48.0), egui::vec2(50.0, 24.0)),
        ];
        let origin = egui::pos2(30.0, 100.0);
        let out = ctx.run_ui(egui::RawInput::default(), |ui| {
            paint_drop_silhouette(
                ui,
                origin,
                &shape,
                egui::Rect::from_min_size(origin, egui::vec2(400.0, 72.0)),
                &th,
            );
        });

        let painted: Vec<egui::Rect> = out
            .shapes
            .iter()
            .filter_map(|c| match &c.shape {
                egui::Shape::Rect(r) => Some(r.rect),
                _ => None,
            })
            .collect();
        assert_eq!(painted.len(), 3, "one rect per row, not one bounding box");
        for (drawn, want) in painted.iter().zip(&shape) {
            assert_eq!(
                drawn.left(),
                origin.x + want.left(),
                "the row keeps its own indent"
            );
            assert_eq!(drawn.width(), want.width(), "the row keeps its own width");
        }
        // The stepped outline really is stepped: the three rows differ.
        assert_ne!(painted[0].left(), painted[1].left());
        assert_ne!(painted[0].width(), painted[2].width());
        // A bounding-box marker would have covered this corner; the silhouette
        // does not.
        let under_the_end = egui::pos2(origin.x + 150.0, origin.y + 60.0);
        assert!(
            !painted.iter().any(|r| r.contains(under_the_end)),
            "the marker filled in a gap the block does not occupy"
        );
    }

    /// While the gap animates open the marker is *revealed*, never stretched —
    /// a block that grew from a sliver to full height would read as the block
    /// changing size on the way in.
    #[test]
    fn a_half_open_drop_marker_is_clipped_not_stretched() {
        let ctx = egui::Context::default();
        let th = GuiTheme::from_spec(&crate::theme::preset_for_language(&Language::English));
        let shape = vec![
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(140.0, 24.0)),
            egui::Rect::from_min_size(egui::pos2(0.0, 24.0), egui::vec2(140.0, 24.0)),
        ];
        let origin = egui::pos2(0.0, 0.0);
        let clipped = |h: f32| -> Vec<(egui::Rect, egui::Rect)> {
            ctx.run_ui(egui::RawInput::default(), |ui| {
                paint_drop_silhouette(
                    ui,
                    origin,
                    &shape,
                    egui::Rect::from_min_size(origin, egui::vec2(400.0, h)),
                    &th,
                );
            })
            .shapes
            .iter()
            .filter_map(|c| match &c.shape {
                egui::Shape::Rect(r) => Some((r.rect, c.clip_rect)),
                _ => None,
            })
            .collect()
        };

        let full = clipped(48.0);
        let half = clipped(20.0);
        assert_eq!(full.len(), half.len(), "the same rows are always emitted");
        for (a, b) in full.iter().zip(&half) {
            assert_eq!(a.0, b.0, "the row's own geometry never changes");
        }
        assert!(
            half[0].1.height() < full[0].1.height(),
            "the half-open marker is held back by a shorter clip"
        );
    }

    /// A raw input with a pointer sitting at `pos` and a real screen rect, so
    /// `Context::pointer_interact_pos` reports something during the test frame.
    fn input_with_pointer(pos: egui::Pos2) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000.0, 800.0),
            )),
            events: vec![egui::Event::PointerMoved(pos)],
            ..Default::default()
        }
    }

    /// Every part of a picked-up subtree must move by the *same* delta.
    ///
    /// Regression test for dragging a whole `FOR` loop: the transform used to be
    /// applied from inside the head row, and `Context::transform_layer_shapes`
    /// only moves the shapes *already in* the layer — so the loop's body and its
    /// `END`, painted after the head, stayed at their layout positions while the
    /// head alone tracked the cursor, making the parts of one block appear to
    /// move at different speeds.
    #[test]
    fn a_dragged_subtree_moves_every_row_by_the_same_delta() {
        let ctx = egui::Context::default();
        let head_rect = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(120.0, 20.0));
        let body_rect = egui::Rect::from_min_size(egui::pos2(10.0, 34.0), egui::vec2(120.0, 20.0));
        let end_rect = egui::Rect::from_min_size(egui::pos2(10.0, 58.0), egui::vec2(120.0, 20.0));
        let fills = [
            Color32::from_rgb(1, 2, 3),
            Color32::from_rgb(4, 5, 6),
            Color32::from_rgb(7, 8, 9),
        ];
        let rects = [head_rect, body_rect, end_rect];
        let pointer = egui::pos2(400.0, 300.0);

        // Two passes: egui settles its layout/ids on the first frame.
        let mut out = ctx.run_ui(input_with_pointer(pointer), |_| {});
        for _ in 0..2 {
            out = ctx.run_ui(input_with_pointer(pointer), |ui| {
                let layer = egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("pt_test_drag"));
                let mut lift = DragLift::default();
                // Exactly what `block_row` does for a lifted row: paint it into
                // the shared drag layer, then hand it to the lift.
                for (i, (rect, fill)) in rects.iter().zip(fills).enumerate() {
                    ui.scope_builder(egui::UiBuilder::new().layer_id(layer), |ui| {
                        ui.painter()
                            .rect_filled(*rect, egui::CornerRadius::ZERO, fill);
                    });
                    lift.add(layer, *rect, i == 0);
                }
                lift.follow_pointer(ui.ctx());
            });
        }

        let painted = |fill: Color32| -> egui::Rect {
            out.shapes
                .iter()
                .find_map(|clipped| match &clipped.shape {
                    egui::Shape::Rect(r) if r.fill == fill => Some(r.rect),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("no rect painted with fill {fill:?}"))
        };

        let deltas: Vec<egui::Vec2> = rects
            .iter()
            .zip(fills)
            .map(|(rect, fill)| painted(fill).min - rect.min)
            .collect();
        for (i, delta) in deltas.iter().enumerate() {
            assert!(
                (*delta - deltas[0]).length() < 0.5,
                "row {i} moved by {delta:?}, head moved by {:?}",
                deltas[0]
            );
        }
        // The subtree really did move (a no-op transform would trivially pass
        // the equal-delta check above), anchored with the head on the pointer.
        assert!(deltas[0].length() > 1.0, "the subtree never moved");
        assert!(
            (painted(fills[0]).center() - pointer).length() < 0.5,
            "the head is not centred on the pointer"
        );
    }

    /// The lift measures the whole subtree, not just its head, and keeps each
    /// row separately — that silhouette is what the drop markers take the shape
    /// of.
    #[test]
    fn the_lift_records_the_whole_subtrees_silhouette_for_the_drop_marker() {
        let ctx = egui::Context::default();
        // A `FOR` loop's real shape: a head, an indented (and here narrower)
        // body row, then the `END` back at the head's indent.
        let rects = [
            egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(120.0, 20.0)),
            egui::Rect::from_min_size(egui::pos2(20.0, 34.0), egui::vec2(90.0, 20.0)),
            egui::Rect::from_min_size(egui::pos2(10.0, 58.0), egui::vec2(60.0, 20.0)),
        ];
        let layer = egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("pt_test_measure"));
        let mut lift = DragLift::default();
        for (i, rect) in rects.iter().enumerate() {
            lift.add(layer, *rect, i == 0);
        }
        lift.follow_pointer(&ctx);
        let stored: Vec<egui::Rect> = ctx
            .data(|d| d.get_temp(lifted_shape_id()))
            .expect("the lift stashed a silhouette");
        // One rect per row, positioned relative to the subtree's own top-left,
        // so the whole thing spans 0.0 → 68.0 rather than the head's 20.0.
        assert_eq!(stored.len(), rects.len());
        assert_eq!(stored[0].min, egui::pos2(0.0, 0.0));
        assert_eq!(stored.last().unwrap().bottom(), 68.0);
        // The body row keeps its own indent and width — that stepped outline is
        // the whole point of storing rows rather than a bounding box.
        assert_eq!(stored[1].min, egui::pos2(10.0, 24.0));
        assert_eq!(stored[1].width(), 90.0);

        // And with nothing in hand the measurement is forgotten, so the next
        // drag's first frame can't inherit this (bigger) block's shape.
        DragLift::default().follow_pointer(&ctx);
        assert_eq!(
            ctx.data(|d| d.get_temp::<Vec<egui::Rect>>(lifted_shape_id())),
            None
        );
    }

    /// The drop gap is sized to the block actually being dragged: one row for a
    /// plain palette block, two for a `FOR` (which inserts a head *and* its
    /// `END`), and the measured subtree height when an existing block is moved.
    #[test]
    fn the_drop_ghost_is_sized_to_the_block_being_dragged() {
        let measure = |setup: &dyn Fn(&egui::Context)| -> f32 {
            let ctx = egui::Context::default();
            ctx.all_styles_mut(|s| s.spacing.button_padding = egui::vec2(8.0, 4.0));
            let mut h = 0.0;
            for _ in 0..2 {
                setup(&ctx);
                let _ = ctx.run_ui(egui::RawInput::default(), |ui| h = dragged_block_h(ui));
            }
            h
        };

        let idle = measure(&|_| {});
        let leaf = measure(&|ctx| egui::DragAndDrop::set_payload(ctx, NodeKind::Request));
        let loop_kind = measure(&|ctx| egui::DragAndDrop::set_payload(ctx, NodeKind::ForFiles));
        let moved_subtree = measure(&|ctx| {
            egui::DragAndDrop::set_payload(ctx, DragItem::Row(vec![0]));
            ctx.data_mut(|d| {
                d.insert_temp(
                    lifted_shape_id(),
                    vec![egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(120.0, 137.0),
                    )],
                )
            });
        });

        assert!(idle > 0.0);
        assert_eq!(leaf, idle, "a plain palette block opens a one-row gap");
        assert_eq!(
            loop_kind,
            idle * 2.0,
            "a FOR block inserts a head and an END, so its gap is two rows"
        );
        assert_eq!(
            moved_subtree, 137.0,
            "an existing block's gap matches its measured height"
        );
    }

    /// A measured height smaller than a single block (a stale/degenerate
    /// reading) never shrinks the ghost below one row.
    #[test]
    fn a_degenerate_measurement_never_shrinks_the_drop_ghost_below_one_block() {
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|s| s.spacing.button_padding = egui::vec2(8.0, 4.0));
        let mut h = 0.0;
        let mut one = 0.0;
        for _ in 0..2 {
            egui::DragAndDrop::set_payload(&ctx, DragItem::Row(vec![0]));
            ctx.data_mut(|d| {
                d.insert_temp(
                    lifted_shape_id(),
                    vec![egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(120.0, 1.0),
                    )],
                )
            });
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
                h = dragged_block_h(ui);
                one = chip_h(ui) + 10.0;
            });
        }
        assert_eq!(h, one);
    }
}

/// The `FOR` loop head's inline editors: which parts of a loop the chip breaks
/// out into boxes, and which it leaves as plain text for the wizard.
#[cfg(test)]
mod loop_chip_tests {
    use super::{ChipEdit, LoopEdit, node_chips};
    use crate::gui::theme::GuiTheme;
    use crate::i18n::{Language, Strings};

    /// The `LoopEdit` the first node of `src` builds its head chip from.
    fn loop_edit(src: &str) -> LoopEdit {
        let flow = crate::report::parser::parse_flow(src).expect("the fixture flow parses");
        let th = GuiTheme::from_spec(&crate::theme::preset_for_language(&Language::English));
        let s = Strings::for_language(&Language::English);
        node_chips(&flow.nodes[0], None, &th, &s)
            .into_iter()
            .find_map(|c| match c.edit {
                ChipEdit::Loop(l) => Some(l),
                _ => None,
            })
            .expect("a loop renders a loop-head chip")
    }

    /// The commonest loop: every part of it is a box.
    #[test]
    fn a_files_loop_offers_boxes_for_its_variable_folder_and_glob() {
        let l = loop_edit("FOR doc IN FILES \"cases\" MATCH \"*.json\"\n  REQUEST A\nEND\n");
        assert_eq!(l.var.as_deref(), Some("doc"));
        assert_eq!(l.keyword, "IN FILES");
        assert_eq!(
            l.dir,
            Some(("cases".to_string(), false)),
            "the folder is editable and picked with a *folder* dialog"
        );
        assert_eq!(l.glob.as_deref(), Some("*.json"));
        assert!(l.tail.is_empty(), "nothing is left over as dead text");
    }

    /// A `FILES` loop with no `MATCH` still offers the glob box: an empty box
    /// invites the glob, where nothing at all hides that the loop can be
    /// narrowed at all.
    #[test]
    fn a_files_loop_without_a_match_still_offers_an_empty_glob_box() {
        let l = loop_edit("FOR f IN FILES \"cases\"\n  REQUEST A\nEND\n");
        assert_eq!(l.glob.as_deref(), Some(""));
    }

    /// `TUPLES FROM` names a file, not a folder, so its picker must be a file
    /// dialog — a folder picker could not select `rows.csv` at all.
    #[test]
    fn a_tuples_loop_picks_a_file_rather_than_a_folder() {
        let l = loop_edit("FOR row IN TUPLES FROM \"rows.csv\"\n  REQUEST A\nEND\n");
        assert_eq!(l.keyword, "IN TUPLES FROM");
        assert_eq!(l.dir, Some(("rows.csv".to_string(), true)));
        assert_eq!(l.glob, None, "a tuples loop has no glob to offer");
    }

    /// A `FOLDERS … WITH role="glob"` list is several named globs rather than
    /// one, so it stays with the wizard and is shown as plain text. Its single
    /// `MATCH` glob, however, is the same one box `FILES` gets -- offered empty
    /// when the loop has none, so the way to narrow the walk is visible.
    #[test]
    fn a_folders_loops_role_list_is_left_as_text_beside_its_editable_folder() {
        let l = loop_edit("FOR d IN FOLDERS \"envs\" WITH req=\"*.hurl\"\n  REQUEST A\nEND\n");
        assert_eq!(l.dir, Some(("envs".to_string(), false)));
        assert_eq!(l.glob, Some(String::new()));
        assert_eq!(l.tail, "WITH req=\"*.hurl\"");
    }

    /// A recursive, filtered `FOLDERS` walk: the `MATCH` glob fills the same box
    /// `FILES` uses, and an optional role keeps its `?` in the tail so the chip
    /// reads back exactly what the file says.
    #[test]
    fn a_folders_loop_shows_its_match_glob_in_the_glob_box() {
        let l = loop_edit(
            "FOR d IN FOLDERS \"cases\" MATCH \"**/case_*\" WITH front=\"*f.jpg\", back=\"*b.jpg\"?\n  REQUEST A\nEND\n",
        );
        assert_eq!(l.glob, Some("**/case_*".to_string()));
        assert_eq!(l.tail, "WITH front=\"*f.jpg\", back=\"*b.jpg\"?");
    }

    /// A pattern that binds more than one name has no single thing a box could
    /// rename, so no box is offered -- rather than one that silently renamed
    /// whichever binder came first.
    #[test]
    fn a_destructuring_loop_offers_no_variable_box() {
        let l = loop_edit("FOR (NAME, URL) IN DOCS\n  REQUEST A\nEND\n");
        assert_eq!(l.var, None);
        assert_eq!(l.tail, "DOCS", "the source is still shown");
    }

    /// A producer with no single path keeps the text it always had, so nothing
    /// disappears from the head just because it can't be boxed.
    #[test]
    fn a_list_literal_keeps_its_text_and_offers_no_folder_picker() {
        let l = loop_edit("FOR x IN [\"a\", \"b\"]\n  REQUEST A\nEND\n");
        assert_eq!(l.var.as_deref(), Some("x"));
        assert_eq!(l.keyword, "IN");
        assert_eq!(l.dir, None, "there is no one folder to pick");
        assert_eq!(l.tail, "[\"a\", \"b\"]");
    }

    /// An ENVS loop's variable is editable, but its roles are chips of their
    /// own beside the head, so the head carries no tail for them.
    #[test]
    fn an_envs_loop_offers_its_variable_box_and_leaves_the_roles_to_their_chips() {
        let l = loop_edit(
            "FOR TARGET IN ENVS BASELINE(\"prod\"), COMPARISON(\"stage\")\n  REQUEST A\nEND\n",
        );
        assert_eq!(l.var.as_deref(), Some("TARGET"));
        assert_eq!(l.keyword, "IN ENVS");
        assert_eq!(l.dir, None);
        assert!(
            l.tail.is_empty(),
            "BASELINE/COMPARISON are chips, not text on the head"
        );
    }

    /// A plain list of environments has no chips of its own, so it must stay
    /// visible on the head rather than vanishing.
    #[test]
    fn an_envs_loop_over_named_environments_still_shows_them() {
        let l = loop_edit("FOR e IN ENVS \"dev\", \"prod\"\n  REQUEST A\nEND\n");
        assert_eq!(l.var.as_deref(), Some("e"));
        assert_eq!(l.tail, "\"dev\", \"prod\"");
    }

    /// A `PARALLEL` loop's head is built from the node, not from its label, so
    /// the prefix belongs to the PARALLEL chip alone and is never repeated.
    #[test]
    fn a_parallel_loops_head_does_not_repeat_the_parallel_prefix() {
        let l = loop_edit("PARALLEL(4) FOR f IN FILES \"cases\"\n  REQUEST A\nEND\n");
        assert_eq!(l.var.as_deref(), Some("f"));
        assert_eq!(l.keyword, "IN FILES");
        assert!(!l.tail.contains("PARALLEL"));
    }
}

#[cfg(test)]
mod baseline_show_chip_tests {
    use super::{
        CHIP_RADIUS, Chip, Color32, DetachWhich, ROUND_CHIP, chip_corners, link_tethers,
        node_chips, split_tether,
    };
    use crate::gui::theme::GuiTheme;
    use crate::i18n::{Language, Strings};

    /// The chip labels a flow's first node renders as.
    fn chip_labels(src: &str) -> Vec<String> {
        let flow = crate::report::parser::parse_flow(src).expect("the fixture flow parses");
        let th = GuiTheme::from_spec(&crate::theme::preset_for_language(&Language::English));
        let s = Strings::for_language(&Language::English);
        node_chips(&flow.nodes[0], None, &th, &s)
            .into_iter()
            .map(|c| c.text)
            .collect()
    }

    #[test]
    fn a_baselines_show_clause_is_chipped_between_the_baseline_and_the_comparison() {
        let chips = chip_labels(
            "FOR TARGET IN ENVS BASELINE(\"prod\") SHOW(Time, Status), COMPARISON(\"stage\")\n    REQUEST A\nEND\n",
        );
        let at = |needle: &str| {
            chips
                .iter()
                .position(|c| c.contains(needle))
                .unwrap_or_else(|| panic!("expected a {needle} chip in {chips:?}"))
        };
        assert!(
            at("prod") < at("SHOW(") && at("SHOW(") < at("stage"),
            "the SHOW sits with the BASELINE it belongs to, not the comparison: {chips:?}"
        );
        assert!(
            chips.iter().any(|c| c == "SHOW(Time, Status)"),
            "and it names the fields it selects: {chips:?}"
        );
    }

    #[test]
    fn the_baselines_show_is_tethered_to_it_and_keeps_its_own_colour() {
        let flow = crate::report::parser::parse_flow(
            "FOR TARGET IN ENVS BASELINE(\"prod\") SHOW(Time), COMPARISON(\"stage\")\n    REQUEST A\nEND\n",
        )
        .expect("the fixture flow parses");
        let th = GuiTheme::from_spec(&crate::theme::preset_for_language(&Language::English));
        let s = Strings::for_language(&Language::English);
        let mut chips = node_chips(&flow.nodes[0], None, &th, &s);

        let show_at = chips
            .iter()
            .position(|c| c.text.starts_with("SHOW("))
            .expect("the SHOW is chipped");
        assert!(
            chips[show_at].tethered,
            "it is tied to the chip before it, not left floating between three peers"
        );
        assert!(
            chips[show_at - 1].text.contains("prod"),
            "and the chip it is tied to is the BASELINE, so the tie says who owns it"
        );

        link_tethers(&mut chips);
        let (baseline, show) = (&chips[show_at - 1], &chips[show_at]);
        assert!(
            baseline.join_next && show.join_prev,
            "the pair is drawn as one segmented pill rather than two loose chips"
        );
        assert!(
            !chips
                .iter()
                .any(|c| c.text.starts_with("COMPARISON(") && (c.join_prev || c.join_next)),
            "but the COMPARISON stays a peer — it qualifies the loop, not the baseline"
        );
        assert_eq!(
            show.color, th.ok,
            "and the SHOW keeps the colour every SHOW has, so it stays recognisable \
             as a SHOW — the pill is what says which chip it belongs to"
        );
        assert_ne!(
            show.color, baseline.color,
            "so the two halves are still told apart at a glance"
        );
    }

    #[test]
    fn a_tethered_chip_squares_off_only_the_edge_it_shares() {
        let mut chips = vec![
            Chip::modifier(
                "BASELINE(prod)".into(),
                Color32::RED,
                DetachWhich::Statistics,
            ),
            Chip::modifier(
                "SHOW(Time)".into(),
                Color32::GREEN,
                DetachWhich::BaselineShow,
            )
            .tether(),
        ];
        link_tethers(&mut chips);

        let (left, right) = (chip_corners(&chips[0]), chip_corners(&chips[1]));
        assert_eq!(
            (left.nw, left.sw, right.ne, right.se),
            (CHIP_RADIUS, CHIP_RADIUS, CHIP_RADIUS, CHIP_RADIUS),
            "the outside of the pair stays rounded, so it reads as a single pill"
        );
        assert_eq!(
            (left.ne, left.se, right.nw, right.sw),
            (0, 0, 0, 0),
            "and the meeting edges are square, so no gap or bulge shows between the halves"
        );
    }

    #[test]
    fn a_pair_being_pulled_apart_is_no_longer_drawn_joined() {
        let mut chips = vec![
            Chip::modifier(
                "BASELINE(prod)".into(),
                Color32::RED,
                DetachWhich::Statistics,
            ),
            Chip::modifier(
                "SHOW(Time)".into(),
                Color32::GREEN,
                DetachWhich::BaselineShow,
            )
            .tether(),
        ];
        link_tethers(&mut chips);
        split_tether(&mut chips, 1);

        assert!(
            !chips[0].join_next && !chips[1].join_prev,
            "a squared-off edge facing an empty slot would look like damage, not a join"
        );
        assert!(
            !chips[1].tethered,
            "and the pair falls back to the normal gap, so the row does not close up around the hole"
        );
        assert_eq!(
            chip_corners(&chips[0]),
            ROUND_CHIP,
            "the chip left behind is a whole chip again"
        );
    }

    #[test]
    fn a_baseline_without_a_show_clause_gets_no_show_chip() {
        let chips = chip_labels(
            "FOR TARGET IN ENVS BASELINE(\"prod\"), COMPARISON(\"stage\")\n    REQUEST A\nEND\n",
        );
        assert!(
            !chips.iter().any(|c| c.starts_with("SHOW(")),
            "no clause, no chip — an empty SHOW would claim a restriction that isn't there: {chips:?}"
        );
    }
}

#[cfg(test)]
mod results_table_tests {
    use super::{MIN_COL_W, fit_column_widths};

    /// Total width of a laid-out table, gaps included.
    fn spanned(widths: &[f32], spacing: f32) -> f32 {
        widths.iter().sum::<f32>() + spacing * (widths.len() as f32 - 1.0)
    }

    #[test]
    fn a_table_with_room_to_spare_grows_to_fill_the_whole_window() {
        let widths = fit_column_widths(&[60.0, 100.0, 40.0], 800.0, 10.0);
        assert!(
            (spanned(&widths, 10.0) - 800.0).abs() < 0.01,
            "the table spans the full width instead of huddling at the left edge: {widths:?}"
        );
        assert!(
            widths[1] > widths[0] && widths[0] > widths[2],
            "and the spare room is shared in proportion, so the column order is unchanged: {widths:?}"
        );
    }

    #[test]
    fn a_squeezed_table_takes_the_room_from_its_widest_columns_first() {
        // A narrow status column beside a sprawling body column: shrinking both
        // by the same proportion would leave the status column unreadable for
        // no gain, so only the greedy one should give ground.
        let widths = fit_column_widths(&[50.0, 600.0], 400.0, 10.0);
        assert!(
            (widths[0] - 50.0).abs() < 0.01,
            "the column that was already narrow is left alone: {widths:?}"
        );
        assert!(
            widths[1] < 600.0,
            "and the wide one absorbs the whole squeeze: {widths:?}"
        );
        assert!(
            spanned(&widths, 10.0) <= 400.01,
            "so everything still fits on screen: {widths:?}"
        );
    }

    #[test]
    fn columns_that_all_want_the_same_width_are_squeezed_equally() {
        let widths = fit_column_widths(&[300.0, 300.0, 300.0], 600.0, 0.0);
        for w in &widths {
            assert!(
                (w - 200.0).abs() < 0.01,
                "with nothing to choose between them they share the shortfall: {widths:?}"
            );
        }
    }

    #[test]
    fn a_table_too_wide_even_for_its_minimums_overflows_so_it_can_be_scrolled() {
        // Twenty columns can't be made readable in 200 pixels. Rather than
        // grinding them all to slivers, the widths overrun the viewport and the
        // caller's horizontal scroll bar takes over.
        let widths = fit_column_widths(&[100.0; 20], 200.0, 0.0);
        for w in &widths {
            assert!(
                *w >= MIN_COL_W,
                "no column is shrunk past the point of showing anything: {widths:?}"
            );
        }
        assert!(
            spanned(&widths, 0.0) > 200.0,
            "the overflow is what makes the scroll bar appear: {widths:?}"
        );
    }

    #[test]
    fn a_table_with_no_columns_is_not_a_division_by_zero() {
        assert!(
            fit_column_widths(&[], 500.0, 10.0).is_empty(),
            "an empty report lays out to nothing rather than panicking"
        );
    }
}

#[cfg(test)]
mod results_render_tests {
    use super::{MIN_COL_W, SPACING_X, fitted_column_widths, results_grid};
    use crate::gui::theme::GuiTheme;
    use crate::i18n::Language;
    use crate::report::model::{OutputColumn, ReportResult, ReportRow};
    use eframe::egui;

    /// A result of `rows` rows whose every cell in column `c` holds `fills[c]`.
    fn fixture(headers: &[&str], fills: &[&str], rows: usize) -> (ReportResult, Vec<OutputColumn>) {
        let columns: Vec<OutputColumn> = headers
            .iter()
            .map(|h| OutputColumn {
                header: h.to_string(),
                sources: vec![h.to_string()],
                stats: Vec::new(),
                image: None,
            })
            .collect();
        let mut result = ReportResult::default();
        for _ in 0..rows {
            let mut row = ReportRow::default();
            for (h, v) in headers.iter().zip(fills) {
                row.cells.insert(h.to_string(), v.to_string());
            }
            result.rows.push(row);
        }
        (result, columns)
    }

    /// The source editor asks its layouter for a job on every frame, so the job
    /// is cached — but it has to follow every input the highlighter colours by.
    #[test]
    fn the_highlight_key_notices_every_input_it_guards() {
        use crate::tui::report_highlight::HlCtx;
        let text = "REPORT REQUEST login AS l\n";
        let ctx = HlCtx {
            error_line: None,
            collection_resolves: true,
            loaded_envs: ["dev".to_string()].into_iter().collect(),
            request_names: ["login".to_string()].into_iter().collect(),
        };
        let spec = crate::theme::default_preset();
        let font = egui::FontId::monospace(12.0);
        let key = |t: &str, c: &HlCtx, s: &crate::theme::ThemeSpec, f: &egui::FontId, w: f32| {
            super::highlight_key(t, c, s, f, w)
        };
        let base = key(text, &ctx, &spec, &font, 800.0);

        assert_ne!(
            base,
            key("REPORT REQUEST logout AS l\n", &ctx, &spec, &font, 800.0),
            "the source text"
        );

        let mut c = ctx.clone();
        c.error_line = Some(1);
        assert_ne!(base, key(text, &c, &spec, &font, 800.0), "the error line");

        c.error_line = None;
        c.collection_resolves = false;
        assert_ne!(
            base,
            key(text, &c, &spec, &font, 800.0),
            "whether the collection binds — it is what makes a name green"
        );

        c.collection_resolves = true;
        c.request_names = ["other".to_string()].into_iter().collect();
        assert_ne!(
            base,
            key(text, &c, &spec, &font, 800.0),
            "which requests exist"
        );

        c.request_names = ctx.request_names.clone();
        c.loaded_envs = ["prod".to_string()].into_iter().collect();
        assert_ne!(
            base,
            key(text, &c, &spec, &font, 800.0),
            "which environments are loaded"
        );

        let other_theme = crate::theme::preset_for_language(&crate::i18n::Language::French);
        assert_ne!(
            base,
            key(text, &ctx, &other_theme, &font, 800.0),
            "the theme, which is where every colour comes from"
        );

        assert_ne!(
            base,
            key(text, &ctx, &spec, &egui::FontId::monospace(18.0), 800.0),
            "the font size"
        );
        assert_ne!(
            base,
            key(text, &ctx, &spec, &font, 400.0),
            "the wrap width, which the job carries"
        );

        // Two contexts holding the same names in a different insertion order are
        // the same context: both are hash sets, and iterating one straight into
        // the hasher is what made the validation panel flicker.
        let mut same = ctx.clone();
        same.request_names.insert("zzz".to_string());
        same.request_names.remove("zzz");
        assert_eq!(
            base,
            key(text, &same, &spec, &font, 800.0),
            "the same set is the same key"
        );
    }

    /// Render the grid twice in one context, so the second pass sees whatever
    /// the first left in egui's memory, and report the widths each time.
    fn widths_across_two_frames(
        first: &(ReportResult, Vec<OutputColumn>),
        then: impl FnOnce(&mut ReportResult),
    ) -> (Vec<f32>, Vec<f32>) {
        let ctx = egui::Context::default();
        let (mut result, columns) = (first.0.clone(), first.1.clone());
        let mut a = Vec::new();
        let mut b = Vec::new();
        let draw = |ctx: &egui::Context, result: &ReportResult, out: &mut Vec<f32>| {
            for _ in 0..2 {
                let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
                    ui.set_max_width(4000.0);
                    ui.set_min_width(4000.0);
                    // The *natural* widths are what is cached; fitting them to
                    // the window is pure arithmetic on top.
                    *out = super::cached_natural_widths(ui, result, &columns);
                });
            }
        };
        draw(&ctx, &result, &mut a);
        then(&mut result);
        draw(&ctx, &result, &mut b);
        (a, b)
    }

    /// The widths are cached between frames, and a streaming run replaces its
    /// rows **in place** — the grid starts as a skeleton of empty rows that are
    /// overwritten as results arrive. A cache keyed on anything less than the
    /// cell text would therefore pin the columns at the width of an empty table
    /// for the whole run.
    #[test]
    fn cached_widths_follow_a_row_that_is_filled_in_place() {
        let empty = fixture(&["Result"], &[""], 4);
        let (before, after) = widths_across_two_frames(&empty, |result| {
            for row in &mut result.rows {
                row.cells.insert(
                    "Result".to_string(),
                    "a much longer value than the header".to_string(),
                );
            }
        });
        assert!(
            after[0] > before[0] + 20.0,
            "the column grew for the arriving values: {before:?} then {after:?}"
        );
    }

    /// The same table two frames running must measure the same, or the columns
    /// would twitch as the mouse moves.
    #[test]
    fn cached_widths_are_stable_when_nothing_changes() {
        let table = fixture(&["A", "B"], &["one", "two"], 5);
        let (before, after) = widths_across_two_frames(&table, |_| {});
        assert_eq!(before, after);
    }

    /// The fingerprint is hand-maintained, so it gets the same contract test the
    /// validation cache has: it must move for every input the measurement reads.
    #[test]
    fn the_width_fingerprint_notices_every_input_it_guards() {
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            let (base_res, base_cols) = fixture(&["A"], &["one"], 2);
            let base = super::widths_fingerprint(ui, &base_res, &base_cols);

            let mut r = base_res.clone();
            r.rows[0]
                .cells
                .insert("A".to_string(), "changed".to_string());
            assert_ne!(
                base,
                super::widths_fingerprint(ui, &r, &base_cols),
                "a cell's text"
            );

            let mut r = base_res.clone();
            r.rows.push(ReportRow::default());
            assert_ne!(
                base,
                super::widths_fingerprint(ui, &r, &base_cols),
                "a new row"
            );

            let mut r = base_res.clone();
            r.no_match_marker = "\u{2014}".to_string();
            assert_ne!(
                base,
                super::widths_fingerprint(ui, &r, &base_cols),
                "the no-match marker, which is what an empty cell renders as"
            );

            let mut r = base_res.clone();
            r.rows[0].target = Some("staging".to_string());
            assert_ne!(
                base,
                super::widths_fingerprint(ui, &r, &base_cols),
                "the row's ENVS target"
            );

            let mut r = base_res.clone();
            r.rows[0].vars.insert("v".to_string(), "x".to_string());
            assert_ne!(
                base,
                super::widths_fingerprint(ui, &r, &base_cols),
                "a variable a columns: directive could show"
            );

            let (_, wider) = fixture(&["A Much Longer Header"], &["one"], 2);
            assert_ne!(
                base,
                super::widths_fingerprint(ui, &base_res, &wider),
                "the column header"
            );

            let mut cols = base_cols.clone();
            cols[0].stats = vec![crate::report::model::StatKind::Count];
            assert_ne!(
                base,
                super::widths_fingerprint(ui, &base_res, &cols),
                "statistics, which add summary rows to measure"
            );

            // Two rows whose cells were inserted in a different order are the
            // same table, and must not look like a change: `cells` is a hash
            // map, and iterating one straight into the hasher is exactly what
            // made the validation panel flicker.
            let (a, cols2) = fixture(&["A", "B"], &["one", "two"], 1);
            let mut b = ReportResult::default();
            let mut row = ReportRow::default();
            row.cells.insert("B".to_string(), "two".to_string());
            row.cells.insert("A".to_string(), "one".to_string());
            b.rows.push(row);
            assert_eq!(
                super::widths_fingerprint(ui, &a, &cols2),
                super::widths_fingerprint(ui, &b, &cols2),
                "insertion order is not a difference"
            );
        });
    }

    /// The widths the grid would give `columns` in a window `avail` wide, and
    /// the total they span. Renders for real, so the measurement is the font's
    /// and not a guess.
    fn widths_at(result: &ReportResult, columns: &[OutputColumn], avail: f32) -> (Vec<f32>, f32) {
        let ctx = egui::Context::default();
        let mut widths = Vec::new();
        // Twice: galley measurements only settle once the layout has been seen.
        for _ in 0..2 {
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
                ui.set_max_width(avail);
                ui.set_min_width(avail);
                widths = fitted_column_widths(ui, result, columns, false);
            });
        }
        let span = widths.iter().sum::<f32>() + SPACING_X * (widths.len() as f32 - 1.0);
        (widths, span)
    }

    #[test]
    fn a_narrow_table_is_stretched_across_the_whole_window() {
        // Three tiny columns in a wide window: left to themselves they would
        // huddle in the first hundred pixels and leave the rest blank.
        let (result, columns) = fixture(&["A", "B", "C"], &["1", "2", "3"], 3);
        let (widths, span) = widths_at(&result, &columns, 900.0);
        assert!(
            span > 850.0,
            "the table spans essentially the whole 900px window: {widths:?} ({span})"
        );
    }

    #[test]
    fn a_wide_table_is_squeezed_to_stay_inside_the_window() {
        // Six columns of long values in a narrow window have to be cut down
        // rather than run off the right edge — the cell viewer is what recovers
        // whatever the truncation hides.
        let long = "a value long enough to want a column all to itself";
        let (result, columns) = fixture(
            &["One", "Two", "Three", "Four", "Five", "Six"],
            &[long; 6],
            4,
        );
        let (widths, span) = widths_at(&result, &columns, 600.0);
        assert!(
            span <= 600.5,
            "everything fits inside the window: {widths:?} ({span})"
        );
    }

    #[test]
    fn a_column_of_long_values_is_given_more_room_than_a_column_of_short_ones() {
        let (result, columns) = fixture(
            &["Id", "Body"],
            &["7", "a considerably longer captured value"],
            5,
        );
        let (widths, _) = widths_at(&result, &columns, 800.0);
        assert!(
            widths[1] > widths[0] * 2.0,
            "width follows what a column actually has to show: {widths:?}"
        );
    }

    #[test]
    fn a_header_wider_than_its_values_still_gets_room_for_its_own_name() {
        // A column of one-character values under a long header must be sized by
        // the header, or the column would be unidentifiable.
        let (result, columns) = fixture(&["A", "AnUncommonlyLongHeader"], &["1", "2"], 3);
        let (widths, _) = widths_at(&result, &columns, 900.0);
        assert!(
            widths[1] > widths[0],
            "the long header claims the wider column: {widths:?}"
        );
    }

    #[test]
    fn a_table_with_more_columns_than_can_ever_fit_overflows_for_the_scroll_bar() {
        // Thirty columns can't be shown legibly in 400 pixels at any width, so
        // the grid deliberately overruns its window and the scroll area takes
        // over, rather than grinding every column down to an ellipsis.
        let headers: Vec<String> = (0..30).map(|i| format!("Col{i}")).collect();
        let heads: Vec<&str> = headers.iter().map(|h| h.as_str()).collect();
        let (result, columns) = fixture(&heads, &["value"; 30], 2);
        let (widths, span) = widths_at(&result, &columns, 400.0);
        assert!(
            widths.iter().all(|w| *w >= MIN_COL_W),
            "every column keeps a readable minimum: {widths:?}"
        );
        assert!(
            span > 400.0,
            "and the overflow is what puts the scroll bar there: {span}"
        );
    }

    #[test]
    fn drawing_the_grid_itself_survives_a_window_too_narrow_for_one_column() {
        // A pathological pane width must not panic or divide by zero — it just
        // scrolls.
        let (result, columns) = fixture(&["A", "B", "C"], &["1", "2", "3"], 2);
        let ctx = egui::Context::default();
        let th = GuiTheme::from_spec(&crate::theme::preset_for_language(&Language::English));
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.set_max_width(1.0);
            results_grid(&th, ui, &result, &columns, None);
        });
    }
}

#[cfg(test)]
mod dry_run_view_tests {
    use super::{EditorView, ReportEditor, ReportOrigin};

    /// The reindent button re-indents as a single undo step, and — unlike every
    /// other editor action — must not round-trip through the AST, because the
    /// flow has nowhere to keep a body comment.
    #[test]
    fn reindenting_is_one_undo_step_and_keeps_body_comments() {
        let before = "# collection: c\n\nFOR T IN FILES \"*.txt\"\n# keep me\nREQUEST a\nEND\n";
        let report = crate::report::Report::from_text("r", before);
        let mut ed = ReportEditor::new(ReportOrigin::Session(0), report);

        assert!(matches!(ed.reformat(), Ok(true)), "text moved");
        assert!(
            ed.report.text.contains("    # keep me"),
            "the comment is kept and indented: {:?}",
            ed.report.text
        );
        assert!(
            ed.report.text.contains("    REQUEST a"),
            "the body is indented: {:?}",
            ed.report.text
        );

        // Already tidy the second time, so it costs no further undo entry.
        assert!(matches!(ed.reformat(), Ok(false)));
        ed.undo();
        assert_eq!(ed.report.text, before, "one undo restores the original");
    }

    /// The Dry run button lives on the toolbar, which is above every view, but
    /// the preview is drawn by the Results view. Pressing it from Blocks (where
    /// you build the report, and so where you press it from) has to take you to
    /// the preview — otherwise the button looks dead.
    #[test]
    fn a_dry_run_switches_to_the_view_that_actually_shows_it() {
        let report = crate::report::Report::scratch("r");
        let mut ed = ReportEditor::new(ReportOrigin::Session(0), report);
        assert!(
            ed.view == EditorView::Blocks,
            "an editor opens on the Blocks view"
        );

        ed.show_preview(Box::new(crate::report::dry_run::DryRunReport::from_result(
            crate::report::ReportResult::default(),
            crate::report::flow::Header::default(),
            Vec::new(),
        )));

        assert!(
            ed.view == EditorView::Results,
            "the preview should be on screen, not waiting in a view nobody is looking at"
        );
        assert!(ed.dry_run.is_some(), "and the preview itself is held");
    }
}

#[cfg(test)]
mod toolbar_commit_tests {
    use super::*;
    use crate::gui::app::GuiApp;

    /// Every piece of text painted this frame, with the rect it was painted in,
    /// so a test can click a widget by the label it shows.
    fn painted(shapes: &[egui::epaint::ClippedShape]) -> Vec<(String, egui::Rect)> {
        fn walk(s: &egui::epaint::Shape, out: &mut Vec<(String, egui::Rect)>) {
            match s {
                egui::epaint::Shape::Text(t) => {
                    out.push((t.galley.text().to_string(), t.visual_bounding_rect()))
                }
                egui::epaint::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let mut out = Vec::new();
        for c in shapes {
            walk(&c.shape, &mut out);
        }
        out
    }

    fn input() -> egui::RawInput {
        let mut i = egui::RawInput::default();
        i.screen_rect = Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(1000.0, 760.0),
        ));
        i
    }

    fn click_at(pos: egui::Pos2) -> egui::RawInput {
        let mut i = input();
        i.events.push(egui::Event::PointerMoved(pos));
        i.events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Default::default(),
        });
        i.events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        });
        i
    }

    /// An app with one bound collection and the given report open in its editor.
    fn app_with_report(text: &str) -> GuiApp {
        let mut session = crate::session::Session::default();
        session.collections.clear();
        let entry = crate::hurl::HurlEntry {
            title: "A".into(),
            url: "http://127.0.0.1:1/".into(),
            ..Default::default()
        };
        session.collections.push(crate::collection::Collection::new(
            "api".into(),
            vec![entry],
        ));
        let mut app = GuiApp::for_test(session);
        let mut report = crate::report::Report::scratch("r");
        report.set_text(text.to_string());
        app.report_editor = Some(ReportEditor::new(ReportOrigin::Session(0), report));
        app
    }

    /// The same collision inside the Blocks view, where the button queues a
    /// *structural* edit: the field commit carries the path of the block it was
    /// drawn on, so it has to be applied while that path still means what it
    /// meant on screen. Applied after a deletion instead, it lands on whichever
    /// block shuffled into that slot — renaming a block the user never touched.
    #[test]
    fn a_field_commit_is_not_applied_to_a_block_that_moved_under_it() {
        let mut app = app_with_report(
            "# collection: api\nREPORT REQUEST A AS One\nREPORT REQUEST A AS Two\nREPORT REQUEST A AS Three\n",
        );
        let ctx = egui::Context::default();

        let out = ctx.run_ui(input(), |ui| super::ui(&mut app, ui));
        let painted = painted(&out.shapes);
        let two = painted
            .iter()
            .find(|(t, _)| t == "Two")
            .map(|(_, r)| *r)
            .expect("the second block's alias field");
        let trash = painted
            .iter()
            .find(|(t, _)| t.contains(app.strings.gui_report_delete_block))
            .map(|(_, r)| *r)
            .expect("the Delete block button");

        // Click into the second block's alias and type, without leaving it.
        let caret = egui::pos2(two.right() - 1.0, two.center().y);
        let _ = ctx.run_ui(click_at(caret), |ui| super::ui(&mut app, ui));
        let mut typing = input();
        typing.events.push(egui::Event::Text("x".into()));
        let _ = ctx.run_ui(typing, |ui| super::ui(&mut app, ui));

        // With the *first* block selected, delete it while the second block's
        // field still holds uncommitted text.
        app.report_editor.as_mut().unwrap().selection = vec![0];
        // egui resolves a click against the previous frame's widgets, so the
        // Delete button has to have been drawn enabled once before it can be
        // pressed.
        let _ = ctx.run_ui(input(), |ui| super::ui(&mut app, ui));
        let _ = ctx.run_ui(click_at(trash.center()), |ui| super::ui(&mut app, ui));

        let text = &app.report_editor.as_ref().expect("editor open").report.text;
        assert!(!text.contains("AS One"), "the selected block is deleted");
        assert!(
            text.contains("AS Twox"),
            "the edited block keeps its own edit: {text:?}"
        );
        assert!(
            text.contains("AS Three"),
            "and the block below it is left alone: {text:?}"
        );
    }

    /// Typing in an inline chip field and pressing a toolbar button in one go
    /// must keep what was typed: the field commits when the click takes its
    /// focus away, and the button acts on the result. Before, the button acted
    /// first and switched view, so the field was never redrawn, its commit
    /// never ran, and the typing was silently thrown away.
    #[test]
    fn a_toolbar_press_keeps_what_was_being_typed() {
        let mut app = app_with_report("# collection: api\nREPORT REQUEST A AS Old\n");
        let ctx = egui::Context::default();

        // Frame 1: lay the editor out so the alias field has a position.
        let out = ctx.run_ui(input(), |ui| super::ui(&mut app, ui));
        let painted = painted(&out.shapes);
        let alias = painted
            .iter()
            .find(|(t, _)| t == "Old")
            .map(|(_, r)| *r)
            .expect("the alias field shows its current value");
        let dry = painted
            .iter()
            .find(|(t, _)| t.contains(app.strings.gui_report_dry_run))
            .map(|(_, r)| *r)
            .expect("the Dry run button is on the toolbar");

        // Frame 2: click into the alias field, putting the caret after "Old".
        let caret = egui::pos2(alias.right() - 1.0, alias.center().y);
        let _ = ctx.run_ui(click_at(caret), |ui| super::ui(&mut app, ui));

        // Frame 3: type, without leaving the field.
        let mut typing = input();
        typing.events.push(egui::Event::Text("er".into()));
        let _ = ctx.run_ui(typing, |ui| super::ui(&mut app, ui));
        assert!(
            app.report_editor
                .as_ref()
                .is_some_and(|ed| ed.report.text.contains("AS Old")),
            "still uncommitted while the field has focus"
        );

        // Frame 4: press Dry run straight from the field.
        let _ = ctx.run_ui(click_at(dry.center()), |ui| super::ui(&mut app, ui));
        let ed = app.report_editor.as_ref().expect("the editor stays open");
        assert!(
            ed.report.text.contains("AS Older"),
            "the typing survives the button press: {:?}",
            ed.report.text
        );
        assert!(ed.view == EditorView::Results, "and the button still acted");
    }
}

#[cfg(test)]
mod stop_run_tests {
    use super::*;
    use crate::gui::report_run::{RunUpdate, test_handle};
    use crate::report::Report;
    use crate::report::model::{ReportResult, ReportRow};
    use crate::session::Session;

    fn editor() -> (GuiApp, ReportEditor) {
        let mut app = GuiApp::for_test(Session::default());
        app.open_report_editor(ReportOrigin::Workspace, Report::scratch("nightly"));
        let ed = app.report_editor.take().expect("editor is open");
        (app, ed)
    }

    fn result_with(cell: &str) -> ReportResult {
        let mut res = ReportResult::default();
        let mut row = ReportRow::default();
        row.cells.insert("A".to_string(), cell.to_string());
        res.rows.push(row);
        res
    }

    /// Stopping used to only raise the cancel flag and leave the handle in
    /// place, so `is_running()` stayed true — and the button stayed a Stop
    /// button — until the worker's `Done` finally arrived. Cancelling doesn't
    /// abort an in-flight request, and a `PARALLEL` batch can take a long time
    /// to wind down, so the report was unrunnable for a while. The TUI retires
    /// the run at once; so must this.
    #[test]
    fn stopping_frees_the_report_to_be_run_again_at_once() {
        let (mut app, mut ed) = editor();
        let (handle, tx) = test_handle();
        ed.run = Some(handle);
        assert!(ed.is_running(), "a live handle is a live run");

        ed.stop_run(&mut app);

        assert!(
            !ed.is_running(),
            "the run is retired immediately, without waiting for the worker"
        );
        assert!(matches!(app.session.status, Some(Status::ReportRunStopped)));

        // The worker is still alive and still winding down: it has not sent
        // `Done`, and sending now must not resurrect the run.
        assert!(
            tx.send(RunUpdate::Done(ReportResult::default())).is_err(),
            "our end of the channel went with the handle, so late updates \
             land nowhere rather than being folded in"
        );
        assert!(!ed.is_running());
    }

    /// Retiring the handle must not take the partial grid with it: rows that
    /// completed before the stop keep their real responses, and the user can
    /// still read, save or export them.
    #[test]
    fn stopping_keeps_the_rows_that_already_arrived_but_clears_the_progress() {
        let (mut app, mut ed) = editor();
        let (handle, _tx) = test_handle();
        ed.run = Some(handle);
        ed.result = Some(result_with("200"));
        ed.progress = Some(crate::gui::report_run::RunProgress {
            states: vec![crate::gui::report_run::RowState::Running],
            index: Default::default(),
            done: 0,
            total: 1,
        });

        ed.stop_run(&mut app);

        assert_eq!(
            ed.result.as_ref().map(|r| r.rows.len()),
            Some(1),
            "the partial grid survives the stop"
        );
        assert!(
            ed.progress.is_none(),
            "but no row is left rendering as still running"
        );
    }

    /// The cancel flag still has to be raised, or the detached worker would
    /// carry on firing requests at the network after the user stopped it.
    #[test]
    fn stopping_still_tells_the_worker_to_wind_down() {
        let (mut app, mut ed) = editor();
        let (handle, _tx) = test_handle();
        let flag = handle.cancel_flag_for_test();
        ed.run = Some(handle);

        ed.stop_run(&mut app);

        assert!(
            flag.load(std::sync::atomic::Ordering::Relaxed),
            "dropping our end alone would not stop the worker: it watches the flag"
        );
    }
}
