//! GUI node-configure **wizards** — the friendly, form-based editors behind a
//! double-click on a report block, the direct analogue of the terminal UI's
//! `Overlay::ReportNode*` forms in [`crate::tui::report_nodes`]. Rather than
//! hand-editing a `REPORT REQUEST … SHOW(…) HIDE(…)` line or a
//! `FOR … IN ENVS BASELINE(…), COMPARISON(…)` clause, the user picks the request
//! name, ticks which fields to show, chooses baseline/comparison environments
//! from the loaded ones, and so on. Each wizard rebuilds the one node it edits
//! and commits it through [`ReportEditor::wizard_apply`], keeping the AST the
//! single source of truth exactly like every other block edit.
//!
//! egui gives us native checkboxes, combo boxes and radio buttons, so the same
//! forms the TUI drives with row-cycling key handlers are a fraction of the code
//! here.

use eframe::egui::{self, RichText};

use crate::report::context;
use crate::report::edit::node_at;
use crate::report::flow::{
    EnvClause, FlowNode, ParallelSpec, Pattern, Producer, ReportStmt, ResponseFmt, RoleRef,
    WithItem,
};
use crate::report::model::StatKind;

use super::app::GuiApp;
use super::report_editor::ReportEditor;

/// An open node-configure wizard, one variant per form-backed node kind.
pub enum Wizard {
    Request(RequestForm),
    Envs(EnvsForm),
    Files(FilesForm),
    Assign(AssignForm),
    List(ListForm),
    Folders(FoldersForm),
    /// `REPORT <var>` / `REPORT <var> AS <name>`: which in-scope variables become
    /// columns.
    Vars(VarsForm),
    /// `REPORT "<template>" AS <name>`: a computed column.
    Computed(ComputedForm),
    /// Fallback for kinds without a dedicated form (tuple-pattern loops, tuple
    /// list literals, exotic producers): edit the raw statement text.
    Raw(RawForm),
    /// One `name: query` field of a report request's `WITH … END` block.
    WithField(WithFieldForm),
}

/// The `REPORT <var>` form. The checklist covers the variables the static scan
/// can see; `other` is the escape hatch for names bound only at run time (by
/// `ZIP`/`TUPLES FROM`, or supplied by an environment file).
pub struct VarsForm {
    path: Vec<usize>,
    /// `(name, ticked)` over every variable in scope, in flow order.
    vars: Vec<(String, bool)>,
    other: String,
    /// The `AS` name. Only meaningful for a single variable — `REPORT (A, B)`
    /// emits two columns and so has no one name to give.
    alias: String,
    stats: Vec<(StatKind, bool)>,
    /// An `IMAGE(…)` render hint the statement already carries. The form
    /// doesn't expose it, but must not drop it when writing back.
    image: Option<crate::report::flow::ImageSpec>,
}

impl VarsForm {
    /// Every variable the form would report, ticked ones first then `other`.
    fn chosen(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .vars
            .iter()
            .filter(|(_, on)| *on)
            .map(|(n, _)| n.clone())
            .collect();
        let other = self.other.trim();
        if !other.is_empty() && !v.iter().any(|n| n == other) {
            v.push(other.to_string());
        }
        v
    }

    /// The node this form describes, or `None` when nothing is picked.
    fn node(&self) -> Option<FlowNode> {
        let chosen = self.chosen();
        let (first, rest) = chosen.split_first()?;
        let alias = self.alias.trim();
        // An alias applies to exactly one column, so a multi-variable pick
        // ignores it rather than silently dropping the extra variables.
        if rest.is_empty() && !alias.is_empty() {
            return Some(FlowNode::Report(ReportStmt::VarAs {
                var: first.clone(),
                name: alias.to_string(),
                stats: self
                    .stats
                    .iter()
                    .filter(|(_, on)| *on)
                    .map(|(k, _)| *k)
                    .collect(),
                image: self.image,
            }));
        }
        Some(FlowNode::Report(ReportStmt::Vars(chosen)))
    }
}

/// The `REPORT "<template>" AS <name>` form. Both halves are mandatory: the
/// statement will not re-parse without them.
pub struct ComputedForm {
    path: Vec<usize>,
    template: String,
    alias: String,
    stats: Vec<(StatKind, bool)>,
    /// Preserved verbatim, as in [`VarsForm`].
    image: Option<crate::report::flow::ImageSpec>,
}

/// The `VARIABLE = VALUE` (`Assign`) form: two plain text fields.
pub struct AssignForm {
    path: Vec<usize>,
    key: String,
    value: String,
}

/// The `LIST NAME = [ … ]` form: a name and one list-literal scalar per line.
/// Only a list-literal producer is edited here; any other producer falls back to
/// the [`RawForm`].
pub struct ListForm {
    path: Vec<usize>,
    name: String,
    /// The list elements, one per line (scalars; tuples are written `a, b`).
    values: String,
}

/// The `FOR … IN FOLDERS "dir"` form: loop variable, source folder and the
/// `PARALLEL` toggle. Any `WITH role="glob"` clauses are preserved verbatim.
pub struct FoldersForm {
    path: Vec<usize>,
    var: String,
    dir: String,
    parallel: bool,
    /// `PARALLEL`'s optional max-concurrency, kept as text so the box can be
    /// left blank (meaning "use the prelude's MAX_PARALLEL") and so half-typed
    /// input isn't clamped under the user's cursor.
    degree: String,
}

/// The generic fallback form: the node's single statement line, edited as text
/// and re-parsed on apply (the old inline line editor, now a modal).
pub struct RawForm {
    path: Vec<usize>,
    text: String,
    is_loop: bool,
}

/// The `WITH` *field* form: one `name: query` column of a report request's
/// `WITH … END` block. `index` is `Some` when editing an existing field and
/// `None` when adding a new one (appended on apply). The request node itself is
/// left in place — only the one field is added/rewritten.
pub struct WithFieldForm {
    path: Vec<usize>,
    index: Option<usize>,
    name: String,
    query: String,
    /// The `STATISTICS(…)` checklist: `(stat, on)` over every choosable stat, in
    /// [`StatKind::CHOOSABLE`] order. None ticked ⇒ no clause.
    stats: Vec<(StatKind, bool)>,
}

impl WithFieldForm {
    fn stats(&self) -> Vec<StatKind> {
        self.stats
            .iter()
            .filter(|(_, on)| *on)
            .map(|(k, _)| *k)
            .collect()
    }
}

/// The request form: pick the name, toggle whether it is *reported*, and — when
/// reported — its response format, `AS` alias and which fields it emits (`SHOW`).
pub struct RequestForm {
    path: Vec<usize>,
    name: String,
    /// Request titles from the bound collection (the name picker's choices).
    titles: Vec<String>,
    report: bool,
    response: Option<ResponseFmt>,
    alias: String,
    /// The `SHOW(…)` checklist: `(field, included)`. All ticked ⇒ no clause.
    fields: Vec<(String, bool)>,
    /// The `HIDE(…)` checklist over the same field list: `(field, hidden)`.
    /// None ticked ⇒ no clause. `HIDE` subtracts from whatever `SHOW` selected,
    /// so both lists cover the same names.
    hide_fields: Vec<(String, bool)>,
    /// Preserved verbatim across the edit (the form doesn't expose them).
    with: Vec<WithItem>,
}

impl RequestForm {
    /// The `SHOW(…)` list for the ticked rows; empty when every field is ticked
    /// (⇒ emit all, no clause).
    fn show(&self) -> Vec<String> {
        if self.fields.iter().all(|(_, on)| *on) {
            return Vec::new();
        }
        self.fields
            .iter()
            .filter(|(_, on)| *on)
            .map(|(n, _)| n.clone())
            .collect()
    }

    /// The `HIDE(…)` list for the ticked rows; empty ⇒ no clause.
    fn hide(&self) -> Vec<String> {
        self.hide_fields
            .iter()
            .filter(|(_, on)| *on)
            .map(|(n, _)| n.clone())
            .collect()
    }

    fn alias_opt(&self) -> Option<String> {
        let a = self.alias.trim();
        (!a.is_empty()).then(|| a.to_string())
    }

    /// The node this form describes.
    fn to_node(&self) -> FlowNode {
        if self.report {
            FlowNode::Report(ReportStmt::Request {
                name: self.name.trim().to_string(),
                alias: self.alias_opt(),
                response_fmt: self.response,
                show: self.show(),
                hide: self.hide(),
                with: self.with.clone(),
            })
        } else {
            FlowNode::Request {
                name: self.name.trim().to_string(),
            }
        }
    }
}

/// One chosen environment in an [`EnvsForm`]. `baseline` matters only in compare
/// mode (at most one is the baseline); `file` marks a `FILE("…")` snapshot.
pub struct EnvEntry {
    name: String,
    baseline: bool,
    file: bool,
}

/// The `FOR … IN ENVS` form: loop variable, iterate-vs-compare mode, `PARALLEL`,
/// and the baseline/comparison environments picked from the loaded ones.
pub struct EnvsForm {
    path: Vec<usize>,
    var: String,
    compare: bool,
    parallel: bool,
    /// `PARALLEL`'s optional max-concurrency, kept as text so the box can be
    /// left blank (meaning "use the prelude's MAX_PARALLEL") and so half-typed
    /// input isn't clamped under the user's cursor.
    degree: String,
    entries: Vec<EnvEntry>,
    /// Loaded environment names an entry cycles through.
    choices: Vec<String>,
    /// Discovered `.baseline` snapshot paths a `FILE(…)` entry cycles through.
    snapshots: Vec<String>,
    /// `BASELINE(…) SHOW(…)` as a checklist: every field the loop body's
    /// requests can emit, ticked when it is in `baseline_show`. Empty
    /// `baseline_show` means "no SHOW clause", which is NOT the same as "show
    /// everything" — so nothing is ticked by default.
    baseline_show_fields: Vec<(String, bool)>,
}

impl EnvsForm {
    fn var_or_default(&self) -> String {
        let v = self.var.trim();
        if v.is_empty() {
            "TARGET".to_string()
        } else {
            v.to_string()
        }
    }

    /// The ticked `BASELINE(…) SHOW(…)` fields, in the checklist's canonical
    /// order.
    ///
    /// Ticking nothing writes no `SHOW` clause at all — for a baseline that
    /// means "copy nothing across", which is the language's default, so there
    /// is no ambiguity with the request-level `SHOW` where empty means "show
    /// everything".
    fn selected_baseline_show(&self) -> Vec<String> {
        self.baseline_show_fields
            .iter()
            .filter(|(_, on)| *on)
            .map(|(n, _)| n.clone())
            .collect()
    }

    /// The [`EnvClause`] the current rows describe, or `None` when nothing is
    /// named (so the caller leaves the node unchanged rather than writing an
    /// unparseable empty clause).
    fn clause(&self) -> Option<EnvClause> {
        if self.compare {
            let refs = |want_baseline: bool| -> Vec<RoleRef> {
                self.entries
                    .iter()
                    .filter(|e| e.baseline == want_baseline && !e.name.trim().is_empty())
                    .map(|e| {
                        let name = e.name.trim().to_string();
                        if e.file {
                            RoleRef::File(name)
                        } else {
                            RoleRef::Env(name)
                        }
                    })
                    .collect()
            };
            let baseline = refs(true);
            let comparisons = refs(false);
            if baseline.is_empty() && comparisons.is_empty() {
                return None;
            }
            Some(EnvClause::Roles {
                baseline,
                comparisons,
                baseline_show: self.selected_baseline_show(),
            })
        } else {
            let names: Vec<String> = self
                .entries
                .iter()
                .map(|e| e.name.trim().to_string())
                .filter(|n| !n.is_empty())
                .collect();
            (!names.is_empty()).then_some(EnvClause::Plain(names))
        }
    }
}

/// The `FOR … IN FILES` form: loop variable, source folder, optional `MATCH`
/// glob and the `PARALLEL` toggle.
pub struct FilesForm {
    path: Vec<usize>,
    var: String,
    dir: String,
    glob: String,
    parallel: bool,
    /// `PARALLEL`'s optional max-concurrency, kept as text so the box can be
    /// left blank (meaning "use the prelude's MAX_PARALLEL") and so half-typed
    /// input isn't clamped under the user's cursor.
    degree: String,
}

impl FilesForm {
    fn var_or_default(&self) -> String {
        let v = self.var.trim();
        if v.is_empty() {
            "FILE".to_string()
        } else {
            v.to_string()
        }
    }

    fn glob_opt(&self) -> Option<String> {
        let g = self.glob.trim();
        (!g.is_empty()).then(|| g.to_string())
    }
}

// ---------------------------------------------------------------------------
// Opening a wizard for a node
// ---------------------------------------------------------------------------

/// Build the [`VarsForm`] for a reported-variable node. The candidate list needs
/// the bound collection so that the `[Captures]` of requests earlier in the flow
/// are offered alongside the flow's own assignments and loop binders.
fn build_vars(
    app: &GuiApp,
    flow: &crate::report::flow::ReportFlow,
    report_path: Option<&std::path::Path>,
    path: &[usize],
    node: &FlowNode,
) -> VarsForm {
    let (chosen, alias, stats, image) = match node {
        FlowNode::Report(ReportStmt::Vars(vars)) => (vars.clone(), String::new(), Vec::new(), None),
        FlowNode::Report(ReportStmt::VarAs {
            var,
            name,
            stats,
            image,
        }) => (vec![var.clone()], name.clone(), stats.clone(), *image),
        _ => (Vec::new(), String::new(), Vec::new(), None),
    };
    let entries = context::resolve_bound_collection(&app.session.collections, flow, report_path)
        .map(|ci| app.session.collections[ci].entries.as_slice())
        .unwrap_or(&[]);
    let in_scope = crate::report::edit::vars_in_scope(flow, path, entries);
    let mut vars: Vec<(String, bool)> = in_scope
        .iter()
        .map(|n| (n.clone(), chosen.contains(n)))
        .collect();
    // A variable already reported but no longer in scope still has to be
    // visible, or opening the form would quietly drop it.
    for c in &chosen {
        if !vars.iter().any(|(n, _)| n == c) {
            vars.push((c.clone(), true));
        }
    }
    VarsForm {
        path: path.to_vec(),
        vars,
        other: String::new(),
        alias,
        stats: StatKind::CHOOSABLE
            .iter()
            .map(|k| (*k, stats.contains(k)))
            .collect(),
        image,
    }
}

/// Open the appropriate configure wizard for the node at `path`. Requests, ENVS
/// loops, single-variable FILES loops, assignments, list literals and FOLDERS
/// loops, reported variables and computed columns each get a purpose-built form;
/// the handful of kinds with no palette entry (tuple-pattern loops, tuple list
/// literals, `ZIP`/`CONCAT`/`TUPLES FROM` producers) fall back to a raw
/// single-line editor, so no node is ever left without a way to edit it.
pub fn open(ed: &mut ReportEditor, app: &GuiApp, path: &[usize]) {
    let Some(flow) = ed.flow.clone() else {
        return;
    };
    let Some(node) = node_at(&flow, path).cloned() else {
        return;
    };
    let report_path = ed.report.path.clone();
    let wiz = match &node {
        FlowNode::Request { .. } | FlowNode::Report(ReportStmt::Request { .. }) => Wizard::Request(
            build_request(app, &flow, report_path.as_deref(), path.to_vec(), &node),
        ),
        FlowNode::ForEnvs { .. } => Wizard::Envs(build_envs(
            app,
            &flow,
            report_path.as_deref(),
            path.to_vec(),
            &node,
        )),
        FlowNode::ForEach {
            pattern,
            producer: Producer::Files { .. },
            ..
        } if single_named_binder(pattern).is_some() => {
            Wizard::Files(build_files(path.to_vec(), &node))
        }
        FlowNode::ForEach {
            pattern,
            producer: Producer::Folders { .. },
            ..
        } if single_named_binder(pattern).is_some() => {
            Wizard::Folders(build_folders(path.to_vec(), &node))
        }
        FlowNode::Assign { key, value } => Wizard::Assign(AssignForm {
            path: path.to_vec(),
            key: key.clone(),
            value: value.clone(),
        }),
        FlowNode::ListDecl {
            name,
            producer: Producer::List(elems),
        } if elems
            .iter()
            .all(|e| matches!(e, crate::report::flow::Element::Scalar(_))) =>
        {
            Wizard::List(ListForm {
                path: path.to_vec(),
                name: name.clone(),
                values: list_values_text(elems),
            })
        }
        FlowNode::Report(ReportStmt::Vars(_)) | FlowNode::Report(ReportStmt::VarAs { .. }) => {
            Wizard::Vars(build_vars(app, &flow, report_path.as_deref(), path, &node))
        }
        FlowNode::Report(ReportStmt::Computed {
            template,
            name,
            stats,
            image,
        }) => Wizard::Computed(ComputedForm {
            path: path.to_vec(),
            template: template.clone(),
            alias: name.clone(),
            image: *image,
            stats: StatKind::CHOOSABLE
                .iter()
                .map(|k| (*k, stats.contains(k)))
                .collect(),
        }),
        _ => Wizard::Raw(RawForm {
            path: path.to_vec(),
            text: node.header_line(),
            is_loop: node.is_loop(),
        }),
    };
    ed.wizard = Some(wiz);
}

/// Open the `WITH`-field wizard for the report request at `path`. `index` is
/// `Some` to edit an existing `name: query` field (seeding the form from it) and
/// `None` to add a new one. Only a report request has a `WITH` block, so any
/// other node (or an out-of-range / non-`Field` index) opens an empty new-field
/// form defensively rather than doing nothing.
pub fn open_with_field(ed: &mut ReportEditor, path: &[usize], index: Option<usize>) {
    let existing = index.and_then(|i| {
        ed.flow
            .as_ref()
            .and_then(|f| node_at(f, path))
            .and_then(|n| match n {
                FlowNode::Report(ReportStmt::Request { with, .. }) => with.get(i).cloned(),
                _ => None,
            })
    });
    // The `IMAGE(…)` hint isn't edited here; `set_with_field` leaves the one
    // already on the item alone, so the form has no need to carry it.
    let (index, name, query, stats) = match existing {
        Some(WithItem::Field {
            name, query, stats, ..
        }) => (index, name, query, stats),
        // Editing a non-field (bare `WITH RESPONSE`) or a stale index falls
        // through to a fresh append rather than silently doing nothing.
        _ => (None, String::new(), String::new(), Vec::new()),
    };
    ed.wizard = Some(Wizard::WithField(WithFieldForm {
        path: path.to_vec(),
        index,
        name,
        query,
        stats: StatKind::CHOOSABLE
            .iter()
            .map(|k| (*k, stats.contains(k)))
            .collect(),
    }));
}

/// The single *named* binder of a `FOR X IN …` pattern, if the pattern is
/// exactly one named binder. `FOR _ IN …` (a discard) and multi-binder patterns
/// return `None`, so they route to the raw editor rather than a form that would
/// silently rename or flatten them.
fn single_named_binder(pattern: &Pattern) -> Option<&str> {
    if pattern.is_single() {
        pattern.named().next()
    } else {
        None
    }
}

/// Render list-literal *scalar* elements as one line each. Only called for
/// all-scalar lists (tuples route to the raw editor), so this round-trips
/// losslessly with [`parse_list_values`].
fn list_values_text(elems: &[crate::report::flow::Element]) -> String {
    use crate::report::flow::Element;
    elems
        .iter()
        .map(|e| match e {
            Element::Scalar(s) => s.clone(),
            // Unreachable for the List form (guarded to all-scalar), but keep a
            // sensible rendering rather than panicking.
            Element::Tuple(parts) => parts.join(", "),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Parse the multiline values box back into scalar list elements: one element
/// per non-empty line, taken verbatim (no comma splitting), so a scalar is never
/// silently turned into a tuple.
fn parse_list_values(text: &str) -> Vec<crate::report::flow::Element> {
    use crate::report::flow::Element;
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| Element::Scalar(l.to_string()))
        .collect()
}

/// Render `PARALLEL`'s max-concurrency box beside its checkbox. Disabled (but
/// still visible) while the loop is serial, so the setting is discoverable
/// without the checkbox row jumping in size as it is toggled.
fn parallel_row(
    ui: &mut egui::Ui,
    th: &super::theme::GuiTheme,
    s: &crate::i18n::Strings,
    parallel: &mut bool,
    degree: &mut String,
) {
    ui.horizontal(|ui| {
        ui.checkbox(
            &mut *parallel,
            RichText::new(s.report_node_parallel_label).color(th.text),
        );
        ui.add_enabled_ui(*parallel, |ui| {
            ui.add(
                egui::TextEdit::singleline(degree)
                    .hint_text(s.node_form_parallel_degree)
                    .desired_width(48.0),
            );
            ui.label(RichText::new(s.node_form_parallel_degree_label).color(th.dim));
        });
    });
}

/// The text shown in the max-concurrency box for an existing `PARALLEL` spec —
/// blank for a plain `PARALLEL` (no explicit limit) or a serial loop.
fn degree_text(parallel: &Option<ParallelSpec>) -> String {
    parallel
        .as_ref()
        .and_then(|p| p.degree)
        .map(|n| n.to_string())
        .unwrap_or_default()
}

/// Turn the typed max-concurrency into a `ParallelSpec`, preserving `existing`
/// when the loop stays serial is not the case. Anything that isn't a positive
/// integer (including a blank box) becomes "no explicit limit", which is the
/// plain `PARALLEL` form — the parser rejects `PARALLEL(0)`, so a 0 must never
/// reach the flow.
fn parallel_spec(on: bool, degree: &str) -> Option<ParallelSpec> {
    on.then(|| ParallelSpec {
        degree: degree.trim().parse::<u32>().ok().filter(|n| *n > 0),
    })
}

fn build_folders(path: Vec<usize>, node: &FlowNode) -> FoldersForm {
    let (var, dir, parallel) = match node {
        FlowNode::ForEach {
            pattern,
            producer: Producer::Folders { dir, .. },
            parallel,
            ..
        } => (
            pattern.named().next().unwrap_or("FOLDER").to_string(),
            dir.clone(),
            *parallel,
        ),
        _ => unreachable!("build_folders called on a non-FOLDERS node"),
    };
    FoldersForm {
        path,
        var,
        dir,
        parallel: parallel.is_some(),
        degree: degree_text(&parallel),
    }
}

fn build_request(
    app: &GuiApp,
    flow: &crate::report::flow::ReportFlow,
    report_path: Option<&std::path::Path>,
    path: Vec<usize>,
    node: &FlowNode,
) -> RequestForm {
    let (name, report, alias, response, show, hide, with) = match node {
        FlowNode::Request { name } => (
            name.clone(),
            false,
            None,
            None,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ),
        FlowNode::Report(ReportStmt::Request {
            name,
            alias,
            response_fmt,
            show,
            hide,
            with,
        }) => (
            name.clone(),
            true,
            alias.clone(),
            *response_fmt,
            show.clone(),
            hide.clone(),
            with.clone(),
        ),
        _ => unreachable!("build_request called on a non-request node"),
    };

    let titles: Vec<String> =
        context::resolve_bound_collection(&app.session.collections, flow, report_path)
            .map(|ci| {
                app.session.collections[ci]
                    .entries
                    .iter()
                    .map(|e| e.title.clone())
                    .collect()
            })
            .unwrap_or_default();

    // The fields the request can emit, in canonical order: intrinsics, the
    // request's own `[Reports]` fields, then this node's `WITH` fields, then any
    // unknown `SHOW` entry (so applying can't silently drop it), de-duplicated.
    let report_fields =
        context::resolve_bound_collection(&app.session.collections, flow, report_path)
            .and_then(|ci| {
                crate::report::run::resolve_title(&app.session.collections[ci].entries, &name)
                    .map(|e| e.reports.iter().map(|(f, _)| f.clone()).collect::<Vec<_>>())
            })
            .unwrap_or_default();
    let mut names: Vec<String> = Vec::new();
    let push = |n: &str, names: &mut Vec<String>| {
        if !names.iter().any(|x| x == n) {
            names.push(n.to_string());
        }
    };
    for f in crate::report::run::INTRINSIC_FIELDS {
        push(f, &mut names);
    }
    for f in &report_fields {
        push(f, &mut names);
    }
    for w in &with {
        if let WithItem::Field { name, .. } = w {
            push(name, &mut names);
        }
    }
    for f in &show {
        push(f, &mut names);
    }
    // A HIDE entry naming a field nothing else knows about still has to appear
    // in the checklist, or applying the form would silently drop it.
    for f in &hide {
        push(f, &mut names);
    }
    let all = show.is_empty();
    let fields = names
        .iter()
        .map(|n| {
            let on = all || show.iter().any(|s| s == n);
            (n.clone(), on)
        })
        .collect();
    let hide_fields = names
        .iter()
        .map(|n| (n.clone(), hide.iter().any(|h| h == n)))
        .collect();

    RequestForm {
        path,
        name,
        titles,
        report,
        response,
        alias: alias.unwrap_or_default(),
        fields,
        hide_fields,
        with,
    }
}

fn build_envs(
    app: &GuiApp,
    flow: &crate::report::flow::ReportFlow,
    report_path: Option<&std::path::Path>,
    path: Vec<usize>,
    node: &FlowNode,
) -> EnvsForm {
    let (var, clause, parallel, body) = match node {
        FlowNode::ForEnvs {
            var,
            clause,
            parallel,
            body,
        } => (var.clone(), clause.clone(), *parallel, body.as_slice()),
        _ => unreachable!("build_envs called on a non-ENVS node"),
    };

    let (compare, mut entries, baseline_show) = match &clause {
        EnvClause::Plain(names) => (
            false,
            names
                .iter()
                .map(|n| EnvEntry {
                    name: n.clone(),
                    baseline: false,
                    file: false,
                })
                .collect::<Vec<_>>(),
            Vec::new(),
        ),
        EnvClause::Roles {
            baseline,
            comparisons,
            baseline_show,
        } => {
            let entry = |r: &RoleRef, is_baseline: bool| EnvEntry {
                name: r.target().to_string(),
                baseline: is_baseline,
                file: matches!(r, RoleRef::File(_)),
            };
            let mut es: Vec<EnvEntry> = baseline.iter().map(|r| entry(r, true)).collect();
            es.extend(comparisons.iter().map(|r| entry(r, false)));
            (true, es, baseline_show.clone())
        }
    };

    let choices: Vec<String> = app
        .session
        .global_envs
        .iter()
        .map(|e| e.name.clone())
        .collect();
    let mut snapshots = discover_snapshots(flow, report_path);
    for e in &entries {
        if e.file && !e.name.trim().is_empty() && !snapshots.iter().any(|s| s == &e.name) {
            snapshots.push(e.name.clone());
        }
    }
    if entries.is_empty() {
        entries.push(EnvEntry {
            name: choices.first().cloned().unwrap_or_default(),
            baseline: compare,
            file: false,
        });
    }

    let bound_entries =
        context::resolve_bound_collection(&app.session.collections, flow, report_path)
            .map(|ci| app.session.collections[ci].entries.as_slice())
            .unwrap_or(&[]);
    let baseline_show_fields =
        crate::report::edit::baseline_show_choices(bound_entries, body, &baseline_show);

    EnvsForm {
        path,
        var,
        compare,
        parallel: parallel.is_some(),
        degree: degree_text(&parallel),
        entries,
        choices,
        snapshots,
        baseline_show_fields,
    }
}

fn build_files(path: Vec<usize>, node: &FlowNode) -> FilesForm {
    let (var, dir, glob, parallel) = match node {
        FlowNode::ForEach {
            pattern,
            producer: Producer::Files { dir, glob },
            parallel,
            ..
        } => (
            pattern.named().next().unwrap_or("FILE").to_string(),
            dir.clone(),
            glob.clone().unwrap_or_default(),
            *parallel,
        ),
        _ => unreachable!("build_files called on a non-FILES node"),
    };
    FilesForm {
        path,
        var,
        dir,
        glob,
        parallel: parallel.is_some(),
        degree: degree_text(&parallel),
    }
}

/// The `.baseline` snapshot file names in the report's root directory, relative
/// to it — the candidates a `FILE(…)` role entry picks from. Empty on any I/O
/// error (the form then simply offers no snapshots).
fn discover_snapshots(
    flow: &crate::report::flow::ReportFlow,
    report_path: Option<&std::path::Path>,
) -> Vec<String> {
    let (root, _) = context::report_base_dir(flow, report_path);
    let mut out: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&root) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().is_some_and(|e| e == "baseline")
                && let Some(name) = p.file_name().and_then(|n| n.to_str())
            {
                out.push(name.to_string());
            }
        }
    }
    out.sort();
    out
}

// ---------------------------------------------------------------------------
// Rendering the modal
// ---------------------------------------------------------------------------

/// What the modal's buttons (or a click-away / Escape) decided this frame.
enum Outcome {
    None,
    Apply,
    Cancel,
}

/// Draw the open wizard (if any) as a modal over the editor, applying or
/// cancelling it. A no-op when no wizard is open.
pub fn show(ed: &mut ReportEditor, app: &mut GuiApp, ctx: &egui::Context) {
    if ed.wizard.is_none() {
        return;
    }
    let th = app.theme;
    // Where a folder picker opens when the form's own field is still blank.
    let fallback = app
        .session
        .picker_dir(crate::session::PickerKind::Other)
        .map(|p| p.to_path_buf());
    let s = &app.strings;
    let mut outcome = Outcome::None;

    let modal = egui::Modal::new(egui::Id::new("pt_node_wizard")).show(ctx, |ui| {
        ui.set_min_width(360.0);
        let wiz = ed.wizard.as_mut().expect("wizard is Some");
        match wiz {
            Wizard::Request(f) => request_ui(ui, &th, s, f),
            Wizard::Envs(f) => envs_ui(ui, &th, s, f),
            Wizard::Files(f) => files_ui(ui, &th, s, f, fallback.as_deref()),
            Wizard::Assign(f) => assign_ui(ui, &th, s, f),
            Wizard::List(f) => list_ui(ui, &th, s, f),
            Wizard::Folders(f) => folders_ui(ui, &th, s, f, fallback.as_deref()),
            Wizard::Vars(f) => vars_ui(ui, &th, s, f),
            Wizard::Computed(f) => computed_ui(ui, &th, s, f),
            Wizard::Raw(f) => raw_ui(ui, &th, s, f),
            Wizard::WithField(f) => with_field_ui(ui, &th, s, f),
        }
        ui.add_space(8.0);
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button(RichText::new(s.gui_ok).color(th.text)).clicked() {
                outcome = Outcome::Apply;
            }
            if ui
                .button(RichText::new(s.gui_cancel).color(th.text))
                .clicked()
            {
                outcome = Outcome::Cancel;
            }
        });
    });
    if modal.should_close() {
        outcome = Outcome::Cancel;
    }

    match outcome {
        Outcome::None => {}
        Outcome::Cancel => ed.wizard = None,
        Outcome::Apply => {
            apply(ed, app);
            ed.wizard = None;
        }
    }
}

fn request_ui(
    ui: &mut egui::Ui,
    th: &super::theme::GuiTheme,
    s: &crate::i18n::Strings,
    f: &mut RequestForm,
) {
    ui.heading(RichText::new(s.node_request_title).color(th.text));
    ui.add_space(4.0);
    egui::Grid::new("pt_req_grid")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label(RichText::new(s.node_form_name).color(th.dim));
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut f.name).desired_width(180.0));
                if !f.titles.is_empty() {
                    egui::ComboBox::from_id_salt("pt_req_name_pick")
                        .selected_text(RichText::new("…").color(th.dim))
                        .show_ui(ui, |ui| {
                            for t in f.titles.clone() {
                                if ui.selectable_label(f.name == t, &t).clicked() {
                                    f.name = t;
                                }
                            }
                        });
                }
            });
            ui.end_row();

            ui.label(RichText::new(s.node_form_report).color(th.dim));
            ui.checkbox(
                &mut f.report,
                RichText::new(s.node_form_report_hint).color(th.text),
            );
            ui.end_row();

            if f.report {
                ui.label(RichText::new(s.node_form_response).color(th.dim));
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut f.response, None, s.node_form_response_default);
                    ui.selectable_value(&mut f.response, Some(ResponseFmt::Raw), "RAW");
                    ui.selectable_value(&mut f.response, Some(ResponseFmt::Pretty), "PRETTY");
                });
                ui.end_row();

                ui.label(RichText::new(s.node_form_alias).color(th.dim));
                ui.add(egui::TextEdit::singleline(&mut f.alias).desired_width(180.0));
                ui.end_row();
            }
        });

    if f.report && !f.fields.is_empty() {
        ui.add_space(6.0);
        ui.label(RichText::new(s.node_form_show).color(th.dim));
        egui::ScrollArea::vertical()
            .id_salt("pt_req_fields")
            .max_height(160.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for (name, on) in &mut f.fields {
                    ui.checkbox(on, RichText::new(name.as_str()).color(th.text));
                }
            });

        ui.add_space(6.0);
        ui.label(RichText::new(s.node_form_hide).color(th.dim));
        ui.label(RichText::new(s.node_form_hide_hint).color(th.dim));
        egui::ScrollArea::vertical()
            .id_salt("pt_req_hide_fields")
            .max_height(120.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for (name, on) in &mut f.hide_fields {
                    ui.checkbox(on, RichText::new(name.as_str()).color(th.text));
                }
            });
    }
}

fn envs_ui(
    ui: &mut egui::Ui,
    th: &super::theme::GuiTheme,
    s: &crate::i18n::Strings,
    f: &mut EnvsForm,
) {
    ui.heading(RichText::new(s.report_node_envs_title).color(th.text));
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new(s.report_node_envs_var_label).color(th.dim));
        ui.add(egui::TextEdit::singleline(&mut f.var).desired_width(160.0));
    });
    ui.horizontal(|ui| {
        ui.label(RichText::new(s.report_node_envs_mode_label).color(th.dim));
        let mut compare = f.compare;
        ui.selectable_value(&mut compare, false, s.report_node_envs_mode_plain);
        ui.selectable_value(&mut compare, true, s.report_node_envs_mode_roles);
        if compare != f.compare {
            f.compare = compare;
            // Entering compare with no baseline promotes the first entry so a
            // comparison always has a reference.
            if f.compare
                && !f.entries.iter().any(|e| e.baseline)
                && let Some(first) = f.entries.first_mut()
            {
                first.baseline = true;
            }
        }
    });
    parallel_row(ui, th, s, &mut f.parallel, &mut f.degree);

    ui.add_space(6.0);
    ui.label(RichText::new(s.node_envs_environments).color(th.dim));
    let mut remove: Option<usize> = None;
    let mut make_baseline: Option<usize> = None;
    let mut toggled_file: Option<usize> = None;
    // Iterate by index over cloned pick-lists so the row closures can mutate the
    // matching entry without also borrowing the rest of `f`.
    let compare = f.compare;
    let count = f.entries.len();
    let choices = f.choices.clone();
    let snapshots = f.snapshots.clone();
    for i in 0..count {
        ui.horizontal(|ui| {
            let list = if f.entries[i].file {
                &snapshots
            } else {
                &choices
            };
            let selected = f.entries[i].name.clone();
            egui::ComboBox::from_id_salt(("pt_env_pick", i))
                .selected_text(if selected.is_empty() {
                    RichText::new("—").color(th.dim)
                } else {
                    RichText::new(selected.as_str()).color(th.text)
                })
                .show_ui(ui, |ui| {
                    for c in list.clone() {
                        if ui.selectable_label(f.entries[i].name == c, &c).clicked() {
                            f.entries[i].name = c;
                        }
                    }
                });
            if compare {
                if ui
                    .radio(f.entries[i].baseline, s.report_node_envs_baseline)
                    .clicked()
                {
                    make_baseline = Some(i);
                }
                if ui
                    .checkbox(&mut f.entries[i].file, s.report_node_envs_file)
                    .changed()
                {
                    toggled_file = Some(i);
                }
            }
            if count > 1 && ui.button(RichText::new("×").color(th.err)).clicked() {
                remove = Some(i);
            }
        });
        // The `SHOW` belongs to the BASELINE role, not to the loop, so it is
        // drawn as an indented, boxed continuation of the baseline's own row —
        // close enough to read as part of it, and impossible to mistake for
        // something governing the comparisons listed below.
        if compare && f.entries[i].baseline {
            baseline_show_ui(ui, th, s, &mut f.baseline_show_fields);
        }
    }
    if let Some(i) = make_baseline {
        for (j, e) in f.entries.iter_mut().enumerate() {
            e.baseline = j == i;
        }
    }
    if let Some(i) = toggled_file {
        // Switching a role between a live env and a snapshot resets its value to
        // the first item of the newly relevant list so it starts valid.
        let list = if f.entries[i].file {
            &f.snapshots
        } else {
            &f.choices
        };
        if !list.iter().any(|c| c == &f.entries[i].name)
            && let Some(first) = list.first().cloned()
        {
            f.entries[i].name = first;
        }
    }
    if let Some(i) = remove {
        f.entries.remove(i);
    }
    if ui
        .button(RichText::new(format!("{} {}", super::icons::PLUS, s.node_envs_add)).color(th.text))
        .clicked()
    {
        f.entries.push(EnvEntry {
            name: f.choices.first().cloned().unwrap_or_default(),
            baseline: false,
            file: false,
        });
    }
}

fn files_ui(
    ui: &mut egui::Ui,
    th: &super::theme::GuiTheme,
    s: &crate::i18n::Strings,
    f: &mut FilesForm,
    fallback: Option<&std::path::Path>,
) {
    ui.heading(RichText::new(s.report_node_files_title).color(th.text));
    ui.add_space(4.0);
    egui::Grid::new("pt_files_grid")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label(RichText::new(s.report_node_files_var_label).color(th.dim));
            ui.add(egui::TextEdit::singleline(&mut f.var).desired_width(220.0));
            ui.end_row();
            ui.label(RichText::new(s.report_node_files_folder_label).color(th.dim));
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut f.dir).desired_width(220.0));
                if ui.button(s.gui_browse).clicked() {
                    if let Some(p) = super::filepick::pick_folder(
                        s.report_node_files_folder_label,
                        super::filepick::seed_dir(&f.dir).as_deref().or(fallback),
                    ) {
                        f.dir = p.to_string_lossy().into_owned();
                    }
                }
            });
            ui.end_row();
            ui.label(RichText::new(s.report_node_files_match_label).color(th.dim));
            ui.add(egui::TextEdit::singleline(&mut f.glob).desired_width(220.0));
            ui.end_row();
        });
    parallel_row(ui, th, s, &mut f.parallel, &mut f.degree);
}

fn assign_ui(
    ui: &mut egui::Ui,
    th: &super::theme::GuiTheme,
    s: &crate::i18n::Strings,
    f: &mut AssignForm,
) {
    ui.heading(RichText::new(s.node_assign_title).color(th.text));
    ui.add_space(4.0);
    egui::Grid::new("pt_assign_grid")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label(RichText::new(s.node_form_var).color(th.dim));
            ui.add(egui::TextEdit::singleline(&mut f.key).desired_width(220.0));
            ui.end_row();
            ui.label(RichText::new(s.node_form_value).color(th.dim));
            ui.add(egui::TextEdit::singleline(&mut f.value).desired_width(220.0));
            ui.end_row();
        });
}

fn list_ui(
    ui: &mut egui::Ui,
    th: &super::theme::GuiTheme,
    s: &crate::i18n::Strings,
    f: &mut ListForm,
) {
    ui.heading(RichText::new(s.node_list_title).color(th.text));
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new(s.node_form_list_name).color(th.dim));
        ui.add(egui::TextEdit::singleline(&mut f.name).desired_width(200.0));
    });
    ui.add_space(6.0);
    ui.label(RichText::new(s.node_form_list_values).color(th.dim));
    ui.add(
        egui::TextEdit::multiline(&mut f.values)
            .desired_width(f32::INFINITY)
            .desired_rows(5)
            .font(egui::TextStyle::Monospace),
    );
}

fn folders_ui(
    ui: &mut egui::Ui,
    th: &super::theme::GuiTheme,
    s: &crate::i18n::Strings,
    f: &mut FoldersForm,
    fallback: Option<&std::path::Path>,
) {
    ui.heading(RichText::new(s.node_folders_title).color(th.text));
    ui.add_space(4.0);
    egui::Grid::new("pt_folders_grid")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label(RichText::new(s.report_node_files_var_label).color(th.dim));
            ui.add(egui::TextEdit::singleline(&mut f.var).desired_width(220.0));
            ui.end_row();
            ui.label(RichText::new(s.report_node_files_folder_label).color(th.dim));
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut f.dir).desired_width(220.0));
                if ui.button(s.gui_browse).clicked() {
                    if let Some(p) = super::filepick::pick_folder(
                        s.report_node_files_folder_label,
                        super::filepick::seed_dir(&f.dir).as_deref().or(fallback),
                    ) {
                        f.dir = p.to_string_lossy().into_owned();
                    }
                }
            });
            ui.end_row();
        });
    parallel_row(ui, th, s, &mut f.parallel, &mut f.degree);
}

fn raw_ui(
    ui: &mut egui::Ui,
    th: &super::theme::GuiTheme,
    s: &crate::i18n::Strings,
    f: &mut RawForm,
) {
    ui.heading(RichText::new(s.node_raw_title).color(th.text));
    ui.add_space(4.0);
    ui.label(RichText::new(s.node_form_raw).color(th.dim));
    ui.add(
        egui::TextEdit::multiline(&mut f.text)
            .desired_width(f32::INFINITY)
            .desired_rows(2)
            .font(egui::TextStyle::Monospace),
    );
}

/// Draw the statistics checklist shared by the `WITH`-field, reported-variable
/// and computed-column forms. Laid out in columns because nine stacked
/// checkboxes would push the OK / Cancel row off a short window.
fn stats_ui(
    ui: &mut egui::Ui,
    th: &super::theme::GuiTheme,
    s: &crate::i18n::Strings,
    stats: &mut [(StatKind, bool)],
) {
    ui.label(RichText::new(s.node_form_statistics).color(th.dim));
    ui.label(RichText::new(s.node_form_statistics_hint).color(th.dim));
    ui.horizontal_top(|ui| {
        for chunk in stats.chunks_mut(5) {
            ui.vertical(|ui| {
                for (kind, on) in chunk {
                    ui.checkbox(on, RichText::new(kind.label()).color(th.text));
                }
            });
        }
    });
}

fn vars_ui(
    ui: &mut egui::Ui,
    th: &super::theme::GuiTheme,
    s: &crate::i18n::Strings,
    f: &mut VarsForm,
) {
    ui.heading(RichText::new(s.report_node_vars_title).color(th.text));
    ui.add_space(4.0);
    if f.vars.is_empty() {
        ui.label(RichText::new(s.report_node_vars_none).color(th.dim));
    }
    for (name, on) in &mut f.vars {
        ui.checkbox(on, RichText::new(name.as_str()).color(th.text));
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new(s.report_node_vars_other_label).color(th.dim));
        ui.add(egui::TextEdit::singleline(&mut f.other).desired_width(200.0));
    });
    // A single column can be renamed and summarised; several cannot, so the
    // rows disappear rather than sit there doing nothing.
    if f.chosen().len() == 1 {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(s.report_node_alias_label).color(th.dim));
            ui.add(egui::TextEdit::singleline(&mut f.alias).desired_width(200.0));
        });
        if !f.alias.trim().is_empty() {
            ui.add_space(6.0);
            stats_ui(ui, th, s, &mut f.stats);
        }
    }
}

fn computed_ui(
    ui: &mut egui::Ui,
    th: &super::theme::GuiTheme,
    s: &crate::i18n::Strings,
    f: &mut ComputedForm,
) {
    ui.heading(RichText::new(s.report_node_computed_title).color(th.text));
    ui.add_space(4.0);
    egui::Grid::new("pt_computed_grid")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label(RichText::new(s.report_node_computed_template_label).color(th.dim));
            ui.add(
                egui::TextEdit::singleline(&mut f.template)
                    .hint_text(s.report_node_computed_template_hint)
                    .desired_width(240.0)
                    .font(egui::TextStyle::Monospace),
            );
            ui.end_row();
            ui.label(RichText::new(s.report_node_computed_name_label).color(th.dim));
            ui.add(egui::TextEdit::singleline(&mut f.alias).desired_width(240.0));
            ui.end_row();
        });
    ui.add_space(6.0);
    stats_ui(ui, th, s, &mut f.stats);
}

fn with_field_ui(
    ui: &mut egui::Ui,
    th: &super::theme::GuiTheme,
    s: &crate::i18n::Strings,
    f: &mut WithFieldForm,
) {
    ui.heading(RichText::new(s.node_with_title).color(th.text));
    ui.add_space(4.0);
    egui::Grid::new("pt_with_grid")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label(RichText::new(s.node_with_name).color(th.dim));
            ui.add(egui::TextEdit::singleline(&mut f.name).desired_width(220.0));
            ui.end_row();

            ui.label(RichText::new(s.node_with_query).color(th.dim));
            ui.add(
                egui::TextEdit::singleline(&mut f.query)
                    .hint_text(s.node_with_query_hint)
                    .desired_width(220.0)
                    .font(egui::TextStyle::Monospace),
            );
            ui.end_row();
        });

    ui.add_space(6.0);
    stats_ui(ui, th, s, &mut f.stats);
}

fn apply(ed: &mut ReportEditor, app: &mut GuiApp) {
    let Some(wiz) = ed.wizard.as_ref() else {
        return;
    };
    match wiz {
        Wizard::Request(f) => {
            let path = f.path.clone();
            let node = f.to_node();
            ed.wizard_apply(app, &path, node);
        }
        Wizard::Envs(f) => {
            let Some(clause) = f.clause() else {
                return;
            };
            let path = f.path.clone();
            // Preserve the node's body; the var/clause and the PARALLEL
            // toggle and degree are what this form edits.
            let body = match ed.flow.as_ref().and_then(|fl| node_at(fl, &path)) {
                Some(FlowNode::ForEnvs { body, .. }) => body.clone(),
                _ => return,
            };
            let parallel = parallel_spec(f.parallel, &f.degree);
            let node = FlowNode::ForEnvs {
                var: f.var_or_default(),
                clause,
                body,
                parallel,
            };
            ed.wizard_apply(app, &path, node);
        }
        Wizard::Files(f) => {
            let path = f.path.clone();
            let body = match ed.flow.as_ref().and_then(|fl| node_at(fl, &path)) {
                Some(FlowNode::ForEach { body, .. }) => body.clone(),
                _ => return,
            };
            let parallel = parallel_spec(f.parallel, &f.degree);
            let node = FlowNode::ForEach {
                pattern: Pattern::single(f.var_or_default()),
                producer: Producer::Files {
                    dir: f.dir.clone(),
                    glob: f.glob_opt(),
                },
                body,
                parallel,
            };
            ed.wizard_apply(app, &path, node);
        }
        Wizard::Assign(f) => {
            let path = f.path.clone();
            let key = f.key.trim();
            if key.is_empty() {
                return;
            }
            let node = FlowNode::Assign {
                key: key.to_string(),
                value: f.value.clone(),
            };
            ed.wizard_apply(app, &path, node);
        }
        Wizard::List(f) => {
            let path = f.path.clone();
            let name = f.name.trim();
            if name.is_empty() {
                return;
            }
            let node = FlowNode::ListDecl {
                name: name.to_string(),
                producer: Producer::List(parse_list_values(&f.values)),
            };
            ed.wizard_apply(app, &path, node);
        }
        Wizard::Folders(f) => {
            let path = f.path.clone();
            // Preserve the loop body and any WITH role globs; var/dir and the
            // PARALLEL toggle and degree are what this form edits.
            let (body, glob, roles) = match ed.flow.as_ref().and_then(|fl| node_at(fl, &path)) {
                Some(FlowNode::ForEach {
                    body,
                    producer: Producer::Folders { glob, roles, .. },
                    ..
                }) => (body.clone(), glob.clone(), roles.clone()),
                _ => return,
            };
            let parallel = parallel_spec(f.parallel, &f.degree);
            let node = FlowNode::ForEach {
                pattern: Pattern::single(if f.var.trim().is_empty() {
                    "FOLDER".to_string()
                } else {
                    f.var.trim().to_string()
                }),
                producer: Producer::Folders {
                    dir: f.dir.clone(),
                    glob,
                    roles,
                },
                body,
                parallel,
            };
            ed.wizard_apply(app, &path, node);
        }
        Wizard::Vars(f) => {
            let path = f.path.clone();
            let Some(node) = f.node() else { return };
            ed.wizard_apply(app, &path, node);
        }
        Wizard::Computed(f) => {
            let path = f.path.clone();
            let (template, alias) = (f.template.trim(), f.alias.trim());
            // Either half missing and the statement would not re-parse, which
            // would eject the user from the node editor.
            if template.is_empty() || alias.is_empty() {
                return;
            }
            let node = FlowNode::Report(ReportStmt::Computed {
                template: template.to_string(),
                name: alias.to_string(),
                stats: f
                    .stats
                    .iter()
                    .filter(|(_, on)| *on)
                    .map(|(k, _)| *k)
                    .collect(),
                image: f.image,
            });
            ed.wizard_apply(app, &path, node);
        }
        Wizard::Raw(f) => {
            let path = f.path.clone();
            if let Some(node) = crate::report::edit::parse_one_node(&f.text, f.is_loop) {
                ed.wizard_apply(app, &path, node);
            }
        }
        Wizard::WithField(f) => {
            let path = f.path.clone();
            let name = f.name.trim().to_string();
            let query = f.query.trim().to_string();
            // A field needs both a column name and a query; an incomplete form
            // is dropped rather than writing a broken `WITH` item.
            if name.is_empty() || query.is_empty() {
                return;
            }
            let index = f.index;
            let stats = f.stats();
            ed.commit_edit(app, |flow| match index {
                Some(i) => {
                    crate::report::edit::set_with_field(flow, &path, i, &name, &query, stats);
                }
                None => {
                    crate::report::edit::add_with_field(flow, &path, &name, &query, stats);
                }
            });
            ed.selection = path;
        }
    }
}

/// The `BASELINE(…) SHOW(…)` checklist, drawn indented and boxed beneath the
/// baseline it belongs to.
///
/// Kept a free function taking only the checklist so it can be exercised
/// without building a whole [`EnvsForm`], and so the caller can render it
/// mid-loop without holding a borrow on the rest of the form.
fn baseline_show_ui(
    ui: &mut egui::Ui,
    th: &super::theme::GuiTheme,
    s: &crate::i18n::Strings,
    fields: &mut [(String, bool)],
) {
    if fields.is_empty() {
        return;
    }
    ui.horizontal(|ui| {
        // The indent is the visual tie to the baseline row above it.
        ui.add_space(24.0);
        egui::Frame::new()
            .fill(th.panel)
            .stroke(egui::Stroke::new(1.0, th.dim))
            .corner_radius(4.0)
            .inner_margin(egui::Margin::symmetric(8, 6))
            .show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(s.node_envs_baseline_show).color(th.accent));
                        ui.label(
                            RichText::new(format!("({})", s.node_envs_baseline_show_applies))
                                .italics()
                                .color(th.dim),
                        );
                    });
                    ui.label(RichText::new(s.node_envs_baseline_show_hint).color(th.dim));
                    egui::ScrollArea::vertical()
                        .id_salt("pt_envs_baseline_show")
                        .max_height(140.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            for (name, on) in fields.iter_mut() {
                                ui.checkbox(on, RichText::new(name.as_str()).color(th.text));
                            }
                        });
                });
            });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::flow::{EnvClause, RoleRef};

    /// An ENVS form in compare mode with the given `SHOW` checklist.
    fn compare_form(fields: Vec<(String, bool)>) -> EnvsForm {
        EnvsForm {
            path: vec![0],
            var: "TARGET".into(),
            compare: true,
            parallel: false,
            degree: String::new(),
            entries: vec![
                EnvEntry {
                    name: "prod".into(),
                    baseline: true,
                    file: false,
                },
                EnvEntry {
                    name: "staging".into(),
                    baseline: false,
                    file: false,
                },
            ],
            choices: vec!["prod".into(), "staging".into()],
            snapshots: Vec::new(),
            baseline_show_fields: fields,
        }
    }

    /// One ticked variable plus a name is a renamed column; a second tick has to
    /// drop back to the plain multi-column form, because `AS` names one column.
    #[test]
    fn a_named_single_variable_becomes_an_alias_and_a_pair_stays_plain() {
        let mut form = VarsForm {
            path: vec![0],
            vars: vec![("TIER".into(), true), ("REGION".into(), false)],
            other: String::new(),
            alias: "Plan".into(),
            stats: StatKind::CHOOSABLE.iter().map(|k| (*k, false)).collect(),
            image: None,
        };
        assert!(
            matches!(
                form.node(),
                Some(FlowNode::Report(ReportStmt::VarAs { ref var, ref name, .. }))
                    if var == "TIER" && name == "Plan"
            ),
            "a single ticked variable with a name is reported AS that name"
        );
        form.vars[1].1 = true;
        assert!(
            matches!(form.node(), Some(FlowNode::Report(ReportStmt::Vars(ref v))) if v == &["TIER".to_string(), "REGION".to_string()]),
            "two ticked variables report both columns and ignore the name"
        );
    }

    /// The free-text row exists for variables the static scan cannot see, so it
    /// has to reach the node even with nothing ticked.
    #[test]
    fn a_typed_variable_is_reported_even_when_nothing_is_ticked() {
        let form = VarsForm {
            path: vec![0],
            vars: vec![("TIER".into(), false)],
            other: "  RUNTIME_ONLY ".into(),
            alias: String::new(),
            stats: Vec::new(),
            image: None,
        };
        assert!(
            matches!(form.node(), Some(FlowNode::Report(ReportStmt::Vars(ref v))) if v == &["RUNTIME_ONLY".to_string()]),
            "the typed name is trimmed and reported"
        );
    }

    #[test]
    fn ticking_a_baseline_field_writes_a_show_clause_on_the_baseline_role() {
        let form = compare_form(vec![
            ("Time".into(), true),
            ("Status".into(), false),
            ("Response".into(), true),
        ]);
        let Some(EnvClause::Roles {
            baseline,
            comparisons,
            baseline_show,
        }) = form.clause()
        else {
            panic!("a compare form describes a Roles clause")
        };
        assert_eq!(
            baseline,
            vec![RoleRef::Env("prod".into())],
            "the baseline role is unaffected by the SHOW"
        );
        assert_eq!(
            comparisons,
            vec![RoleRef::Env("staging".into())],
            "and so are the comparisons"
        );
        assert_eq!(
            baseline_show,
            vec!["Time".to_string(), "Response".to_string()],
            "the ticked fields become the SHOW list, in the checklist's own order"
        );
    }

    #[test]
    fn ticking_nothing_leaves_the_baseline_with_no_show_clause_at_all() {
        // Unlike a request's SHOW, where empty means "emit everything", an
        // empty baseline SHOW means "carry nothing across" — so an untouched
        // checklist must serialise to no clause rather than to every field.
        let form = compare_form(vec![("Time".into(), false), ("Status".into(), false)]);
        let Some(EnvClause::Roles { baseline_show, .. }) = form.clause() else {
            panic!("a compare form describes a Roles clause")
        };
        assert!(
            baseline_show.is_empty(),
            "nothing ticked writes no SHOW: {baseline_show:?}"
        );
    }

    #[test]
    fn a_show_field_no_request_offers_is_still_listed_so_editing_cannot_drop_it() {
        // Hand-written source may name a field the bound collection knows
        // nothing about (or there may be no collection bound at all). It has to
        // survive an open-and-apply round trip.
        let flow = crate::report::parser::parse_flow(
            "# collection: api.hurl\nFOR TARGET IN ENVS BASELINE(\"p\") SHOW(Handwritten), COMPARISON(\"s\")\n    REPORT REQUEST login\nEND\n",
        )
        .expect("the fixture flow parses");
        let FlowNode::ForEnvs { clause, body, .. } = &flow.nodes[0] else {
            panic!("the fixture's first node is the ENVS loop")
        };
        let EnvClause::Roles { baseline_show, .. } = clause else {
            panic!("the fixture's clause has roles")
        };
        // No collection bound, so nothing but the intrinsics is discoverable —
        // which is exactly the case where dropping the field would be easiest.
        let fields = crate::report::edit::baseline_show_choices(&[], body, baseline_show);
        assert!(
            fields.iter().any(|(n, on)| n == "Handwritten" && *on),
            "the unrecognised field is offered, and offered already ticked: {fields:?}"
        );
        assert!(
            fields.iter().any(|(n, _)| n == "Time"),
            "alongside the intrinsics every request has: {fields:?}"
        );
    }

    #[test]
    fn a_baseline_show_checklist_is_only_offered_for_reported_requests() {
        // `REQUEST x` sends but emits nothing, so it contributes no fields for
        // a baseline to carry across; only `REPORT REQUEST x` does.
        let nodes = vec![
            FlowNode::Request {
                name: "warmup".into(),
            },
            FlowNode::Report(crate::report::flow::ReportStmt::Request {
                name: "login".into(),
                alias: None,
                response_fmt: None,
                show: Vec::new(),
                hide: Vec::new(),
                with: Vec::new(),
            }),
        ];
        assert_eq!(
            crate::report::edit::reported_requests(&nodes),
            vec!["login".to_string()],
            "only the reported request is a source of fields"
        );
    }
}
