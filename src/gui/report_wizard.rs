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
    EnvClause, FlowNode, Pattern, Producer, ReportStmt, ResponseFmt, RoleRef, WithItem,
};

use super::app::GuiApp;
use super::report_editor::ReportEditor;

/// An open node-configure wizard, one variant per form-backed node kind.
pub enum Wizard {
    Request(RequestForm),
    Envs(EnvsForm),
    Files(FilesForm),
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
    /// Preserved verbatim across the edit (the form doesn't expose them).
    with: Vec<WithItem>,
    hide: Vec<String>,
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
                hide: self.hide.clone(),
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
    entries: Vec<EnvEntry>,
    /// Loaded environment names an entry cycles through.
    choices: Vec<String>,
    /// Discovered `.baseline` snapshot paths a `FILE(…)` entry cycles through.
    snapshots: Vec<String>,
    /// `BASELINE(…) SHOW(…)` fields, preserved verbatim (no editing UI).
    baseline_show: Vec<String>,
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
                baseline_show: self.baseline_show.clone(),
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

/// Open the appropriate configure wizard for the node at `path`, if it is one of
/// the form-backed kinds (request / ENVS loop / single-variable FILES loop).
/// Other kinds (assignments, lists, FOLDERS loops, computed columns) have no
/// wizard and are edited via the inline line editor, so this is a no-op for
/// them.
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
        } if pattern.is_single() => Wizard::Files(build_files(path.to_vec(), &node)),
        _ => return,
    };
    ed.wizard = Some(wiz);
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
    let all = show.is_empty();
    let fields = names
        .into_iter()
        .map(|n| {
            let on = all || show.iter().any(|s| s == &n);
            (n, on)
        })
        .collect();

    RequestForm {
        path,
        name,
        titles,
        report,
        response,
        alias: alias.unwrap_or_default(),
        fields,
        with,
        hide,
    }
}

fn build_envs(
    app: &GuiApp,
    flow: &crate::report::flow::ReportFlow,
    report_path: Option<&std::path::Path>,
    path: Vec<usize>,
    node: &FlowNode,
) -> EnvsForm {
    let (var, clause, parallel) = match node {
        FlowNode::ForEnvs {
            var,
            clause,
            parallel,
            ..
        } => (var.clone(), clause.clone(), parallel.is_some()),
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

    EnvsForm {
        path,
        var,
        compare,
        parallel,
        entries,
        choices,
        snapshots,
        baseline_show,
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
            parallel.is_some(),
        ),
        _ => unreachable!("build_files called on a non-FILES node"),
    };
    FilesForm {
        path,
        var,
        dir,
        glob,
        parallel,
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
    let s = &app.strings;
    let mut outcome = Outcome::None;

    let modal = egui::Modal::new(egui::Id::new("pt_node_wizard")).show(ctx, |ui| {
        ui.set_min_width(360.0);
        let wiz = ed.wizard.as_mut().expect("wizard is Some");
        match wiz {
            Wizard::Request(f) => request_ui(ui, &th, s, f),
            Wizard::Envs(f) => envs_ui(ui, &th, s, f),
            Wizard::Files(f) => files_ui(ui, &th, s, f),
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
    ui.checkbox(
        &mut f.parallel,
        RichText::new(s.report_node_parallel_label).color(th.text),
    );

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
            ui.add(egui::TextEdit::singleline(&mut f.dir).desired_width(220.0));
            ui.end_row();
            ui.label(RichText::new(s.report_node_files_match_label).color(th.dim));
            ui.add(egui::TextEdit::singleline(&mut f.glob).desired_width(220.0));
            ui.end_row();
        });
    ui.checkbox(
        &mut f.parallel,
        RichText::new(s.report_node_parallel_label).color(th.text),
    );
}

// ---------------------------------------------------------------------------
// Applying a wizard
// ---------------------------------------------------------------------------

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
            // Preserve the node's body and any explicit PARALLEL(n) degree; only
            // the var/clause and the parallel on/off state change here.
            let (body, existing_parallel) = match ed.flow.as_ref().and_then(|fl| node_at(fl, &path))
            {
                Some(FlowNode::ForEnvs { body, parallel, .. }) => (body.clone(), *parallel),
                _ => return,
            };
            let parallel = f.parallel.then(|| existing_parallel.unwrap_or_default());
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
            let (body, existing_parallel) = match ed.flow.as_ref().and_then(|fl| node_at(fl, &path))
            {
                Some(FlowNode::ForEach { body, parallel, .. }) => (body.clone(), *parallel),
                _ => return,
            };
            let parallel = f.parallel.then(|| existing_parallel.unwrap_or_default());
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
    }
}
