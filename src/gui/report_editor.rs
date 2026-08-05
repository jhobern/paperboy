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
    flatten, insert_node, insert_pos_after, move_node, node_at, remove_node, replace_node,
    report_assignment, request_node, set_request_name,
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
            undo: Vec::new(),
            result: None,
            progress: None,
            run: None,
            results_exported: false,
            wizard: None,
            diag_h: 132.0,
            palette_w: 168.0,
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
}

impl Chip {
    fn base(text: String, color: Color32) -> Chip {
        Chip {
            text,
            color,
            is_base: true,
            detach: None,
            edit: ChipEdit::None,
        }
    }
    fn modifier(text: String, color: Color32, which: DetachWhich) -> Chip {
        Chip {
            text,
            color,
            is_base: false,
            detach: Some(which),
            edit: ChipEdit::None,
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
            edit: ChipEdit::None,
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
        }
    }
    /// A `BASELINE`/`COMPARISON` chip carrying a single-environment dropdown.
    fn env_role(baseline: bool, index: usize, name: &str, color: Color32) -> Chip {
        let kw = if baseline { "BASELINE" } else { "COMPARISON" };
        Chip {
            text: format!("{kw}({name})"),
            color,
            is_base: false,
            detach: None,
            edit: ChipEdit::EnvRole {
                baseline,
                index,
                name: name.to_string(),
            },
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
            vec![Chip::request(name, req_col(req_ok))]
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
            chips.push(Chip::request(name, req_col(req_ok)));
            // RESPONSE / SHOW / HIDE are their own detachable chips so a long
            // reported request reads as a row of small, legible clauses rather
            // than one dense line.
            if let Some(fmt) = response_fmt {
                let text = match fmt {
                    crate::report::flow::ResponseFmt::Raw => "RESPONSE RAW",
                    crate::report::flow::ResponseFmt::Pretty => "RESPONSE PRETTY",
                };
                chips.push(Chip::modifier(
                    text.into(),
                    th.accent,
                    DetachWhich::Response,
                ));
            }
            if !show.is_empty() {
                chips.push(Chip::modifier(
                    format!("SHOW({})", show.join(", ")),
                    th.ok,
                    DetachWhich::Show,
                ));
            }
            if !hide.is_empty() {
                chips.push(Chip::modifier(
                    format!("HIDE({})", hide.join(", ")),
                    th.dim,
                    DetachWhich::Hide,
                ));
            }
            if let Some(a) = alias {
                chips.push(Chip::alias(a, th.pending, Some(DetachWhich::As)));
            }
            // The `WITH … END` fields are rendered as a *nested block* under the
            // request line (see `with_block` in `block_row`); the line itself
            // only carries the opening `WITH` keyword so it reads like the
            // textual form (`… SHOW(Time) WITH`).
            if !with.is_empty() {
                chips.push(Chip::fixed("WITH".into(), th.accent));
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
                Chip::modifier("REPORT".into(), th.subst, DetachWhich::Report),
                Chip::base(text, th.text),
            ]
        }
        FlowNode::Report(ReportStmt::VarAs { var, name, .. }) => vec![
            Chip::modifier("REPORT".into(), th.subst, DetachWhich::Report),
            Chip::base(var.clone(), th.text),
            Chip::alias(name, th.pending, Some(DetachWhich::As)),
        ],
        FlowNode::Report(ReportStmt::Computed { template, name, .. }) => vec![
            Chip::modifier("REPORT".into(), th.subst, DetachWhich::Report),
            Chip::base(format!("\"{template}\""), th.text),
            // A computed column requires its AS name, so this chip is fixed
            // (inline-editable, but not detachable).
            Chip::alias(name, th.pending, None),
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
                // it is not duplicated by the base chip below. PARALLEL uses the
                // theme's *error* hue so it stands apart from the blue loop/set
                // chips it sits beside (`PARALLEL(8) FOR …`).
                chips.push(Chip::modifier(text, th.err, DetachWhich::Parallel));
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
                // A single live-environment role becomes an inline dropdown; any
                // other shape (multiple refs, or a FILE snapshot) stays a fixed
                // chip edited through the ENVS wizard.
                use crate::report::flow::RoleRef;
                if let [RoleRef::Env(name)] = baseline.as_slice() {
                    chips.push(Chip::env_role(true, 0, name, th.pending));
                } else if !baseline.is_empty() {
                    chips.push(Chip::fixed(
                        format!("BASELINE({})", role_refs_text(baseline)),
                        th.pending,
                    ));
                }
                if let [RoleRef::Env(name)] = comparisons.as_slice() {
                    chips.push(Chip::env_role(false, 0, name, th.pending));
                } else if !comparisons.is_empty() {
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
    let avail = ui.available_height();
    let diag_h = ed.diag_h.clamp(48.0, (avail - 100.0).max(48.0));
    let edit_h = (avail - diag_h - 8.0).max(80.0);
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
            }
        });
    diag_splitter(ed, ui);
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
                        for (i, row) in rows.iter().enumerate() {
                            let selected = row.path == ed.selection
                                && (row.kind != RowKind::LoopEnd || ed.selection.is_empty());
                            let drop_pos = insert_pos_after(&rows, i);
                            block_row(ed, app, ui, row, i, selected, &drop_pos, &titles, &mut acts);
                        }
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
        acts.push(Act::Delete);
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
/// statement row. `ReportRequest` and `ReportVar` are intentionally absent — a
/// reported request is composed by dropping the `REPORT` modifier onto a
/// `REQUEST`, and a reported variable by dropping `REPORT` onto a `VARIABLE`
/// (`Assign`) block, so there is a single `REPORT` in the palette.
const BASE_KINDS: [NodeKind; 6] = [
    NodeKind::Request,
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
        .corner_radius(6);
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

/// The palette colour for a modifier chip. Kept in step with [`node_chips`] so
/// a palette modifier reads the same colour as the chip it drops in.
fn modifier_color(m: Modifier, th: &GuiTheme) -> Color32 {
    match m {
        Modifier::Report => th.subst,
        Modifier::With => th.accent,
        Modifier::As => th.pending,
        Modifier::Parallel => th.err,
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

/// Horizontal indent applied per nesting level in the block editor, so a
/// statement inside a `FOR`/`PARALLEL`/`WITH` block sits clearly further right
/// than its parent. Used for both the chip clusters and the drop-placeholder /
/// nested-field indents so they all line up at the same depth.
const INDENT_STEP: f32 = 24.0;

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
    let s = &app.strings;
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
    let block = ui.vertical(|ui| {
        // Top-align the chip cluster (rather than the default centre alignment):
        // every chip is the same height, so top-alignment keeps them level while
        // avoiding egui's horizontal-centre re-centring, which otherwise drifts
        // each successive chip on the line progressively lower.
        let inner = ui.horizontal_top(|ui| {
            ui.add_space(row.depth as f32 * INDENT_STEP);
            match row.kind {
                RowKind::Begin => static_chip(ui, &th, app.strings.report_node_begin, th.accent),
                RowKind::LoopEnd => static_chip(ui, &th, "END", th.accent),
                RowKind::Leaf | RowKind::LoopHead => {
                    let chips = node
                        .as_ref()
                        .map(|n| node_chips(n, row.req_ok, &th))
                        .unwrap_or_default();
                    for chip in &chips {
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
                        );
                    }
                }
            }
        });
        if !with_items.is_empty() {
            with_block(ui, &th, s, &row.path, row.depth, &with_items, acts);
        }
        inner.response.rect
    });
    let cluster = block.inner;
    let block_rect = block.response.rect;

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
        if let Some(m) = zresp.dnd_hover_payload::<Modifier>()
            && m.applies_to(n)
        {
            ui.painter().rect_stroke(
                zone_rect.expand(2.0),
                egui::CornerRadius::same(6),
                egui::Stroke::new(2.0, th.accent),
                egui::StrokeKind::Outside,
            );
        }
        if let Some(m) = release_payload::<Modifier>(&zresp)
            && m.applies_to(n)
        {
            acts.push(Act::AttachMod {
                path: row.path.clone(),
                modifier: *m,
            });
        }
    }

    // ── Base insert strip: a full-width strip over the block that, when a base
    // block is dragged over it, opens an animated gap below the block (the
    // existing blocks slide down to make room) with a dashed placeholder where
    // the new block will land. The strip is sized to include the currently-open
    // gap (read from last frame) so the pointer stays over it as the gap opens —
    // avoiding open/close flicker at the seam. The gap height matches a single
    // block so the ghost is the same size as the block being dropped.
    let gap_h = chip_h(ui) + 10.0;
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
        let indent = row.depth as f32 * INDENT_STEP;
        let top = block_rect.bottom() + 2.0;
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
) {
    let field_indent = (depth as f32 + 1.0) * INDENT_STEP;
    let fill = mix(th.panel, th.subst, 0.22);
    let stroke = egui::Stroke::new(1.0, mix(th.panel, th.subst, 0.5));
    for (i, item) in items.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.add_space(field_indent);
            let text = match item {
                WithItem::Field { name, query, .. } => format!("{name}: {query}"),
                WithItem::ResponseFmt(fmt) => format!(
                    "RESPONSE {}",
                    match fmt {
                        crate::report::flow::ResponseFmt::Raw => "RAW",
                        crate::report::flow::ResponseFmt::Pretty => "PRETTY",
                    }
                ),
            };
            let lbl = chip_shell(ui, fill, stroke, true, |ui| {
                let lbl = ui.add(
                    egui::Label::new(RichText::new(&text).color(th.subst))
                        .selectable(false)
                        .sense(egui::Sense::click()),
                );
                if detach_x(ui, th.subst) {
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
        static_chip(ui, th, "END", th.accent);
    });
}

/// A catch-all drop target filling the empty space beneath the last row: a base
/// block dropped here is appended as the last top-level line, and an existing
/// row dragged here is moved to the end. `top_len` is the current number of
/// top-level nodes (the append index). A no-op when there is no spare vertical
/// space (the report already fills / overflows the viewport — the last row's own
/// insert strip covers appending in that case).
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
        let mark = egui::Rect::from_min_size(rect.left_top(), egui::vec2(rect.width() - 8.0, 26.0));
        ui.painter().rect(
            mark,
            egui::CornerRadius::same(6),
            mix(th.panel, th.accent, 0.18),
            egui::Stroke::new(1.5, th.accent),
            egui::StrokeKind::Inside,
        );
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
    let row = ui.text_style_height(&egui::TextStyle::Button);
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
fn chip_shell<R>(
    ui: &mut egui::Ui,
    fill: Color32,
    stroke: egui::Stroke,
    grow: bool,
    content: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let h = chip_h(ui);
    egui::Frame::NONE
        .fill(fill)
        .stroke(stroke)
        .inner_margin(egui::Margin::symmetric(8, 3))
        .corner_radius(6)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if grow {
                    ui.set_min_height(h);
                }
                content(ui)
            })
            .inner
        })
        .inner
}

/// The fill / stroke / text colours for a chip, honouring the selected-base
/// highlight.
fn chip_colors(th: &GuiTheme, chip: &Chip, selected: bool) -> (Color32, egui::Stroke, Color32) {
    if chip.is_base && selected {
        // Keep the stroke *width* identical to the unselected state (only the
        // colour and fill change): a thicker stroke would expand the frame by a
        // pixel and shift the chip and its neighbours, so selecting a block
        // would visibly nudge it. Selection must recolour in place, never
        // resize or move.
        (
            th.select_bg,
            egui::Stroke::new(1.0, th.select_fg),
            th.select_fg,
        )
    } else {
        (
            mix(th.panel, chip.color, 0.22),
            egui::Stroke::new(1.0, mix(th.panel, chip.color, 0.5)),
            chip.color,
        )
    }
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

/// An inline single-line text field that commits on blur. The in-progress buffer
/// lives in egui temp memory keyed by `id` (so it survives across frames while
/// focused) and is dropped once committed / idle, keeping the field synced to
/// the AST. Returns `Some(trimmed value)` on the frame focus is lost.
fn inline_text_edit(
    ui: &mut egui::Ui,
    id: egui::Id,
    current: &str,
    hint: &str,
    width: f32,
) -> Option<String> {
    let mut buf = ui
        .data(|d| d.get_temp::<String>(id))
        .unwrap_or_else(|| current.to_string());
    let resp = ui.add(
        egui::TextEdit::singleline(&mut buf)
            .hint_text(hint)
            // Match the fill a combo-box chip (BASELINE/COMPARISON/REQUEST) uses
            // for its button, so an inline field (the AS alias) doesn't read as
            // a darker, sunken box beside them.
            .background_color(ui.visuals().widgets.inactive.weak_bg_fill)
            .desired_width(width),
    );
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
fn static_chip(ui: &mut egui::Ui, th: &GuiTheme, text: &str, color: Color32) {
    let fill = mix(th.panel, color, 0.22);
    let stroke = egui::Stroke::new(1.0, mix(th.panel, color, 0.5));
    chip_shell(ui, fill, stroke, true, |ui| {
        ui.add(egui::Label::new(RichText::new(text).color(color)).selectable(false));
    });
}

/// Render one [`Chip`]. The base chip is click-to-select, double-click-to-open
/// the wizard, and a drag source that relocates or bins its whole row; a
/// modifier chip shows a `×` that detaches it and is itself a drag source that
/// bins the modifier.
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
        _ => {}
    }

    let (fill, stroke, text_col) = chip_colors(th, chip, selected);
    let handle = chip_shell(ui, fill, stroke, true, |ui| {
        // The label is the drag/select handle, kept separate from the `×`
        // button so the button's click is never stolen by the drag sense.
        let handle = ui.add(
            egui::Label::new(RichText::new(&chip.text).color(text_col))
                .selectable(false)
                .sense(egui::Sense::click_and_drag()),
        );
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
        let payload = match (chip.is_base, chip.detach) {
            (false, Some(which)) => DragItem::Chip {
                path: path.to_vec(),
                which,
            },
            _ => DragItem::Row(path.to_vec()),
        };
        handle.dnd_set_drag_payload(payload);
    }
    if chip.is_base {
        if handle.double_clicked() {
            acts.push(Act::OpenWizard(path.to_vec()));
        } else if handle.clicked() {
            acts.push(Act::Select(path.to_vec()));
        }
    }
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
    let (fill, stroke, text_col) = chip_colors(th, chip, false);
    let handle = chip_shell(ui, fill, stroke, true, |ui| {
        let handle = ui.add(
            egui::Label::new(RichText::new("AS").color(text_col))
                .selectable(false)
                .sense(egui::Sense::click_and_drag()),
        );
        let id = ui.make_persistent_id(("pt_alias", path));
        if let Some(text) = inline_text_edit(ui, id, current, s.gui_report_alias_hint, 96.0)
            && text != current
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
        let payload = match chip.detach {
            Some(which) => DragItem::Chip {
                path: path.to_vec(),
                which,
            },
            None => DragItem::Row(path.to_vec()),
        };
        handle.dnd_set_drag_payload(payload);
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
    let (fill, stroke, text_col) = chip_colors(th, chip, selected);
    let mut picked: Option<String> = None;
    // A combo box already renders at the tallest chip height, so this chip does
    // not grow its row (which would only inflate the combo box further).
    let handle = chip_shell(ui, fill, stroke, false, |ui| {
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
        let cy = ui.min_rect().center().y;
        ui.painter().galley(
            egui::pos2(label_rect.left(), cy - gsize.y / 2.0),
            galley,
            text_col,
        );
        handle
    });

    if handle.dragged() {
        handle.dnd_set_drag_payload(DragItem::Row(path.to_vec()));
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
fn apply_block_actions(ed: &mut ReportEditor, app: &mut GuiApp, acts: Vec<Act>) {
    for act in acts {
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
            }
            Act::Delete => {
                let path = ed.selection.clone();
                ed.edit_flow(|flow| {
                    remove_node(flow, &path);
                });
                ed.selection = Vec::new();
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

    /// Render a single chip in isolation and return the height of the frame it
    /// draws. Runs a few frames so egui's sizing settles, and matches the GUI's
    /// enlarged `button_padding` so combo/text-field chips are measured at the
    /// same size they render at in the app.
    fn chip_height(build: impl Fn(&mut egui::Ui, &GuiTheme, &Strings, &mut Vec<Act>)) -> f32 {
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|s| s.spacing.button_padding = egui::vec2(8.0, 4.0));
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
        // All three must be the same height (uniform chips), within a sub-pixel
        // rounding tolerance.
        assert!(
            (label - combo).abs() < 0.5,
            "label {label} vs combo {combo}"
        );
        assert!(
            (label - alias).abs() < 0.5,
            "label {label} vs alias {alias}"
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
}
