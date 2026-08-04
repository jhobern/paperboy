//! Left-top panel: the request list for the active collection, shown as a
//! Postman-style collapsible folder tree (folders come from the `/`-encoded
//! request titles via [`crate::tree::entry_path`], the same convention the
//! terminal UI uses). Add / select / rename / delete / run requests, plus
//! per-collection Run All.

use std::collections::BTreeMap;
use std::path::PathBuf;

use eframe::egui::{self, RichText};

use crate::collection::WsRow;
use crate::hurl::{HurlEntry, RunStatus};
use crate::tree::entry_path;

use super::app::{Dialog, GuiApp, RenameTarget};
use super::theme::GuiTheme;

fn run_marker(status: RunStatus) -> (&'static str, bool) {
    match status {
        RunStatus::Passed => (super::icons::PASS, true),
        RunStatus::Failed => (super::icons::FAIL, false),
        RunStatus::Running => (super::icons::RUNNING, true),
        RunStatus::NotRun => ("", true),
    }
}

/// A folder node of the request tree: named subfolders (sorted) and the flat
/// indices of the requests directly inside this folder (original order).
#[derive(Default)]
struct Node {
    folders: BTreeMap<String, Node>,
    entries: Vec<usize>,
}

/// Group a collection's entries into a folder tree using their `/`-encoded
/// titles. The leaf segment is the request's display name; everything before
/// it is its folder path.
fn build_tree(entries: &[HurlEntry]) -> Node {
    let mut root = Node::default();
    for (i, e) in entries.iter().enumerate() {
        let path = entry_path(&e.title);
        let (folders, _leaf) = path.split_at(path.len() - 1);
        let mut node = &mut root;
        for seg in folders {
            node = node.folders.entry(seg.clone()).or_default();
        }
        node.entries.push(i);
    }
    root
}

/// Actions collected while rendering the (immutably-borrowed) tree, applied to
/// the session afterwards.
#[derive(Default)]
struct Actions {
    select: Option<usize>,
    run: Option<usize>,
    rename: Option<usize>,
    delete: Option<usize>,
}

fn render_node(
    ui: &mut egui::Ui,
    node: &Node,
    entries: &[HurlEntry],
    selected: usize,
    theme: &GuiTheme,
    id_prefix: &str,
    lbl_untitled: &str,
    lbl_run: &str,
    lbl_rename: &str,
    lbl_delete: &str,
    actions: &mut Actions,
) {
    for (name, child) in &node.folders {
        let salt = format!("{id_prefix}/{name}");
        super::widgets::tree_header(
            ui,
            &salt,
            true,
            RichText::new(format!("{} {name}", super::icons::FOLDER)).color(theme.text),
            |ui| {
                render_node(
                    ui,
                    child,
                    entries,
                    selected,
                    theme,
                    &salt,
                    lbl_untitled,
                    lbl_run,
                    lbl_rename,
                    lbl_delete,
                    actions,
                );
            },
        );
    }
    for &i in &node.entries {
        let entry = &entries[i];
        let leaf = entry_path(&entry.title).pop().unwrap_or_default();
        let label = if leaf.trim().is_empty() {
            if entry.url.trim().is_empty() {
                lbl_untitled.to_string()
            } else {
                entry.url.clone()
            }
        } else {
            leaf
        };
        let (marker, ok) = run_marker(entry.last_run);
        let is_sel = i == selected;

        let row = ui
            .horizontal(|ui| {
                super::widgets::method_badge(ui, &entry.method);
                let text = if is_sel {
                    RichText::new(&label).strong().color(theme.text)
                } else {
                    RichText::new(&label).color(theme.dim)
                };
                // Reserve the run marker on the right, then let the name fill
                // (and truncate within) the remaining space, so a long name
                // never pushes the row — and the panel — wider than its width.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if !marker.is_empty() {
                        let mc = if ok { theme.ok } else { theme.err };
                        ui.colored_label(mc, marker);
                    }
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        super::widgets::selectable(ui, is_sel, text)
                    })
                    .inner
                })
                .inner
            })
            .inner;
        if row.clicked() {
            actions.select = Some(i);
        }
        if row.double_clicked() {
            actions.run = Some(i);
        }
        row.context_menu(|ui| {
            if ui.button(lbl_run).clicked() {
                actions.run = Some(i);
                ui.close();
            }
            if ui.button(lbl_rename).clicked() {
                actions.rename = Some(i);
                ui.close();
            }
            if ui.button(lbl_delete).clicked() {
                actions.delete = Some(i);
                ui.close();
            }
        });
    }
}

pub fn ui(app: &mut GuiApp, ui: &mut egui::Ui) {
    let ci = app.active_ci();
    // A Workspace tab shows the real filesystem tree instead of one file's
    // request tree (see `workspace_ui`).
    if app.session.collections[ci].is_workspace() {
        workspace_ui(app, ui, ci);
        return;
    }
    let theme = app.theme;
    let s = &app.strings;
    let (
        lbl_run_all,
        tip_run_all,
        tip_add,
        default_title,
        lbl_untitled,
        lbl_no_requests,
        lbl_run,
        lbl_rename,
        lbl_delete,
    ) = (
        s.gui_run_all,
        s.gui_run_all_tooltip,
        s.gui_add_request,
        s.gui_new_request,
        s.gui_untitled_request,
        s.gui_no_requests_tree,
        s.gui_run,
        s.gui_rename_ellipsis,
        s.gui_delete,
    );

    // Header: collection name (truncates) + Run All / Add (always visible).
    let name = app.session.collections[ci].name.clone();
    super::widgets::panel_header(ui, &theme, name, |ui| {
        let run_all = format!("{} {}", super::icons::PLAY, lbl_run_all);
        if ui.button(run_all).on_hover_text(tip_run_all).clicked() {
            app.session.run_all_entries(ci);
        }
        if ui
            .button(super::icons::PLUS)
            .on_hover_text(tip_add)
            .clicked()
        {
            let mut e = HurlEntry::default();
            e.method = "GET".into();
            e.url = app.session.vars.base_url.clone();
            e.title = default_title.into();
            e.user_added = true;
            let col = &mut app.session.collections[ci];
            col.entries.push(e);
            col.selected_entry = col.entries.len() - 1;
            col.invalidate_request_json();
        }
    });
    ui.separator();

    let selected = app.session.collections[ci].selected_entry;
    let mut actions = Actions::default();

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            // Truncate long request/folder names to the panel width instead of
            // letting them extend: a name wider than the panel would report an
            // over-wide content size, and while the splitter is being dragged
            // narrower egui clips the content to the drag width but still places
            // the neighbouring panel at the wider content edge — leaving an
            // unpainted strip. Truncating keeps the content within the panel.
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
            let entries = &app.session.collections[ci].entries;
            if entries.is_empty() {
                ui.add_space(8.0);
                ui.colored_label(theme.dim, lbl_no_requests);
                return;
            }
            let tree = build_tree(entries);
            render_node(
                ui,
                &tree,
                entries,
                selected,
                &theme,
                "req",
                lbl_untitled,
                lbl_run,
                lbl_rename,
                lbl_delete,
                &mut actions,
            );
        });

    if let Some(i) = actions.select {
        let col = &mut app.session.collections[ci];
        col.selected_entry = i;
        col.list_cursor = i;
        col.invalidate_request_json();
        app.focus = super::Focus::List;
    }
    if let Some(i) = actions.rename {
        let title = app.session.collections[ci].entries[i].title.clone();
        app.dialog = Some(Dialog::Rename {
            target: RenameTarget::Request { ci, idx: i },
            text: title,
        });
    }
    if let Some(i) = actions.delete {
        let col = &mut app.session.collections[ci];
        if i < col.entries.len() {
            col.entries.remove(i);
            if col.selected_entry >= col.entries.len() {
                col.selected_entry = col.entries.len().saturating_sub(1);
            }
            col.invalidate_request_json();
        }
    }
    if let Some(i) = actions.run {
        app.session.collections[ci].selected_entry = i;
        app.run_active();
    }
}

/// Row indentation per tree depth, in pixels.
const WS_INDENT: f32 = 14.0;

/// A PaperTrail report (`.trail`) selected from a Workspace tree, shown
/// read-only in the centre pane. The interactive node editor is step 2.
pub struct OpenReport {
    pub path: PathBuf,
    pub name: String,
    pub text: String,
}

/// Read-only viewer for a Workspace-selected `.trail` report, shown in the
/// centre pane in place of the request editor while a report row is selected.
pub fn report_view(app: &mut GuiApp, ui: &mut egui::Ui) {
    let theme = app.theme;
    let (name, text) = match &app.workspace_report {
        Some(r) => (r.name.clone(), r.text.clone()),
        None => return,
    };
    let lbl_close = format!("{} {}", super::icons::CLOSE, app.strings.gui_close);
    let note = app.strings.gui_papertrail_note;
    ui.horizontal(|ui| {
        ui.label(RichText::new(&name).strong().color(theme.text));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button(lbl_close).clicked() {
                app.workspace_report = None;
            }
        });
    });
    ui.colored_label(theme.dim, note);
    ui.separator();
    egui::ScrollArea::both()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let mut t = text;
            ui.add(
                egui::TextEdit::multiline(&mut t)
                    .code_editor()
                    .desired_width(f32::INFINITY)
                    .interactive(false),
            );
        });
}

/// An action collected while rendering the (immutably-read) workspace tree,
/// applied to the session afterwards.
enum WsAction {
    ToggleFolder(PathBuf),
    ToggleCollection {
        path: PathBuf,
        open: bool,
    },
    SelectRequest {
        collection: PathBuf,
        idx: usize,
        loaded: bool,
    },
    RunRequest {
        collection: PathBuf,
        idx: usize,
        loaded: bool,
    },
    OpenReport(PathBuf),
    OpenEnv(PathBuf),
}

/// An indented, clickable tree row: a leading spacer for its depth, then a
/// static-frame selectable carrying the (already coloured) label. Truncates
/// within the panel width like the request rows, so a long name can't push the
/// panel wider (see the note above the request `ScrollArea`).
fn ws_row(ui: &mut egui::Ui, depth: usize, selected: bool, text: RichText) -> egui::Response {
    ui.horizontal(|ui| {
        ui.add_space(depth as f32 * WS_INDENT);
        super::widgets::selectable(ui, selected, text)
    })
    .inner
}

/// The left-top panel for a Workspace tab: the real filesystem tree under the
/// workspace root (folders, collection files with inline requests, `.trail`
/// reports and `.vars` environments), driven by [`WsRow`] exactly like the
/// terminal UI. `workspace_expanded` on the collection is the single source of
/// truth for what's open, so expand/collapse just toggles that set.
fn workspace_ui(app: &mut GuiApp, ui: &mut egui::Ui, ci: usize) {
    let theme = app.theme;
    let (lbl_run_all, tip_run_all, lbl_filter, tip_filter, lbl_empty) = {
        let s = &app.strings;
        (
            s.gui_run_all,
            s.gui_run_all_tooltip,
            s.gui_workspace_filter,
            s.gui_workspace_filter_tooltip,
            s.gui_no_requests_tree,
        )
    };

    let name = app.session.collections[ci].name.clone();
    let filter_on = app.session.collections[ci].workspace_filter_hurl_json;
    super::widgets::panel_header(ui, &theme, name, |ui| {
        let run_all = format!("{} {}", super::icons::PLAY, lbl_run_all);
        if ui.button(run_all).on_hover_text(tip_run_all).clicked() {
            app.session.run_all_entries(ci);
        }
        let ftxt =
            RichText::new(lbl_filter).color(if filter_on { theme.accent } else { theme.dim });
        if ui.button(ftxt).on_hover_text(tip_filter).clicked() {
            let col = &mut app.session.collections[ci];
            col.workspace_filter_hurl_json = !col.workspace_filter_hurl_json;
            app.session.save();
        }
    });
    ui.separator();

    let rows = app.session.collections[ci].ws_rows();
    if rows.is_empty() {
        ui.add_space(8.0);
        ui.colored_label(theme.dim, lbl_empty);
        return;
    }
    let selected_entry = app.session.collections[ci].selected_entry;
    let loaded_path = app.session.collections[ci].path.clone();
    let report_path = app.workspace_report.as_ref().map(|r| r.path.clone());

    let mut actions: Vec<WsAction> = Vec::new();
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
            for row in &rows {
                match row {
                    WsRow::Folder {
                        path,
                        name,
                        depth,
                        expanded,
                    } => {
                        let chev = if *expanded {
                            super::icons::CARET_DOWN
                        } else {
                            super::icons::CARET_RIGHT
                        };
                        let text = RichText::new(format!("{chev} {} {name}", super::icons::FOLDER))
                            .color(theme.text);
                        if ws_row(ui, *depth, false, text).clicked() {
                            actions.push(WsAction::ToggleFolder(path.clone()));
                        }
                    }
                    WsRow::Collection {
                        path,
                        name,
                        depth,
                        open,
                    } => {
                        let chev = if *open {
                            super::icons::CARET_DOWN
                        } else {
                            super::icons::CARET_RIGHT
                        };
                        let is_loaded = loaded_path.as_deref() == Some(path.as_path());
                        let color = if is_loaded { theme.accent } else { theme.text };
                        let text = RichText::new(format!("{chev} {} {name}", super::icons::FILE))
                            .color(color);
                        if ws_row(ui, *depth, is_loaded, text).clicked() {
                            actions.push(WsAction::ToggleCollection {
                                path: path.clone(),
                                open: *open,
                            });
                        }
                    }
                    WsRow::Request {
                        collection,
                        idx,
                        name,
                        depth,
                        loaded,
                    } => {
                        let is_sel = *loaded
                            && loaded_path.as_deref() == Some(collection.as_path())
                            && *idx == selected_entry;
                        // Method badge + run marker only for the loaded file's
                        // rows (other collections are listed by name only).
                        let (method, marker) = if *loaded {
                            app.session.collections[ci]
                                .entries
                                .get(*idx)
                                .map(|e| (Some(e.method.clone()), run_marker(e.last_run)))
                                .unwrap_or((None, ("", true)))
                        } else {
                            (None, ("", true))
                        };
                        let resp = ui
                            .horizontal(|ui| {
                                ui.add_space(*depth as f32 * WS_INDENT);
                                if let Some(m) = &method {
                                    super::widgets::method_badge(ui, m);
                                }
                                let text = if is_sel {
                                    RichText::new(name).strong().color(theme.text)
                                } else {
                                    RichText::new(name).color(theme.dim)
                                };
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        let (mk, ok) = marker;
                                        if !mk.is_empty() {
                                            let mc = if ok { theme.ok } else { theme.err };
                                            ui.colored_label(mc, mk);
                                        }
                                        ui.with_layout(
                                            egui::Layout::left_to_right(egui::Align::Center),
                                            |ui| super::widgets::selectable(ui, is_sel, text),
                                        )
                                        .inner
                                    },
                                )
                                .inner
                            })
                            .inner;
                        if resp.clicked() {
                            actions.push(WsAction::SelectRequest {
                                collection: collection.clone(),
                                idx: *idx,
                                loaded: *loaded,
                            });
                        }
                        if resp.double_clicked() {
                            actions.push(WsAction::RunRequest {
                                collection: collection.clone(),
                                idx: *idx,
                                loaded: *loaded,
                            });
                        }
                    }
                    WsRow::Report { path, name, depth } => {
                        let is_open = report_path.as_deref() == Some(path.as_path());
                        let color = if is_open { theme.accent } else { theme.text };
                        let text =
                            RichText::new(format!("{} {name}", super::icons::REPORT)).color(color);
                        if ws_row(ui, *depth, is_open, text).clicked() {
                            actions.push(WsAction::OpenReport(path.clone()));
                        }
                    }
                    WsRow::Environment { path, name, depth } => {
                        let text = RichText::new(format!("{} {name}", super::icons::ENV))
                            .color(theme.subst);
                        if ws_row(ui, *depth, false, text).clicked() {
                            actions.push(WsAction::OpenEnv(path.clone()));
                        }
                    }
                }
            }
        });

    for action in actions {
        apply_ws_action(app, ci, action);
    }
}

/// Apply one collected [`WsAction`] to the session (mutations are deferred out
/// of the render pass so the tree is read immutably while drawing).
fn apply_ws_action(app: &mut GuiApp, ci: usize, action: WsAction) {
    match action {
        WsAction::ToggleFolder(p) => {
            let col = &mut app.session.collections[ci];
            if col.workspace_expanded.contains(&p) {
                col.workspace_expanded.remove(&p);
            } else {
                col.workspace_expanded.insert(p);
            }
            app.session.save();
        }
        WsAction::ToggleCollection { path, open } => {
            if open {
                app.session.collections[ci].workspace_expanded.remove(&path);
            } else if app.session.collections[ci].path.as_deref() == Some(path.as_path()) {
                // Already the loaded file: just re-expand it so its requests show.
                app.session.collections[ci].workspace_expanded.insert(path);
                app.session.collections[ci].sync_ws_cursor();
            } else {
                app.session.load_workspace_file(ci, path);
            }
            app.workspace_report = None;
            app.focus = super::Focus::List;
            app.session.save();
        }
        WsAction::SelectRequest {
            collection,
            idx,
            loaded,
        } => {
            if loaded {
                let col = &mut app.session.collections[ci];
                col.selected_entry = idx;
                col.sync_folder_to_selected();
                col.invalidate_request_json();
            } else if app.session.load_workspace_file(ci, collection.clone())
                && app.session.collections[ci].path.as_deref() == Some(collection.as_path())
            {
                let col = &mut app.session.collections[ci];
                let n = col.entries.len();
                col.selected_entry = idx.min(n.saturating_sub(1));
                col.sync_folder_to_selected();
                col.invalidate_request_json();
            }
            app.workspace_report = None;
            app.focus = super::Focus::List;
        }
        WsAction::RunRequest {
            collection,
            idx,
            loaded,
        } => {
            if loaded {
                app.session.collections[ci].selected_entry = idx;
            } else if app.session.load_workspace_file(ci, collection.clone())
                && app.session.collections[ci].path.as_deref() == Some(collection.as_path())
            {
                let n = app.session.collections[ci].entries.len();
                app.session.collections[ci].selected_entry = idx.min(n.saturating_sub(1));
            }
            app.workspace_report = None;
            app.run_active();
        }
        WsAction::OpenReport(path) => match std::fs::read_to_string(&path) {
            Ok(text) => {
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("report")
                    .to_string();
                app.workspace_report = Some(OpenReport { path, name, text });
                app.focus = super::Focus::Main;
            }
            Err(e) => {
                app.session.status = Some(crate::i18n::Status::Error(e.to_string()));
            }
        },
        WsAction::OpenEnv(path) => {
            app.session.open_workspace_environment(&path);
            app.workspace_report = None;
        }
    }
}
