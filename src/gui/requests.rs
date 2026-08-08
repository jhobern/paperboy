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

/// The request list scrolls, and egui draws its scrollbar *over* the right
/// edge of the content rather than reserving a gutter for it. The pass/fail
/// marker is the rightmost thing on a row, so without this nudge the bar sits
/// right on top of it. Wide enough to clear the bar plus a little breathing
/// room.
const SCROLLBAR_GUTTER: f32 = 10.0;

fn run_marker(status: RunStatus) -> (&'static str, bool) {
    match status {
        RunStatus::Passed => (super::icons::PASS, true),
        RunStatus::Failed => (super::icons::FAIL, false),
        RunStatus::Running => (super::icons::RUNNING, true),
        RunStatus::NotRun => ("", true),
    }
}

/// Draw the "edited, not saved yet" pencil for a request row. Sits in the same
/// right-hand gutter as the run marker (and to its left, so the run result
/// keeps the outermost, most-scannable position), and is silent for a request
/// that matches what is on disk.
fn edited_marker(ui: &mut egui::Ui, entry: &HurlEntry, theme: &GuiTheme, tip: &str) {
    if !(entry.user_added || entry.modified) {
        return;
    }
    ui.colored_label(theme.pending, super::icons::EDITED)
        .on_hover_text(tip);
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
    lbl_edited: &str,
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
                    lbl_edited,
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

        // Give the row its own id namespace, keyed by the request it draws.
        // egui derives a widget's id from how many widgets preceded it, so the
        // optional decorations on a row (method badge, run marker, edit pencil)
        // silently renumber everything after them the moment one appears or
        // disappears — which egui then flags with a red id-clash outline. A
        // stable per-row salt keeps the row's ids tied to the row itself.
        let row = ui
            .push_id(("req_row", i), |ui| {
                ui.horizontal(|ui| {
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
                        ui.add_space(SCROLLBAR_GUTTER);
                        if !marker.is_empty() {
                            let mc = if ok { theme.ok } else { theme.err };
                            ui.colored_label(mc, marker);
                        }
                        edited_marker(ui, entry, theme, lbl_edited);
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            super::widgets::selectable(ui, is_sel, text)
                        })
                        .inner
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
        lbl_edited,
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
        s.gui_edited_request,
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
                lbl_edited,
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
    /// Load a workspace `.vars` file. `reveal` also expands and scrolls to it
    /// in the Environments panel — a double-click, since a single click on a
    /// tree row shouldn't yank a different panel around under the user.
    OpenEnv {
        path: PathBuf,
        reveal: bool,
    },
    /// Make a workspace environment file the active one, loading it first if it
    /// isn't open yet — the whole point being to activate it without having to
    /// open it and then find it again in the Environments panel.
    ActivateEnv(PathBuf),
    /// Add a new collection/report/environment to the workspace, inside `dir`.
    NewItem {
        dir: PathBuf,
        kind: crate::workspace::NewItemKind,
    },
    /// Move a file or folder into another folder of the same workspace.
    MoveItem {
        src: PathBuf,
        dest_dir: PathBuf,
    },
}

/// What's being dragged around the workspace tree: the item's own path is all
/// the drop target needs, since where it lands is decided by the target.
#[derive(Clone, Debug)]
struct WsDrag(PathBuf);

/// An indented, clickable tree row: a leading spacer for its depth, then a
/// static-frame selectable carrying the (already coloured) label. Truncates
/// within the panel width like the request rows, so a long name can't push the
/// panel wider (see the note above the request `ScrollArea`).
fn ws_row(ui: &mut egui::Ui, depth: usize, selected: bool, text: RichText) -> egui::Response {
    ui.horizontal(|ui| {
        ui.add_space(depth as f32 * WS_INDENT);
        // Rows sense drags as well as clicks so a file can be dragged into
        // another folder; a click still lands as a click, since egui only calls
        // it a drag once the pointer actually moves.
        ui.add(
            egui::Button::selectable(selected, text)
                .frame_when_inactive(true)
                .sense(egui::Sense::click_and_drag()),
        )
    })
    .inner
}

/// Make a tree row draggable, and (for a folder) somewhere a dragged item can
/// be dropped.
///
/// The highlight while hovering is the whole point: a file dragged over a tree
/// needs to say *which* of the folders under the pointer it will land in, and
/// rows are only a few pixels apart.
fn ws_drag_and_drop(
    ui: &mut egui::Ui,
    resp: &egui::Response,
    theme: &super::theme::GuiTheme,
    path: &std::path::Path,
    is_folder: bool,
    actions: &mut Vec<WsAction>,
) {
    if resp.drag_started() {
        egui::DragAndDrop::set_payload(ui.ctx(), WsDrag(path.to_path_buf()));
    }
    // Something has to follow the pointer, or a drag looks like nothing is
    // happening: the tree rows themselves stay put (unlike the report editor's
    // blocks, this is a move between folders, not a reordering).
    if resp.dragged()
        && let Some(pos) = ui.ctx().pointer_interact_pos()
    {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let layer = egui::LayerId::new(egui::Order::Tooltip, ui.id().with("ws_drag_label"));
        ui.ctx().layer_painter(layer).text(
            pos + egui::vec2(12.0, 4.0),
            egui::Align2::LEFT_TOP,
            name,
            egui::TextStyle::Button.resolve(ui.style()),
            theme.accent,
        );
    }
    if !is_folder {
        return;
    }
    let Some(dragged) = egui::DragAndDrop::payload::<WsDrag>(ui.ctx()) else {
        return;
    };
    // A folder can't be dropped into itself or its own descendant, so don't
    // offer to.
    if path.starts_with(&dragged.0) || !resp.contains_pointer() {
        return;
    }
    ui.painter().rect_stroke(
        resp.rect,
        6.0,
        egui::Stroke::new(1.0, theme.accent),
        egui::StrokeKind::Inside,
    );
    if ui.input(|i| i.pointer.any_released()) {
        actions.push(WsAction::MoveItem {
            src: dragged.0.clone(),
            dest_dir: path.to_path_buf(),
        });
        egui::DragAndDrop::clear_payload(ui.ctx());
    }
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

    let (lbl_new, tip_new, lbl_in_folder, lbl_in_root, lbl_set_active_env) = {
        let s = &app.strings;
        (
            s.gui_ws_new,
            s.gui_ws_new_tooltip,
            s.gui_ws_new_in_folder,
            s.gui_ws_new_in_root,
            s.gui_ws_set_active_env,
        )
    };
    let s_new = new_item_labels(&app.strings);

    let name = app.session.collections[ci].name.clone();
    let filter_on = app.session.collections[ci].workspace_filter_hurl_json;
    let ws_root = app.session.collections[ci].workspace_root.clone();
    let mut header_new: Option<WsAction> = None;
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
        // Adds at the top of the tree; the per-folder right-click menu is how
        // you add somewhere deeper.
        if let Some(root) = ws_root.clone() {
            let label = format!("{} {}", super::icons::PLUS, lbl_new);
            let menu = ui.menu_button(label, |ui| {
                if let Some(kind) = new_item_menu(ui, s_new) {
                    header_new = Some(WsAction::NewItem { dir: root, kind });
                    ui.close();
                }
            });
            menu.response.on_hover_text(tip_new);
        }
    });
    ui.separator();

    let rows = app.session.collections[ci].ws_rows();
    if rows.is_empty() {
        ui.add_space(8.0);
        ui.colored_label(theme.dim, lbl_empty);
        // An empty workspace is exactly when you most need to add something to
        // it, so the New menu still has to work with no tree to draw.
        if let Some(action) = header_new {
            apply_ws_action(app, ci, action);
        }
        return;
    }
    let selected_entry = app.session.collections[ci].selected_entry;
    let loaded_path = app.session.collections[ci].path.clone();
    let lbl_edited = app.strings.gui_edited_request;
    let lbl_edited_col = app.strings.gui_edited_collection;
    let report_path = app
        .report_editor
        .as_ref()
        .and_then(|e| e.path().map(std::path::Path::to_path_buf));

    let mut actions: Vec<WsAction> = Vec::new();
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
            // The empty space around the tree stands in for the workspace root,
            // so an item can be dragged back out to the top level (there is no
            // row representing the root to drop it on) and the New menu is
            // reachable without having to find a row to right-click.
            //
            // Registered *before* the rows on purpose: egui gives an overlapped
            // click to whichever widget was added last, so the rows drawn on top
            // of this keep their own clicks and menus, and this only ever picks
            // up what lands between or beyond them.
            let bg_rect = ui.clip_rect();
            let bg = ui.interact(
                bg_rect,
                ui.id().with("ws_background"),
                egui::Sense::click_and_drag(),
            );
            if let Some(root) = &ws_root {
                ws_row_menu(&bg, root.clone(), lbl_in_root, s_new, &mut actions);
            }
            let mut folder_rects: Vec<egui::Rect> = Vec::new();
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
                        let resp = ws_row(ui, *depth, false, text);
                        if resp.clicked() {
                            actions.push(WsAction::ToggleFolder(path.clone()));
                        }
                        ws_row_menu(&resp, path.clone(), lbl_in_folder, s_new, &mut actions);
                        ws_drag_and_drop(ui, &resp, &theme, path, true, &mut actions);
                        folder_rects.push(resp.rect);
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
                        // A collection whose edits are only in memory — either
                        // it is the loaded file or its entries are parked in
                        // `workspace_pending` — is flagged so the user can see
                        // at a glance which files still need saving, including
                        // the ones they have since switched away from.
                        let pencil = if app.session.collections[ci].workspace_file_edited(path) {
                            format!(" {}", super::icons::EDITED)
                        } else {
                            String::new()
                        };
                        let text =
                            RichText::new(format!("{chev} {} {name}{pencil}", super::icons::FILE))
                                .color(color);
                        let resp = ws_row(ui, *depth, is_loaded, text);
                        let resp = if pencil.is_empty() {
                            resp
                        } else {
                            resp.on_hover_text(lbl_edited_col)
                        };
                        if resp.clicked() {
                            actions.push(WsAction::ToggleCollection {
                                path: path.clone(),
                                open: *open,
                            });
                        }
                        ws_row_menu(&resp, sibling_dir(path), lbl_in_folder, s_new, &mut actions);
                        ws_drag_and_drop(ui, &resp, &theme, path, false, &mut actions);
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
                        // Unlike the method badge and the run marker, the edit
                        // pencil is shown for every collection's rows, not just
                        // the loaded one's — an edit parked while the user looks
                        // at another collection is still an edit they need to
                        // see (and save).
                        let edited =
                            app.session.collections[ci].workspace_request_edited(collection, *idx);
                        // A stable per-request id namespace: the badge and
                        // run marker only appear for the loaded collection, so
                        // without it every row's ids shift the moment the tab
                        // changes which file it holds (see `render_node`).
                        let resp = ui
                            .push_id(("ws_req", collection, idx), |ui| {
                                ui.horizontal(|ui| {
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
                                            ui.add_space(SCROLLBAR_GUTTER);
                                            let (mk, ok) = marker;
                                            if !mk.is_empty() {
                                                let mc = if ok { theme.ok } else { theme.err };
                                                ui.colored_label(mc, mk);
                                            }
                                            if edited {
                                                ui.colored_label(
                                                    theme.pending,
                                                    super::icons::EDITED,
                                                )
                                                .on_hover_text(lbl_edited);
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
                        ws_row_menu(
                            &resp,
                            sibling_dir(collection),
                            lbl_in_folder,
                            s_new,
                            &mut actions,
                        );
                    }
                    WsRow::Report { path, name, depth } => {
                        let is_open = report_path.as_deref() == Some(path.as_path());
                        // Reports carry their own hue as well as their own
                        // glyph. Collections are the tree's default text
                        // colour, environments are `subst`; leaving reports on
                        // the default made them read as collections at a glance
                        // — the icon alone was too small a difference to catch
                        // while scanning a long tree.
                        let color = if is_open { theme.accent } else { theme.pending };
                        let text =
                            RichText::new(format!("{} {name}", super::icons::REPORT)).color(color);
                        let resp = ws_row(ui, *depth, is_open, text);
                        if resp.clicked() {
                            actions.push(WsAction::OpenReport(path.clone()));
                        }
                        ws_row_menu(&resp, sibling_dir(path), lbl_in_folder, s_new, &mut actions);
                        ws_drag_and_drop(ui, &resp, &theme, path, false, &mut actions);
                    }
                    WsRow::Environment { path, name, depth } => {
                        let text = RichText::new(format!("{} {name}", super::icons::ENV))
                            .color(theme.subst);
                        let resp = ws_row(ui, *depth, false, text);
                        if resp.clicked() || resp.double_clicked() {
                            actions.push(WsAction::OpenEnv {
                                path: path.clone(),
                                reveal: resp.double_clicked(),
                            });
                        }
                        ws_row_menu_with(
                            &resp,
                            sibling_dir(path),
                            lbl_in_folder,
                            s_new,
                            &mut actions,
                            |ui| {
                                let hit = ui.button(lbl_set_active_env).clicked();
                                ui.separator();
                                hit.then(|| WsAction::ActivateEnv(path.clone()))
                            },
                        );
                        ws_drag_and_drop(ui, &resp, &theme, path, false, &mut actions);
                    }
                }
            }
            if let Some(root) = &ws_root {
                ws_root_drop(ui, bg_rect, &folder_rects, &theme, root, &mut actions);
            }
        });

    for action in header_new.into_iter().chain(actions) {
        apply_ws_action(app, ci, action);
    }
}

/// The folder a file's siblings live in — where "new, next to this" means.
fn sibling_dir(path: &std::path::Path) -> PathBuf {
    path.parent().unwrap_or(path).to_path_buf()
}

/// The `New …` entries, shared by the header menu and every right-click menu so
/// they can't drift apart. Returns the kind chosen, if any.
///
/// The folder entry sits below a separator: the other three make a file to work
/// in, while a folder makes somewhere to put those files, which is a different
/// enough intent to be worth the line.
fn new_item_menu(
    ui: &mut egui::Ui,
    labels: NewItemLabels,
) -> Option<crate::workspace::NewItemKind> {
    use crate::workspace::NewItemKind;
    let (collection, report, env, folder) = labels;
    for (label, kind) in [
        (collection, NewItemKind::Collection),
        (report, NewItemKind::Report),
        (env, NewItemKind::Environment),
    ] {
        if ui.button(label).clicked() {
            return Some(kind);
        }
    }
    ui.separator();
    if ui.button(folder).clicked() {
        return Some(NewItemKind::Folder);
    }
    None
}

/// The `New …` menu labels: collection, report, environment, folder.
type NewItemLabels = (&'static str, &'static str, &'static str, &'static str);

/// The `New …` labels, pulled out of `Strings` once so the menu can be built
/// while the tree is borrowed.
fn new_item_labels(s: &crate::i18n::Strings) -> NewItemLabels {
    (
        s.gui_ws_new_collection,
        s.gui_ws_new_report,
        s.gui_ws_new_environment,
        s.gui_ws_new_folder,
    )
}

/// Create a new collection/report/environment in `dir` and open it.
///
/// The name is asked for with the platform's own save dialog seeded at `dir`,
/// rather than an in-app prompt: it already knows how to warn about
/// overwriting, shows what's in the folder, and lets the user put the file in a
/// subfolder — all of which an inline text box would have to reinvent. Anything
/// it comes back with is still checked against the workspace root, because the
/// dialog will happily let you navigate anywhere.
fn new_workspace_item(
    app: &mut GuiApp,
    ci: usize,
    dir: &std::path::Path,
    kind: crate::workspace::NewItemKind,
) {
    use crate::workspace::{NewItemError, NewItemKind};

    let Some(root) = app.session.collections[ci].workspace_root.clone() else {
        return;
    };
    // A folder is asked for by name rather than through the platform's save
    // dialog, which is built around naming a *file* and would append an
    // extension the folder must not have.
    if kind == crate::workspace::NewItemKind::Folder {
        app.dialog = Some(super::app::Dialog::Prompt {
            kind: super::app::PromptKind::NewWorkspaceFolder {
                ci,
                dir: dir.to_path_buf(),
            },
            text: String::new(),
        });
        return;
    }
    let s = &app.strings;
    let (title, default) = match kind {
        NewItemKind::Collection => (s.gui_ws_new_collection_title, "collection.hurl"),
        NewItemKind::Report => (s.gui_ws_new_report_title, "report.trail"),
        NewItemKind::Environment => (s.gui_ws_new_environment_title, "environment.vars"),
        // Diverted to the name prompt above.
        NewItemKind::Folder => return,
    };
    let ext = kind.extension();
    let Some(chosen) = super::filepick::save_file(title, Some(dir), default, &[(ext, &[ext])])
    else {
        return;
    };

    // The dialog returns an absolute path, which may be anywhere; hand it back
    // as a folder plus a name so the same containment checks apply as for a
    // name typed by hand.
    let parent = chosen.parent().unwrap_or(&root).to_path_buf();
    let name = chosen
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    match crate::workspace::create_item(&root, &parent, &name, kind) {
        Ok(path) => {
            // Reveal it: without its ancestors expanded, a file created in a
            // collapsed subfolder would appear to have gone nowhere.
            reveal_in_tree(app, ci, &path, &root);
            app.session.status = Some(crate::i18n::Status::WsItemCreated(
                crate::workspace::display_name(&root, &path),
            ));
            // Then open it, exactly as clicking its new row would.
            let follow = match kind {
                NewItemKind::Collection => WsAction::ToggleCollection {
                    path: path.clone(),
                    open: false,
                },
                NewItemKind::Report => WsAction::OpenReport(path.clone()),
                NewItemKind::Environment => WsAction::OpenEnv {
                    path: path.clone(),
                    // A just-created environment is empty and worth showing.
                    reveal: true,
                },
                NewItemKind::Folder => return,
            };
            // The status is set first because opening may replace it with
            // something more specific, which is the more useful message.
            let created = app.session.status.clone();
            apply_ws_action(app, ci, follow);
            if app.session.status.is_none() {
                app.session.status = created;
            }
        }
        Err(NewItemError::EmptyName) => {}
        Err(NewItemError::Escapes(what)) => {
            app.session.status = Some(crate::i18n::Status::WsItemEscaped(what));
        }
        Err(NewItemError::Exists(what)) => {
            app.session.status = Some(crate::i18n::Status::WsItemExists(what));
        }
        Err(NewItemError::Io(what)) => {
            app.session.status = Some(crate::i18n::Status::Error(what));
        }
    }
}

/// Create a subfolder in a Workspace tab, from the name typed into the prompt.
///
/// Folders are what makes a workspace navigable once it holds more than a
/// handful of files, and until now the tree could only ever show the folders
/// that already existed on disk. The name goes through the same
/// [`crate::workspace::create_item`] containment checks as a new file, so a
/// name with `..` or an absolute path in it is refused rather than quietly
/// creating a folder outside the workspace.
pub(super) fn new_workspace_folder(app: &mut GuiApp, ci: usize, dir: &std::path::Path, name: &str) {
    use crate::workspace::{NewItemError, NewItemKind};

    let Some(root) = app.session.collections[ci].workspace_root.clone() else {
        return;
    };
    match crate::workspace::create_item(&root, dir, name, NewItemKind::Folder) {
        Ok(path) => {
            // Reveal *and* expand it. A new folder is empty, so without being
            // opened it is an unremarkable closed row, and the next thing the
            // user wants is to drag something into it.
            reveal_in_tree(app, ci, &path, &root);
            app.session.collections[ci]
                .workspace_expanded
                .insert(path.clone());
            app.session.status = Some(crate::i18n::Status::WsItemCreated(
                crate::workspace::display_name(&root, &path),
            ));
            app.session.save();
        }
        Err(NewItemError::EmptyName) => {}
        Err(NewItemError::Escapes(what)) => {
            app.session.status = Some(crate::i18n::Status::WsItemEscaped(what));
        }
        Err(NewItemError::Exists(what)) => {
            app.session.status = Some(crate::i18n::Status::WsItemExists(what));
        }
        Err(NewItemError::Io(what)) => {
            app.session.status = Some(crate::i18n::Status::Error(what));
        }
    }
}

/// Move a workspace item into another folder and keep the app pointing at it.
///
/// The move itself is one `rename`; the work is everything that was holding the
/// old path — the loaded collection, the open report, the expanded-folder set,
/// the remembered selection. Left alone, those would all point at a file that
/// no longer exists, and the tab would appear to have lost its contents.
fn move_workspace_item(
    app: &mut GuiApp,
    ci: usize,
    src: &std::path::Path,
    dest_dir: &std::path::Path,
) {
    use crate::workspace::{MoveError, move_item, repoint};

    let Some(root) = app.session.collections[ci].workspace_root.clone() else {
        return;
    };
    let dest = match move_item(&root, src, dest_dir) {
        Ok(dest) => dest,
        Err(MoveError::Exists(what)) => {
            app.session.status = Some(crate::i18n::Status::WsItemMoveExists(what));
            return;
        }
        Err(MoveError::IntoItself) => {
            app.session.status = Some(crate::i18n::Status::WsItemMoveIntoItself);
            return;
        }
        Err(MoveError::Escapes(what)) => {
            app.session.status = Some(crate::i18n::Status::WsItemEscaped(what));
            return;
        }
        Err(MoveError::Io(what)) => {
            app.session.status = Some(crate::i18n::Status::Error(what));
            return;
        }
    };
    if dest == src {
        return;
    }

    // Every tab can be showing the moved file, not just the one dragged in:
    // two workspace tabs may be open on the same root.
    for col in &mut app.session.collections {
        if let Some(p) = col.path.clone().and_then(|p| repoint(&p, src, &dest)) {
            col.path = Some(p);
        }
        if let Some(p) = col
            .workspace_selected
            .clone()
            .and_then(|p| repoint(&p, src, &dest))
        {
            col.workspace_selected = Some(p);
        }
        col.workspace_expanded = col
            .workspace_expanded
            .iter()
            .map(|p| repoint(p, src, &dest).unwrap_or_else(|| p.clone()))
            .collect();
        col.workspace_titles = col
            .workspace_titles
            .drain()
            .map(|(p, v)| (repoint(&p, src, &dest).unwrap_or(p), v))
            .collect();
    }
    if let Some(ed) = app.report_editor.as_mut()
        && let Some(p) = ed.path().and_then(|p| repoint(p, src, &dest))
    {
        ed.report.path = Some(p);
    }

    reveal_in_tree(app, ci, &dest, &root);
    app.session.status = Some(crate::i18n::Status::WsItemMoved(
        crate::workspace::display_name(&root, &dest),
    ));
}

/// Expand every folder between the workspace root and `path`, so a newly
/// created (or newly moved) file is actually visible in the tree.
fn reveal_in_tree(app: &mut GuiApp, ci: usize, path: &std::path::Path, root: &std::path::Path) {
    let col = &mut app.session.collections[ci];
    let mut cur = path.parent();
    while let Some(dir) = cur {
        if !dir.starts_with(root) {
            break;
        }
        col.workspace_expanded.insert(dir.to_path_buf());
        if dir == root {
            break;
        }
        cur = dir.parent();
    }
    col.workspace_selected = Some(path.to_path_buf());
    app.session.save();
}

/// Treat the tree's empty space as the workspace root, so a dragged item can be
/// moved back out of a subfolder.
///
/// Whether the pointer is over a folder is decided geometrically rather than by
/// asking egui who is hovered: during a drag, hover is not a reliable way to
/// tell an occluded background from a visible one, and getting it wrong here
/// would move the file twice — once into the folder and once into the root.
fn ws_root_drop(
    ui: &mut egui::Ui,
    area: egui::Rect,
    folder_rects: &[egui::Rect],
    theme: &super::theme::GuiTheme,
    root: &std::path::Path,
    actions: &mut Vec<WsAction>,
) {
    let Some(dragged) = egui::DragAndDrop::payload::<WsDrag>(ui.ctx()) else {
        return;
    };
    let Some(pos) = ui.ctx().pointer_interact_pos() else {
        return;
    };
    if !area.contains(pos) || folder_rects.iter().any(|r| r.contains(pos)) {
        return;
    }
    // Something already at the top level has nowhere to go, so don't suggest it.
    if dragged.0.parent() == Some(root) {
        return;
    }
    ui.painter().rect_stroke(
        area.shrink(1.0),
        6.0,
        egui::Stroke::new(1.0, theme.accent),
        egui::StrokeKind::Inside,
    );
    if ui.input(|i| i.pointer.any_released()) {
        actions.push(WsAction::MoveItem {
            src: dragged.0.clone(),
            dest_dir: root.to_path_buf(),
        });
        egui::DragAndDrop::clear_payload(ui.ctx());
    }
}

/// Attach the "New in this folder" right-click menu to a tree row.
///
/// Every row gets one, not just folders: right-clicking a file to add another
/// one beside it is the same gesture, and having a menu appear on some rows but
/// not others is worse than having it everywhere. `dir` is the folder the new
/// file lands in — the folder itself for a folder row, its parent for a file.
fn ws_row_menu(
    resp: &egui::Response,
    dir: PathBuf,
    header: &'static str,
    labels: NewItemLabels,
    actions: &mut Vec<WsAction>,
) {
    ws_row_menu_with(resp, dir, header, labels, actions, |_| None);
}

/// [`ws_row_menu`] plus row-specific entries above the shared "New …" ones.
///
/// A row can only carry one context menu, so anything extra has to be built
/// into the same closure rather than attached separately.
fn ws_row_menu_with(
    resp: &egui::Response,
    dir: PathBuf,
    header: &'static str,
    labels: NewItemLabels,
    actions: &mut Vec<WsAction>,
    extra: impl FnOnce(&mut egui::Ui) -> Option<WsAction>,
) {
    resp.context_menu(|ui| {
        let mut extra = Some(extra);
        if let Some(action) = extra.take().and_then(|f| f(ui)) {
            actions.push(action);
            ui.close();
        }
        ui.label(header);
        ui.separator();
        if let Some(kind) = new_item_menu(ui, labels) {
            actions.push(WsAction::NewItem { dir, kind });
            ui.close();
        }
    });
}

/// Apply one collected [`WsAction`] to the session (mutations are deferred out
/// of the render pass so the tree is read immutably while drawing).
fn apply_ws_action(app: &mut GuiApp, ci: usize, action: WsAction) {
    // Whatever the user just clicked is what the tab should reopen on next
    // launch (see `Collection::workspace_selected`). Recorded up front so every
    // arm below gets it, including the ones that bail out early on an error.
    match &action {
        WsAction::ToggleCollection { path, .. }
        | WsAction::SelectRequest {
            collection: path, ..
        }
        | WsAction::RunRequest {
            collection: path, ..
        }
        | WsAction::OpenReport(path)
        | WsAction::OpenEnv { path, .. }
        | WsAction::ActivateEnv(path) => {
            app.session.collections[ci].workspace_selected = Some(path.clone());
        }
        // Opening or closing a folder isn't "working on" anything; a new or
        // moved file records itself once it is actually there.
        WsAction::ToggleFolder(_) | WsAction::NewItem { .. } | WsAction::MoveItem { .. } => {}
    }
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
            app.close_report_editor();
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
            app.close_report_editor();
            app.focus = super::Focus::List;
            app.session.save();
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
            app.close_report_editor();
            app.session.save();
            app.run_active();
        }
        WsAction::OpenReport(path) => match crate::report::Report::load_local(&path) {
            Ok(report) => {
                app.open_report_editor(super::report_editor::ReportOrigin::Workspace, report);
                app.focus = super::Focus::Main;
                app.session.save();
            }
            Err(e) => {
                app.session.status = Some(crate::i18n::Status::Error(e));
            }
        },
        WsAction::NewItem { dir, kind } => new_workspace_item(app, ci, &dir, kind),
        WsAction::MoveItem { src, dest_dir } => move_workspace_item(app, ci, &src, &dest_dir),
        WsAction::OpenEnv { path, reveal } => {
            let id = app.session.open_workspace_environment(&path);
            if reveal {
                app.reveal_env = id;
            }
            app.close_report_editor();
            app.session.save();
        }
        // Activating a file that isn't open yet has to open it first. An
        // already-open one is reused rather than loaded again, so activating
        // twice can't leave two copies of the same file in the panel — and
        // `set_active_env` is a toggle, so re-activating the active one turns
        // substitution off, exactly as the Environments panel's button does.
        WsAction::ActivateEnv(path) => {
            let existing = app
                .session
                .global_envs
                .iter()
                .find(|e| e.path.as_deref() == Some(path.as_path()))
                .map(|e| e.id);
            let id = match existing {
                Some(id) => Some(id),
                None => app.session.open_workspace_environment(&path),
            };
            if id.is_some() {
                // `set_active_env` is a toggle, so an already-active
                // environment would be *deactivated* by it — not what "Set as
                // active" asks for.
                if app.session.active_env_id != id {
                    app.session.set_active_env(id);
                }
                app.reveal_env = id;
            }
            app.session.save();
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Right-clicking a workspace environment file offers making it the active
    /// environment. The menu itself is egui-driven, so what's checked here is
    /// the action it raises: loading the file if needed and activating it,
    /// without the toggle behaviour that would *deactivate* an already-active
    /// one.
    #[test]
    fn activating_a_workspace_environment_from_the_tree_loads_it_and_makes_it_active() {
        let dir = ws_tmp("activate");
        let env = dir.join("api/v1/dev.vars");
        let mut session = crate::session::Session::default();
        session.collections.clear();
        let ci = session.open_workspace(dir.clone());
        session.active_tab = ci;
        let mut app = GuiApp::for_test(session);

        apply_ws_action(&mut app, ci, WsAction::ActivateEnv(env.clone()));

        assert_eq!(app.session.global_envs.len(), 1, "the file was loaded");
        let id = app.session.global_envs[0].id;
        assert_eq!(app.session.active_env_id, Some(id));
        assert_eq!(app.reveal_env, Some(id), "and it is shown in the panel");

        // Again: neither a second copy nor a deactivation.
        apply_ws_action(&mut app, ci, WsAction::ActivateEnv(env));
        assert_eq!(app.session.global_envs.len(), 1);
        assert_eq!(app.session.active_env_id, Some(id));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Collect the text of every id-clash complaint egui painted this frame.
    ///
    /// egui flags two interactive widgets that share an id (but not a rect) by
    /// stroking a red rectangle around the offender and writing a `🔥 …` note
    /// next to it — with no log line and no return value, so the only way to
    /// see it from a test is to read the shapes back out. The note is the
    /// searchable half; the red rectangle the user actually notices is painted
    /// immediately before it.
    fn id_clashes(shapes: &[egui::epaint::ClippedShape]) -> Vec<String> {
        fn walk(shape: &egui::epaint::Shape, out: &mut Vec<String>) {
            match shape {
                egui::epaint::Shape::Text(t) => {
                    let text = t.galley.text();
                    if text.contains('\u{1f525}') {
                        out.push(text.to_string());
                    }
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

    /// Draw a whole application frame and report egui's id-clash complaints.
    /// The panel-only harness can't see a clash between two *different* panels
    /// (the tab strip and the tree both draw the edit pencil), so the flashing
    /// red square has to be hunted for across a complete frame.
    fn draw_app(ctx: &egui::Context, app: &mut GuiApp, pointer: egui::Pos2) -> Vec<String> {
        redirect_saved_state();
        // Match the real app's font set-up: the icon glyphs are a big part of
        // every row's width, and layout is exactly what decides whether two
        // widgets end up sharing an id at different rects.
        let mut fonts = egui::FontDefinitions::default();
        egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Light);
        ctx.set_fonts(fonts);
        let mut input = egui::RawInput::default();
        input.screen_rect = Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(1200.0, 800.0),
        ));
        input.events.push(egui::Event::PointerMoved(pointer));
        let out = ctx.run_ui(input, |ui| app.draw(ui));
        id_clashes(&out.shapes)
    }

    #[test]
    fn the_id_clash_detector_really_sees_a_clash() {
        let ctx = egui::Context::default();
        let mut input = egui::RawInput::default();
        input.screen_rect = Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(400.0, 600.0),
        ));
        let out = ctx.run_ui(input, |ui| {
            let r1 = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(10.0, 10.0));
            let r2 = egui::Rect::from_min_size(egui::pos2(50.0, 50.0), egui::vec2(10.0, 10.0));
            ui.interact(r1, egui::Id::new("dup"), egui::Sense::click());
            ui.interact(r2, egui::Id::new("dup"), egui::Sense::click());
        });
        let c = id_clashes(&out.shapes);
        assert!(!c.is_empty(), "detector should see the clash");
    }

    /// Point saved state at a scratch directory for the whole test binary.
    ///
    /// Drawing a frame persists the layout (see `GuiApp::record_layout`), so a
    /// test that renders the app would otherwise overwrite the *developer's own*
    /// `state.json`. Set once and never unset: every test in this binary is
    /// better off writing to a scratch dir, and the environment is process-wide,
    /// so flipping it back mid-run would race the other test threads.
    pub(crate) fn redirect_saved_state() {
        static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        ONCE.get_or_init(|| {
            let dir =
                std::env::temp_dir().join(format!("paperboy_gui_state_{}", std::process::id()));
            let _ = std::fs::create_dir_all(&dir);
            // SAFETY: set before any frame is drawn and never changed again, so
            // no other thread can observe it mid-write.
            unsafe { std::env::set_var("PAPERBOY_STATE_DIR", &dir) };
        });
    }

    /// A workspace fixture shaped like a real one: a nested folder holding two
    /// collection files alongside an environment and a report, so every row
    /// kind the tree can draw is in the frame.
    fn ws_tmp(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "paperboy_gui_reqs_{tag}_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let api = dir.join("api/v1");
        std::fs::create_dir_all(&api).unwrap();
        std::fs::write(dir.join("health.hurl"), "GET https://example.com/health\n").unwrap();
        std::fs::write(
            api.join("one.hurl"),
            "GET https://example.com/a\nHTTP 200\n\nGET https://example.com/a2\n",
        )
        .unwrap();
        std::fs::write(
            api.join("two.hurl"),
            "GET https://example.com/b\nHTTP 200\n\nGET https://example.com/b2\n",
        )
        .unwrap();
        std::fs::write(api.join("dev.vars"), "BASE_URL=https://example.com\n").unwrap();
        std::fs::write(api.join("run.trail"), "{\"nodes\":[]}\n").unwrap();
        dir
    }

    /// Open the whole fixture tree so every row is drawn.
    fn expand_all(col: &mut crate::collection::Collection, dir: &std::path::Path) {
        for p in [
            dir.to_path_buf(),
            dir.join("api"),
            dir.join("api/v1"),
            dir.join("api/v1/one.hurl"),
            dir.join("api/v1/two.hurl"),
            dir.join("health.hurl"),
        ] {
            col.workspace_expanded.insert(p);
        }
    }

    /// Draw the requests panel once and hand back whatever egui complained
    /// about. A real screen rect matters: the panel lays rows out inside a
    /// `ScrollArea`, which culls anything it thinks is offscreen.
    fn draw_panel(app: &mut GuiApp) -> Vec<String> {
        let ctx = egui::Context::default();
        let mut input = egui::RawInput::default();
        input.screen_rect = Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(400.0, 600.0),
        ));
        let out = ctx.run_ui(input, |panel| crate::gui::requests::ui(app, panel));
        id_clashes(&out.shapes)
    }

    /// The red square the user sees flash around the edit pencil is egui's
    /// id-clash warning: two interactive widgets in one frame claiming the same
    /// id. A row's decorations (method badge, run marker, pencil) come and go
    /// with which collection the tab holds, and egui numbers widgets by how
    /// many preceded them — so without a stable per-row id namespace, changing
    /// the focused collection makes rows collide.
    #[test]
    fn switching_the_loaded_collection_never_provokes_an_egui_id_clash() {
        let dir = ws_tmp("clash");
        let mut session = crate::session::Session::default();
        session.collections.clear();
        let ci = session.open_workspace(dir.clone());
        expand_all(&mut session.collections[ci], &dir);
        assert!(session.load_workspace_file(ci, dir.join("api/v1/one.hurl")));
        // An unsaved edit, so the pencil is actually drawn.
        session.collections[ci].entries[0].modified = true;

        // Guard the fixture: if the tree ever stops listing request rows, the
        // test would pass by drawing nothing at all.
        assert!(
            session.collections[ci]
                .ws_rows()
                .iter()
                .any(|r| matches!(r, crate::collection::WsRow::Request { .. })),
            "fixture must list request rows, or there is no pencil to clash over"
        );
        assert!(
            session.collections[ci].workspace_request_edited(&dir.join("api/v1/one.hurl"), 0),
            "fixture must have an edited request, or no pencil is drawn"
        );

        let mut app = GuiApp::for_test(session);
        // Two frames per state: egui settles layout over a frame boundary, and
        // a clash is a within-frame duplicate, so the settled frame is the one
        // that matters.
        for _ in 0..2 {
            let clashes = draw_panel(&mut app);
            assert!(
                clashes.is_empty(),
                "drawing the workspace tree with an edited request clashed: {clashes:?}"
            );
        }

        // Now switch which file the tab holds — the transition the user saw the
        // red square on. `one.hurl`'s rows lose their badge and run marker, and
        // `two.hurl`'s gain them, while the parked edit keeps its pencil.
        assert!(
            app.session
                .load_workspace_file(ci, dir.join("api/v1/two.hurl"))
        );
        // The parked file must still be listed *and* still flagged edited,
        // or the frame no longer contains the row this test is about.
        assert!(
            app.session.collections[ci].workspace_request_edited(&dir.join("api/v1/one.hurl"), 0),
            "the parked collection must keep its pencil after the switch"
        );
        assert!(
            app.session.collections[ci]
                .ws_rows()
                .iter()
                .any(|r| matches!(
                    r,
                    crate::collection::WsRow::Request { collection, loaded: false, .. }
                        if collection == &dir.join("api/v1/one.hurl")
                )),
            "the parked collection's request rows must still be drawn"
        );
        for _ in 0..2 {
            let clashes = draw_panel(&mut app);
            assert!(
                clashes.is_empty(),
                "switching the loaded collection clashed: {clashes:?}"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The same red square, hunted across a whole frame: the edit pencil is
    /// drawn on the collection *tab* as well as in the tree, and changing which
    /// tab is active relays out every panel at once.
    #[test]
    fn switching_the_active_tab_never_provokes_an_egui_id_clash() {
        let dir = ws_tmp("tabclash");
        let mut session = crate::session::Session::default();
        session.collections.clear();
        session.add_collection("scratch");
        // A plain (non-workspace) collection with an edit of its own, so the
        // request-list rows are exercised as well as the workspace tree.
        for (i, url) in ["https://example.com/x", "https://example.com/y"]
            .into_iter()
            .enumerate()
        {
            let mut e = crate::hurl::HurlEntry::default();
            e.title = format!("req {i}");
            e.method = "GET".into();
            e.url = url.into();
            session.collections[0].entries.push(e);
        }
        session.collections[0].entries[1].modified = true;
        session.collections[0].entries[0].last_run = RunStatus::Passed;
        let ci = session.open_workspace(dir.clone());
        expand_all(&mut session.collections[ci], &dir);
        assert!(session.load_workspace_file(ci, dir.join("api/v1/one.hurl")));
        session.collections[ci].entries[0].modified = true;

        let mut app = GuiApp::for_test(session);
        let ctx = egui::Context::default();
        // Park the pointer over the left panel, where the pencils are, so any
        // hover-only decoration is drawn too.
        let over_tree = egui::pos2(80.0, 200.0);
        for _ in 0..2 {
            let c = draw_app(&ctx, &mut app, over_tree);
            assert!(c.is_empty(), "the workspace tab clashed: {c:?}");
        }
        for active in [0usize, ci, 0, ci] {
            app.session.activate_tab(active);
            // Two frames: the transition frame and the settled one.
            for _ in 0..2 {
                let c = draw_app(&ctx, &mut app, over_tree);
                assert!(c.is_empty(), "activating tab {active} clashed: {c:?}");
            }
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The workspace tree's background target (the empty space that stands in
    /// for the workspace root) is registered *before* the rows, and relies on
    /// egui giving an overlapped click to whichever widget was registered last.
    /// If that ever changed, the background would swallow every row's click and
    /// right-click menu, so it is worth pinning down.
    #[test]
    fn an_overlapped_click_goes_to_the_widget_registered_last() {
        let ctx = egui::Context::default();
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0));
        let mut input = egui::RawInput::default();
        input
            .events
            .push(egui::Event::PointerMoved(egui::pos2(50.0, 50.0)));

        let (mut background, mut row) = (false, false);
        // Several passes: egui settles interaction state over a frame boundary.
        for _ in 0..3 {
            let _ = ctx.run_ui(input.clone(), |ui| {
                let bg = ui.interact(rect, ui.id().with("background"), egui::Sense::click());
                let r = ui.interact(rect, ui.id().with("row"), egui::Sense::click());
                background = bg.hovered();
                row = r.hovered();
            });
        }
        assert!(row, "the row, registered last, is the one hovered");
        assert!(
            !background,
            "so the background behind it never steals the row's clicks"
        );
    }
}
