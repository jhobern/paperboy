//! Left-top panel: the request list for the active collection, shown as a
//! Postman-style collapsible folder tree (folders come from the `/`-encoded
//! request titles via [`crate::tree::entry_path`], the same convention the
//! terminal UI uses). Add / select / rename / delete / run requests, plus
//! per-collection Run All.

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

/// Draw a tree row and make **all** of it clickable, not just the label in it.
///
/// A row is a method badge, a name and a couple of markers, but only the name
/// was a widget — clicking the badge, or the gap between the name and the
/// marker, did nothing at all, so selecting a request meant aiming at its
/// text. The row's full width is interacted with as one target and unioned
/// with whatever the content returned, so a click anywhere on the line counts
/// (and a click on the label still behaves exactly as it did).
///
/// The hover wash is painted *behind* the content — reserved before it is
/// drawn and filled in afterwards — so the row shows its own extent, which is
/// how the widened target announces itself. The terminal UI needs none of
/// this: it hit-tests a click by which line it landed on, so its rows have
/// always been clickable end to end.
fn clickable_row(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    content: impl FnOnce(&mut egui::Ui) -> egui::Response,
) -> egui::Response {
    let bg = ui.painter().add(egui::Shape::Noop);
    let drawn = ui.push_id(id, content);
    let rect = drawn.response.rect;
    let hit = ui.interact(
        rect,
        drawn.response.id.with("whole_row"),
        // Senses drags as well as clicks so a request can be dragged to a new
        // position in the collection; a click still lands as a click, since
        // egui only calls it a drag once the pointer actually moves. Same
        // bargain the workspace tree's rows make (see `ws_row`).
        egui::Sense::click_and_drag(),
    );
    if hit.hovered() || drawn.inner.hovered() {
        let visuals = ui.visuals().widgets.hovered;
        ui.painter().set(
            bg,
            egui::epaint::RectShape::filled(rect, visuals.corner_radius, visuals.weak_bg_fill),
        );
    }
    drawn.inner | hit
}

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

/// A folder node of the request tree: its children in the order the file lists
/// them, folders and requests interleaved.
#[derive(Default)]
struct Node {
    children: Vec<Child>,
}

/// One row of a [`Node`], either a nested folder or a flat index into the
/// collection's `entries`.
enum Child {
    Folder(String, Node),
    Entry(usize),
}

/// The navigable request rows of `col`, in the order they appear on screen, as
/// flat entry indices. This is what the Requests-panel keyboard steps through:
/// a filtered list is the flat set of matches (folders are gone), an unfiltered
/// one is the folder tree walked exactly as [`render_node`] draws it — every
/// folder's contents before the entries beside it — so Up/Down move to the row
/// the eye expects rather than jumping around the underlying `entries` order.
/// Folder rows are not navigable targets (there is no "selected folder"), so
/// they are absent; the result holds only request indices.
pub(super) fn nav_entry_order(col: &crate::collection::Collection) -> Vec<usize> {
    if col.list_filter_active() {
        col.rows()
            .into_iter()
            .filter_map(|r| match r {
                crate::tree::Row::Entry(i) => Some(i),
                _ => None,
            })
            .collect()
    } else {
        let tree = build_tree(&col.entries, col.list_sort);
        let mut out = Vec::with_capacity(col.entries.len());
        collect_entry_order(&tree, &mut out);
        out
    }
}

/// Flatten a folder tree into entry indices the way [`render_node`] renders it:
/// every child in turn, recursing into folders.
fn collect_entry_order(node: &Node, out: &mut Vec<usize>) {
    for child in &node.children {
        match child {
            Child::Folder(_, sub) => collect_entry_order(sub, out),
            Child::Entry(i) => out.push(*i),
        }
    }
}

/// Group a collection's entries into a folder tree using their `/`-encoded
/// titles. The leaf segment is the request's display name; everything before
/// it is its folder path.
///
/// Order is the file's, at every level: a folder is created where its first
/// request appears and keeps that position, so what is drawn matches what Run
/// All executes. See [`crate::tree::rows_for`], which the terminal UI builds
/// the same way. `sort` then reorders each level's children for display only.
fn build_tree(entries: &[HurlEntry], sort: crate::tree::SortMode) -> Node {
    let mut root = Node::default();
    for (i, e) in entries.iter().enumerate() {
        let path = entry_path(&e.title);
        let (folders, _leaf) = path.split_at(path.len() - 1);
        insert(&mut root, folders, i);
    }
    sort_node(&mut root, entries, sort);
    root
}

/// Reorder every level of the tree by display name. Folders and requests sort
/// together rather than in separate blocks, so A-Z means what it looks like,
/// and the sort is stable, so `SortMode::File` leaves the file's order alone.
fn sort_node(node: &mut Node, entries: &[HurlEntry], sort: crate::tree::SortMode) {
    let name = |child: &Child| match child {
        Child::Folder(n, _) => n.clone(),
        Child::Entry(i) => entries
            .get(*i)
            .map(crate::tree::leaf_name)
            .unwrap_or_default(),
    };
    node.children
        .sort_by(|a, b| crate::tree::cmp_names(sort, &name(a), &name(b)));
    for child in &mut node.children {
        if let Child::Folder(_, sub) = child {
            sort_node(sub, entries, sort);
        }
    }
}

fn insert(node: &mut Node, folders: &[String], i: usize) {
    let Some((head, rest)) = folders.split_first() else {
        node.children.push(Child::Entry(i));
        return;
    };
    let existing = node
        .children
        .iter_mut()
        .find(|c| matches!(c, Child::Folder(name, _) if name == head));
    if let Some(Child::Folder(_, sub)) = existing {
        insert(sub, rest, i);
        return;
    }
    let mut sub = Node::default();
    insert(&mut sub, rest, i);
    node.children.push(Child::Folder(head.clone(), sub));
}

/// Actions collected while rendering the (immutably-borrowed) tree, applied to
/// the session afterwards.
#[derive(Default)]
struct Actions {
    select: Option<usize>,
    run: Option<usize>,
    rename: Option<usize>,
    duplicate: Option<usize>,
    delete: Option<usize>,
    /// A request dragged to a new position: `(from, before)`, where `before` is
    /// the index of the request it was dropped above (`entries.len()` for the
    /// gap past the last one). Applied with
    /// [`crate::collection::Collection::move_entry_before`].
    reorder: Option<(usize, usize)>,
}

/// The request being dragged, identified by its index into `entries` — which is
/// all the drop target needs, since the gap it lands in is decided by whichever
/// row the pointer is over.
#[derive(Clone, Debug)]
struct ReqDrag(usize);

/// The row's context-menu / marker labels, bundled so adding one doesn't grow
/// `render_node`'s parameter list again — it had already picked up a run,
/// rename, delete and edited-marker label as separate arguments, and
/// duplicate would have made a fifth.
struct RowLabels<'a> {
    untitled: &'a str,
    run: &'a str,
    rename: &'a str,
    duplicate: &'a str,
    delete: &'a str,
    edited: &'a str,
}

#[allow(clippy::too_many_arguments)]
fn render_node(
    ui: &mut egui::Ui,
    node: &Node,
    entries: &[HurlEntry],
    selected: usize,
    reveal: bool,
    theme: &GuiTheme,
    id_prefix: &str,
    labels: &RowLabels<'_>,
    reorderable: bool,
    actions: &mut Actions,
) {
    for child in &node.children {
        match child {
            Child::Folder(name, sub) => {
                let salt = format!("{id_prefix}/{name}");
                super::widgets::tree_header(
                    ui,
                    &salt,
                    true,
                    RichText::new(format!("{} {name}", super::icons::FOLDER)).color(theme.text),
                    |ui| {
                        render_node(
                            ui,
                            sub,
                            entries,
                            selected,
                            reveal,
                            theme,
                            &salt,
                            labels,
                            reorderable,
                            actions,
                        );
                    },
                );
            }
            Child::Entry(i) => {
                let i = *i;
                let leaf = crate::tree::leaf_name(&entries[i]);
                render_entry_row(
                    ui,
                    i,
                    entries,
                    leaf,
                    selected,
                    reveal,
                    theme,
                    labels,
                    reorderable,
                    actions,
                );
            }
        }
    }
}

/// One request row — the shared body of the folder tree and the flat filtered
/// list, which differ only in the label they show and whether a row can be
/// dragged.
///
/// `label` is the request's own name in the tree (the folder rows above it
/// supply the rest) but its whole title when filtered, where the tree has been
/// flattened and two folders may each hold a `Login`.
///
/// `reorderable` is false while the list is filtered or sorted: in both, the
/// gap between two rows can span any number of requests that aren't beside
/// them in the file, so a drop there would move the request an unpredictable
/// distance for reasons the user can't see.
#[allow(clippy::too_many_arguments)]
fn render_entry_row(
    ui: &mut egui::Ui,
    i: usize,
    entries: &[HurlEntry],
    label: String,
    selected: usize,
    reveal: bool,
    theme: &GuiTheme,
    labels: &RowLabels<'_>,
    reorderable: bool,
    actions: &mut Actions,
) {
    {
        let entry = &entries[i];
        let label = if label.trim().is_empty() {
            if entry.url.trim().is_empty() {
                labels.untitled.to_string()
            } else {
                entry.url.clone()
            }
        } else {
            label
        };
        let (marker, ok) = run_marker(entry.last_run);
        let is_sel = i == selected;

        // Give the row its own id namespace, keyed by the request it draws.
        // egui derives a widget's id from how many widgets preceded it, so the
        // optional decorations on a row (method badge, run marker, edit pencil)
        // silently renumber everything after them the moment one appears or
        // disappears — which egui then flags with a red id-clash outline. A
        // stable per-row salt keeps the row's ids tied to the row itself.
        let row = clickable_row(ui, ("req_row", i), |ui| {
            ui.horizontal(|ui| {
                super::widgets::method_badge(ui, theme, &entry.method);
                // A request that isn't selected is still a request: `dim` is
                // the colour this app uses for things that don't apply
                // (disabled rows, hints), and spending it on every name in the
                // tree made the whole list look switched off. The selected one
                // leads with weight instead.
                let text = if is_sel {
                    RichText::new(&label).strong().color(theme.text)
                } else {
                    RichText::new(&label).color(theme.text)
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
                    edited_marker(ui, entry, theme, labels.edited);
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        super::widgets::selectable_row(ui, is_sel, text)
                    })
                    .inner
                })
                .inner
            })
            .inner
        });
        if row.clicked() {
            actions.select = Some(i);
        }
        // A keyboard move can select a row that is scrolled out of sight (a
        // long collection, or Home/End jumping to an end); bring it back into
        // view. Only on the selected row, and only when the move asked for it,
        // so an ordinary scroll or click isn't yanked back.
        //
        // `None` rather than an alignment: it scrolls the least it can to get
        // the row on screen, and does nothing at all when the row is already
        // visible. Asking for `Align::Center` instead re-centres the list on
        // *every* step, so a row-by-row walk drags the whole scrollbar along
        // under a cursor that never appears to move — which is not how a list
        // behaves anywhere else.
        if reveal && is_sel {
            row.scroll_to_me(None);
        }
        if row.double_clicked() {
            actions.run = Some(i);
        }
        row.context_menu(|ui| {
            if ui.button(labels.run).clicked() {
                actions.run = Some(i);
                ui.close();
            }
            if ui.button(labels.rename).clicked() {
                actions.rename = Some(i);
                ui.close();
            }
            // Duplicate sits next to Rename — both are creative actions on the
            // row's identity — leaving Delete alone at the bottom as the one
            // destructive entry.
            if ui.button(labels.duplicate).clicked() {
                actions.duplicate = Some(i);
                ui.close();
            }
            if ui.button(labels.delete).clicked() {
                actions.delete = Some(i);
                ui.close();
            }
        });
        if reorderable {
            if row.drag_started() {
                egui::DragAndDrop::set_payload(ui.ctx(), ReqDrag(i));
            }
            // Something has to follow the pointer, or a drag looks like nothing
            // is happening — the rows themselves stay put, and the insertion
            // line only appears once the pointer is over a row.
            if row.dragged()
                && let Some(pos) = ui.ctx().pointer_interact_pos()
            {
                let layer =
                    egui::LayerId::new(egui::Order::Tooltip, ui.id().with("req_drag_label"));
                ui.ctx().layer_painter(layer).text(
                    pos + egui::vec2(12.0, 4.0),
                    egui::Align2::LEFT_TOP,
                    &label,
                    egui::TextStyle::Button.resolve(ui.style()),
                    theme.accent,
                );
            }
            request_drop_zone(ui, &row, theme, i, actions);
        }
    }
}

/// Offer the gap above or below this row as somewhere the dragged request can
/// land, and draw the line showing which.
///
/// A line *between* rows rather than a highlight *on* one: this is a reorder,
/// so the question is which two requests it will end up between — an outlined
/// row would say "into here", which is what the workspace tree's folder drop
/// means and would read as moving the request into another request.
fn request_drop_zone(
    ui: &mut egui::Ui,
    resp: &egui::Response,
    theme: &GuiTheme,
    i: usize,
    actions: &mut Actions,
) {
    let Some(dragged) = egui::DragAndDrop::payload::<ReqDrag>(ui.ctx()) else {
        return;
    };
    if !resp.contains_pointer() {
        return;
    }
    let Some(pos) = ui.ctx().pointer_interact_pos() else {
        return;
    };
    let rect = resp.rect;
    // Which half of the row the pointer is in decides which gap is being aimed
    // at, so every pixel of the list targets a gap and there is no dead zone
    // between rows to fall down.
    let above = pos.y < rect.center().y;
    let before = if above { i } else { i + 1 };
    // Both gaps touching the dragged request are where it already is. Drawing a
    // line there would promise a move that `move_entry_before` correctly
    // refuses to make.
    if dragged.0 == before || dragged.0 + 1 == before {
        return;
    }
    let y = if above { rect.top() } else { rect.bottom() };
    ui.painter()
        .hline(rect.x_range(), y, egui::Stroke::new(2.0, theme.accent));
    if ui.input(|i| i.pointer.any_released()) {
        actions.reorder = Some((dragged.0, before));
        egui::DragAndDrop::clear_payload(ui.ctx());
    }
}

/// How a "Run All" of these entries should be gated: `(total, non_get)`, where
/// `total` is how many requests would run and `non_get` how many of them use a
/// method other than GET. A collection with `non_get == 0` is read-only and
/// runs with no prompt; any other needs [`GuiApp::request_run_all`] to confirm
/// first, because a stray click on Run All could otherwise fire a collection
/// full of writes. GET is matched case-insensitively, the way Hurl treats it.
pub(super) fn run_all_confirm_counts(entries: &[HurlEntry]) -> (usize, usize) {
    let non_get = entries
        .iter()
        .filter(|e| !e.method.eq_ignore_ascii_case("GET"))
        .count();
    (entries.len(), non_get)
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
        lbl_duplicate,
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
        s.gui_duplicate,
        s.gui_delete,
        s.gui_edited_request,
    );
    let (lbl_import, lbl_import_file, lbl_import_account, tip_import_file, tip_import_account) = (
        s.gui_import_postman_button,
        s.gui_menu_import_file,
        s.gui_menu_import_account,
        s.help_menu_import_file,
        s.help_menu_import_account,
    );
    let (hint_filter, lbl_filter_no_matches) =
        (s.gui_request_filter_hint, s.gui_request_filter_no_matches);
    let (lbl_sort_file, lbl_sort_alpha, lbl_sort_reverse, tip_sort) = (
        s.gui_sort_file,
        s.gui_sort_alpha,
        s.gui_sort_reverse,
        s.help_sort_button,
    );

    // Header: collection name (truncates) + Run All / Add (always visible).
    let name = app.session.collections[ci].name.clone();
    super::widgets::panel_header(ui, &theme, name, |ui| {
        let run_all = format!("{} {}", super::icons::PLAY, lbl_run_all);
        if ui.button(run_all).on_hover_text(tip_run_all).clicked() {
            app.request_run_all(ci);
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

    // Filter box and sort button. Shown whenever there is more than one
    // request, and always once it is: the same reasoning as the Environments
    // panel's, where an empty box costs one line and a tree of a few hundred
    // requests is unusable without one. Hidden for a collection small enough
    // to read at a glance, where they would be controls with nothing to do.
    if app.session.collections[ci].entries.len() > 1 {
        ui.horizontal(|ui| {
            // Right-to-left so the button takes its natural width first and
            // the filter's `desired_width(INFINITY)` fills whatever is left,
            // instead of claiming the row and pushing the button off it.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let sort = app.session.collections[ci].list_sort;
                let (icon, label) = match sort {
                    crate::tree::SortMode::File => (super::icons::SORT_FILE, lbl_sort_file),
                    crate::tree::SortMode::Alpha => (super::icons::SORT_ASC, lbl_sort_alpha),
                    crate::tree::SortMode::ReverseAlpha => {
                        (super::icons::SORT_DESC, lbl_sort_reverse)
                    }
                };
                // The icon alone can't say which of three states the button is
                // in, so the current mode is named in the tooltip above the
                // explanation.
                if ui
                    .button(icon)
                    .on_hover_text(format!("{label}\n\n{tip_sort}"))
                    .clicked()
                {
                    app.session.collections[ci].list_sort = sort.next();
                }
                super::widgets::flat_fields(ui, |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut app.session.collections[ci].list_query)
                            .hint_text(hint_filter)
                            .desired_width(f32::INFINITY),
                    )
                });
            });
        });
        ui.separator();
    }

    let selected = app.session.collections[ci].selected_entry;
    let reveal = app.reveal_selected;
    let filtering = app.session.collections[ci].list_filter_active();
    let sort = app.session.collections[ci].list_sort;
    let query = app.session.collections[ci].list_query.clone();
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
            super::widgets::tree_rhythm(ui);
            let entries = &app.session.collections[ci].entries;
            if entries.is_empty() {
                ui.add_space(8.0);
                ui.colored_label(theme.dim, lbl_no_requests);
                // The other way a first collection arrives. Someone coming from
                // Postman meets this panel before they meet the File menu, and
                // an empty list is exactly the moment "where are my requests?"
                // is being asked.
                ui.add_space(6.0);
                ui.menu_button(lbl_import, |ui| {
                    if ui
                        .button(lbl_import_file)
                        .on_hover_text(tip_import_file)
                        .clicked()
                    {
                        super::menu::open_via_picker(app, super::app::OpenKind::PostmanExport);
                        ui.close();
                    }
                    if ui
                        .button(lbl_import_account)
                        .on_hover_text(tip_import_account)
                        .clicked()
                    {
                        app.postman.open();
                        ui.close();
                    }
                });
                return;
            }
            let tree = build_tree(entries, sort);
            let labels = RowLabels {
                untitled: lbl_untitled,
                run: lbl_run,
                rename: lbl_rename,
                duplicate: lbl_duplicate,
                delete: lbl_delete,
                edited: lbl_edited,
            };
            // A filtered list is flat and folder-blind (see
            // `crate::tree::rows_matching`): the tree shows one collapsible
            // folder per level, and leaving those in would mean hiding matches
            // inside collapsed folders — a search that can't show you what it
            // found. Each match carries its whole title instead, since the
            // folder rows that told two `Login`s apart are gone.
            if filtering {
                let mut matches = crate::tree::rows_matching(entries, &query);
                crate::tree::sort_rows(&mut matches, entries, sort);
                if matches.is_empty() {
                    ui.add_space(8.0);
                    ui.colored_label(theme.dim, lbl_filter_no_matches);
                    return;
                }
                for row in matches {
                    let crate::tree::Row::Entry(i) = row else {
                        continue;
                    };
                    let title = entries[i].title.trim().to_string();
                    render_entry_row(
                        ui,
                        i,
                        entries,
                        title,
                        selected,
                        reveal,
                        &theme,
                        &labels,
                        false,
                        &mut actions,
                    );
                }
                return;
            }
            render_node(
                ui,
                &tree,
                entries,
                selected,
                reveal,
                &theme,
                "req",
                &labels,
                sort == crate::tree::SortMode::File,
                &mut actions,
            );
        });

    // One-shot: the row asked to be revealed has had its frame, so clear the
    // request whether or not a row honoured it (an empty or filtered-away list
    // has nothing to scroll to).
    app.reveal_selected = false;
    apply_actions(app, ci, actions);
}

/// Apply the actions collected while rendering the request tree
/// (immutably-borrowed above) to the session. Pulled out of [`ui`] — the same
/// shape as [`apply_ws_action`] for the workspace tree — so the effect of a
/// context-menu click can be exercised directly in tests without having to
/// drive an actual right-click through egui.
fn apply_actions(app: &mut GuiApp, ci: usize, actions: Actions) {
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
    if let Some(i) = actions.duplicate {
        let col = &mut app.session.collections[ci];
        if let Some(mut clone) = col.entries.get(i).cloned() {
            // The title is the request's identifier (reports resolve requests
            // by name), so two entries sharing one would make the name
            // ambiguous for both — the copy needs one of its own.
            clone.title = crate::collection::unique_entry_title(&col.entries, &clone.title);
            clone.user_added = true;
            clone.modified = true;
            // A copy has never been sent, so carrying the original's response
            // over would credit it with a result it did not produce.
            clone.last_response = None;
            // Insert right after the original, not at the end, so the copy
            // lands beside the request it came from rather than out of sight
            // at the bottom of a long collection.
            col.entries.insert(i + 1, clone);
            col.selected_entry = i + 1;
            col.invalidate_request_json();
        }
    }
    if let Some(i) = actions.delete {
        // Route through the preference-honouring path so the context menu and
        // the Requests-panel Delete key can't disagree about whether to ask
        // first (see `GuiApp::request_delete_request`).
        app.request_delete_request(ci, i);
    }
    if let Some((from, before)) = actions.reorder {
        // The order is what `run_all_entries` follows, so this is a real edit to
        // the collection rather than a view preference — `move_entry_before`
        // marks the file unsaved, and a drop that changed nothing returns false
        // so it doesn't.
        if app.session.collections[ci].move_entry_before(from, before) {
            app.session.save();
        }
    }
    if let Some(i) = actions.run {
        app.session.collections[ci].selected_entry = i;
        app.run_active();
    }
}

/// Row indentation per tree depth, in pixels.
const WS_INDENT: f32 = 14.0;

/// A screenful for the tree's PageUp/PageDown. The plain request list binds
/// neither key, so there is no on-screen page height to match; a fixed jump is
/// predictable and enough to cross a long tree in a few presses.
const WS_PAGE_JUMP: usize = 10;

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
    /// Discard a request's in-memory edits, restoring it from the collection's
    /// on-disk file. Only offered for the loaded file's edited requests.
    RevertRequest {
        collection: PathBuf,
        idx: usize,
    },
    /// Discard every in-memory edit to a workspace collection file.
    RevertFile(PathBuf),
    /// Move a file or folder into another folder of the same workspace.
    MoveItem {
        src: PathBuf,
        dest_dir: PathBuf,
    },
    /// Rename a workspace file or folder (its own name on disk). Raises the
    /// rename dialog seeded with the current name; the filesystem rename runs
    /// only once that is confirmed. Distinct from a *request* rename
    /// ([`ReqEdit::Rename`]), which edits a `# name` comment inside a `.hurl`
    /// file rather than the file's own name.
    RenameItem {
        path: PathBuf,
    },
    /// Delete a workspace file or folder from disk. Always raises a
    /// confirmation first (a disk delete has no undo, so unlike a request
    /// delete it can't be turned off), then removes it and prunes everything
    /// the tab was holding about it.
    DeleteItem {
        path: PathBuf,
        is_dir: bool,
    },
    /// Run/Rename/Duplicate/Delete a request from the workspace tree — the same
    /// actions the plain request list offers on its rows. Kept as one variant
    /// carrying the [`ReqEdit`] kind so all of them share the load-then-act
    /// plumbing below: a `loaded: false` row indexes a cached snapshot, not the
    /// live `entries`, so it loads that collection first (exactly as
    /// double-click-to-run does) and only then acts, never on a stale index.
    RequestAction {
        collection: PathBuf,
        idx: usize,
        loaded: bool,
        kind: ReqEdit,
    },
    /// Reorder a request within the loaded collection: move the request at
    /// `from` into the gap above `before` (`before == entries.len()` for the
    /// gap past the last one). Only ever emitted for the loaded file's rows —
    /// a non-loaded row's index isn't the live one, and a request can only move
    /// within its own file — so `collection` is carried purely to re-check that
    /// the loaded file is still the one dragged before touching it.
    ReorderRequest {
        collection: PathBuf,
        from: usize,
        before: usize,
    },
}

/// Which of the plain list's request actions a [`WsAction::RequestAction`]
/// stands for. Run is not here: it already has its own [`WsAction::RunRequest`]
/// (shared with double-click), so the menu reuses that rather than a second
/// path to the same place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReqEdit {
    Rename,
    Duplicate,
    Delete,
}

/// What's being dragged around the workspace tree: the item's own path is all
/// the drop target needs, since where it lands is decided by the target.
#[derive(Clone, Debug)]
struct WsDrag(PathBuf);

/// A workspace *request* being dragged to a new position within its own
/// collection file. A distinct payload type from [`WsDrag`] (a *file* being
/// moved between folders) on purpose: the two gestures share the same tree, so
/// every drop target checks the payload's type before reacting, and a request
/// drag can never be mistaken for a file move or vice versa. The `collection`
/// rides along so the drop zone can refuse a drop onto a different file —
/// reordering is within one file only.
#[derive(Clone, Debug)]
struct WsReqDrag {
    collection: PathBuf,
    idx: usize,
}

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

/// The keyboard cursor's own look: a thin outline around its row, plus the
/// scroll-into-view a keyboard move asked for.
///
/// Deliberately *not* the filled highlight `ws_row` already gives the loaded
/// file and the open report through its `selected` flag. That flag marks a
/// *state* — "this is the file the tab has open" — while the cursor is a
/// *position* that moves on its own and is frequently on a different row.
/// Reusing the fill would make the two indistinguishable, and a plain move
/// would read as changing which file is loaded. An outline reads as "focus is
/// here" instead, and composes with the fill, so one row can be both the loaded
/// file and under the cursor at once. Its colour is the theme's `accent` (never
/// a literal), the same focus cue the drag targets and the panel border use.
///
/// The ring is drawn only when the panel actually holds focus (`show_ring`): an
/// unfocused tree showing a cursor would put two "here" marks on screen at once
/// — the cursor and the loaded-file highlight — competing for the eye when the
/// keyboard isn't even aimed at the panel.
fn ws_cursor_decoration(
    ui: &egui::Ui,
    resp: &egui::Response,
    show_ring: bool,
    reveal: bool,
    theme: &super::theme::GuiTheme,
) {
    if show_ring {
        ui.painter().rect_stroke(
            resp.rect,
            4.0,
            egui::Stroke::new(1.0, theme.accent),
            egui::StrokeKind::Inside,
        );
    }
    if reveal {
        // The least scrolling that puts the row on screen, and none at all when
        // it is already there: stepping through a tree should move the cursor
        // against a still list, not haul the scrollbar along to keep the cursor
        // pinned mid-panel. See the plain list's copy of this for the full
        // reasoning.
        resp.scroll_to_me(None);
    }
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

/// The workspace-tree twin of [`request_drop_zone`]: offer the gap above or
/// below a loaded request row as somewhere the dragged request can land, and
/// draw the insertion line showing which.
///
/// A line *between* rows, never a highlight *on* one — the same visual language
/// the plain list uses, and deliberately unlike [`ws_drag_and_drop`]'s folder
/// highlight: that one means "drop the file *into* this folder", so a
/// highlighted request row would read as dropping a request *into* a request,
/// which is not a thing. Keeping the two distinct is why this paints a line and
/// that paints an outline.
///
/// Refuses a drop whose source is a different collection file — a request only
/// reorders within its own file, and moving one between files is what the
/// transfer flow is for — by matching the target's `collection` against the
/// dragged payload's. It checks for a [`WsReqDrag`] payload specifically, so a
/// file drag (a [`WsDrag`]) passing over the row is ignored here, just as this
/// request drag is ignored by the file drop targets.
fn ws_request_drop_zone(
    ui: &mut egui::Ui,
    resp: &egui::Response,
    theme: &super::theme::GuiTheme,
    collection: &std::path::Path,
    i: usize,
    actions: &mut Vec<WsAction>,
) {
    let Some(dragged) = egui::DragAndDrop::payload::<WsReqDrag>(ui.ctx()) else {
        return;
    };
    // A request dragged out of another file only ever passes over this row on
    // its way somewhere; it must not land here.
    if dragged.collection.as_path() != collection {
        return;
    }
    if !resp.contains_pointer() {
        return;
    }
    let Some(pos) = ui.ctx().pointer_interact_pos() else {
        return;
    };
    let rect = resp.rect;
    // Which half of the row the pointer is in decides which gap is aimed at, so
    // every pixel of the list targets a gap and there is no dead zone to fall
    // down between rows.
    let above = pos.y < rect.center().y;
    let before = if above { i } else { i + 1 };
    // Both gaps touching the dragged request are where it already is; a line
    // there would promise a move `move_entry_before` correctly refuses to make.
    if dragged.idx == before || dragged.idx + 1 == before {
        return;
    }
    let y = if above { rect.top() } else { rect.bottom() };
    ui.painter()
        .hline(rect.x_range(), y, egui::Stroke::new(2.0, theme.accent));
    if ui.input(|i| i.pointer.any_released()) {
        actions.push(WsAction::ReorderRequest {
            collection: collection.to_path_buf(),
            from: dragged.idx,
            before,
        });
        egui::DragAndDrop::clear_payload(ui.ctx());
    }
}

/// Keyboard for a Workspace tab's file tree — the counterpart to the plain
/// list's [`GuiApp::handle_list_keys`], for the tree that isn't a flat request
/// list. Only reached when the Requests panel holds focus and no widget owns
/// the keyboard (see the call site), so, exactly like the plain list, it stands
/// down for dialogs and text entry without a second mechanism of its own: the
/// whole of [`GuiApp::handle_global_keys`] has already returned early while a
/// dialog is up, and the caller has already checked panel focus and that
/// nothing is being typed into. That gating is *why* Delete here can never
/// reach past a rename box to destroy the file behind it.
///
/// Every action goes through the same [`WsAction`]s the mouse raises, so the
/// two input methods can't drift apart, and every key that is handled has
/// already been consumed by `consume_key` so it doesn't also reach the tab
/// strip or the ScrollArea behind the tree.
pub(super) fn handle_ws_tree_keys(app: &mut GuiApp, ctx: &egui::Context, ci: usize) {
    // Read the rows once and clamp the cursor to them before doing anything
    // with it: the tree reshapes constantly (a scan refresh, a rename, a
    // delete), so a cursor saved against an older shape must never index the
    // new one — a wrong-row Delete is unrecoverable.
    let rows = app.session.collections[ci].ws_rows();
    if rows.is_empty() {
        return;
    }
    let last = rows.len() - 1;
    let cur = app.session.collections[ci].list_cursor.min(last);

    let (up, down, home, end, pgup, pgdn, left, right, enter, f2, del) = ctx.input_mut(|i| {
        (
            i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
            i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
            i.consume_key(egui::Modifiers::NONE, egui::Key::Home),
            i.consume_key(egui::Modifiers::NONE, egui::Key::End),
            i.consume_key(egui::Modifiers::NONE, egui::Key::PageUp),
            i.consume_key(egui::Modifiers::NONE, egui::Key::PageDown),
            i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft),
            i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight),
            i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
            i.consume_key(egui::Modifiers::NONE, egui::Key::F2),
            i.consume_key(egui::Modifiers::NONE, egui::Key::Delete),
        )
    });
    if !(up || down || home || end || pgup || pgdn || left || right || enter || f2 || del) {
        return; // nothing for us this frame; leave the cursor exactly as it was
    }

    // Vertical movement: clamped at both ends, never wrapping, exactly as the
    // plain list moves — the tree is still a list to step down, whatever its
    // rows happen to mean.
    let mut new_cursor = if up {
        cur.saturating_sub(1)
    } else if down {
        (cur + 1).min(last)
    } else if home {
        0
    } else if end {
        last
    } else if pgup {
        cur.saturating_sub(WS_PAGE_JUMP)
    } else if pgdn {
        (cur + WS_PAGE_JUMP).min(last)
    } else {
        cur
    };

    let row = &rows[cur];
    let mut action: Option<WsAction> = None;

    if right {
        match ws_expansion(row) {
            // A collapsed folder/collection opens where it stands — the same
            // action a click raises, so keyboard and mouse can't diverge.
            Some(false) => action = ws_toggle_action(row),
            // Already open: step onto the first child. The tree is drawn
            // depth-first, so the first child is simply the next row, when it is
            // indented one level deeper than this one.
            Some(true) => {
                if cur < last && rows[cur + 1].depth() > row.depth() {
                    new_cursor = cur + 1;
                }
            }
            // A leaf (a request, report or environment) has nothing to open or
            // to descend into, so Right does nothing rather than guessing.
            None => {}
        }
    } else if left {
        match ws_expansion(row) {
            // Left collapses what the cursor is looking into *before* it leaves
            // it: the standard tree idiom, and the whole reason Left is worth
            // binding — a folder can be shut without first walking off it. Only
            // once nothing is open under the cursor does Left step out.
            Some(true) => action = ws_toggle_action(row),
            _ => {
                if let Some(parent) = ws_parent_of(&rows, cur) {
                    new_cursor = parent;
                }
            }
        }
    }

    // Activation and editing, each dispatched by row kind to the very action
    // the mouse uses. These and the movement keys are mutually exclusive in a
    // real frame (one key press), so the later assignment simply wins if two
    // somehow arrive together.
    if enter {
        action = ws_enter_action(row);
    }
    if f2 {
        action = ws_rename_action(row);
    }
    if del {
        action = ws_delete_action(row);
    }

    if let Some(action) = action {
        apply_ws_action(app, ci, action);
    }
    // Place the cursor *after* any action has reshaped the tree, so an action
    // that repositioned it itself (loading a file runs `sync_ws_cursor`) can't
    // leave the keyboard cursor somewhere the keypress didn't ask for. Re-clamp
    // against the *new* row count, for the same reason the read above clamped.
    let col = &mut app.session.collections[ci];
    let last = col.list_row_count().saturating_sub(1);
    col.list_cursor = new_cursor.min(last);
    // Bring the cursor row into view next frame, as the plain list does for its
    // selection — a step in a long tree, or Home/End, can land off-screen.
    app.reveal_selected = true;
}

/// The expansion state of a workspace row that can hold children: `Some(true)`
/// when open, `Some(false)` when collapsed, `None` for a leaf (a request,
/// report or environment) that has none. A collection's `open` flag *is* its
/// expansion, so it steps and toggles just like a folder.
fn ws_expansion(row: &WsRow) -> Option<bool> {
    match row {
        WsRow::Folder { expanded, .. } | WsRow::RequestFolder { expanded, .. } => Some(*expanded),
        WsRow::Collection { open, .. } => Some(*open),
        _ => None,
    }
}

/// The action that flips a row's expansion — the very one the mouse raises, so
/// keyboard and click expand/collapse can't diverge. A folder toggles its
/// `workspace_expanded` membership; a collection carries its current `open` so
/// [`apply_ws_action`] toggles from the right state (opening a collapsed file
/// loads it, exactly as a click does). `None` for a leaf.
fn ws_toggle_action(row: &WsRow) -> Option<WsAction> {
    match row {
        WsRow::Folder { path, .. } | WsRow::RequestFolder { path, .. } => {
            Some(WsAction::ToggleFolder(path.clone()))
        }
        WsRow::Collection { path, open, .. } => Some(WsAction::ToggleCollection {
            path: path.clone(),
            open: *open,
        }),
        _ => None,
    }
}

/// The row Left steps out to: the nearest row above the cursor with a shallower
/// depth, i.e. the parent in the tree. `None` at the top level, where there is
/// no parent to climb to.
fn ws_parent_of(rows: &[WsRow], cur: usize) -> Option<usize> {
    let depth = rows[cur].depth();
    rows[..cur].iter().rposition(|r| r.depth() < depth)
}

/// What Enter does, per row kind — each mapped to the same [`WsAction`] a click
/// or double-click raises: a folder toggles, a collection opens/loads (or
/// closes if already open, exactly as a click does), a request runs, a report
/// or environment opens. Enter on an environment opens (loads) it without
/// yanking the Environments panel around, which is what a single click does;
/// the double-click's reveal is a mouse-only affordance.
fn ws_enter_action(row: &WsRow) -> Option<WsAction> {
    match row {
        WsRow::Folder { .. } | WsRow::RequestFolder { .. } | WsRow::Collection { .. } => {
            ws_toggle_action(row)
        }
        WsRow::Request {
            collection,
            idx,
            loaded,
            ..
        } => Some(WsAction::RunRequest {
            collection: collection.clone(),
            idx: *idx,
            loaded: *loaded,
        }),
        WsRow::Report { path, .. } => Some(WsAction::OpenReport(path.clone())),
        WsRow::Environment { path, .. } => Some(WsAction::OpenEnv {
            path: path.clone(),
            reveal: false,
        }),
    }
}

/// What F2 renames, per row kind. A real file/folder (folder, collection,
/// report, environment) renames the item on disk through the same dialog the
/// right-click Rename raises; a request renames its `# name` inside the file
/// through the plain list's own rename. A virtual request-folder has no file of
/// its own to rename, so F2 does nothing on it rather than pretending to.
fn ws_rename_action(row: &WsRow) -> Option<WsAction> {
    match row {
        WsRow::Folder { path, .. }
        | WsRow::Collection { path, .. }
        | WsRow::Report { path, .. }
        | WsRow::Environment { path, .. } => Some(WsAction::RenameItem { path: path.clone() }),
        WsRow::Request {
            collection,
            idx,
            loaded,
            ..
        } => Some(WsAction::RequestAction {
            collection: collection.clone(),
            idx: *idx,
            loaded: *loaded,
            kind: ReqEdit::Rename,
        }),
        WsRow::RequestFolder { .. } => None,
    }
}

/// What Delete removes, per row kind. A real file/folder goes through the
/// always-on disk-delete confirmation (a disk delete has no undo, unlike a
/// request delete); a request goes through the plain list's delete, which
/// honours the `confirm_on_delete_request` preference. A virtual request-folder
/// has no file to delete, so Delete does nothing on it.
fn ws_delete_action(row: &WsRow) -> Option<WsAction> {
    match row {
        WsRow::Folder { path, .. } => Some(WsAction::DeleteItem {
            path: path.clone(),
            is_dir: true,
        }),
        WsRow::Collection { path, .. }
        | WsRow::Report { path, .. }
        | WsRow::Environment { path, .. } => Some(WsAction::DeleteItem {
            path: path.clone(),
            is_dir: false,
        }),
        WsRow::Request {
            collection,
            idx,
            loaded,
            ..
        } => Some(WsAction::RequestAction {
            collection: collection.clone(),
            idx: *idx,
            loaded: *loaded,
            kind: ReqEdit::Delete,
        }),
        WsRow::RequestFolder { .. } => None,
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
    let (lbl_revert_req, lbl_revert_file) = (
        app.strings.gui_ws_revert_request,
        app.strings.gui_ws_revert_file,
    );
    // Rename / Delete of a file or folder on disk — shared by every real
    // file/folder row (a folder, a collection, a report, an environment), and
    // pulled out here once so the closures can be built while the tree is
    // borrowed immutably below.
    let ws_rd = (app.strings.gui_ws_rename, app.strings.gui_ws_delete);
    // The request row's own actions, borrowed verbatim from the plain request
    // list so a request in a workspace tree can be Run/Renamed/Duplicated/
    // Deleted with exactly the same menu it has in a single-collection tab.
    let (lbl_run, lbl_rename, lbl_duplicate, lbl_delete) = (
        app.strings.gui_run,
        app.strings.gui_rename_ellipsis,
        app.strings.gui_duplicate,
        app.strings.gui_delete,
    );
    let s_new = new_item_labels(&app.strings);

    let name = app.session.collections[ci].name.clone();
    let filter_on = app.session.collections[ci].workspace_filter_hurl_json;
    let ws_root = app.session.collections[ci].workspace_root.clone();
    let mut header_new: Option<WsAction> = None;
    super::widgets::panel_header(ui, &theme, name, |ui| {
        let run_all = format!("{} {}", super::icons::PLAY, lbl_run_all);
        if ui.button(run_all).on_hover_text(tip_run_all).clicked() {
            app.request_run_all(ci);
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
    // The keyboard cursor's row, clamped to the rows actually on screen: the
    // tree reshapes constantly (a scan refresh, a collapse, a delete), so an
    // index saved against an older shape must never be trusted against the new
    // one. Shown only while the panel holds focus (see `ws_cursor_decoration`),
    // so an unfocused tree paints no phantom cursor beside the loaded file.
    let cursor = app.session.collections[ci].list_cursor.min(rows.len() - 1);
    let show_cursor = app.focus == super::Focus::List;
    let reveal = app.reveal_selected;
    // Filled in when a row is clicked, so a mouse click can hand the keyboard
    // cursor to that row after the render (see the end of this function).
    let mut click_cursor: Option<usize> = None;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
            super::widgets::tree_rhythm(ui);
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
            for (row_idx, row) in rows.iter().enumerate() {
                // Each arm draws its row and yields the row's `Response`, so the
                // keyboard-cursor ring, the reveal scroll and the click that
                // hands the cursor to a row are all handled once below rather
                // than repeated per row kind.
                let resp: egui::Response = match row {
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
                        ws_row_menu_with(
                            &resp,
                            path.clone(),
                            lbl_in_folder,
                            s_new,
                            &mut actions,
                            |ui| {
                                let chosen = ws_rename_delete_entries(ui, path, true, ws_rd);
                                ui.separator();
                                chosen
                            },
                        );
                        ws_drag_and_drop(ui, &resp, &theme, path, true, &mut actions);
                        folder_rects.push(resp.rect);
                        resp
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
                        let edited_file = app.session.collections[ci].workspace_file_edited(path);
                        ws_row_menu_with(
                            &resp,
                            sibling_dir(path),
                            lbl_in_folder,
                            s_new,
                            &mut actions,
                            |ui| {
                                let mut chosen = ws_rename_delete_entries(ui, path, false, ws_rd);
                                // A file with edits only in memory can be put
                                // back to what is on disk; a clean one has
                                // nothing to offer, so the entry isn't shown at
                                // all rather than shown greyed.
                                if edited_file && ui.button(lbl_revert_file).clicked() {
                                    chosen = chosen.or(Some(WsAction::RevertFile(path.clone())));
                                }
                                ui.separator();
                                chosen
                            },
                        );
                        ws_drag_and_drop(ui, &resp, &theme, path, false, &mut actions);
                        resp
                    }
                    // A virtual folder inside a collection file. It looks and
                    // toggles like a filesystem folder, but deliberately has
                    // neither the New/right-click menu nor drag-and-drop: those
                    // act on real files, and this row has none — the requests
                    // under it live inside their collection.
                    WsRow::RequestFolder {
                        path,
                        name,
                        depth,
                        expanded,
                        ..
                    } => {
                        let chev = if *expanded {
                            super::icons::CARET_DOWN
                        } else {
                            super::icons::CARET_RIGHT
                        };
                        let text = RichText::new(format!("{chev} {} {name}", super::icons::FOLDER))
                            .color(theme.text);
                        let resp = ui
                            .push_id(("ws_req_folder", path), |ui| {
                                ws_row(ui, *depth, false, text)
                            })
                            .inner;
                        if resp.clicked() {
                            actions.push(WsAction::ToggleFolder(path.clone()));
                        }
                        resp
                    }
                    WsRow::Request {
                        collection,
                        idx,
                        name,
                        method,
                        depth,
                        loaded,
                    } => {
                        let is_sel = *loaded
                            && loaded_path.as_deref() == Some(collection.as_path())
                            && *idx == selected_entry;
                        // The method is known for every listed request — which row
                        // is the POST is the question the badge answers, and a
                        // reader shouldn't have to open a file to ask it. The
                        // run marker used to be loaded-only, on the grounds
                        // that nothing else could have been run; a run's result
                        // now outlives the file being loaded (see
                        // `collection::RunRecord`), so a request run before the
                        // tab moved on still shows how it fared.
                        let marker = run_marker(
                            app.session.collections[ci].workspace_run_status(collection, *idx),
                        );
                        // Unlike the method badge and the run marker, the edit
                        // pencil is shown for every collection's rows, not just
                        // the loaded one's — an edit parked while the user looks
                        // at another collection is still an edit they need to
                        // see (and save).
                        let edited =
                            app.session.collections[ci].workspace_request_edited(collection, *idx);
                        // A stable per-request id namespace: the run marker
                        // only appears for the loaded collection, so without it
                        // every row's ids shift the moment the tab changes which
                        // file it holds (see `render_node`).
                        let resp = clickable_row(ui, ("ws_req", collection, idx), |ui| {
                            ui.horizontal(|ui| {
                                ui.add_space(*depth as f32 * WS_INDENT);
                                if !method.is_empty() {
                                    super::widgets::method_badge(ui, &theme, method);
                                }
                                let text = if is_sel {
                                    RichText::new(name).strong().color(theme.text)
                                } else {
                                    // See the collection tree: an unselected
                                    // request is not a disabled one.
                                    RichText::new(name).color(theme.text)
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
                                            ui.colored_label(theme.pending, super::icons::EDITED)
                                                .on_hover_text(lbl_edited);
                                        }
                                        ui.with_layout(
                                            egui::Layout::left_to_right(egui::Align::Center),
                                            |ui| super::widgets::selectable_row(ui, is_sel, text),
                                        )
                                        .inner
                                    },
                                )
                                .inner
                            })
                            .inner
                        });
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
                        // A row belongs to the loaded file when it says so and
                        // the tab still holds that file — the test both the
                        // reorder drag and the single-request revert turn on.
                        let is_loaded_here =
                            *loaded && loaded_path.as_deref() == Some(collection.as_path());
                        // Only the loaded file's requests can be reverted one
                        // at a time: another file's edits are parked as a whole,
                        // with no on-disk entry to put back in place of a single
                        // one of them (its file row reverts the lot). An *added*
                        // request has no saved version either, so `modified` is
                        // the test, not the pencil.
                        let revertable = is_loaded_here
                            && app.session.collections[ci]
                                .entries
                                .get(*idx)
                                .is_some_and(|e| e.modified);
                        ws_row_menu_with(
                            &resp,
                            sibling_dir(collection),
                            lbl_in_folder,
                            s_new,
                            &mut actions,
                            |ui| {
                                // The plain request list's row menu, so a
                                // workspace request is Run/Renamed/Duplicated/
                                // Deleted with the very same entries — and the
                                // very same handlers (see `apply_actions`), so
                                // the two can't drift apart. Run reuses the
                                // existing double-click action; the rest carry
                                // `loaded` so a not-loaded row loads its file
                                // before acting rather than on a cached index.
                                if ui.button(lbl_run).clicked() {
                                    return Some(WsAction::RunRequest {
                                        collection: collection.clone(),
                                        idx: *idx,
                                        loaded: *loaded,
                                    });
                                }
                                if ui.button(lbl_rename).clicked() {
                                    return Some(WsAction::RequestAction {
                                        collection: collection.clone(),
                                        idx: *idx,
                                        loaded: *loaded,
                                        kind: ReqEdit::Rename,
                                    });
                                }
                                if ui.button(lbl_duplicate).clicked() {
                                    return Some(WsAction::RequestAction {
                                        collection: collection.clone(),
                                        idx: *idx,
                                        loaded: *loaded,
                                        kind: ReqEdit::Duplicate,
                                    });
                                }
                                if ui.button(lbl_delete).clicked() {
                                    return Some(WsAction::RequestAction {
                                        collection: collection.clone(),
                                        idx: *idx,
                                        loaded: *loaded,
                                        kind: ReqEdit::Delete,
                                    });
                                }
                                if revertable && ui.button(lbl_revert_req).clicked() {
                                    return Some(WsAction::RevertRequest {
                                        collection: collection.clone(),
                                        idx: *idx,
                                    });
                                }
                                ui.separator();
                                None
                            },
                        );
                        // Drag-to-reorder, matching the plain list's rows — but
                        // only for the loaded file's. A reorder edits `entries`,
                        // and a non-loaded row indexes a cached snapshot, not
                        // the live list; loading that file mid-drag to make it
                        // live would reshape the tree out from under the pointer,
                        // so those rows simply aren't draggable. A request can
                        // also only move *within* its own file (a cross-file
                        // move is what dragging into a folder is for), which the
                        // payload and `ws_request_drop_zone` enforce together.
                        if is_loaded_here {
                            if resp.drag_started() {
                                egui::DragAndDrop::set_payload(
                                    ui.ctx(),
                                    WsReqDrag {
                                        collection: collection.clone(),
                                        idx: *idx,
                                    },
                                );
                            }
                            // Something has to follow the pointer, or the drag
                            // reads as nothing happening: the rows stay put and
                            // the insertion line only shows once the pointer is
                            // over a row.
                            if resp.dragged()
                                && let Some(pos) = ui.ctx().pointer_interact_pos()
                            {
                                let layer = egui::LayerId::new(
                                    egui::Order::Tooltip,
                                    ui.id().with("ws_req_drag_label"),
                                );
                                ui.ctx().layer_painter(layer).text(
                                    pos + egui::vec2(12.0, 4.0),
                                    egui::Align2::LEFT_TOP,
                                    name,
                                    egui::TextStyle::Button.resolve(ui.style()),
                                    theme.accent,
                                );
                            }
                            ws_request_drop_zone(ui, &resp, &theme, collection, *idx, &mut actions);
                        }
                        resp
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
                        ws_row_menu_with(
                            &resp,
                            sibling_dir(path),
                            lbl_in_folder,
                            s_new,
                            &mut actions,
                            |ui| {
                                let chosen = ws_rename_delete_entries(ui, path, false, ws_rd);
                                ui.separator();
                                chosen
                            },
                        );
                        ws_drag_and_drop(ui, &resp, &theme, path, false, &mut actions);
                        resp
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
                                let mut chosen = ws_rename_delete_entries(ui, path, false, ws_rd);
                                if ui.button(lbl_set_active_env).clicked() {
                                    chosen = chosen.or(Some(WsAction::ActivateEnv(path.clone())));
                                }
                                ui.separator();
                                chosen
                            },
                        );
                        ws_drag_and_drop(ui, &resp, &theme, path, false, &mut actions);
                        resp
                    }
                };
                // Common to every row: remember a click so the mouse can hand
                // the keyboard cursor to it (applied after the render, below),
                // and — for the cursor row — draw the focus ring and honour a
                // reveal request. The ring is gated on the panel holding focus;
                // the reveal is a one-shot from a keyboard move, so it scrolls
                // whether or not the panel is focused this instant.
                if resp.clicked() {
                    click_cursor = Some(row_idx);
                }
                ws_cursor_decoration(
                    ui,
                    &resp,
                    show_cursor && row_idx == cursor,
                    reveal && row_idx == cursor,
                    &theme,
                );
            }
            if let Some(root) = &ws_root {
                ws_root_drop(ui, bg_rect, &folder_rects, &theme, root, &mut actions);
            }
        });
    // A one-shot, like the plain list's: the cursor row has had its frame to
    // scroll itself into view, so clear the request whether or not a row
    // honoured it (an empty or fully-visible tree has nothing to scroll).
    app.reveal_selected = false;

    for action in header_new.into_iter().chain(actions) {
        apply_ws_action(app, ci, action);
    }

    // A click anywhere in the tree moves the keyboard cursor onto that row, so
    // the mouse and the keyboard never fight over where "here" is: click a row,
    // press Down, and the cursor steps from the row you clicked rather than
    // jumping back to wherever it last was. Applied *after* the actions above
    // because a few of them (selecting a request, re-expanding the loaded file)
    // reposition the cursor themselves via `sync_ws_cursor`, and the row the
    // user actually clicked is the one that should win. Expanding, collapsing
    // or loading only ever inserts or removes rows *below* the clicked one, so
    // its index still names the row that was clicked.
    if let Some(idx) = click_cursor {
        let col = &mut app.session.collections[ci];
        let last = col.list_row_count().saturating_sub(1);
        col.list_cursor = idx.min(last);
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
    use crate::workspace::NewItemKind;

    // Nothing to create into, so not even worth opening a dialog.
    if app.session.collections[ci].workspace_root.is_none() {
        return;
    }
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
    app.request_pick(
        super::filepick::PickKind::Save {
            default_name: default.to_string(),
            filters: super::filepick::owned_filters(&[(ext, &[ext])]),
        },
        title,
        Some(dir),
        super::menu::PickAction::NewWorkspaceItem { ci, kind },
    );
}

/// Create the workspace item the save dialog named, frames later (see
/// [`super::filepick`] for why the naming and the creating are separate).
///
/// The workspace root is re-read here rather than captured with the request:
/// the collection could have been closed or repointed while the dialog was up,
/// and creating a file under a root that is no longer open would be worse than
/// doing nothing.
pub(super) fn apply_new_workspace_item(
    app: &mut GuiApp,
    ci: usize,
    kind: crate::workspace::NewItemKind,
    picked: Option<std::path::PathBuf>,
) {
    use crate::workspace::{NewItemError, NewItemKind};

    let Some(chosen) = picked else {
        return; // cancelled
    };
    let Some(root) = app
        .session
        .collections
        .get(ci)
        .and_then(|c| c.workspace_root.clone())
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
    use crate::workspace::{MoveError, move_item};

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

    repoint_workspace_holdings(app, src, &dest);
    reveal_in_tree(app, ci, &dest, &root);
    app.session.status = Some(crate::i18n::Status::WsItemMoved(
        crate::workspace::display_name(&root, &dest),
    ));
}

/// Rename a workspace file or folder on disk, then fix up everything the app was
/// holding it by — the exact same repointing a move needs, because to the app a
/// rename *is* a move (the item's path changes), just within its own folder.
///
/// The safety — no path separators, no clobbering, keeping a file's extension
/// so it doesn't drop out of the tree — all lives in
/// [`crate::workspace::rename_item`]; this only translates its result into the
/// session's held paths and a status line. A rename to the name it already has
/// comes back unchanged and is silently nothing, exactly as a drop onto a file's
/// own folder is.
pub(super) fn rename_workspace_item(
    app: &mut GuiApp,
    ci: usize,
    src: &std::path::Path,
    new_name: &str,
) {
    use crate::workspace::{RenameError, rename_item};

    let Some(root) = app.session.collections[ci].workspace_root.clone() else {
        return;
    };
    let dest = match rename_item(&root, src, new_name) {
        Ok(dest) => dest,
        Err(RenameError::EmptyName) => return,
        Err(RenameError::Exists(what)) => {
            app.session.status = Some(crate::i18n::Status::WsItemRenameExists(what));
            return;
        }
        Err(RenameError::Escapes(what)) => {
            app.session.status = Some(crate::i18n::Status::WsItemEscaped(what));
            return;
        }
        Err(RenameError::Io(what)) => {
            app.session.status = Some(crate::i18n::Status::Error(what));
            return;
        }
    };
    if dest == src {
        return;
    }

    repoint_workspace_holdings(app, src, &dest);
    reveal_in_tree(app, ci, &dest, &root);
    app.session.status = Some(crate::i18n::Status::WsItemRenamed(
        crate::workspace::display_name(&root, &dest),
    ));
}

/// Delete a workspace file or folder from disk, then drop everything the app was
/// holding it by.
///
/// The removal itself is [`crate::workspace::delete_item`], which alone decides
/// what is safe to delete (never outside the root, never the root itself); this
/// handles the *aftermath* — the mirror image of a move's repointing. A move
/// gives every held path a new home; a delete has none to give, so each tab
/// forgets the item and, if the deleted thing was the file it had loaded, falls
/// back to the file-less state a fresh Workspace tab starts in rather than
/// showing a phantom of a file that no longer exists. The open report is closed
/// if it was the one deleted (or lived under a deleted folder).
pub(super) fn delete_workspace_item(app: &mut GuiApp, ci: usize, path: &std::path::Path) {
    use crate::workspace::{DeleteError, delete_item};

    let Some(root) = app.session.collections[ci].workspace_root.clone() else {
        return;
    };
    match delete_item(&root, path) {
        Ok(()) => {}
        Err(DeleteError::IsRoot) => {
            app.session.status = Some(crate::i18n::Status::WsItemDeleteRoot);
            return;
        }
        Err(DeleteError::Escapes(what)) => {
            app.session.status = Some(crate::i18n::Status::WsItemEscaped(what));
            return;
        }
        Err(DeleteError::Io(what)) => {
            app.session.status = Some(crate::i18n::Status::Error(what));
            return;
        }
    }

    let name = crate::workspace::display_name(&root, path);
    // Every tab on this root has to forget it, not just the active one — two
    // workspace tabs can be open on the same folder.
    for col in &mut app.session.collections {
        col.prune_workspace_paths(path);
    }
    // A report open in the editor from under the deleted item has nowhere to
    // save back to; close it rather than leave it editing a deleted file.
    if let Some(ed) = app.report_editor.as_ref()
        && ed.path().is_some_and(|p| p.starts_with(path))
    {
        app.close_report_editor();
    }
    app.session.status = Some(crate::i18n::Status::WsItemDeleted(name));
    app.session.save();
}

/// Repoint every held path from `src` to `dest` after a workspace item has been
/// moved or renamed on disk — across *all* tabs (two may be open on one root)
/// and the open report editor. Each collection's own by-path state is repointed
/// by [`crate::collection::Collection::repoint_workspace_paths`]; the report
/// editor lives on the app, so it is handled here.
fn repoint_workspace_holdings(app: &mut GuiApp, src: &std::path::Path, dest: &std::path::Path) {
    for col in &mut app.session.collections {
        col.repoint_workspace_paths(src, dest);
    }
    if let Some(ed) = app.report_editor.as_mut()
        && let Some(p) = ed
            .path()
            .and_then(|p| crate::workspace::repoint(p, src, dest))
    {
        ed.report.path = Some(p);
    }
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

/// The Rename / Delete entries every real file/folder row's menu leads with,
/// returning whichever was clicked. Kept in one place so the four row kinds
/// that draw it (folder, collection, report, environment) can't drift apart,
/// and deliberately *not* baked into [`ws_row_menu_with`]: the tree's
/// background stands in for the workspace root, which can be neither renamed nor
/// deleted, and a *request* row's menu already has its own Rename/Delete that
/// mean something else (a title inside a file, an undoable in-memory removal).
fn ws_rename_delete_entries(
    ui: &mut egui::Ui,
    path: &std::path::Path,
    is_dir: bool,
    labels: (&'static str, &'static str),
) -> Option<WsAction> {
    let (lbl_rename, lbl_delete) = labels;
    if ui.button(lbl_rename).clicked() {
        return Some(WsAction::RenameItem {
            path: path.to_path_buf(),
        });
    }
    if ui.button(lbl_delete).clicked() {
        return Some(WsAction::DeleteItem {
            path: path.to_path_buf(),
            is_dir,
        });
    }
    None
}

/// Make `collection` the tab's loaded file if it isn't already, and return the
/// live `entries` index to act on. A not-loaded row's `idx` indexes the cached
/// title snapshot rather than the live list, so it can only be acted on once
/// its file is loaded — the same load-then-act the double-click run path uses.
/// The index is clamped into range and then checked to actually exist, because
/// a file re-read off disk can be shorter than the cache promised; `None` there
/// stops a request action from falling onto the wrong request.
fn resolve_ws_request(
    app: &mut GuiApp,
    ci: usize,
    collection: &std::path::Path,
    idx: usize,
    loaded: bool,
) -> Option<usize> {
    let already = loaded && app.session.collections[ci].path.as_deref() == Some(collection);
    if !already
        && !(app
            .session
            .load_workspace_file(ci, collection.to_path_buf())
            && app.session.collections[ci].path.as_deref() == Some(collection))
    {
        return None;
    }
    let n = app.session.collections[ci].entries.len();
    let idx = idx.min(n.saturating_sub(1));
    app.session.collections[ci].entries.get(idx).map(|_| idx)
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
        | WsAction::RequestAction {
            collection: path, ..
        }
        | WsAction::ReorderRequest {
            collection: path, ..
        }
        | WsAction::OpenReport(path)
        | WsAction::OpenEnv { path, .. }
        | WsAction::ActivateEnv(path) => {
            app.session.collections[ci].workspace_selected = Some(path.clone());
        }
        // Opening or closing a folder isn't "working on" anything; a new or
        // moved file records itself once it is actually there.
        // A revert isn't "working on" the file either — it undoes work — and
        // the dialog it raises names its own target. Rename/Delete only raise a
        // dialog here; the rename records the item's new path once it lands, and
        // a delete has no selection to remember.
        WsAction::ToggleFolder(_)
        | WsAction::NewItem { .. }
        | WsAction::MoveItem { .. }
        | WsAction::RenameItem { .. }
        | WsAction::DeleteItem { .. }
        | WsAction::RevertRequest { .. }
        | WsAction::RevertFile(_) => {}
    }
    match action {
        WsAction::RevertRequest { collection, idx } => {
            let name = app.session.collections[ci]
                .entries
                .get(idx)
                .map(|e| {
                    let leaf = crate::tree::entry_path(&e.title).pop().unwrap_or_default();
                    if leaf.is_empty() { e.url.clone() } else { leaf }
                })
                .unwrap_or_default();
            app.dialog = Some(super::app::Dialog::RevertToSaved {
                ci,
                path: collection,
                entry: Some(idx),
                name,
            });
        }
        WsAction::RevertFile(path) => {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            app.dialog = Some(super::app::Dialog::RevertToSaved {
                ci,
                path,
                entry: None,
                name,
            });
        }
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
        WsAction::RequestAction {
            collection,
            idx,
            loaded,
            kind,
        } => {
            // Resolve the *live* index first: a not-loaded row's index points
            // into the cached title snapshot, so its file is loaded here (the
            // same load-then-act the double-click run path uses) to make the
            // index real before acting. `None` means the file couldn't be
            // loaded, or the request is gone after a re-read — acting on nothing
            // beats acting on a stale index.
            if let Some(idx) = resolve_ws_request(app, ci, &collection, idx, loaded) {
                // Route through the plain list's own handler, so the workspace
                // menu and the single-collection menu can never disagree about
                // what Rename/Duplicate/Delete do (the delete-confirm
                // preference, the unique-title copy, the rename dialog target).
                let actions = match kind {
                    ReqEdit::Rename => Actions {
                        rename: Some(idx),
                        ..Default::default()
                    },
                    ReqEdit::Duplicate => Actions {
                        duplicate: Some(idx),
                        ..Default::default()
                    },
                    ReqEdit::Delete => Actions {
                        delete: Some(idx),
                        ..Default::default()
                    },
                };
                apply_actions(app, ci, actions);
            }
        }
        WsAction::ReorderRequest {
            collection,
            from,
            before,
        } => {
            // Guard on the loaded file still being the one the drag started in:
            // `from`/`before` index the list that was on screen then, so a
            // reorder that somehow outlived a tab switch must not land on a
            // different file's entries. Reuses the plain list's handler, so a
            // no-op drop (`move_entry_before` returns false) doesn't mark the
            // file unsaved.
            if app.session.collections[ci].path.as_deref() == Some(collection.as_path()) {
                apply_actions(
                    app,
                    ci,
                    Actions {
                        reorder: Some((from, before)),
                        ..Default::default()
                    },
                );
            }
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
        // Rename and Delete both raise a dialog first; the filesystem change
        // happens when it is confirmed (see `rename_workspace_item` /
        // `delete_workspace_item`). Rename reuses the request rename dialog via
        // its own `RenameTarget`; Delete has a dedicated confirmation because it
        // must always ask and needs to say how much is about to go.
        WsAction::RenameItem { path } => {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            app.dialog = Some(super::app::Dialog::Rename {
                target: super::app::RenameTarget::WorkspaceItem { ci, path },
                text: name,
            });
        }
        WsAction::DeleteItem { path, is_dir } => {
            let name = app.session.collections[ci]
                .workspace_root
                .as_deref()
                .map(|root| crate::workspace::display_name(root, &path))
                .unwrap_or_else(|| {
                    path.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                });
            // A folder delete takes everything under it, so the count is what
            // turns "delete" into an informed choice; a file is just itself.
            let file_count = if is_dir {
                crate::workspace::descendant_file_count(&path)
            } else {
                1
            };
            let unsaved = app.session.collections[ci].workspace_unsaved_under(&path);
            app.dialog = Some(super::app::Dialog::DeleteWorkspaceItem {
                ci,
                path,
                is_dir,
                name,
                file_count,
                unsaved,
            });
        }
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
    pub(crate) fn ws_tmp(tag: &str) -> std::path::PathBuf {
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

    /// Every text rectangle painted this frame, with the text it holds.
    fn texts(shapes: &[egui::epaint::ClippedShape]) -> Vec<(String, egui::Rect)> {
        fn walk(shape: &egui::epaint::Shape, out: &mut Vec<(String, egui::Rect)>) {
            match shape {
                egui::epaint::Shape::Text(t) => {
                    out.push((t.galley.text().to_string(), t.visual_bounding_rect()))
                }
                egui::epaint::Shape::Vec(v) => {
                    for s in v {
                        walk(s, out);
                    }
                }
                _ => {}
            }
        }
        let mut out = Vec::new();
        for cs in shapes {
            walk(&cs.shape, &mut out);
        }
        out
    }

    /// The colour a name is painted in, by name.
    fn text_color(shapes: &[egui::epaint::ClippedShape], needle: &str) -> egui::Color32 {
        fn walk(shape: &egui::epaint::Shape, needle: &str, out: &mut Option<egui::Color32>) {
            match shape {
                egui::epaint::Shape::Text(t) if t.galley.text().contains(needle) => {
                    *out = t
                        .override_text_color
                        .or_else(|| t.galley.job.sections.first().map(|s| s.format.color));
                }
                egui::epaint::Shape::Vec(v) => {
                    for s in v {
                        walk(s, needle, out);
                    }
                }
                _ => {}
            }
        }
        let mut out = None;
        for cs in shapes {
            walk(&cs.shape, needle, &mut out);
        }
        out.expect("the name is drawn")
    }

    /// `dim` is this app's colour for what doesn't apply — hints, disabled
    /// rows, empty states. Spending it on every request that merely isn't the
    /// selected one made the tree look switched off.
    #[test]
    fn an_unselected_request_is_not_painted_in_the_disabled_colour() {
        redirect_saved_state();
        let dir = ws_tmp("dimrow");
        let mut session = crate::session::Session::default();
        session.collections.clear();
        let ci = session.open_workspace(dir.clone());
        expand_all(&mut session.collections[ci], &dir);
        assert!(session.load_workspace_file(ci, dir.join("api/v1/one.hurl")));
        session.collections[ci].selected_entry = 0;
        session.active_tab = ci;
        let mut app = GuiApp::for_test(session);
        let dim = app.theme.dim;
        let text = app.theme.text;

        let ctx = egui::Context::default();
        let out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(400.0, 600.0),
                )),
                ..Default::default()
            },
            |ui| crate::gui::requests::ui(&mut app, ui),
        );

        let got = text_color(&out.shapes, "example.com/a2");
        assert_ne!(got, dim, "an unselected request is not disabled");
        assert_eq!(got, text, "it is ordinary content");
    }

    /// Tree rows sit on their own, tighter rhythm than the app-wide gap
    /// between controls. A list spaced like a stack of buttons throws away a
    /// good part of a long folder's screen — and once the selection border
    /// stopped resizing rows, that gap was all that was left to trim.
    #[test]
    fn tree_rows_sit_closer_together_than_two_controls_would() {
        redirect_saved_state();
        let dir = ws_tmp("rowpitch");
        let mut session = crate::session::Session::default();
        session.collections.clear();
        let ci = session.open_workspace(dir.clone());
        expand_all(&mut session.collections[ci], &dir);
        assert!(session.load_workspace_file(ci, dir.join("api/v1/one.hurl")));
        session.active_tab = ci;
        let mut app = GuiApp::for_test(session);

        let ctx = egui::Context::default();
        // The app's own spacing, or the gap being measured isn't the one the
        // user sees: egui's defaults are tighter than the theme's.
        crate::gui::theme::GuiTheme::from_spec(&crate::theme::default_preset()).apply(&ctx);
        let out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(400.0, 600.0),
                )),
                ..Default::default()
            },
            |ui| crate::gui::requests::ui(&mut app, ui),
        );
        let drawn = texts(&out.shapes);
        let row = |needle: &str| {
            drawn
                .iter()
                .find(|(t, _)| t == needle)
                .unwrap_or_else(|| panic!("{needle} is drawn"))
                .1
        };
        // Two requests of the same file, one directly under the other.
        let first = row("https://example.com/a");
        let second = row("https://example.com/a2");
        let pitch = second.top() - first.top();
        assert!(pitch > 0.0, "the rows are drawn in order: {pitch}");
        // A row is its text plus `widgets::TREE_ROW_PADDING` either side, so
        // anything past that plus `TREE_ROW_SPACING` is the app-wide control
        // gap (5px, see `theme.rs`) having crept back in between the rows.
        assert!(
            pitch <= 21.0,
            "tree rows are spaced like separate controls ({pitch}px apart)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A row is a method badge, a name and some markers, but only the name used
    /// to be a widget: clicking the badge, or the gap after the name, did
    /// nothing, so picking a request meant aiming at its text. The whole line
    /// is one target now. Clicked here on the *badge*, which is as far from the
    /// old target as a row gets.
    #[test]
    fn clicking_anywhere_on_a_request_row_selects_it() {
        redirect_saved_state();
        let dir = ws_tmp("rowclick");
        let mut session = crate::session::Session::default();
        session.collections.clear();
        let ci = session.open_workspace(dir.clone());
        expand_all(&mut session.collections[ci], &dir);
        assert!(session.load_workspace_file(ci, dir.join("api/v1/one.hurl")));
        session.collections[ci].selected_entry = 0;
        session.active_tab = ci;
        let mut app = GuiApp::for_test(session);

        let ctx = egui::Context::default();
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 600.0));
        let frame = |events: Vec<egui::Event>| egui::RawInput {
            screen_rect: Some(screen),
            events,
            ..Default::default()
        };

        // Where the *second* request's name is drawn — the row that isn't
        // already selected.
        let out = ctx.run_ui(frame(Vec::new()), |ui| {
            crate::gui::requests::ui(&mut app, ui)
        });
        let row = texts(&out.shapes)
            .into_iter()
            .find(|(t, _)| t.contains("example.com/a2"))
            .expect("the second request's row is drawn")
            .1;
        // The badge sits at the row's left edge, well clear of the name.
        let badge = egui::pos2(row.left() - 20.0, row.center().y);
        assert!(badge.x > 0.0, "the badge is inside the panel");

        let mut events = vec![egui::Event::PointerMoved(badge)];
        for pressed in [true, false] {
            events.push(egui::Event::PointerButton {
                pos: badge,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
        }
        let _ = ctx.run_ui(frame(events), |ui| crate::gui::requests::ui(&mut app, ui));

        assert_eq!(
            app.session.collections[ci].selected_entry, 1,
            "clicking the method badge selects the row it belongs to"
        );
        let _ = std::fs::remove_dir_all(&dir);
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

    fn entry(title: &str) -> HurlEntry {
        let mut e = HurlEntry::default();
        e.title = title.into();
        e.method = "GET".into();
        e
    }

    #[test]
    fn duplicate_gives_the_copy_a_fresh_title_and_lands_it_right_after_the_original() {
        let mut session = crate::session::Session::default();
        session.collections[0].entries = vec![entry("Login"), entry("Logout")];
        let ci = 0;
        let mut app = GuiApp::for_test(session);

        apply_actions(
            &mut app,
            ci,
            Actions {
                duplicate: Some(0),
                ..Default::default()
            },
        );

        let titles: Vec<&str> = app.session.collections[ci]
            .entries
            .iter()
            .map(|e| e.title.as_str())
            .collect();
        assert_eq!(titles, vec!["Login", "Login (2)", "Logout"]);
        assert_eq!(app.session.collections[ci].selected_entry, 1);
    }

    #[test]
    fn duplicate_never_carries_over_the_originals_last_response() {
        let mut session = crate::session::Session::default();
        let mut original = entry("Login");
        original.last_response = Some(crate::http::ApiResponse {
            status: 200,
            ..Default::default()
        });
        session.collections[0].entries = vec![original];
        let ci = 0;
        let mut app = GuiApp::for_test(session);

        apply_actions(
            &mut app,
            ci,
            Actions {
                duplicate: Some(0),
                ..Default::default()
            },
        );

        let copy = &app.session.collections[ci].entries[1];
        assert!(
            copy.last_response.is_none(),
            "a copy that has never been sent shouldn't inherit a result it didn't produce"
        );
    }

    /// A drop is aimed at the gap between two rows, so dragging downwards has
    /// to account for the dragged request being lifted out first — otherwise it
    /// lands one place short of where it was let go.
    #[test]
    fn dragging_a_request_downwards_lands_it_in_the_gap_it_was_dropped_into() {
        let mut session = crate::session::Session::default();
        session.collections[0].entries = vec![entry("a"), entry("b"), entry("c"), entry("d")];
        let ci = 0;
        let mut app = GuiApp::for_test(session);

        // "a" dropped into the gap above "d".
        apply_actions(
            &mut app,
            ci,
            Actions {
                reorder: Some((0, 3)),
                ..Default::default()
            },
        );

        assert_eq!(titles(&app, ci), vec!["b", "c", "a", "d"]);
        assert!(
            app.session.collections[ci].structure_modified,
            "the order is what Run All follows, so this is an unsaved edit"
        );
    }

    #[test]
    fn dragging_a_request_upwards_lands_it_in_the_gap_it_was_dropped_into() {
        let mut session = crate::session::Session::default();
        session.collections[0].entries = vec![entry("a"), entry("b"), entry("c"), entry("d")];
        let ci = 0;
        let mut app = GuiApp::for_test(session);

        // "d" dropped into the gap above "b".
        apply_actions(
            &mut app,
            ci,
            Actions {
                reorder: Some((3, 1)),
                ..Default::default()
            },
        );

        assert_eq!(titles(&app, ci), vec!["a", "d", "b", "c"]);
    }

    /// Dropping a request back where it started must not mark the file unsaved
    /// — an aborted drag would otherwise leave the tab claiming an edit.
    #[test]
    fn a_drag_that_goes_nowhere_is_not_an_edit() {
        let mut session = crate::session::Session::default();
        session.collections[0].entries = vec![entry("a"), entry("b"), entry("c")];
        let ci = 0;
        let mut app = GuiApp::for_test(session);

        for before in [1, 2] {
            apply_actions(
                &mut app,
                ci,
                Actions {
                    reorder: Some((1, before)),
                    ..Default::default()
                },
            );
        }

        assert_eq!(titles(&app, ci), vec!["a", "b", "c"]);
        assert!(!app.session.collections[ci].structure_modified);
    }

    /// The filter is the collection's, not the panel's, so both front-ends
    /// narrow the same tab the same way and each tab keeps its own.
    #[test]
    fn the_request_filter_flattens_the_tree_and_matches_the_whole_title() {
        let mut session = crate::session::Session::default();
        session.collections[0].entries = vec![
            entry("Auth/Login"),
            entry("Users/List"),
            entry("Auth/Logout"),
        ];
        let col = &mut session.collections[0];

        col.list_query = "log".into();
        assert!(col.list_filter_active());
        assert_eq!(
            crate::tree::rows_matching(&col.entries, &col.list_query),
            vec![crate::tree::Row::Entry(0), crate::tree::Row::Entry(2)],
        );

        // Whitespace alone is not a filter — it matches everything anyway, and
        // treating it as one would flatten the tree for no visible reason.
        col.list_query = "  ".into();
        assert!(!col.list_filter_active());
    }

    fn titles(app: &GuiApp, ci: usize) -> Vec<&str> {
        app.session.collections[ci]
            .entries
            .iter()
            .map(|e| e.title.as_str())
            .collect()
    }

    #[test]
    fn deleting_a_request_records_it_for_undo_and_restoring_reinserts_it() {
        let mut session = crate::session::Session::default();
        session.collections[0].entries = vec![entry("a"), entry("b"), entry("c")];
        let ci = 0;
        let mut app = GuiApp::for_test(session);
        // This test is about the undo history, not the confirmation guard, so
        // delete straight away (the guard has its own test below).
        app.session.confirm_on_delete_request = false;

        apply_actions(
            &mut app,
            ci,
            Actions {
                delete: Some(1),
                ..Default::default()
            },
        );
        assert_eq!(
            app.session.collections[ci]
                .entries
                .iter()
                .map(|e| e.title.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "c"]
        );
        assert_eq!(app.session.collections[ci].deleted_entries.len(), 1);

        app.undo_delete_request();
        assert_eq!(
            app.session.collections[ci]
                .entries
                .iter()
                .map(|e| e.title.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b", "c"]
        );
        assert!(app.session.collections[ci].deleted_entries.is_empty());
        assert_eq!(app.session.collections[ci].selected_entry, 1);
    }

    /// Only a method other than GET makes Run All worth a prompt; a read-only
    /// collection should run with no friction. GET is matched the way Hurl
    /// treats it — case-insensitively.
    #[test]
    fn run_all_confirm_counts_flags_only_the_non_get_requests() {
        let get_only = vec![entry("a"), entry("b")];
        assert_eq!(run_all_confirm_counts(&get_only), (2, 0));

        let mut mixed = get_only.clone();
        let mut post = entry("c");
        post.method = "post".into();
        mixed.push(post);
        assert_eq!(run_all_confirm_counts(&mixed), (3, 1));

        assert_eq!(run_all_confirm_counts(&[]), (0, 0));
    }

    /// An all-GET collection runs immediately; one with a write is held behind
    /// a confirmation that carries the counts.
    #[test]
    fn request_run_all_asks_only_when_a_write_is_in_the_collection() {
        let mut session = crate::session::Session::default();
        session.collections[0].entries = vec![entry("a"), entry("b")];
        let mut app = GuiApp::for_test(session);
        app.request_run_all(0);
        assert!(
            app.dialog.is_none(),
            "a read-only collection runs with no prompt"
        );

        let mut post = entry("c");
        post.method = "POST".into();
        app.session.collections[0].entries.push(post);
        app.request_run_all(0);
        match &app.dialog {
            Some(Dialog::ConfirmRunAll { ci, total, non_get }) => {
                assert_eq!((*ci, *total, *non_get), (0, 3, 1));
            }
            _ => panic!("a collection with a write asks first, and says how many"),
        }
    }

    /// The tree keyboard steps through requests in the order they are drawn,
    /// which is the order the file lists them: a folder sits where its first
    /// request does, so Up/Down move to the row the eye expects.
    #[test]
    fn nav_entry_order_follows_the_file_order() {
        let mut session = crate::session::Session::default();
        session.collections[0].entries = vec![
            entry("Root"),
            entry("Users/List"),
            entry("Auth/Login"),
            entry("Auth/Logout"),
        ];
        let col = &session.collections[0];
        // Users comes first because its request does, not because of its name,
        // and the loose root request keeps the top slot the file gave it.
        assert_eq!(nav_entry_order(col), vec![0, 1, 2, 3]);
    }

    /// A folder is drawn once, where it first appears, and later requests from
    /// it join that folder rather than opening a second one further down.
    #[test]
    fn nav_entry_order_reuses_a_folder_that_the_file_returns_to() {
        let mut session = crate::session::Session::default();
        session.collections[0].entries = vec![
            entry("Auth/Login"),
            entry("Users/List"),
            entry("Auth/Logout"),
        ];
        let col = &session.collections[0];
        assert_eq!(nav_entry_order(col), vec![0, 2, 1]);
    }

    /// The sort button reorders the drawn tree at every level, so the keyboard
    /// has to walk it the same way or Up/Down would jump around the screen.
    #[test]
    fn nav_entry_order_follows_the_sort_mode() {
        let mut session = crate::session::Session::default();
        session.collections[0].entries = vec![
            entry("Zed/One"),
            entry("loose"),
            entry("Abe/Solo"),
            entry("Zed/Two"),
        ];
        let order = |s: &crate::session::Session| nav_entry_order(&s.collections[0]);

        assert_eq!(order(&session), vec![0, 3, 1, 2], "the file's own order");

        session.collections[0].list_sort = crate::tree::SortMode::Alpha;
        // Abe, the loose request, then Zed with its own two sorted inside it.
        assert_eq!(order(&session), vec![2, 1, 0, 3]);

        session.collections[0].list_sort = crate::tree::SortMode::ReverseAlpha;
        assert_eq!(order(&session), vec![3, 0, 1, 2]);

        // Back to the file's order, not to whatever the last sort left behind.
        session.collections[0].list_sort = crate::tree::SortMode::File;
        assert_eq!(order(&session), vec![0, 3, 1, 2]);
    }

    /// While filtered the list is flat — folders are gone — so the keyboard
    /// steps through exactly the matches, in match order.
    #[test]
    fn nav_entry_order_follows_the_filter_when_one_is_typed() {
        let mut session = crate::session::Session::default();
        session.collections[0].entries = vec![
            entry("Auth/Login"),
            entry("Users/List"),
            entry("Auth/Logout"),
        ];
        session.collections[0].list_query = "log".into();
        let col = &session.collections[0];
        assert_eq!(nav_entry_order(col), vec![0, 2]);
    }

    /// The context menu's Delete goes through the same preference-honouring path
    /// as the Delete key: on by default, it asks first and leaves the request
    /// in place until the prompt is answered.
    #[test]
    fn context_menu_delete_asks_before_removing_when_the_guard_is_on() {
        let mut session = crate::session::Session::default();
        session.collections[0].entries = vec![entry("a"), entry("b")];
        let ci = 0;
        let mut app = GuiApp::for_test(session);
        assert!(
            app.session.confirm_on_delete_request,
            "the guard is on by default, like the environment one"
        );

        apply_actions(
            &mut app,
            ci,
            Actions {
                delete: Some(1),
                ..Default::default()
            },
        );
        match &app.dialog {
            Some(Dialog::ConfirmDeleteRequest { ci: dci, idx, name }) => {
                assert_eq!((*dci, *idx), (0, 1));
                assert_eq!(name, "b");
            }
            _ => panic!("a delete confirmation naming the request should be up"),
        }
        assert_eq!(
            app.session.collections[ci].entries.len(),
            2,
            "nothing is removed until the prompt is answered"
        );
    }

    /// Every URL of a workspace tab's loaded file, in order — the shape the
    /// reorder tests compare against.
    fn ws_urls(app: &GuiApp, ci: usize) -> Vec<String> {
        app.session.collections[ci]
            .entries
            .iter()
            .map(|e| e.url.clone())
            .collect()
    }

    /// A workspace request could be selected and run, but never reordered — the
    /// tree's rows skipped the drag-to-reorder the plain list's have always had.
    /// Applying the action the drop zone raises moves the request within its
    /// file and marks that file unsaved, exactly as the plain list's does.
    #[test]
    fn reordering_a_request_within_a_workspace_collection_moves_it_and_marks_the_file_unsaved() {
        redirect_saved_state();
        let dir = ws_tmp("wsreorder");
        let one = dir.join("api/v1/one.hurl");
        let mut session = crate::session::Session::default();
        session.collections.clear();
        let ci = session.open_workspace(dir.clone());
        session.active_tab = ci;
        assert!(session.load_workspace_file(ci, one.clone()));
        let mut app = GuiApp::for_test(session);
        // one.hurl holds `a` then `a2`, in that order.
        assert_eq!(
            ws_urls(&app, ci),
            vec!["https://example.com/a", "https://example.com/a2"]
        );

        // The first request dragged into the gap past the last one.
        apply_ws_action(
            &mut app,
            ci,
            WsAction::ReorderRequest {
                collection: one.clone(),
                from: 0,
                before: 2,
            },
        );

        assert_eq!(
            ws_urls(&app, ci),
            vec!["https://example.com/a2", "https://example.com/a"],
            "the request landed in the gap it was dropped into"
        );
        assert!(
            app.session.collections[ci].structure_modified,
            "a reorder is an unsaved structural edit, the same as in a plain tab"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A request may only be reordered within its own file — moving one between
    /// files is what the transfer flow is for. The drop zone refuses a drag
    /// whose payload names a different collection than the row it is over; the
    /// handler's guard is the backstop, and pinning it proves a stray cross-file
    /// reorder can't fall onto the loaded file's entries.
    #[test]
    fn a_workspace_reorder_addressed_to_a_different_file_leaves_the_loaded_one_untouched() {
        redirect_saved_state();
        let dir = ws_tmp("wsreorderxfile");
        let one = dir.join("api/v1/one.hurl");
        let two = dir.join("api/v1/two.hurl");
        let mut session = crate::session::Session::default();
        session.collections.clear();
        let ci = session.open_workspace(dir.clone());
        session.active_tab = ci;
        assert!(session.load_workspace_file(ci, one.clone()));
        let mut app = GuiApp::for_test(session);
        let before = ws_urls(&app, ci);

        // Address a reorder to two.hurl while one.hurl is loaded.
        apply_ws_action(
            &mut app,
            ci,
            WsAction::ReorderRequest {
                collection: two.clone(),
                from: 0,
                before: 2,
            },
        );

        assert_eq!(
            before,
            ws_urls(&app, ci),
            "the loaded file's order is unchanged"
        );
        assert!(
            !app.session.collections[ci].structure_modified,
            "and nothing is marked unsaved"
        );
        assert_eq!(
            app.session.collections[ci].path.as_deref(),
            Some(one.as_path()),
            "nor is a different file loaded — a reorder never load-then-acts"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Right-clicking a workspace request used to offer only the tree's "New…"
    /// entries — none of the actions that apply to a request. The menu now
    /// carries Run/Rename/Duplicate/Delete, routed through the very handlers the
    /// plain list uses, so the two menus can't disagree: Rename raises the
    /// request rename dialog, Duplicate inserts a uniquely-titled copy, Delete
    /// (guard off) removes it.
    #[test]
    fn the_workspace_request_menu_renames_duplicates_and_deletes_through_the_plain_lists_handlers()
    {
        redirect_saved_state();
        let dir = ws_tmp("wsmenu");
        let one = dir.join("api/v1/one.hurl");
        let mut session = crate::session::Session::default();
        session.collections.clear();
        let ci = session.open_workspace(dir.clone());
        session.active_tab = ci;
        assert!(session.load_workspace_file(ci, one.clone()));
        let mut app = GuiApp::for_test(session);

        // Rename raises the same dialog the plain list's Rename does, aimed at
        // the row it was invoked on.
        apply_ws_action(
            &mut app,
            ci,
            WsAction::RequestAction {
                collection: one.clone(),
                idx: 1,
                loaded: true,
                kind: ReqEdit::Rename,
            },
        );
        match &app.dialog {
            Some(Dialog::Rename {
                target: RenameTarget::Request { ci: dci, idx },
                ..
            }) => assert_eq!((*dci, *idx), (ci, 1)),
            _ => panic!("Rename should raise the request rename dialog"),
        }
        app.dialog = None;

        // Duplicate inserts a copy right after the original and selects it.
        let before = app.session.collections[ci].entries.len();
        apply_ws_action(
            &mut app,
            ci,
            WsAction::RequestAction {
                collection: one.clone(),
                idx: 0,
                loaded: true,
                kind: ReqEdit::Duplicate,
            },
        );
        assert_eq!(
            app.session.collections[ci].entries.len(),
            before + 1,
            "a copy was inserted"
        );
        assert_eq!(
            app.session.collections[ci].selected_entry, 1,
            "and selected, right after the original it came from"
        );

        // Delete, with the guard off, removes it straight away.
        app.session.confirm_on_delete_request = false;
        let n = app.session.collections[ci].entries.len();
        apply_ws_action(
            &mut app,
            ci,
            WsAction::RequestAction {
                collection: one.clone(),
                idx: 0,
                loaded: true,
                kind: ReqEdit::Delete,
            },
        );
        assert_eq!(
            app.session.collections[ci].entries.len(),
            n - 1,
            "the request was deleted"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A request row for a collection the tab hasn't loaded indexes a cached
    /// name/method snapshot, not the live `entries`. A menu action on such a row
    /// must load that file first (as double-click-to-run already does) and only
    /// then act — acting on the loaded file's index would hit the wrong request.
    #[test]
    fn a_request_action_on_a_not_loaded_row_loads_that_file_before_acting() {
        redirect_saved_state();
        let dir = ws_tmp("wsloadact");
        let one = dir.join("api/v1/one.hurl");
        let two = dir.join("api/v1/two.hurl");
        let mut session = crate::session::Session::default();
        session.collections.clear();
        let ci = session.open_workspace(dir.clone());
        session.active_tab = ci;
        assert!(session.load_workspace_file(ci, one.clone()));
        let mut app = GuiApp::for_test(session);
        app.session.confirm_on_delete_request = false;
        assert_eq!(
            app.session.collections[ci].path.as_deref(),
            Some(one.as_path()),
            "one.hurl is the loaded file, two.hurl is not"
        );

        // Delete the second request of the *other* file. Its row is
        // `loaded: false`, so this must load two.hurl before acting — deleting
        // one.hurl's index 1 would remove the wrong request.
        apply_ws_action(
            &mut app,
            ci,
            WsAction::RequestAction {
                collection: two.clone(),
                idx: 1,
                loaded: false,
                kind: ReqEdit::Delete,
            },
        );

        assert_eq!(
            app.session.collections[ci].path.as_deref(),
            Some(two.as_path()),
            "the addressed file is now the loaded one"
        );
        assert_eq!(
            ws_urls(&app, ci),
            vec!["https://example.com/b"],
            "and its second request — not one.hurl's — was the one deleted"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Renaming the file a tab has loaded moves it on disk *and* keeps the tab
    /// pointing at it — the whole reason a rename needs the same repointing a
    /// move does. Without it the tab would hold a path that no longer exists and
    /// look as though its contents had vanished.
    #[test]
    fn renaming_the_loaded_workspace_file_repoints_the_tab_that_had_it_open() {
        redirect_saved_state();
        let dir = ws_tmp("wsrenload");
        let one = dir.join("api/v1/one.hurl");
        let mut session = crate::session::Session::default();
        session.collections.clear();
        let ci = session.open_workspace(dir.clone());
        session.active_tab = ci;
        assert!(session.load_workspace_file(ci, one.clone()));
        let mut app = GuiApp::for_test(session);

        rename_workspace_item(&mut app, ci, &one, "renamed");

        let dest = dir.join("api/v1/renamed.hurl");
        assert!(dest.exists(), "the file is at its new name on disk");
        assert!(!one.exists(), "and gone from the old one");
        assert_eq!(
            app.session.collections[ci].path.as_deref(),
            Some(dest.as_path()),
            "the tab follows the file it had loaded"
        );
        assert!(
            matches!(
                app.session.status,
                Some(crate::i18n::Status::WsItemRenamed(_))
            ),
            "and it reports the rename"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A rename of a request row is a different thing from a rename of the file:
    /// the menu's file Rename raises a `WorkspaceItem` target, not a `Request`
    /// one, so the two can't be confused.
    #[test]
    fn the_file_rename_menu_entry_targets_the_file_not_a_request() {
        redirect_saved_state();
        let dir = ws_tmp("wsrenmenu");
        let health = dir.join("health.hurl");
        let mut session = crate::session::Session::default();
        session.collections.clear();
        let ci = session.open_workspace(dir.clone());
        session.active_tab = ci;
        let mut app = GuiApp::for_test(session);

        apply_ws_action(
            &mut app,
            ci,
            WsAction::RenameItem {
                path: health.clone(),
            },
        );
        match &app.dialog {
            Some(Dialog::Rename {
                target: RenameTarget::WorkspaceItem { ci: dci, path },
                text,
            }) => {
                assert_eq!(*dci, ci);
                assert_eq!(path, &health);
                assert_eq!(text, "health.hurl", "seeded with the current name");
            }
            _ => panic!("file Rename should raise a WorkspaceItem rename dialog"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Deleting the file a tab has loaded removes it from disk and leaves the
    /// tab in the file-less state a fresh Workspace tab starts in, rather than
    /// showing a phantom of a file that is gone.
    #[test]
    fn deleting_the_loaded_workspace_file_unloads_the_tab() {
        redirect_saved_state();
        let dir = ws_tmp("wsdelload");
        let one = dir.join("api/v1/one.hurl");
        let mut session = crate::session::Session::default();
        session.collections.clear();
        let ci = session.open_workspace(dir.clone());
        session.active_tab = ci;
        assert!(session.load_workspace_file(ci, one.clone()));
        let mut app = GuiApp::for_test(session);
        assert!(!app.session.collections[ci].entries.is_empty());

        delete_workspace_item(&mut app, ci, &one);

        assert!(!one.exists(), "the file is gone from disk");
        assert_eq!(
            app.session.collections[ci].path, None,
            "the tab no longer has a loaded file"
        );
        assert!(
            app.session.collections[ci].entries.is_empty(),
            "and isn't left showing the deleted file's requests"
        );
        assert!(
            matches!(
                app.session.status,
                Some(crate::i18n::Status::WsItemDeleted(_))
            ),
            "and it reports the delete"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Deleting a folder takes its whole subtree with it, and the tab forgets
    /// everything it was holding under it: the loaded file (which was inside),
    /// the expanded rows, the remembered selection.
    #[test]
    fn deleting_a_workspace_folder_removes_it_and_forgets_its_children() {
        redirect_saved_state();
        let dir = ws_tmp("wsdelfolder");
        let api = dir.join("api");
        let v1 = dir.join("api/v1");
        let one = v1.join("one.hurl");
        let mut session = crate::session::Session::default();
        session.collections.clear();
        let ci = session.open_workspace(dir.clone());
        session.active_tab = ci;
        assert!(session.load_workspace_file(ci, one.clone()));
        let mut app = GuiApp::for_test(session);
        app.session.collections[ci]
            .workspace_expanded
            .insert(api.clone());
        app.session.collections[ci]
            .workspace_expanded
            .insert(v1.clone());
        app.session.collections[ci].workspace_selected = Some(one.clone());

        delete_workspace_item(&mut app, ci, &api);

        assert!(
            !api.exists(),
            "the folder and its subtree are gone from disk"
        );
        assert_eq!(
            app.session.collections[ci].path, None,
            "the loaded file, which was inside, is unloaded"
        );
        assert!(
            !app.session.collections[ci]
                .workspace_expanded
                .iter()
                .any(|p| p.starts_with(&api)),
            "no expanded row under the deleted folder survives"
        );
        assert_eq!(
            app.session.collections[ci].workspace_selected, None,
            "and the remembered selection, which was inside, is cleared"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The delete menu entry always raises a confirmation — never acting on the
    /// spot — and the confirmation carries what is about to go: a folder's file
    /// count, so a folder delete says how much it will take.
    #[test]
    fn the_file_delete_menu_entry_always_confirms_and_counts_a_folder() {
        redirect_saved_state();
        let dir = ws_tmp("wsdelconfirm");
        let api = dir.join("api");
        let mut session = crate::session::Session::default();
        session.collections.clear();
        let ci = session.open_workspace(dir.clone());
        session.active_tab = ci;
        let mut app = GuiApp::for_test(session);
        // Even with the request-delete guard off, a disk delete still confirms.
        app.session.confirm_on_delete_request = false;

        apply_ws_action(
            &mut app,
            ci,
            WsAction::DeleteItem {
                path: api.clone(),
                is_dir: true,
            },
        );

        match &app.dialog {
            Some(Dialog::DeleteWorkspaceItem {
                path,
                is_dir,
                file_count,
                ..
            }) => {
                assert_eq!(path, &api);
                assert!(*is_dir, "api is a folder");
                assert_eq!(
                    *file_count, 4,
                    "api/v1 holds two collections, an env and a report — four files"
                );
            }
            _ => panic!("Delete should always raise the confirmation dialog"),
        }
        // Nothing was deleted just by opening the dialog.
        assert!(api.exists(), "the folder is still there until confirmed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- Keyboard for the Workspace tab's file tree (`handle_ws_tree_keys`). ---

    /// Feed one key press to the workspace tree handler exactly as a frame
    /// would, so `consume_key` runs the real input path rather than a poked
    /// flag. The panel-focus and dialog gating live one level up in
    /// `handle_global_keys`; those are exercised through the `press` helper in
    /// `app.rs`, so here the handler is driven directly.
    fn ws_key(app: &mut GuiApp, ctx: &egui::Context, ci: usize, key: egui::Key) {
        let mut input = egui::RawInput::default();
        input.events.push(egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        input.modifiers = egui::Modifiers::NONE;
        let _ = ctx.run_ui(input, |ui| handle_ws_tree_keys(app, ui.ctx(), ci));
    }

    /// The row index of the first folder whose path is `path`.
    fn folder_row(app: &GuiApp, ci: usize, path: &std::path::Path) -> usize {
        app.session.collections[ci]
            .ws_rows()
            .iter()
            .position(|r| matches!(r, WsRow::Folder { path: p, .. } if p == path))
            .unwrap_or_else(|| panic!("no folder row for {}", path.display()))
    }

    #[test]
    fn the_workspace_tree_cursor_moves_with_the_arrows_and_clamps_at_both_ends() {
        redirect_saved_state();
        let dir = ws_tmp("wskeymove");
        let mut session = crate::session::Session::default();
        session.collections.clear();
        let ci = session.open_workspace(dir.clone());
        session.active_tab = ci;
        let mut app = GuiApp::for_test(session);
        let last = app.session.collections[ci].ws_rows().len() - 1;
        assert!(last >= 1, "the fixture lists at least two top-level rows");
        app.session.collections[ci].list_cursor = 0;
        let ctx = egui::Context::default();

        ws_key(&mut app, &ctx, ci, egui::Key::ArrowDown);
        assert_eq!(app.session.collections[ci].list_cursor, 1);
        assert!(
            app.reveal_selected,
            "a keyboard move asks the row into view for the next render"
        );

        ws_key(&mut app, &ctx, ci, egui::Key::End);
        assert_eq!(
            app.session.collections[ci].list_cursor, last,
            "End jumps to the last row"
        );
        ws_key(&mut app, &ctx, ci, egui::Key::ArrowDown);
        assert_eq!(
            app.session.collections[ci].list_cursor, last,
            "Down at the bottom clamps rather than wrapping"
        );

        ws_key(&mut app, &ctx, ci, egui::Key::Home);
        assert_eq!(
            app.session.collections[ci].list_cursor, 0,
            "Home jumps to the first row"
        );
        ws_key(&mut app, &ctx, ci, egui::Key::ArrowUp);
        assert_eq!(
            app.session.collections[ci].list_cursor, 0,
            "Up at the top clamps rather than wrapping"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn right_expands_a_collapsed_folder_and_left_collapses_it() {
        redirect_saved_state();
        let dir = ws_tmp("wskeyexpand");
        let api = dir.join("api");
        let mut session = crate::session::Session::default();
        session.collections.clear();
        let ci = session.open_workspace(dir.clone());
        session.active_tab = ci;
        let mut app = GuiApp::for_test(session);
        let ctx = egui::Context::default();

        let api_row = folder_row(&app, ci, &api);
        app.session.collections[ci].list_cursor = api_row;
        assert!(
            !app.session.collections[ci]
                .workspace_expanded
                .contains(&api),
            "the api folder starts collapsed"
        );

        ws_key(&mut app, &ctx, ci, egui::Key::ArrowRight);
        assert!(
            app.session.collections[ci]
                .workspace_expanded
                .contains(&api),
            "Right expands a collapsed folder"
        );
        assert_eq!(
            app.session.collections[ci].list_cursor, api_row,
            "the cursor stays on the folder it just opened"
        );

        ws_key(&mut app, &ctx, ci, egui::Key::ArrowLeft);
        assert!(
            !app.session.collections[ci]
                .workspace_expanded
                .contains(&api),
            "Left collapses the folder it is looking into"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn right_steps_into_an_open_folder_and_left_climbs_to_the_parent() {
        redirect_saved_state();
        let dir = ws_tmp("wskeytree");
        let api = dir.join("api");
        let v1 = dir.join("api/v1");
        let mut session = crate::session::Session::default();
        session.collections.clear();
        let ci = session.open_workspace(dir.clone());
        session.active_tab = ci;
        // Open `api` so `v1` shows beneath it, but leave `v1` collapsed.
        session.collections[ci]
            .workspace_expanded
            .insert(api.clone());
        let mut app = GuiApp::for_test(session);
        let ctx = egui::Context::default();

        let api_row = folder_row(&app, ci, &api);
        let v1_row = folder_row(&app, ci, &v1);
        assert_eq!(v1_row, api_row + 1, "v1 is api's first child in the tree");

        app.session.collections[ci].list_cursor = api_row;
        ws_key(&mut app, &ctx, ci, egui::Key::ArrowRight);
        assert_eq!(
            app.session.collections[ci].list_cursor, v1_row,
            "Right on an already-open folder steps onto its first child"
        );

        assert!(
            !app.session.collections[ci].workspace_expanded.contains(&v1),
            "v1 is collapsed, so Left climbs out rather than closing it"
        );
        ws_key(&mut app, &ctx, ci, egui::Key::ArrowLeft);
        assert_eq!(
            app.session.collections[ci].list_cursor, api_row,
            "Left on a collapsed child climbs to the parent row"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn enter_runs_a_request_and_toggles_a_folder() {
        redirect_saved_state();
        let dir = ws_tmp("wskeyenter");
        let api = dir.join("api");
        let one = dir.join("api/v1/one.hurl");
        let mut session = crate::session::Session::default();
        session.collections.clear();
        let ci = session.open_workspace(dir.clone());
        session.active_tab = ci;
        let mut app = GuiApp::for_test(session);
        let ctx = egui::Context::default();

        // Enter on a folder toggles it open (the same action a click raises).
        let api_row = folder_row(&app, ci, &api);
        app.session.collections[ci].list_cursor = api_row;
        ws_key(&mut app, &ctx, ci, egui::Key::Enter);
        assert!(
            app.session.collections[ci]
                .workspace_expanded
                .contains(&api),
            "Enter on a folder toggles it"
        );

        // Load a file so a request row exists, then Enter on it runs it.
        assert!(app.session.load_workspace_file(ci, one.clone()));
        let rows = app.session.collections[ci].ws_rows();
        let (req_row, idx) = rows
            .iter()
            .enumerate()
            .find_map(|(row, r)| match r {
                WsRow::Request {
                    idx, loaded: true, ..
                } => Some((row, *idx)),
                _ => None,
            })
            .expect("the loaded file lists a request row");
        app.session.collections[ci].list_cursor = req_row;
        ws_key(&mut app, &ctx, ci, egui::Key::Enter);
        assert_eq!(
            app.session.collections[ci].selected_entry, idx,
            "Enter selects the request it is about to run"
        );
        assert_eq!(
            app.session.collections[ci].entries[idx].last_run,
            crate::hurl::RunStatus::Running,
            "and starts it running"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn f2_and_delete_reach_the_file_on_a_file_row_and_the_request_on_a_request_row() {
        redirect_saved_state();
        let dir = ws_tmp("wskeyedit");
        let health = dir.join("health.hurl");
        let one = dir.join("api/v1/one.hurl");
        let mut session = crate::session::Session::default();
        session.collections.clear();
        let ci = session.open_workspace(dir.clone());
        session.active_tab = ci;
        // Guard off, so a *request* delete would act at once — the contrast that
        // proves a *file* delete still always confirms.
        session.confirm_on_delete_request = false;
        let mut app = GuiApp::for_test(session);
        let ctx = egui::Context::default();

        // --- On a collection *file* row. ---
        let file_row = app.session.collections[ci]
            .ws_rows()
            .iter()
            .position(|r| matches!(r, WsRow::Collection { path, .. } if path == &health))
            .expect("health.hurl is a top-level collection row");
        app.session.collections[ci].list_cursor = file_row;

        ws_key(&mut app, &ctx, ci, egui::Key::F2);
        assert!(
            matches!(
                app.dialog,
                Some(Dialog::Rename {
                    target: RenameTarget::WorkspaceItem { .. },
                    ..
                })
            ),
            "F2 on a file renames the workspace item, not a request"
        );
        app.dialog = None;

        ws_key(&mut app, &ctx, ci, egui::Key::Delete);
        assert!(
            matches!(app.dialog, Some(Dialog::DeleteWorkspaceItem { .. })),
            "Delete on a file always raises the disk-delete confirmation"
        );
        assert!(health.exists(), "and nothing is gone until it is confirmed");
        app.dialog = None;

        // --- On a request row. ---
        assert!(app.session.load_workspace_file(ci, one.clone()));
        let rows = app.session.collections[ci].ws_rows();
        let req_row = rows
            .iter()
            .position(|r| matches!(r, WsRow::Request { loaded: true, .. }))
            .expect("the loaded file lists a request row");
        app.session.collections[ci].list_cursor = req_row;

        ws_key(&mut app, &ctx, ci, egui::Key::F2);
        assert!(
            matches!(
                app.dialog,
                Some(Dialog::Rename {
                    target: RenameTarget::Request { .. },
                    ..
                })
            ),
            "F2 on a request renames the request"
        );
        app.dialog = None;

        let n = app.session.collections[ci].entries.len();
        ws_key(&mut app, &ctx, ci, egui::Key::Delete);
        assert!(
            app.dialog.is_none(),
            "the request-delete guard is off, so a request delete acts without a prompt"
        );
        assert_eq!(
            app.session.collections[ci].entries.len(),
            n - 1,
            "and the request under the cursor is gone"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clicking_a_tree_row_moves_the_keyboard_cursor_to_it() {
        redirect_saved_state();
        let dir = ws_tmp("wskeyclick");
        let api = dir.join("api");
        let mut session = crate::session::Session::default();
        session.collections.clear();
        let ci = session.open_workspace(dir.clone());
        session.active_tab = ci;
        let mut app = GuiApp::for_test(session);
        app.focus = crate::gui::Focus::List;

        let ctx = egui::Context::default();
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 600.0));
        let frame = |events: Vec<egui::Event>| egui::RawInput {
            screen_rect: Some(screen),
            events,
            ..Default::default()
        };

        // Start the cursor on a *different* row than the one about to be clicked.
        let api_row = folder_row(&app, ci, &api);
        app.session.collections[ci].list_cursor = api_row + 1;

        // Lay the tree out once so the api folder row's rect is known.
        let out = ctx.run_ui(frame(Vec::new()), |ui| {
            crate::gui::requests::ui(&mut app, ui)
        });
        let row = texts(&out.shapes)
            .into_iter()
            .find(|(t, _)| t.contains("api"))
            .expect("the api folder row is drawn")
            .1;
        let at = egui::pos2(row.center().x, row.center().y);

        let mut events = vec![egui::Event::PointerMoved(at)];
        for pressed in [true, false] {
            events.push(egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
        }
        let _ = ctx.run_ui(frame(events), |ui| crate::gui::requests::ui(&mut app, ui));

        assert_eq!(
            app.session.collections[ci].list_cursor, api_row,
            "clicking a row hands the keyboard cursor to it, so the two don't fight"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
