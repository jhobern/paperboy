use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Clear, List, ListItem, ListState, Paragraph, WidgetRef, Wrap,
};

use crate::collection::{Collection, WsRow};
use crate::environment::ValueSource;
use crate::hurl::RunStatus;
use crate::i18n::{Language, Strings};
use crate::request;
use crate::tree;

use super::app::*;
use super::editor::*;
use super::git_save::*;
use super::new_request::*;
use super::postman::*;
use super::remote::*;
use super::theme::*;
use std::sync::Arc;
use tui_panel_select::WrapMarker;
use tui_panel_select::wrapcache::TextPos;

/// Marks a collection/environment title as loaded from git — shown before the
/// name whenever `Collection::git_origin` / `env_git_origin` is set.
pub(crate) const GIT_ICON: &str = "\u{2387}";
/// Marks an Environments panel row as coming from the open Workspace folder,
/// as opposed to an environment loaded from anywhere else. Deliberately a
/// single-cell BMP glyph (⌂) rather than a folder emoji — see the note on
/// [`SHADOW_ICON`] about terminals giving emoji double-cell width.
pub(crate) const WORKSPACE_ICON: &str = "\u{2302}";
/// Joins a collection tab to its linked Global Environment's sub-tab in the
/// tab bar (see `draw_tabs`).
pub(crate) const LINK_ICON: &str = "\u{1F517}";
/// Flags a substituted `{{ VAR }}` in the Request viewer whose Global
/// Environment value is being shadowed by the collection's linked
/// Environment (see `TuiApp::shadowed_env_keys`). Deliberately a plain ASCII
/// glyph rather than a Unicode symbol like ⚠ — several terminals render
/// that one with emoji-style double-cell width even without a variation
/// selector, which visually overlaps the very next character.
pub(crate) const SHADOW_ICON: &str = "!";
/// The end-of-row marker painted in a reserved rightmost column whenever a
/// logical line is soft-wrapped by a body panel (Request/Response), so a
/// wrapped line reads unambiguously as one line rather than several separate
/// ones. Drawn dim so it never competes with the content. Built per-frame
/// from the active theme (see `MultiSelectPanel::set_wrap_marker`).
pub(crate) fn wrap_marker(th: &Theme) -> WrapMarker {
    WrapMarker {
        glyph: '↵',
        style: Style::default().fg(th.dim),
    }
}

/// The display width of a single `char`, measured the same way `ratatui`
/// measures spans (so wide glyphs count as 2). Cheap enough for the capped,
/// modal previews that call [`wrap_lines_with_marker`].
fn char_display_width(ch: char) -> usize {
    Span::raw(ch.to_string()).width().max(1)
}

/// Soft-wrap already-themed `lines` to `width` columns, appending the dim
/// [`wrap_marker`] glyph at every break so a wrapped logical line still reads
/// as one line. This is the manual counterpart of a body panel's wrap marker,
/// for the plain-`Paragraph` overlays (the report dry-run preview) that render
/// outside a `MultiSelectPanel` and so don't get the marker for free. Styles
/// are preserved across breaks; the rightmost column is reserved for the marker
/// (matching the panels), so content wraps at `width - 1`.
pub(crate) fn wrap_lines_with_marker(
    lines: Vec<Line<'static>>,
    width: u16,
    th: &Theme,
) -> Vec<Line<'static>> {
    let width = width as usize;
    if width < 2 {
        return lines;
    }
    let limit = width - 1; // reserve the last column for the marker
    let marker = wrap_marker(th);
    let marker_glyph = marker.glyph.to_string();
    let mut out: Vec<Line<'static>> = Vec::new();
    for line in lines {
        // Lines that already fit pass straight through unchanged.
        if line.width() <= width {
            out.push(line);
            continue;
        }
        let mut row: Vec<Span<'static>> = Vec::new();
        let mut col = 0usize;
        for span in line.spans {
            let style = span.style;
            let mut buf = String::new();
            for ch in span.content.chars() {
                let cw = char_display_width(ch);
                if col + cw > limit {
                    if !buf.is_empty() {
                        row.push(Span::styled(std::mem::take(&mut buf), style));
                    }
                    row.push(Span::styled(marker_glyph.clone(), marker.style));
                    out.push(Line::from(std::mem::take(&mut row)));
                    col = 0;
                }
                buf.push(ch);
                col += cw;
            }
            if !buf.is_empty() {
                row.push(Span::styled(buf, style));
            }
        }
        // The final segment is the logical line's true end — no marker.
        out.push(Line::from(row));
    }
    out
}
/// Marks a subfolder row in the request list tree, and (in the request
/// editor's form) hints that a File-kind field's Value opens a file picker
/// on Enter.
pub(crate) const FOLDER_ICON: &str = "\u{1F4C1}";
/// Chevrons on a Workspace collection file row: expanded (requests inlined)
/// vs collapsed.
const COLLECTION_OPEN_ICON: &str = "\u{25BE}"; // ▾
pub(crate) const COLLECTION_CLOSED_ICON: &str = "\u{25B8}"; // ▸
/// Marks a PaperTrail report file in the Workspace tree (a document/chart glyph).
pub(crate) const REPORT_ICON: &str = "\u{1F4CA}"; // 📊
/// Marks an environment file (`.vars`) in the Workspace tree.
pub(crate) const ENV_ICON: &str = "\u{1F310}"; // 🌐

/// A rendered row of the request list, unifying the ordinary title-folder
/// tree ([`tree::Row`]) and the Workspace file-tree ([`WsRow`]) so
/// [`draw_collection_left`] can lay both out with one loop.
///
/// `WsFolder` is the workspace-specific folder variant (chevron + indented);
/// `Folder` is the non-workspace virtual-folder variant (folder emoji, flat).
/// `depth` on workspace rows drives `"  ".repeat(depth)` indentation in the
/// rendered list.
enum LeftRow {
    Up,
    /// Non-workspace virtual folder (title-encoded); always flat, no expand
    /// state, rendered with FOLDER_ICON.
    Folder(String),
    /// Workspace filesystem folder; indented by `depth * 2` spaces and
    /// rendered with an expand/collapse chevron.
    WsFolder {
        name: String,
        depth: usize,
        expanded: bool,
    },
    Collection {
        name: String,
        depth: usize,
        open: bool,
        /// True when this is the tab's currently-loaded collection — the one
        /// whose requests render in full colour. Drawn in the accent colour so
        /// it's visually obvious which collection the coloured requests belong
        /// to; other collections (and their request names) render dim.
        loaded: bool,
    },
    Report {
        name: String,
        depth: usize,
    },
    /// An environment file (`.vars`) in a Workspace tree; opening it loads the
    /// file as a global environment.
    Environment {
        name: String,
        depth: usize,
    },
    Entry {
        idx: usize,
        /// Indentation depth: 0 for non-workspace, collection-depth+1 for
        /// workspace requests.
        depth: usize,
    },
    /// A request of an expanded but *not-loaded* workspace collection: only its
    /// cached name is known (no entry to draw method/status from), so it renders
    /// as a dim, name-only leaf. Opening it (Enter/Right) loads its collection.
    WsRequestName {
        name: String,
        depth: usize,
    },
}

impl LeftRow {
    /// The list rows for tab `col`: the Workspace file-tree when it's bound to
    /// a folder, otherwise the title-folder tree.
    fn build(col: &Collection) -> Vec<LeftRow> {
        if col.is_workspace() {
            col.ws_rows()
                .into_iter()
                .map(|r| match r {
                    WsRow::Folder {
                        name,
                        depth,
                        expanded,
                        ..
                    } => LeftRow::WsFolder {
                        name,
                        depth,
                        expanded,
                    },
                    WsRow::Collection {
                        path,
                        name,
                        depth,
                        open,
                    } => {
                        let loaded = col.path.as_deref() == Some(path.as_path());
                        LeftRow::Collection {
                            name,
                            depth,
                            open,
                            loaded,
                        }
                    }
                    WsRow::Report { name, depth, .. } => LeftRow::Report { name, depth },
                    WsRow::Environment { name, depth, .. } => LeftRow::Environment { name, depth },
                    WsRow::Request {
                        idx,
                        depth,
                        loaded: true,
                        ..
                    } => LeftRow::Entry { idx, depth },
                    WsRow::Request {
                        name,
                        depth,
                        loaded: false,
                        ..
                    } => LeftRow::WsRequestName { name, depth },
                })
                .collect()
        } else {
            col.rows()
                .into_iter()
                .map(|r| match r {
                    tree::Row::Up => LeftRow::Up,
                    tree::Row::Folder(name) => LeftRow::Folder(name),
                    tree::Row::Entry(idx) => LeftRow::Entry { idx, depth: 0 },
                })
                .collect()
        }
    }

    /// The entry index if this is a request row (for URL scrolling/substitution).
    fn entry_idx(&self) -> Option<usize> {
        match self {
            LeftRow::Entry { idx, .. } => Some(*idx),
            _ => None,
        }
    }
}

pub(crate) fn panel(title: String, focused: bool, th: &Theme) -> Block<'static> {
    let border = if focused { th.accent } else { th.dim };
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border))
        .title(Span::styled(
            title,
            Style::default().fg(th.text).add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(th.panel))
}

pub(crate) fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    let x = area.x + (area.width - w) / 2;
    let y = area.y + (area.height - h) / 2;
    Rect::new(x, y, w, h)
}

/// Draw the report column-picker overlay: a scrollable checklist of candidate
/// output columns. The selected row is highlighted; a renamed column shows its
/// underlying source(s) dimmed after a `←`.
fn draw_report_columns_overlay(
    f: &mut Frame,
    picker: &super::reports::ColumnPicker,
    s: &Strings,
    th: &Theme,
    app: Option<&TuiApp>,
) {
    let title = format!("{}  ({})", s.report_columns_title, s.report_columns_hint);
    let n = picker.rows.len();
    let box_w = f.area().width.saturating_sub(6).clamp(40, 90);
    let box_h = (n as u16 + 2).min(f.area().height.saturating_sub(2)).max(3);
    let area = centered_rect(box_w, box_h, f.area());
    f.render_widget(Clear, area);
    let inner_h = area.height.saturating_sub(2) as usize;
    // Scroll just enough to keep the selected row inside the visible window.
    let scroll = if picker.selected >= inner_h {
        picker.selected + 1 - inner_h
    } else {
        0
    };
    let mut lines: Vec<Line> = Vec::new();
    for (i, row) in picker.rows.iter().enumerate().skip(scroll).take(inner_h) {
        let mark = if row.included { "[x]" } else { "[ ]" };
        let base = if i == picker.selected {
            Style::default()
                .fg(th.bg)
                .bg(th.accent)
                .add_modifier(Modifier::BOLD)
        } else if row.included {
            Style::default().fg(th.text)
        } else {
            Style::default().fg(th.dim)
        };
        let mut spans = vec![Span::styled(format!("{mark} {}", row.header), base)];
        let src = row.sources.join("|");
        if src != row.header {
            spans.push(Span::styled(
                format!("  ← {src}"),
                Style::default().fg(th.dim),
            ));
        }
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines).block(panel(title, true, th)), area);
    if let Some(app) = app {
        app.set_mouse_layer(MouseLayer::Overlay);
        let inner = Rect {
            x: area.x.saturating_add(1),
            y: area.y.saturating_add(1),
            width: area.width.saturating_sub(2),
            height: area.height.saturating_sub(2),
        };
        for row in scroll..picker.rows.len().min(scroll + inner_h) {
            app.push_mouse_hit(
                MouseLayer::Overlay,
                Rect::new(inner.x, inner.y + (row - scroll) as u16, inner.width, 1),
                MouseHitTarget::OverlayRow(row),
            );
        }
    }
    if n > inner_h {
        let bar_area = Rect {
            x: area.x + area.width - 1,
            y: area.y + 1,
            width: 1,
            height: inner_h as u16,
        };
        draw_scrollbar(f, bar_area, n, inner_h, scroll, th);
    }
}

/// Draw the reported-request detail form overlay
/// ([`Overlay::ReportNodeRequest`]): a scrollable form for a `REPORT REQUEST`
/// node — the response-format toggle and alias field on top, then a checklist
/// of the fields the request can emit. The selected row is highlighted; ticked
/// fields are kept, unticked ones are dropped from the node's `SHOW(…)`.
fn draw_report_node_request_overlay(
    f: &mut Frame,
    form: &super::report_nodes::RequestForm,
    s: &Strings,
    th: &Theme,
    app: Option<&TuiApp>,
) {
    use super::report_nodes::FormRow;
    let rows = form.visible_rows();
    let n = rows.len();
    let box_w = f.area().width.saturating_sub(6).clamp(40, 90);
    let box_h = (n as u16 + 2).min(f.area().height.saturating_sub(2)).max(3);
    let area = centered_rect(box_w, box_h, f.area());
    f.render_widget(Clear, area);
    let inner_h = area.height.saturating_sub(2) as usize;
    let selected = form.selected.min(n.saturating_sub(1));
    let scroll = if selected >= inner_h {
        selected + 1 - inner_h
    } else {
        0
    };
    let response_label = match form.response {
        None => s.report_node_response_default,
        Some(crate::report::flow::ResponseFmt::Raw) => "RAW",
        Some(crate::report::flow::ResponseFmt::Pretty) => "PRETTY",
    };
    let alias_shown = if form.alias.is_empty() {
        s.report_node_alias_none
    } else {
        form.alias.as_str()
    };
    let mut lines: Vec<Line> = Vec::new();
    for (i, row) in rows.iter().enumerate().skip(scroll).take(inner_h) {
        let is_sel = i == selected;
        let base = if is_sel {
            Style::default()
                .fg(th.bg)
                .bg(th.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(th.text)
        };
        let line = match *row {
            FormRow::Name => {
                let shown = if form.request.is_empty() {
                    s.report_node_name_none
                } else {
                    form.request.as_str()
                };
                Line::from(Span::styled(
                    format!("{}: ‹{}›", s.report_node_name_label, shown),
                    base,
                ))
            }
            FormRow::Report => {
                let mark = if form.report { "[x]" } else { "[ ]" };
                Line::from(Span::styled(
                    format!("{mark} {}", s.report_node_report_label),
                    base,
                ))
            }
            FormRow::Response => Line::from(Span::styled(
                format!("{}: ‹{}›", s.report_node_response_label, response_label),
                base,
            )),
            FormRow::Alias => {
                let mut text = format!("{}: {}", s.report_node_alias_label, alias_shown);
                if is_sel {
                    text.push('▏');
                }
                Line::from(Span::styled(text, base))
            }
            FormRow::Field(fi) => {
                let fr = &form.fields[fi];
                let mark = if fr.included { "[x]" } else { "[ ]" };
                let style = if is_sel {
                    base
                } else if fr.included {
                    Style::default().fg(th.text)
                } else {
                    Style::default().fg(th.dim)
                };
                // `SHOW` and `HIDE` are two independent clauses over the same
                // field names, so each row names the clause it writes — without
                // it the two blocks of checkboxes are indistinguishable.
                Line::from(Span::styled(format!("{mark} SHOW {}", fr.name), style))
            }
            FormRow::Hidden(fi) => {
                let fr = &form.hide_fields[fi];
                let mark = if fr.included { "[x]" } else { "[ ]" };
                let style = if is_sel {
                    base
                } else if fr.included {
                    Style::default().fg(th.text)
                } else {
                    Style::default().fg(th.dim)
                };
                Line::from(Span::styled(format!("{mark} HIDE {}", fr.name), style))
            }
            FormRow::With(wi) => {
                let text = match form.with.get(wi) {
                    Some(crate::report::flow::WithItem::Field { name, query, stats }) => {
                        let stats = if stats.is_empty() {
                            String::new()
                        } else {
                            format!(
                                "  {}({})",
                                s.report_node_with_stats_label,
                                stats
                                    .iter()
                                    .map(|k| k.keyword())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            )
                        };
                        format!("WITH {name}: {query}{stats}")
                    }
                    // A bare `WITH RESPONSE` carries no name/query.
                    _ => "WITH RESPONSE".to_string(),
                };
                Line::from(Span::styled(text, base))
            }
            FormRow::AddWith => Line::from(Span::styled(
                s.report_node_with_add.to_string(),
                if is_sel {
                    base
                } else {
                    Style::default().fg(th.dim)
                },
            )),
        };
        lines.push(line);
    }
    // The shortcut hint lives on the bottom border (a dim footer) rather than
    // crammed into the title, so a long request name no longer truncates it.
    let block = panel(s.report_node_config_title.to_string(), true, th).title_bottom(
        Line::from(Span::styled(
            format!(" {} ", s.report_node_request_hint),
            Style::default().fg(th.dim),
        ))
        .centered(),
    );
    f.render_widget(Paragraph::new(lines).block(block), area);
    if let Some(app) = app {
        app.set_mouse_layer(MouseLayer::Overlay);
        let inner = Rect {
            x: area.x.saturating_add(1),
            y: area.y.saturating_add(1),
            width: area.width.saturating_sub(2),
            height: area.height.saturating_sub(2),
        };
        for row in scroll..rows.len().min(scroll + inner_h) {
            app.push_mouse_hit(
                MouseLayer::Overlay,
                Rect::new(inner.x, inner.y + (row - scroll) as u16, inner.width, 1),
                MouseHitTarget::OverlayRow(row),
            );
        }
    }
    if n > inner_h {
        let bar_area = Rect {
            x: area.x + area.width - 1,
            y: area.y + 1,
            width: 1,
            height: inner_h as u16,
        };
        draw_scrollbar(f, bar_area, n, inner_h, scroll, th);
    }
}

/// The `PARALLEL(n)` max-concurrency row's text, shared by the ENVS and FILES
/// overlays. A blank box is shown as "no limit" rather than as an empty field,
/// since blank is a meaningful value here (fall back to `MAX_PARALLEL`) and not
/// an unfinished one.
fn degree_row_text(degree: &str, s: &Strings, is_sel: bool) -> String {
    let shown = if degree.trim().is_empty() {
        s.report_node_parallel_degree_none.to_string()
    } else {
        degree.to_string()
    };
    let mut text = format!("  {}: {shown}", s.report_node_parallel_degree_label);
    if is_sel {
        text.push('▏');
    }
    text
}

/// Draw the `REPORT <var>` overlay ([`Overlay::ReportNodeVars`]): a checklist of
/// the variables in scope, a free-text row for the ones the static scan can't
/// see, and — for a single picked variable — its `AS` name and statistics.
fn draw_report_node_vars_overlay(
    f: &mut Frame,
    form: &super::report_nodes::VarsForm,
    s: &Strings,
    th: &Theme,
    app: Option<&TuiApp>,
) {
    use super::report_nodes::VarsRow;
    let lines: Vec<(usize, String)> = form
        .visible_rows()
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let text = match row {
                VarsRow::Var(vi) => {
                    let r = &form.vars[*vi];
                    let mark = if r.included {
                        s.checkbox_checked
                    } else {
                        s.checkbox_unchecked
                    };
                    format!("{mark} {}", r.name)
                }
                // The two typed rows carry a cursor bar when selected.
                VarsRow::Other => caretted(
                    format!("{}: {}", s.report_node_vars_other_label, form.other),
                    i == form.selected,
                ),
                VarsRow::Alias => caretted(
                    format!("{}: {}", s.report_node_alias_label, form.alias),
                    i == form.selected,
                ),
                VarsRow::Stat(si) => {
                    let (kind, on) = form.stats[*si];
                    let mark = if on {
                        s.checkbox_checked
                    } else {
                        s.checkbox_unchecked
                    };
                    format!(
                        "  {mark} {}({})",
                        s.report_node_with_stats_label,
                        kind.keyword()
                    )
                }
            };
            (i, text)
        })
        .collect();
    // With nothing in scope the checklist is empty, which would read as a bug
    // rather than as "type one below".
    let note = form.vars.is_empty().then_some(s.report_node_vars_none);
    draw_simple_form_overlay(
        f,
        s.report_node_vars_title,
        s.report_node_vars_hint,
        &lines,
        form.selected,
        note,
        th,
        app,
    );
}

/// Draw the `REPORT "<template>" AS <name>` overlay
/// ([`Overlay::ReportNodeComputed`]): the template, the column name and the
/// statistics checklist.
fn draw_report_node_computed_overlay(
    f: &mut Frame,
    form: &super::report_nodes::ComputedForm,
    s: &Strings,
    th: &Theme,
    app: Option<&TuiApp>,
) {
    use super::report_nodes::ComputedRow;
    let lines: Vec<(usize, String)> = form
        .visible_rows()
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let text = match row {
                ComputedRow::Template => caretted(
                    format!(
                        "{}: {}",
                        s.report_node_computed_template_label, form.template
                    ),
                    i == form.selected,
                ),
                ComputedRow::Alias => caretted(
                    format!("{}: {}", s.report_node_computed_name_label, form.alias),
                    i == form.selected,
                ),
                ComputedRow::Stat(si) => {
                    let (kind, on) = form.stats[*si];
                    let mark = if on {
                        s.checkbox_checked
                    } else {
                        s.checkbox_unchecked
                    };
                    format!(
                        "  {mark} {}({})",
                        s.report_node_with_stats_label,
                        kind.keyword()
                    )
                }
            };
            (i, text)
        })
        .collect();
    draw_simple_form_overlay(
        f,
        s.report_node_computed_title,
        s.report_node_computed_hint,
        &lines,
        form.selected,
        None,
        th,
        app,
    );
}

/// Draw the `VARIABLE = VALUE` overlay ([`Overlay::ReportNodeAssign`]): two
/// free-text rows, so a `SET` line's value can be changed without dropping into
/// the raw line editor.
fn draw_report_node_assign_overlay(
    f: &mut Frame,
    form: &super::report_nodes::AssignForm,
    s: &Strings,
    th: &Theme,
    app: Option<&TuiApp>,
) {
    use super::report_nodes::AssignRow;
    let rows = form.visible_rows();
    let lines: Vec<(usize, String)> = rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let text = match row {
                AssignRow::Key => format!("{}: {}", s.node_form_var, form.key),
                AssignRow::Value => format!("{}: {}", s.node_form_value, form.value),
            };
            // Both rows are typed into.
            (i, caretted(text, i == form.selected))
        })
        .collect();
    draw_simple_form_overlay(
        f,
        s.node_assign_title,
        s.report_node_assign_hint,
        &lines,
        form.selected,
        None,
        th,
        app,
    );
}

/// Draw the `LIST NAME = [ … ]` overlay ([`Overlay::ReportNodeList`]): the list
/// name and one row per scalar element, plus an "add" row.
fn draw_report_node_list_overlay(
    f: &mut Frame,
    form: &super::report_nodes::ListForm,
    s: &Strings,
    th: &Theme,
    app: Option<&TuiApp>,
) {
    use super::report_nodes::ListRow;
    let rows = form.visible_rows();
    let lines: Vec<(usize, String)> = rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let sel = i == form.selected;
            let text = match row {
                ListRow::Name => caretted(format!("{}: {}", s.node_form_list_name, form.name), sel),
                ListRow::Value(vi) => caretted(format!("  {}", form.values[*vi]), sel),
                // The add row is a button, not a field.
                ListRow::Add => s.report_node_list_add.to_string(),
            };
            (i, text)
        })
        .collect();
    draw_simple_form_overlay(
        f,
        s.node_list_title,
        s.report_node_list_hint,
        &lines,
        form.selected,
        None,
        th,
        app,
    );
}

/// Append a cursor bar to `text` when the row is selected, marking it as a
/// field being typed into rather than one toggled with Space.
fn caretted(mut text: String, selected: bool) -> String {
    if selected {
        text.push('▏');
    }
    text
}

/// Draw a plain list-of-rows overlay: a centred, scrolling box with one text
/// line per row, the selected row highlighted, and the shortcut hint on the
/// bottom border. The `WITH`/`ENVS`/`FILES` overlays each style their rows
/// individually, but the simple text forms have no such needs and would
/// otherwise be three copies of this scaffolding.
///
/// `note` is an optional dim, unselectable line above the rows, for saying why
/// a list is empty. Callers append their own cursor bar to text-entry rows —
/// only they know which rows are typed into rather than ticked.
#[allow(clippy::too_many_arguments)]
fn draw_simple_form_overlay(
    f: &mut Frame,
    title: &str,
    hint: &str,
    lines: &[(usize, String)],
    selected: usize,
    note: Option<&str>,
    th: &Theme,
    app: Option<&TuiApp>,
) {
    // The note occupies a line but no row index, so the scroll window and the
    // box height must both count it while selection ignores it.
    let note_h = usize::from(note.is_some());
    let n = lines.len() + note_h;
    let box_w = f.area().width.saturating_sub(6).clamp(40, 90);
    let box_h = (n as u16 + 2).min(f.area().height.saturating_sub(2)).max(3);
    let area = centered_rect(box_w, box_h, f.area());
    f.render_widget(Clear, area);
    let inner_h = area.height.saturating_sub(2) as usize;
    let selected = selected.min(lines.len().saturating_sub(1));
    // Scroll is measured in drawn lines, so the selected row's line is its row
    // index plus the note above it.
    let sel_line = selected + note_h;
    let scroll = if sel_line >= inner_h {
        sel_line + 1 - inner_h
    } else {
        0
    };
    let mut all: Vec<Line> = Vec::with_capacity(n);
    if let Some(note) = note {
        all.push(Line::from(Span::styled(
            format!("  {note}"),
            Style::default().fg(th.dim),
        )));
    }
    all.extend(lines.iter().map(|(i, text)| {
        let style = if *i == selected {
            Style::default()
                .fg(th.bg)
                .bg(th.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(th.text)
        };
        Line::from(Span::styled(text.clone(), style))
    }));
    let rendered: Vec<Line> = all.into_iter().skip(scroll).take(inner_h).collect();
    let block = panel(title.to_string(), true, th).title_bottom(
        Line::from(Span::styled(
            format!(" {hint} "),
            Style::default().fg(th.dim),
        ))
        .centered(),
    );
    f.render_widget(Paragraph::new(rendered).block(block), area);
    if let Some(app) = app {
        app.set_mouse_layer(MouseLayer::Overlay);
        let inner = Rect {
            x: area.x.saturating_add(1),
            y: area.y.saturating_add(1),
            width: area.width.saturating_sub(2),
            height: area.height.saturating_sub(2),
        };
        for line in scroll..n.min(scroll + inner_h) {
            // The note line is not a row, so it gets no hit target.
            let Some(row) = line.checked_sub(note_h) else {
                continue;
            };
            app.push_mouse_hit(
                MouseLayer::Overlay,
                Rect::new(inner.x, inner.y + (line - scroll) as u16, inner.width, 1),
                MouseHitTarget::OverlayRow(row),
            );
        }
    }
    if n > inner_h {
        let bar_area = Rect {
            x: area.x + area.width - 1,
            y: area.y + 1,
            width: 1,
            height: inner_h as u16,
        };
        draw_scrollbar(f, bar_area, n, inner_h, scroll, th);
    }
}

/// Draw the `WITH` field overlay ([`Overlay::ReportNodeWithField`]): the column
/// name, the Hurl query behind it and the `STATISTICS(…)` checklist. Opened
/// from a `WITH` row of the request form and returns there on Enter or Esc.
fn draw_report_node_with_field_overlay(
    f: &mut Frame,
    form: &super::report_nodes::WithFieldForm,
    s: &Strings,
    th: &Theme,
    app: Option<&TuiApp>,
) {
    use super::report_nodes::WithFieldRow;
    let rows = form.visible_rows();
    let n = rows.len();
    let box_w = f.area().width.saturating_sub(6).clamp(40, 90);
    let box_h = (n as u16 + 2).min(f.area().height.saturating_sub(2)).max(3);
    let area = centered_rect(box_w, box_h, f.area());
    f.render_widget(Clear, area);
    let inner_h = area.height.saturating_sub(2) as usize;
    let selected = form.selected.min(n.saturating_sub(1));
    let scroll = if selected >= inner_h {
        selected + 1 - inner_h
    } else {
        0
    };
    let mut lines: Vec<Line> = Vec::new();
    for (i, row) in rows.iter().enumerate().skip(scroll).take(inner_h) {
        let is_sel = i == selected;
        let base = if is_sel {
            Style::default()
                .fg(th.bg)
                .bg(th.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(th.text)
        };
        let line = match *row {
            WithFieldRow::Name => {
                let mut text = format!("{}: {}", s.report_node_with_name_label, form.name);
                if is_sel {
                    text.push('▏');
                }
                Line::from(Span::styled(text, base))
            }
            WithFieldRow::Query => {
                // An empty query is legal — it reports the whole response — so
                // it is labelled rather than left as a blank field.
                let shown = if form.query.trim().is_empty() && !is_sel {
                    s.report_node_with_query_none.to_string()
                } else {
                    form.query.clone()
                };
                let mut text = format!("{}: {shown}", s.report_node_with_query_label);
                if is_sel {
                    text.push('▏');
                }
                Line::from(Span::styled(text, base))
            }
            WithFieldRow::Stat(si) => {
                let (kind, on) = form.stats[si];
                let mark = if on {
                    s.checkbox_checked
                } else {
                    s.checkbox_unchecked
                };
                let style = if is_sel {
                    base
                } else if on {
                    Style::default().fg(th.text)
                } else {
                    Style::default().fg(th.dim)
                };
                Line::from(Span::styled(
                    format!(
                        "  {mark} {}({})",
                        s.report_node_with_stats_label,
                        kind.keyword()
                    ),
                    style,
                ))
            }
        };
        lines.push(line);
    }
    let block = panel(s.report_node_with_title.to_string(), true, th).title_bottom(
        Line::from(Span::styled(
            format!(" {} ", s.report_node_with_hint),
            Style::default().fg(th.dim),
        ))
        .centered(),
    );
    f.render_widget(Paragraph::new(lines).block(block), area);
    if let Some(app) = app {
        app.set_mouse_layer(MouseLayer::Overlay);
        let inner = Rect {
            x: area.x.saturating_add(1),
            y: area.y.saturating_add(1),
            width: area.width.saturating_sub(2),
            height: area.height.saturating_sub(2),
        };
        for row in scroll..rows.len().min(scroll + inner_h) {
            app.push_mouse_hit(
                MouseLayer::Overlay,
                Rect::new(inner.x, inner.y + (row - scroll) as u16, inner.width, 1),
                MouseHitTarget::OverlayRow(row),
            );
        }
    }
    if n > inner_h {
        let bar_area = Rect {
            x: area.x + area.width - 1,
            y: area.y + 1,
            width: 1,
            height: inner_h as u16,
        };
        draw_scrollbar(f, bar_area, n, inner_h, scroll, th);
    }
}

/// Draw the ENVS-loop configure overlay ([`Overlay::ReportNodeEnvs`]): the loop
/// variable and Iterate/Compare mode on top, then one row per chosen
/// environment. In Compare mode each env row shows its `[Baseline]` /
/// `[Comparison]` role; env names are picked from the loaded environments.
fn draw_report_node_envs_overlay(
    f: &mut Frame,
    form: &super::report_nodes::EnvsForm,
    s: &Strings,
    th: &Theme,
    app: Option<&TuiApp>,
) {
    use super::report_nodes::EnvsRow;
    let rows = form.visible_rows();
    let n = rows.len();
    let box_w = f.area().width.saturating_sub(6).clamp(40, 90);
    let box_h = (n as u16 + 2).min(f.area().height.saturating_sub(2)).max(3);
    let area = centered_rect(box_w, box_h, f.area());
    f.render_widget(Clear, area);
    let inner_h = area.height.saturating_sub(2) as usize;
    let selected = form.selected.min(n.saturating_sub(1));
    let scroll = if selected >= inner_h {
        selected + 1 - inner_h
    } else {
        0
    };
    let mode_label = if form.compare {
        s.report_node_envs_mode_roles
    } else {
        s.report_node_envs_mode_plain
    };
    let mut lines: Vec<Line> = Vec::new();
    for (i, row) in rows.iter().enumerate().skip(scroll).take(inner_h) {
        let is_sel = i == selected;
        let base = if is_sel {
            Style::default()
                .fg(th.bg)
                .bg(th.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(th.text)
        };
        let line = match *row {
            EnvsRow::Var => {
                let mut text = format!("{}: {}", s.report_node_envs_var_label, form.var);
                if is_sel {
                    text.push('▏');
                }
                Line::from(Span::styled(text, base))
            }
            EnvsRow::Mode => Line::from(Span::styled(
                format!("{}: ‹{}›", s.report_node_envs_mode_label, mode_label),
                base,
            )),
            EnvsRow::Parallel => {
                let mark = if form.parallel {
                    s.checkbox_checked
                } else {
                    s.checkbox_unchecked
                };
                Line::from(Span::styled(
                    format!("{} {}", mark, s.report_node_parallel_label),
                    base,
                ))
            }
            EnvsRow::Degree => {
                Line::from(Span::styled(degree_row_text(&form.degree, s, is_sel), base))
            }
            EnvsRow::BaselineShow(fi) => {
                let row = &form.baseline_show[fi];
                let mark = if row.included {
                    s.checkbox_checked
                } else {
                    s.checkbox_unchecked
                };
                // Indented under the env rows and prefixed with the clause it
                // writes, so it reads as "SHOW, on the baseline" rather than as
                // another field list belonging to the loop as a whole.
                Line::from(Span::styled(
                    format!(
                        "  {} {} {}",
                        mark, s.report_node_baseline_show_label, row.name
                    ),
                    base,
                ))
            }
            EnvsRow::Env(ei) => {
                let entry = &form.entries[ei];
                let shown = if entry.name.is_empty() {
                    s.report_node_envs_none
                } else {
                    entry.name.as_str()
                };
                // A snapshot reference reads as `FILE(‹…›)` so it mirrors the
                // grammar it serializes to; a live env is just `‹name›`.
                let value = if entry.file {
                    format!("{}(‹{shown}›)", s.report_node_envs_file)
                } else {
                    format!("‹{shown}›")
                };
                let text = if form.compare {
                    let role = if entry.baseline {
                        s.report_node_envs_baseline
                    } else {
                        s.report_node_envs_comparison
                    };
                    format!("  [{role}] {value}")
                } else {
                    format!("  {value}")
                };
                Line::from(Span::styled(text, base))
            }
        };
        lines.push(line);
    }
    let block = panel(s.report_node_envs_title.to_string(), true, th).title_bottom(
        Line::from(Span::styled(
            format!(" {} ", s.report_node_envs_hint),
            Style::default().fg(th.dim),
        ))
        .centered(),
    );
    f.render_widget(Paragraph::new(lines).block(block), area);
    if let Some(app) = app {
        app.set_mouse_layer(MouseLayer::Overlay);
        let inner = Rect {
            x: area.x.saturating_add(1),
            y: area.y.saturating_add(1),
            width: area.width.saturating_sub(2),
            height: area.height.saturating_sub(2),
        };
        for row in scroll..rows.len().min(scroll + inner_h) {
            app.push_mouse_hit(
                MouseLayer::Overlay,
                Rect::new(inner.x, inner.y + (row - scroll) as u16, inner.width, 1),
                MouseHitTarget::OverlayRow(row),
            );
        }
    }
    if n > inner_h {
        let bar_area = Rect {
            x: area.x + area.width - 1,
            y: area.y + 1,
            width: 1,
            height: inner_h as u16,
        };
        draw_scrollbar(f, bar_area, n, inner_h, scroll, th);
    }
}

/// Draw the FILES-loop configure overlay ([`Overlay::ReportNodeFiles`]): the
/// loop variable, the source folder (opened via the file picker), an optional
/// `MATCH` glob and the `PARALLEL` toggle — the file analogue of the ENVS
/// overlay above.
fn draw_report_node_files_overlay(
    f: &mut Frame,
    form: &super::report_nodes::FilesForm,
    s: &Strings,
    th: &Theme,
    app: Option<&TuiApp>,
) {
    use super::report_nodes::FilesRow;
    let rows = form.visible_rows();
    let n = rows.len();
    let box_w = f.area().width.saturating_sub(6).clamp(40, 90);
    let box_h = (n as u16 + 2).min(f.area().height.saturating_sub(2)).max(3);
    let area = centered_rect(box_w, box_h, f.area());
    f.render_widget(Clear, area);
    let inner_h = area.height.saturating_sub(2) as usize;
    let selected = form.selected.min(n.saturating_sub(1));
    let scroll = if selected >= inner_h {
        selected + 1 - inner_h
    } else {
        0
    };
    let mut lines: Vec<Line> = Vec::new();
    for (i, row) in rows.iter().enumerate().skip(scroll).take(inner_h) {
        let is_sel = i == selected;
        let base = if is_sel {
            Style::default()
                .fg(th.bg)
                .bg(th.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(th.text)
        };
        let line = match *row {
            FilesRow::Var => {
                let mut text = format!("{}: {}", s.report_node_files_var_label, form.var);
                if is_sel {
                    text.push('▏');
                }
                Line::from(Span::styled(text, base))
            }
            FilesRow::Folder => {
                let shown = if form.dir.trim().is_empty() {
                    s.report_node_files_none
                } else {
                    form.dir.as_str()
                };
                Line::from(Span::styled(
                    format!("{}: ‹{shown}›", s.report_node_files_folder_label),
                    base,
                ))
            }
            FilesRow::Match => {
                let mut text = format!("{}: {}", s.report_node_files_match_label, form.glob);
                if is_sel {
                    text.push('▏');
                }
                Line::from(Span::styled(text, base))
            }
            FilesRow::Parallel => {
                let mark = if form.parallel {
                    s.checkbox_checked
                } else {
                    s.checkbox_unchecked
                };
                Line::from(Span::styled(
                    format!("{} {}", mark, s.report_node_parallel_label),
                    base,
                ))
            }
            FilesRow::Degree => {
                Line::from(Span::styled(degree_row_text(&form.degree, s, is_sel), base))
            }
        };
        lines.push(line);
    }
    // One form serves both producers, so the title says which one is open.
    let title = if form.folders {
        s.report_node_folders_title
    } else {
        s.report_node_files_title
    };
    let block = panel(title.to_string(), true, th).title_bottom(
        Line::from(Span::styled(
            format!(" {} ", s.report_node_files_hint),
            Style::default().fg(th.dim),
        ))
        .centered(),
    );
    f.render_widget(Paragraph::new(lines).block(block), area);
    if let Some(app) = app {
        app.set_mouse_layer(MouseLayer::Overlay);
        let inner = Rect {
            x: area.x.saturating_add(1),
            y: area.y.saturating_add(1),
            width: area.width.saturating_sub(2),
            height: area.height.saturating_sub(2),
        };
        for row in scroll..rows.len().min(scroll + inner_h) {
            app.push_mouse_hit(
                MouseLayer::Overlay,
                Rect::new(inner.x, inner.y + (row - scroll) as u16, inner.width, 1),
                MouseHitTarget::OverlayRow(row),
            );
        }
    }
    if n > inner_h {
        let bar_area = Rect {
            x: area.x + area.width - 1,
            y: area.y + 1,
            width: 1,
            height: inner_h as u16,
        };
        draw_scrollbar(f, bar_area, n, inner_h, scroll, th);
    }
}

fn draw_report_bind_overlay(
    f: &mut Frame,
    picker: &super::reports::ReportBindPicker,
    s: &Strings,
    th: &Theme,
    app: Option<&TuiApp>,
) {
    let title = format!("{}  ({})", s.report_bind_title, s.report_bind_hint);
    let n = picker.options.len();
    let box_w = f.area().width.saturating_sub(6).clamp(40, 90);
    let box_h = (n as u16 + 2).min(f.area().height.saturating_sub(2)).max(3);
    let area = centered_rect(box_w, box_h, f.area());
    f.render_widget(Clear, area);
    let inner_h = area.height.saturating_sub(2) as usize;
    let scroll = if picker.selected >= inner_h {
        picker.selected + 1 - inner_h
    } else {
        0
    };
    let mut lines: Vec<Line> = Vec::new();
    for (i, opt) in picker.options.iter().enumerate().skip(scroll).take(inner_h) {
        let base = if i == picker.selected {
            Style::default()
                .fg(th.bg)
                .bg(th.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(th.text)
        };
        let mut spans = vec![Span::styled(opt.name.clone(), base)];
        let detail = opt
            .path
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| s.report_bind_unsaved.to_string());
        spans.push(Span::styled(
            format!("  {detail}"),
            Style::default().fg(th.dim),
        ));
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines).block(panel(title, true, th)), area);
    if let Some(app) = app {
        app.set_mouse_layer(MouseLayer::Overlay);
        let inner = Rect {
            x: area.x.saturating_add(1),
            y: area.y.saturating_add(1),
            width: area.width.saturating_sub(2),
            height: area.height.saturating_sub(2),
        };
        for row in scroll..picker.options.len().min(scroll + inner_h) {
            app.push_mouse_hit(
                MouseLayer::Overlay,
                Rect::new(inner.x, inner.y + (row - scroll) as u16, inner.width, 1),
                MouseHitTarget::OverlayRow(row),
            );
        }
    }
    if n > inner_h {
        let bar_area = Rect {
            x: area.x + area.width - 1,
            y: area.y + 1,
            width: 1,
            height: inner_h as u16,
        };
        draw_scrollbar(f, bar_area, n, inner_h, scroll, th);
    }
}

/// Draw the node editor's insert / request-pick palette
/// ([`Overlay::ReportNodeMenu`]): a simple selectable list — node kinds when
/// adding, request titles when choosing a request name.
fn draw_report_node_menu_overlay(
    f: &mut Frame,
    menu: &super::report_nodes::NodeMenu,
    s: &Strings,
    th: &Theme,
    app: Option<&TuiApp>,
) {
    let hint = match menu.step {
        super::report_nodes::NodeMenuStep::PickKind => s.node_menu_hint,
        super::report_nodes::NodeMenuStep::PickRequest => s.node_pick_request_hint,
    };
    let title = format!("{}  ({})", menu.title(s), hint);
    let n = menu.options.len().max(1);
    let box_w = f.area().width.saturating_sub(6).clamp(40, 90);
    let box_h = (n as u16 + 2).min(f.area().height.saturating_sub(2)).max(3);
    let area = centered_rect(box_w, box_h, f.area());
    f.render_widget(Clear, area);
    let inner_h = area.height.saturating_sub(2) as usize;
    let scroll = if menu.selected >= inner_h {
        menu.selected + 1 - inner_h
    } else {
        0
    };
    let mut lines: Vec<Line> = Vec::new();
    if menu.options.is_empty() {
        lines.push(Line::from(Span::styled(
            s.node_pick_request_none.to_string(),
            Style::default().fg(th.dim),
        )));
    }
    for (i, opt) in menu.options.iter().enumerate().skip(scroll).take(inner_h) {
        let style = if i == menu.selected {
            Style::default()
                .fg(th.bg)
                .bg(th.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(th.text)
        };
        lines.push(Line::from(Span::styled(opt.clone(), style)));
    }
    f.render_widget(Paragraph::new(lines).block(panel(title, true, th)), area);
    if let Some(app) = app {
        app.set_mouse_layer(MouseLayer::Overlay);
        let inner = Rect {
            x: area.x.saturating_add(1),
            y: area.y.saturating_add(1),
            width: area.width.saturating_sub(2),
            height: area.height.saturating_sub(2),
        };
        for row in scroll..menu.options.len().min(scroll + inner_h) {
            app.push_mouse_hit(
                MouseLayer::Overlay,
                Rect::new(inner.x, inner.y + (row - scroll) as u16, inner.width, 1),
                MouseHitTarget::OverlayRow(row),
            );
        }
    }
    if n > inner_h {
        let bar_area = Rect {
            x: area.x + area.width - 1,
            y: area.y + 1,
            width: 1,
            height: inner_h as u16,
        };
        draw_scrollbar(f, bar_area, n, inner_h, scroll, th);
    }
}

pub(crate) fn draw(f: &mut Frame, app: &mut TuiApp) {
    app.begin_mouse_frame();
    let s = Strings::for_language(&app.language);
    let th = app.theme();

    app.refresh_json(app.active_tab);
    app.maybe_auto_open_workspace_picker();

    f.render_widget(Block::default().style(Style::default().bg(th.bg)), f.area());

    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .split(f.area());

    draw_menu(f, rows[0], app, &s, &th);
    draw_topbar(f, rows[1], app, &s, &th);
    draw_tabs(f, rows[2], app, &s, &th);
    draw_body(f, rows[3], app, &s, &th);
    draw_footer(
        f,
        rows[4],
        &s,
        &th,
        app.can_copy(),
        app.focus == Pane::Response,
    );

    // Painted after the panels themselves so it reflects this frame's
    // content (the Main/Response caches used to compute it are refreshed by
    // `draw_collection_main`/`draw_response` inside `draw_body` above).
    paint_selection_highlight(f, app, &th);

    if app.overlay.is_some() {
        draw_overlay(f, app, &s, &th);
    } else {
        app.prompt_editor_area = Rect::default();
    }
}

/// Paint every active text selection region — the active one
/// (`text_selection`) and any additional Alt+Click+Drag regions in
/// `extra_selections` — with a flat, explicit highlight colour (see
/// `Theme::select_bg`/`select_fg`) rather than `Modifier::REVERSED`, so it
/// reads as unambiguously the app's own highlight instead of looking like a
/// terminal's own native (usually plain reverse-video) selection. Each
/// region is confined to its own panel's cached text area/rows, so a
/// highlight can never bleed into a neighbouring panel or the rest of the
/// terminal row.
fn paint_selection_highlight(f: &mut Frame, app: &TuiApp, th: &Theme) {
    if !app.has_any_selection() {
        return;
    }
    let style = Style::default().bg(th.select_bg).fg(th.select_fg);
    // Each panel projects its own logical regions onto the current frame's
    // screen cells (bounded to its own text area), so a highlight can never
    // bleed into a neighbouring panel or the rest of the terminal row.
    let mut cells = app.main_panel.highlight_regions(app.main_text_area);
    cells.extend(app.resp_panel.highlight_regions(app.resp_text_area));
    // The full-screen report view has its own three panels (source /
    // validation / results); paint their selections the same way. Only the
    // active report tab's panels can be showing, and only the panes drawn this
    // frame have a non-default area, so this is safe for every tab kind.
    if let Some(rt) = app.active_report() {
        use crate::tui::reports::ReportPane;
        cells.extend(
            rt.source_panel
                .highlight_regions(app.report_pane_areas[ReportPane::Source.idx()]),
        );
        cells.extend(
            rt.validation_panel
                .highlight_regions(app.report_pane_areas[ReportPane::Validation.idx()]),
        );
        cells.extend(
            rt.results_panel
                .highlight_regions(app.report_pane_areas[ReportPane::Results.idx()]),
        );
    }
    let buf = f.buffer_mut();
    for (row, from, to) in cells {
        for col in from..to {
            if let Some(cell) = buf.cell_mut((col, row)) {
                cell.set_style(style);
            }
        }
    }
}

pub(crate) fn draw_menu(f: &mut Frame, area: Rect, app: &TuiApp, s: &Strings, th: &Theme) {
    let base = Style::default().fg(th.accent).add_modifier(Modifier::BOLD);
    let mut spans = vec![Span::raw(" ")];
    let file_w = Line::from(mnemonic_spans(s.file_menu_label, base)).width() as u16;
    spans.extend(mnemonic_spans(s.file_menu_label, base));
    spans.push(Span::raw("   "));
    let settings_w = Line::from(mnemonic_spans(s.options_menu_label, base)).width() as u16;
    spans.extend(mnemonic_spans(s.options_menu_label, base));
    if let Some(st) = &app.status {
        spans.push(Span::raw("     "));
        let c = if st.is_ok() { th.ok } else { th.err };
        spans.push(Span::styled(st.text(s), Style::default().fg(c)));
        // Advertise the copy shortcut so the (mouse-unselectable) status text
        // can be grabbed — especially useful for long parse-error messages.
        spans.push(Span::styled(
            format!("  ({} {})", s.status_copy_key, s.status_copy_hint),
            Style::default().fg(th.dim),
        ));
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(th.panel)),
        area,
    );
    let file_x = area.x.saturating_add(1);
    if file_x < area.right() {
        app.push_mouse_hit(
            MouseLayer::Base,
            Rect::new(file_x, area.y, file_w.min(area.right() - file_x), 1),
            MouseHitTarget::MenuFile,
        );
    }
    let settings_x = file_x.saturating_add(file_w).saturating_add(3);
    if settings_x < area.right() {
        app.push_mouse_hit(
            MouseLayer::Base,
            Rect::new(
                settings_x,
                area.y,
                settings_w.min(area.right() - settings_x),
                1,
            ),
            MouseHitTarget::MenuSettings,
        );
    }
}

pub(crate) fn draw_topbar(f: &mut Frame, area: Rect, app: &TuiApp, s: &Strings, th: &Theme) {
    let lang = match app.language {
        Language::English => s.lang_english,
        Language::French => s.lang_french,
        Language::Danish => s.lang_danish,
    };
    let mut spans = vec![
        Span::styled(
            s.app_heading,
            Style::default().fg(th.accent).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(format!("[{lang}]"), Style::default().fg(th.dim)),
    ];
    // Surface the last runner error (transport failure / failed assert / parse
    // error) here on the status bar so it is never silently swallowed.
    let error = { app.response.lock().unwrap().error.clone() };
    if !error.is_empty() {
        spans.push(Span::styled(
            format!("   {} {error}", s.req_error_prefix),
            Style::default().fg(th.err).add_modifier(Modifier::BOLD),
        ));
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)).block(panel(String::new(), false, th)),
        area,
    );
}

/// Icon prefix shown before a tab's name (in both the tab bar and the
/// Requests-list panel title): the ⎇ git-branch icon if the tab (or, for a
/// Workspace, the folder itself) was loaded from git, the folder icon if
/// it's bound to a Workspace — a git-downloaded Workspace tab shows both,
/// in that order.
fn tab_icons(col: &crate::collection::Collection) -> String {
    let mut icons = String::new();
    if col.git_origin.is_some() || col.workspace_downloaded_from_git {
        icons.push_str(GIT_ICON);
        icons.push(' ');
    }
    if col.workspace_root.is_some() {
        icons.push_str(FOLDER_ICON);
        icons.push(' ');
    }
    icons
}

pub(crate) fn draw_tabs(f: &mut Frame, area: Rect, app: &TuiApp, s: &Strings, th: &Theme) {
    // A *standalone* report strip tab always keeps focus on its body (the tab
    // bar is never a focus stop), so its tab-bar highlight is never lit. An
    // embedded report rides a collection tab, whose Tabs focus works normally.
    let focused = !app.active_is_strip_report() && app.focus == Pane::Tabs;
    let mk = |label: String, active: bool| -> Span {
        if active {
            Span::styled(
                format!(" {label} "),
                Style::default()
                    .bg(th.accent)
                    .fg(th.bg)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(format!(" {label} "), Style::default().fg(th.dim))
        }
    };
    // Build every tab span up front, tracking the running character offset so
    // the active tab's position within the full strip is known — that lets the
    // bar scroll horizontally to keep the active tab visible when the tabs
    // overflow the available width (otherwise later tabs are unreachable).
    let mut spans: Vec<Span> = Vec::new();
    let mut tab_hits: Vec<(usize, usize, usize)> = Vec::new();
    let mut pos = 0usize;
    let mut active_start = 0usize;
    let mut active_w = 0usize;
    for (i, col) in app.collections.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("│"));
            pos += 1;
        }
        // The tab's own name is persistent (renameable with F2) and never
        // changes when the user picks a different collection within a
        // Workspace — only the Requests-list panel title below reflects
        // that (see `draw_collection_left`).
        let name = if i == 0 {
            s.tab_request.to_string()
        } else {
            format!("{}{}", tab_icons(col), col.name)
        };
        let w = name.chars().count() + 2; // " {name} "
        if app.active_tab == i {
            active_start = pos;
            active_w = w;
        }
        tab_hits.push((i, pos, w));
        spans.push(mk(name, app.active_tab == i));
        pos += w;
    }
    // Standalone report tabs follow the collection tabs in the same strip
    // (unified index: `collections.len() + strip_slot`). Workspace-embedded
    // reports aren't in the strip (they ride inside their Workspace collection
    // tab), so they're skipped here. A leading icon distinguishes report tabs,
    // and a dirty marker flags unsaved source edits.
    let report_base = app.collections.len();
    for (slot, ri) in app.standalone_report_indices().into_iter().enumerate() {
        let rt = &app.reports[ri];
        spans.push(Span::raw("│"));
        pos += 1;
        // Unsaved edits get a trailing dot (with a leading space so it never
        // crowds the name); the report icon leads — the same 📊 icon the
        // Workspace tree uses for report files (`REPORT_ICON`), so a report
        // reads the same whether it's a standalone tab or a workspace row.
        let marker = if rt.report.dirty {
            format!(" {}", s.report_dirty_marker)
        } else {
            String::new()
        };
        let name = format!("{REPORT_ICON} {}{}", rt.report.name, marker);
        let idx = report_base + slot;
        let w = name.chars().count() + 2;
        if app.active_tab == idx {
            active_start = pos;
            active_w = w;
        }
        tab_hits.push((idx, pos, w));
        spans.push(mk(name, app.active_tab == idx));
        pos += w;
    }
    let total_w = pos;
    // Content width available inside the panel borders.
    let avail = area.width.saturating_sub(2) as usize;

    let (line, hit_scroll, hit_content_x, hit_content_w) = if total_w <= avail || avail == 0 {
        (Line::from(spans), 0usize, area.x.saturating_add(1), avail)
    } else {
        // Scroll so the active tab is fully visible. Reserve up to two columns
        // for the ‹ / › overflow markers when deciding the target window, so
        // the active tab never hides behind a marker.
        let target_w = avail.saturating_sub(2).max(1);
        let mut scroll = 0usize;
        if active_start + active_w > scroll + target_w {
            scroll = (active_start + active_w).saturating_sub(target_w);
        }
        if active_start < scroll {
            scroll = active_start;
        }
        // Mirror the collection-list URL scroll: a ‹ marks hidden tabs to the
        // left, a › hidden tabs to the right, each costing one column.
        let show_left = scroll > 0;
        let content_w_before_right = avail.saturating_sub(show_left as usize);
        let remaining = total_w.saturating_sub(scroll);
        let show_right = remaining > content_w_before_right;
        let content_w = content_w_before_right.saturating_sub(show_right as usize);
        let mut out: Vec<Span> = Vec::new();
        if show_left {
            out.push(Span::styled("\u{2039}", Style::default().fg(th.accent)));
        }
        out.extend(take_display(skip_display(spans, scroll), content_w));
        if show_right {
            out.push(Span::styled("\u{203a}", Style::default().fg(th.accent)));
        }
        let content_x = area.x + 1 + u16::from(show_left);
        (Line::from(out), scroll, content_x, content_w)
    };
    f.render_widget(
        Paragraph::new(line).block(panel(s.tabs_heading.to_string(), focused, th)),
        area,
    );
    let hit_y = area.y.saturating_add(1);
    for (idx, start, w) in tab_hits {
        let end = start + w;
        let win_start = hit_scroll;
        let win_end = hit_scroll + hit_content_w;
        let from = start.max(win_start);
        let to = end.min(win_end);
        if from < to {
            app.push_mouse_hit(
                MouseLayer::Base,
                Rect::new(
                    hit_content_x + (from - win_start) as u16,
                    hit_y,
                    (to - from) as u16,
                    1,
                ),
                MouseHitTarget::Tab(idx),
            );
        }
    }
}

pub(crate) fn draw_body(f: &mut Frame, area: Rect, app: &mut TuiApp, s: &Strings, th: &Theme) {
    // A *standalone* report strip tab takes the whole body (no
    // list/environment/response panels) — branch before the collection-tab
    // layout below, which indexes `app.collections[app.active_tab]` and would
    // panic on a report's unified tab index.
    if app.active_is_strip_report() {
        super::reports::draw_report_body(f, area, app, s, th);
        return;
    }
    // Otherwise it's a collection tab: always draw its left column (the request
    // list / Workspace file-tree). For a Workspace tab showing an *embedded*
    // report, the right column is the report body; otherwise it's the usual
    // request editor + response split.
    let cols =
        Layout::horizontal([Constraint::Length(app.list_width), Constraint::Min(10)]).split(area);
    let ci = app.active_tab;
    draw_collection_left(f, cols[0], app, ci, s, th);
    if let Some(idx) = app.active_report_index() {
        super::reports::draw_report_content(f, cols[1], app, idx, s, th);
        return;
    }
    let right = Layout::vertical([Constraint::Min(4), Constraint::Percentage(app.response_pct)])
        .split(cols[1]);
    draw_collection_main(f, right[0], app, ci, s, th);
    draw_response(f, right[1], app, ci, s, th);
}

pub(crate) fn draw_collection_left(
    f: &mut Frame,
    area: Rect,
    app: &TuiApp,
    ci: usize,
    s: &Strings,
    th: &Theme,
) {
    // Use the same split percentage as the right column so the divider between
    // the list and Environment panels lines up with the Main/Response divider
    // (and stays aligned when the response pane is resized with +/-).
    let panes = Layout::vertical([Constraint::Min(3), Constraint::Percentage(app.response_pct)])
        .split(area);

    // Entry list. For an ordinary tab this is the title-folder tree
    // (`col.rows()`); for a Workspace tab it's the real filesystem file-tree
    // with the open collection's requests inlined (`col.ws_rows()`). Both are
    // unified into `LeftRow` so a single loop lays them out — either way
    // deeply nested content stays a flat, scannable list.
    let focused = app.focus == Pane::List;
    let col = &app.collections[ci];
    let view_rows = LeftRow::build(col);
    let sel = col.list_cursor.min(view_rows.len().saturating_sub(1));
    // Classify every `{{ VAR }}` the requests reference so the list URLs can be
    // substituted and colour-coded by whether their value is loaded.
    let env = app.effective_env(ci);
    let smap = crate::request::subst_map(col, env.as_ref());
    // Columns available for the URL text (after the border, user-added marker
    // and the fixed method column). The selected row is shown highlighted
    // rather than with a leftmost caret, so no column is reserved for one.
    // Recorded so h-scrolling can be clamped to stop once the URL's end is
    // visible (no blank overscroll).
    let url_w = panes[0].width.saturating_sub(2 + 2 + 5);
    app.list_scroll_w.set(url_w);
    // Scroll is measured against the SUBSTITUTED display length (what's shown).
    // Folder/Up/collection rows have no scrollable URL text. A row that shows a
    // request's name (title) instead of its URL isn't horizontally scrolled
    // (names are short display labels), so its scroll length is zero.
    let sel_len = view_rows
        .get(sel)
        .and_then(LeftRow::entry_idx)
        .and_then(|idx| col.entries.get(idx))
        .map(|e| {
            if crate::tree::entry_path(&e.title)
                .pop()
                .unwrap_or_default()
                .is_empty()
            {
                crate::request::subst_display(&e.url, &smap).chars().count()
            } else {
                0
            }
        })
        .unwrap_or(0);
    let max_scroll = sel_len.saturating_sub((url_w as usize).saturating_sub(1));
    let hscroll = (app.list_hscroll as usize).min(max_scroll);
    let items: Vec<ListItem> = view_rows
        .iter()
        .enumerate()
        .map(|(i, row)| match row {
            LeftRow::Up => ListItem::new(Line::from(Span::styled(
                s.list_up_row.to_string(),
                Style::default().fg(th.dim),
            ))),
            // Non-workspace virtual folder (title-encoded); no indentation.
            LeftRow::Folder(name) => ListItem::new(Line::from(Span::styled(
                format!("{FOLDER_ICON} {name}/"),
                Style::default().fg(th.accent).add_modifier(Modifier::BOLD),
            ))),
            // Workspace filesystem folder with expand/collapse chevron and
            // depth-based indentation.
            LeftRow::WsFolder {
                name,
                depth,
                expanded,
            } => {
                let indent = "  ".repeat(*depth);
                let chevron = if *expanded {
                    COLLECTION_OPEN_ICON
                } else {
                    COLLECTION_CLOSED_ICON
                };
                ListItem::new(Line::from(Span::styled(
                    format!("{indent}{chevron} {name}/"),
                    Style::default().fg(th.accent).add_modifier(Modifier::BOLD),
                )))
            }
            LeftRow::Collection {
                name,
                depth,
                open,
                loaded,
            } => {
                let indent = "  ".repeat(*depth);
                let chevron = if *open {
                    COLLECTION_OPEN_ICON
                } else {
                    COLLECTION_CLOSED_ICON
                };
                // The loaded collection (the one with coloured requests) is
                // drawn in the accent colour so it clearly reads as the one in
                // focus; every other collection recedes to dim, matching its
                // dim request names.
                let colour = if *loaded { th.accent } else { th.dim };
                ListItem::new(Line::from(Span::styled(
                    format!("{indent}{chevron} {name}"),
                    Style::default().fg(colour).add_modifier(Modifier::BOLD),
                )))
            }
            LeftRow::Report { name, depth } => {
                let indent = "  ".repeat(*depth);
                ListItem::new(Line::from(Span::styled(
                    format!("{indent}{REPORT_ICON} {name}"),
                    Style::default().fg(th.accent),
                )))
            }
            LeftRow::Environment { name, depth } => {
                let indent = "  ".repeat(*depth);
                ListItem::new(Line::from(Span::styled(
                    format!("{indent}{ENV_ICON} {name}"),
                    Style::default().fg(th.accent),
                )))
            }
            // A request of an expanded but not-loaded collection: dim, name only
            // (its collection isn't loaded, so there's no method/status to show).
            // The two-space pad lines the name up under the loaded rows' names.
            LeftRow::WsRequestName { name, depth } => {
                let indent = "  ".repeat(*depth);
                ListItem::new(Line::from(Span::styled(
                    format!("{indent}  {name}"),
                    Style::default().fg(th.dim),
                )))
            }
            LeftRow::Entry { idx, depth } => {
                let e = &col.entries[*idx];
                // A plus marks a request the user added by hand (in a real
                // collection); a pencil marks one edited away from its loaded
                // state — matching the environment-panel convention.
                let (marker, marker_fg) = if e.user_added {
                    ("\u{271a} ", th.ok)
                } else if e.modified {
                    ("\u{270e} ", th.accent)
                } else {
                    ("  ", th.text)
                };
                // Workspace request rows carry a `depth` that reflects their
                // position in the tree (collection depth + 1); non-workspace
                // rows are always depth 0.  Two spaces per level matches the
                // folder/collection indent above.
                let mut spans = Vec::new();
                if *depth > 0 {
                    spans.push(Span::raw("  ".repeat(*depth)));
                }
                spans.push(Span::styled(marker, Style::default().fg(marker_fg)));
                spans.push(Span::styled(
                    format!("{:<5}", e.method),
                    Style::default()
                        .fg(method_color(&e.method))
                        .add_modifier(Modifier::BOLD),
                ));
                // Pass/fail from the most recent "Run All" (Alt+F5); a
                // dotted marker while a run is still in progress; blank
                // until a batch run has actually covered this entry.
                spans.push(match e.last_run {
                    RunStatus::Passed => Span::styled("\u{2713} ", Style::default().fg(th.ok)),
                    RunStatus::Failed => Span::styled("\u{2717} ", Style::default().fg(th.err)),
                    RunStatus::Running => {
                        Span::styled("\u{2026} ", Style::default().fg(th.pending))
                    }
                    RunStatus::NotRun => Span::raw("  "),
                });
                // Show the request's name when it has one; otherwise fall back
                // to the URL. A title encodes a folder path (`Auth/Login`), and
                // those folders are already rows in the tree, so only the leaf
                // segment (the request's own name within its folder) is shown
                // here — never the redundant folder prefix.
                let name = crate::tree::entry_path(&e.title).pop().unwrap_or_default();
                if !name.is_empty() {
                    spans.push(Span::styled(name, Style::default().fg(th.text)));
                    ListItem::new(Line::from(spans))
                } else {
                    // The URL, with `{{ VAR }}` substituted + colour-coded by status.
                    let mut seen = SubstSeen::default();
                    let url_spans = highlight_spans(&e.url, &smap, th, &mut seen, None, None);
                    // Horizontally scroll only the selected row so its full (possibly
                    // long) URL can be read with ← / →; other rows show from the start.
                    // Arrow hints appear on whichever side still has hidden text so
                    // it's clear more can be scrolled into view in that direction.
                    if i == sel {
                        let avail = url_w as usize;
                        let show_left = hscroll > 0;
                        let content_w_before_right = avail.saturating_sub(show_left as usize);
                        let remaining = sel_len.saturating_sub(hscroll);
                        let show_right = remaining > content_w_before_right;
                        let content_w = content_w_before_right.saturating_sub(show_right as usize);
                        if show_left {
                            spans.push(Span::styled("\u{2039}", Style::default().fg(th.dim))); // ‹ = scrolled
                        }
                        spans.extend(take_display(skip_display(url_spans, hscroll), content_w));
                        if show_right {
                            spans.push(Span::styled("\u{203a}", Style::default().fg(th.dim)));
                            // › = more to the right
                        }
                    } else {
                        spans.extend(url_spans);
                    }
                    ListItem::new(Line::from(spans))
                }
            }
        })
        .collect();
    // A Workspace-bound tab with no collection chosen yet would otherwise
    // just show a blank list — a friendly hint (and `w`'s reminder) is much
    // less confusing than empty panels with no obvious next step.
    let items: Vec<ListItem> =
        if items.is_empty() && col.workspace_root.is_some() && col.path.is_none() {
            vec![ListItem::new(Line::styled(
                s.workspace_empty_state.to_string(),
                Style::default().fg(th.dim),
            ))]
        } else {
            items
        };
    let mut title = if ci == 0 {
        s.tab_request.to_string()
    } else {
        // Unlike the tab bar (which always shows the tab's own, renameable
        // name), a Workspace-bound tab shows whichever collection file is
        // currently loaded here — this is the one name that's expected to
        // change every time the user picks a different collection within
        // the Workspace, without touching the tab's own name.
        let display_name = if col.workspace_root.is_some() {
            col.path
                .as_deref()
                .map(|p| collection_name_from_path(&p.to_string_lossy(), &col.name))
                .unwrap_or_else(|| col.name.clone())
        } else {
            col.name.clone()
        };
        format!("{}{}", tab_icons(col), display_name)
    };
    // Non-workspace tabs show the current in-collection folder path as a
    // breadcrumb (the title-encoded virtual folder from `col.folder`).
    // Workspace tabs use a real expand/collapse tree — there is no single
    // "current folder" to display, so the breadcrumb is omitted there.
    if !col.is_workspace() && !col.folder.is_empty() {
        title = format!("{title} › {}", col.folder.join(" › "));
    }
    // A collection linked to a Global Environment shows that environment's
    // name (green, joined by a link icon) in this panel's title bar, so
    // it's visible at a glance which environment its requests will
    // substitute from — the trailing "(v)" hints at the key that opens its
    // full entries popup (works from any pane).
    let mut title_spans = vec![Span::styled(
        title.clone(),
        Style::default().fg(th.text).add_modifier(Modifier::BOLD),
    )];
    let mut title_len = title.chars().count();
    if let Some(env) = col
        .linked_env_id
        .and_then(|id| app.global_envs.iter().find(|e| e.id == id))
    {
        let link_part = format!(" {LINK_ICON} ");
        let env_suffix = " (v)";
        title_len +=
            link_part.chars().count() + env.name.chars().count() + env_suffix.chars().count();
        title_spans.push(Span::styled(link_part, Style::default().fg(th.dim)));
        title_spans.push(Span::styled(
            env.name.clone(),
            Style::default().fg(th.ok).add_modifier(Modifier::BOLD),
        ));
        title_spans.push(Span::styled(env_suffix, Style::default().fg(th.dim)));
    }
    // A brief "w to browse" reminder right in the title bar, next to the
    // folder icon — new users are much more likely to notice it here than
    // in the busier bottom-border hint line below. Only shown when it
    // actually fits without the title overflowing the panel.
    if col.workspace_root.is_some() {
        let workspace_title_hint = format!(" · w {}", s.foot_workspace);
        if title_len + workspace_title_hint.chars().count() < panes[0].width as usize {
            title_spans.push(Span::styled(
                workspace_title_hint,
                Style::default().fg(th.dim),
            ));
        }
    }
    // Run/Run All hints live on this panel's bottom border (rather than the
    // global footer, which was getting overcrowded) since they act on
    // whichever collection is shown here regardless of which pane has focus.
    let run_key = if app.enhanced_keys { "^Enter/F5" } else { "F5" };
    let run_primary_hint = format!("{run_key} {}", s.foot_run);
    let mut run_hint = format!("{run_primary_hint} \u{00b7} Alt+F5 {}", s.foot_run_all);
    // Append the "p" link/unlink-environment hint too, but only when the
    // panel is wide enough to actually show it without the bottom border
    // text overflowing/wrapping onto the panel itself.
    let link_hint = format!(" · p {}", s.foot_env_link);
    if (run_hint.chars().count() + link_hint.chars().count()) < panes[0].width as usize {
        run_hint.push_str(&link_hint);
    }
    // Same treatment for the Workspace-browse hint, shown only on tabs
    // actually bound to a Workspace folder.
    if col.workspace_root.is_some() {
        let workspace_hint = format!(" · w {}", s.foot_workspace);
        if (run_hint.chars().count() + workspace_hint.chars().count()) < panes[0].width as usize {
            run_hint.push_str(&workspace_hint);
        }
    }
    let border = if focused { th.accent } else { th.dim };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border))
        .title(Line::from(title_spans))
        .style(Style::default().bg(th.panel))
        .title_bottom(Line::styled(run_hint, Style::default().fg(th.dim)));
    let list = List::new(items).block(block).highlight_style(
        Style::default()
            .bg(th.accent)
            .fg(th.bg)
            .add_modifier(Modifier::BOLD),
    );
    let mut st = ListState::default();
    if !view_rows.is_empty() {
        st.select(Some(sel));
    }
    f.render_stateful_widget(list, panes[0], &mut st);
    let list_inner = Rect {
        x: panes[0].x.saturating_add(1),
        y: panes[0].y.saturating_add(1),
        width: panes[0].width.saturating_sub(2),
        height: panes[0].height.saturating_sub(2),
    };
    app.push_mouse_hit(
        MouseLayer::Base,
        list_inner,
        MouseHitTarget::FocusPane(Pane::List),
    );
    app.push_mouse_hit(
        MouseLayer::Base,
        list_inner,
        MouseHitTarget::Scroll(MouseScrollTarget::List),
    );
    let first = st.offset();
    let visible = list_inner.height as usize;
    for row in first..view_rows.len().min(first + visible) {
        app.push_mouse_hit(
            MouseLayer::Base,
            Rect::new(
                list_inner.x,
                list_inner.y + (row - first) as u16,
                list_inner.width,
                1,
            ),
            MouseHitTarget::SelectListRow(row),
        );
    }
    let run_hit_w = (Line::from(run_primary_hint).width().min(u16::MAX as usize) as u16)
        .min(panes[0].width.saturating_sub(2));
    if run_hit_w > 0 {
        app.push_mouse_hit(
            MouseLayer::Base,
            Rect::new(
                panes[0].x.saturating_add(1),
                panes[0].y + panes[0].height.saturating_sub(1),
                run_hit_w,
                1,
            ),
            MouseHitTarget::RunRequest,
        );
    }

    // Environment panel
    draw_env_panel(f, panes[1], app, s, th);
}

pub(crate) fn draw_env_panel(f: &mut Frame, area: Rect, app: &TuiApp, s: &Strings, th: &Theme) {
    let focused = app.focus == Pane::GlobalEnv;
    // The activate/deactivate hint lives on this panel's bottom border (same
    // convention as the Requests list's Run/Run All hint) since it acts on
    // whichever row is selected here, regardless of which pane has focus.
    let activate_hint = format!("a {}  / {}", s.foot_env_activate, s.foot_env_filter);
    let source = app.effective_env_source();
    let source_label = match source {
        crate::env_panel::EnvSource::Both => s.env_source_both,
        crate::env_panel::EnvSource::Global => s.env_source_global,
        crate::env_panel::EnvSource::Workspace => s.env_source_workspace,
    };
    let title = if app.has_workspace_env_source() {
        format!("{} · {}", s.env_heading, source_label)
    } else {
        s.env_heading.to_string()
    };
    let block = panel(title, focused, th)
        .title_bottom(Line::styled(activate_hint, Style::default().fg(th.dim)));
    let rows = app.env_rows();
    if rows.is_empty() {
        let all_rows =
            crate::env_panel::rows(&app.global_envs, &app.workspace_env_files(), "", source);
        let any_rows = crate::env_panel::rows(
            &app.global_envs,
            &app.workspace_env_files(),
            "",
            crate::env_panel::EnvSource::Both,
        );
        // There are now three empty states with different fixes: load or create
        // any environment, switch the source toggle, or clear/narrow the text
        // filter. Say which one applies rather than sending the user to the
        // wrong control.
        let (first, hint) = if any_rows.is_empty() {
            (
                s.env_no_envs.to_string(),
                format!("{} \u{2192} {}", s.file_menu, s.load_environment),
            )
        } else if all_rows.is_empty() {
            (
                format!("{}{}", s.env_source_label, source_label),
                s.env_source_no_matches.to_string(),
            )
        } else {
            (
                format!("{}{}", s.env_filter_label, app.env_query),
                s.env_filter_no_matches.to_string(),
            )
        };
        let p = Paragraph::new(vec![
            Line::styled(first, Style::default().fg(th.dim)),
            Line::styled(hint, Style::default().fg(th.dim)),
        ])
        .block(block)
        .wrap(Wrap { trim: false });
        f.render_widget(p, area);
        let inner = Rect {
            x: area.x.saturating_add(1),
            y: area.y.saturating_add(1),
            width: area.width.saturating_sub(2),
            height: area.height.saturating_sub(2),
        };
        app.push_mouse_hit(
            MouseLayer::Base,
            inner,
            MouseHitTarget::FocusPane(Pane::GlobalEnv),
        );
        return;
    }
    // An active filter takes a one-line strip below the list, so it's obvious
    // the list is being narrowed (and by what) rather than mysteriously short —
    // the same treatment the file browser's and Help's filters get.
    let show_source_strip =
        app.has_workspace_env_source() && source != crate::env_panel::EnvSource::Both;
    let (list_area, filter_area) =
        if app.env_query.is_empty() && !app.env_filter_typing && !show_source_strip {
            (area, None)
        } else {
            let parts = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).split(area);
            (parts[0], Some(parts[1]))
        };
    let sel = app.global_env_idx.min(rows.len().saturating_sub(1));
    // Columns available for the name text (after the border, the leftmost
    // pencil column and the active-marker column). The selected row is shown
    // highlighted rather than with a leftmost caret, so no column is reserved
    // for one. Used to clamp scrolling.
    let text_w = list_area.width.saturating_sub(2 + 2 + 2);
    app.global_env_scroll_w.set(text_w);
    let sel_len = rows.get(sel).map(|r| r.name.chars().count()).unwrap_or(0);
    let max_scroll = sel_len.saturating_sub((text_w as usize).saturating_sub(1));
    let hscroll = (app.global_env_hscroll as usize).min(max_scroll);
    let items: Vec<ListItem> = rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let env = row
                .env_id()
                .and_then(|id| app.global_envs.iter().find(|e| e.id == id));
            let is_active = row.env_id().is_some() && app.active_env_id == row.env_id();
            // Active: green name + a checkmark marker; git origin shows the
            // same ⎇ icon convention used elsewhere.
            let (marker, marker_fg) = if is_active {
                ("\u{2713} ", th.ok)
            } else {
                ("  ", th.dim)
            };
            // A workspace environment that hasn't been opened yet is dimmed:
            // it is a file the panel is offering, not something loaded that
            // could be activated or edited until Enter opens it.
            let name_color = if is_active {
                th.ok
            } else if env.is_none() {
                th.dim
            } else {
                th.text
            };
            let selected = i == sel;
            let mark_fg = if selected { th.bg } else { marker_fg };
            // A pencil in the leftmost column flags an environment with unsaved
            // (added or modified) variables — placed left of the name so it
            // matches the Requests list's modified/added marker convention
            // (and stays put rather than trailing a scrolling name).
            let dirty = env.is_some_and(|e| app.changed_env_count(e.id) > 0);
            let (pencil, pencil_fg) = if dirty {
                ("\u{270e} ", th.accent)
            } else {
                ("  ", th.dim)
            };
            let pencil_fg = if selected { th.bg } else { pencil_fg };
            // The two sources this panel merges are told apart by a leading
            // icon: ⌂ for "from the open workspace folder", ⎇ for "from a git
            // remote". A plain loaded environment gets neither.
            let prefix = if row.workspace {
                format!("{WORKSPACE_ICON} ")
            } else if env.is_some_and(|e| e.git_origin.is_some()) {
                format!("{GIT_ICON} ")
            } else {
                String::new()
            };
            let mut spans = vec![
                Span::styled(pencil, Style::default().fg(pencil_fg)),
                Span::styled(marker, Style::default().fg(mark_fg)),
            ];
            let full = format!("{prefix}{}", row.name);
            if selected {
                // Same truncate-with-arrow-hints convention as the Requests
                // list / entries popup: scroll the whole name so a very long
                // one can still be read end-to-end.
                let avail = text_w as usize;
                let show_left = hscroll > 0;
                let content_w_before_right = avail.saturating_sub(show_left as usize);
                let remaining = sel_len.saturating_sub(hscroll);
                let show_right = remaining > content_w_before_right;
                let content_w = content_w_before_right.saturating_sub(show_right as usize);
                let visible: String = full.chars().skip(hscroll).take(content_w).collect();
                let mut text = String::new();
                if show_left {
                    text.push('\u{2039}');
                }
                text.push_str(&visible);
                if show_right {
                    text.push('\u{203a}');
                }
                let fg = if selected { th.bg } else { name_color };
                spans.push(Span::styled(text, Style::default().fg(fg)));
            } else {
                spans.push(Span::styled(full, Style::default().fg(name_color)));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().bg(th.accent).add_modifier(Modifier::BOLD));
    let mut st = ListState::default();
    st.select(Some(sel));
    f.render_stateful_widget(list, list_area, &mut st);
    if let Some(strip) = filter_area {
        // While typing, a block cursor marks where the next character lands —
        // the strip is the only thing with focus, so without it there is no
        // sign the keyboard is being captured.
        let mut spans = vec![
            Span::styled(s.env_source_label, Style::default().fg(th.dim)),
            Span::styled(source_label, Style::default().fg(th.accent)),
            Span::raw("  "),
            Span::styled(s.env_filter_label, Style::default().fg(th.dim)),
            Span::styled(
                app.env_query.clone(),
                Style::default().fg(th.accent).add_modifier(Modifier::BOLD),
            ),
        ];
        if app.env_filter_typing {
            spans.push(Span::styled(
                "\u{2588}",
                Style::default()
                    .fg(th.accent)
                    .add_modifier(Modifier::SLOW_BLINK),
            ));
        }
        f.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::default().bg(th.panel)),
            strip,
        );
    }
    let inner = Rect {
        x: list_area.x.saturating_add(1),
        y: list_area.y.saturating_add(1),
        width: list_area.width.saturating_sub(2),
        height: list_area.height.saturating_sub(2),
    };
    app.push_mouse_hit(
        MouseLayer::Base,
        inner,
        MouseHitTarget::FocusPane(Pane::GlobalEnv),
    );
    app.push_mouse_hit(
        MouseLayer::Base,
        inner,
        MouseHitTarget::Scroll(MouseScrollTarget::GlobalEnv),
    );
    let first = st.offset();
    let visible = inner.height as usize;
    for row in first..rows.len().min(first + visible) {
        app.push_mouse_hit(
            MouseLayer::Base,
            Rect::new(inner.x, inner.y + (row - first) as u16, inner.width, 1),
            MouseHitTarget::SelectGlobalEnvRow(row),
        );
    }
}

/// The popup listing one [`crate::environment::Environment`]'s vars (opened
/// via Enter on a Global Environments list row, or 'v' on a linked
/// collection's Tabs entry) — same rendering as the old inline Environment
/// panel: a status dot per variable (orange=loading, cyan=literal,
/// green=loaded, red=failed), a marker column (➕ user-added, ✎ modified),
/// and horizontal-scrolling of the selected row's `key = value` text with
/// ‹/› arrow hints when there's more text in that direction.
pub(crate) fn draw_env_popup(
    f: &mut Frame,
    app: &TuiApp,
    popup: &EnvPopupState,
    s: &Strings,
    th: &Theme,
) {
    let Some(env) = app.global_envs.iter().find(|e| e.id == popup.env_id) else {
        return;
    };
    let area = centered_rect(78, 20.min(f.area().height.saturating_sub(2)), f.area());
    f.render_widget(Clear, area);
    let title = if env.git_origin.is_some() {
        format!("{} — {GIT_ICON} {}", s.env_heading, env.name)
    } else {
        format!("{} — {}", s.env_heading, env.name)
    };
    if env.vars.is_empty() {
        let block = panel(title, true, th);
        let p = Paragraph::new(Line::styled(
            s.env_no_env.to_string(),
            Style::default().fg(th.dim),
        ))
        .block(block)
        .wrap(Wrap { trim: false });
        f.render_widget(p, area);
        return;
    }
    let sel = popup.idx.min(env.vars.len().saturating_sub(1));
    // Columns available for the `key = value` text (after the border,
    // highlight symbol, marker and status dot); used to clamp scrolling.
    let text_w = area.width.saturating_sub(2 + 2 + 2 + 2);
    popup.scroll_w.set(text_w);
    let sel_len = env
        .vars
        .get(sel)
        .map(|v| v.key.chars().count() + 3 + v.display_value().chars().count())
        .unwrap_or(0);
    let max_scroll = sel_len.saturating_sub((text_w as usize).saturating_sub(1));
    let hscroll = (popup.hscroll as usize).min(max_scroll);
    let items: Vec<ListItem> = env
        .vars
        .iter()
        .enumerate()
        .map(|(i, v)| {
            // Status dot, colour-matched to the request substitution
            // scheme: orange = loading, cyan = literal, green = loaded
            // from a source (env/1Password/SSM), red = failed to resolve.
            let dot = if v.loading {
                th.pending
            } else if v.resolved {
                match v.source {
                    ValueSource::Literal => th.subst,
                    _ => th.ok,
                }
            } else {
                th.err
            };
            let val_color = if v.resolved { th.text } else { th.pending };
            let shown = if v.loading {
                s.env_loading.to_string()
            } else {
                v.display_value()
            };
            // Marker column (left of the status dot): a plus marks a
            // hand-added variable; otherwise a pencil marks a value the
            // user has edited away from its loaded value.
            let (marker, marker_fg) = if v.user_added {
                ("\u{271a} ", th.ok)
            } else if v.modified {
                ("\u{270e} ", th.accent)
            } else {
                ("  ", th.dim)
            };
            // The selected row is highlighted with a background only (see
            // `highlight_style` below), so text spans get a legible dark
            // foreground here while the STATUS DOT keeps its own colour —
            // that colour must stay visible even when the row is selected.
            let selected = i == sel;
            let mark_fg = if selected { th.bg } else { marker_fg };
            let mut spans = vec![
                Span::styled(marker, Style::default().fg(mark_fg)),
                Span::styled("● ", Style::default().fg(dot)),
            ];
            if selected {
                // The selected row scrolls its whole `key = value` so long
                // entries can be read end-to-end (rendered as one span in
                // the highlight foreground). Arrow hints appear on
                // whichever side still has hidden text.
                let combined = format!("{} = {}", v.key, shown);
                let avail = text_w as usize;
                let show_left = hscroll > 0;
                let content_w_before_right = avail.saturating_sub(show_left as usize);
                let remaining = sel_len.saturating_sub(hscroll);
                let show_right = remaining > content_w_before_right;
                let content_w = content_w_before_right.saturating_sub(show_right as usize);
                let visible: String = combined.chars().skip(hscroll).take(content_w).collect();
                let mut text = String::new();
                if show_left {
                    text.push('\u{2039}');
                }
                text.push_str(&visible);
                if show_right {
                    text.push('\u{203a}');
                }
                spans.push(Span::styled(text, Style::default().fg(th.bg)));
            } else {
                spans.push(Span::styled(v.key.clone(), Style::default().fg(th.text)));
                spans.push(Span::styled(" = ", Style::default().fg(th.dim)));
                spans.push(Span::styled(shown, Style::default().fg(val_color)));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    let list = List::new(items)
        .block(panel(title, true, th))
        // Background + bold only (no fg): patching fg would overwrite the
        // status dot's colour, which must remain visible when selected.
        .highlight_style(Style::default().bg(th.accent).add_modifier(Modifier::BOLD))
        .highlight_symbol("› ");
    let mut st = ListState::default();
    st.select(Some(sel));
    f.render_stateful_widget(list, area, &mut st);
    app.set_mouse_layer(MouseLayer::Overlay);
    let inner = Rect {
        x: area.x.saturating_add(1),
        y: area.y.saturating_add(1),
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };
    app.push_mouse_hit(
        MouseLayer::Overlay,
        inner,
        MouseHitTarget::Scroll(MouseScrollTarget::OverlayList),
    );
    let first = st.offset();
    let visible = inner.height as usize;
    for row in first..env.vars.len().min(first + visible) {
        app.push_mouse_hit(
            MouseLayer::Overlay,
            Rect::new(inner.x, inner.y + (row - first) as u16, inner.width, 1),
            MouseHitTarget::OverlayRow(row),
        );
    }
}

/// The colour for a substitution of the given [`SubstKind`].
fn subst_color(kind: crate::request::SubstKind, th: &Theme) -> Color {
    use crate::request::SubstKind;
    match kind {
        SubstKind::Literal => th.subst,   // cyan
        SubstKind::Loaded => th.ok,       // green
        SubstKind::Pending => th.pending, // orange
        SubstKind::Failed => th.err,      // red
    }
}

/// Tracks which [`SubstKind`]s were actually rendered while highlighting a
/// request's text, so the status legend can show only the dots that are
/// relevant to it instead of all four unconditionally.
#[derive(Default)]
struct SubstSeen {
    loaded: bool,
    literal: bool,
    pending: bool,
    failed: bool,
    /// At least one rendered substitution's Global Environment value is
    /// being shadowed by the collection's linked Environment.
    shadowed: bool,
}

impl SubstSeen {
    fn mark(&mut self, kind: crate::request::SubstKind) {
        use crate::request::SubstKind;
        match kind {
            SubstKind::Loaded => self.loaded = true,
            SubstKind::Literal => self.literal = true,
            SubstKind::Pending => self.pending = true,
            SubstKind::Failed => self.failed = true,
        }
    }

    fn any(&self) -> bool {
        self.loaded || self.literal || self.pending || self.failed
    }
}

/// Split one line of raw request text into styled spans, colour-coding each
/// *known* `{{ VAR }}` by its resolution status: a resolved value is substituted
/// (green = loaded from a source, cyan = literal); an unavailable one keeps the
/// `{{ VAR }}` placeholder (orange = loading, red = failed / not initialised).
/// Unknown placeholders are kept in the default colour. Marks `seen` with the
/// status of every known variable that was rendered. `shadowed`, when given,
/// flags variable names whose value from the active Global Environment is
/// being overridden by the collection's linked Environment (see
/// `TuiApp::shadowed_env_keys`) — such substitutions get a trailing warning
/// icon so the collision isn't silently invisible to the user. When
/// `icon_cols` is given, the character offset (within this call's returned
/// spans, i.e. relative to the start of `text`) of every such warning icon
/// is appended to it — used to strip the icon back out of copied/selected
/// text later (see `TuiApp::main_shadow_icon_positions`), since it's a
/// purely visual annotation that would otherwise corrupt a pasted request.
fn highlight_spans(
    text: &str,
    vars: &std::collections::HashMap<String, crate::request::SubstInfo>,
    th: &Theme,
    seen: &mut SubstSeen,
    shadowed: Option<&std::collections::HashSet<String>>,
    mut icon_cols: Option<&mut Vec<usize>>,
) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut cur_len: usize = 0;
    let mut rest = text;
    while let Some(open) = rest.find("{{") {
        let Some(close_rel) = rest[open + 2..].find("}}") else {
            break;
        };
        let close = open + 2 + close_rel; // index of the closing "}}"
        let end = close + 2; // just past "}}"
        let inner = rest[open + 2..close].trim();
        if open > 0 {
            let piece = rest[..open].to_string();
            cur_len += piece.chars().count();
            spans.push(Span::styled(piece, Style::default().fg(th.text)));
        }
        match vars.get(inner) {
            Some(info) => {
                seen.mark(info.kind);
                let color = subst_color(info.kind, th);
                match &info.shown {
                    // Resolved: show the value in its status colour, with a
                    // warning icon immediately before it (no gap) when this
                    // key is shadowing/shadowed — placed on the left so it
                    // never gets crowded out by whatever character follows
                    // the substitution (e.g. a URL path separator).
                    Some(val) => {
                        if shadowed.is_some_and(|s| s.contains(inner)) {
                            if let Some(cols) = icon_cols.as_deref_mut() {
                                cols.push(cur_len);
                            }
                            cur_len += SHADOW_ICON.chars().count();
                            spans.push(Span::styled(SHADOW_ICON, Style::default().fg(th.pending)));
                            seen.shadowed = true;
                        }
                        cur_len += val.chars().count();
                        spans.push(Span::styled(val.clone(), Style::default().fg(color)));
                    }
                    // Unavailable: keep `{{ VAR }}` in its status colour.
                    None => {
                        let piece = rest[open..end].to_string();
                        cur_len += piece.chars().count();
                        spans.push(Span::styled(piece, Style::default().fg(color)));
                    }
                }
            }
            None => {
                let piece = rest[open..end].to_string();
                cur_len += piece.chars().count();
                spans.push(Span::styled(piece, Style::default().fg(th.text)));
            }
        }
        rest = &rest[end..];
    }
    if !rest.is_empty() {
        spans.push(Span::styled(rest.to_string(), Style::default().fg(th.text)));
    }
    spans
}

/// Drop the first `skip` displayed characters from a run of spans (used to apply
/// horizontal scrolling to the coloured, substituted collection-list URL).
fn skip_display(spans: Vec<Span<'static>>, mut skip: usize) -> Vec<Span<'static>> {
    if skip == 0 {
        return spans;
    }
    let mut out: Vec<Span<'static>> = Vec::new();
    for sp in spans {
        let len = sp.content.chars().count();
        if skip >= len {
            skip -= len;
            continue;
        }
        if skip > 0 {
            let kept: String = sp.content.chars().skip(skip).collect();
            out.push(Span::styled(kept, sp.style));
            skip = 0;
        } else {
            out.push(sp);
        }
    }
    out
}

/// Keep only the first `take` displayed characters from a run of spans,
/// dropping everything after. Used together with [`skip_display`] to reserve
/// a column for a "more text this way" arrow hint when a horizontally
/// scrolled line is still truncated on the right.
fn take_display(spans: Vec<Span<'static>>, mut take: usize) -> Vec<Span<'static>> {
    let mut out: Vec<Span<'static>> = Vec::new();
    for sp in spans {
        if take == 0 {
            break;
        }
        let len = sp.content.chars().count();
        if len <= take {
            take -= len;
            out.push(sp);
        } else {
            let kept: String = sp.content.chars().take(take).collect();
            out.push(Span::styled(kept, sp.style));
            take = 0;
        }
    }
    out
}

/// Word-wrap `text` to `width` columns, never splitting a word across lines
/// unless the word alone is longer than `width` (in which case it's
/// hard-broken so it doesn't just overflow the box). Unlike `wrap_line`
/// (used for raw Request/Response body text, where character-exact
/// wrapping is correct), Help popup descriptions are prose, so wrapping on
/// word boundaries reads far more naturally.
pub(crate) fn word_wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        if word.chars().count() > width {
            if !cur.is_empty() {
                lines.push(std::mem::take(&mut cur));
            }
            let mut rest = word;
            while rest.chars().count() > width {
                let take: String = rest.chars().take(width).collect();
                let take_len = take.len();
                lines.push(take);
                rest = &rest[take_len..];
            }
            cur = rest.to_string();
            continue;
        }
        let candidate_len = if cur.is_empty() {
            word.chars().count()
        } else {
            cur.chars().count() + 1 + word.chars().count()
        };
        if candidate_len > width {
            lines.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    if !cur.is_empty() || lines.is_empty() {
        lines.push(cur);
    }
    lines
}

/// A section-heading line for the Help popup: the title flanked by thin
/// grey horizontal rules filling the rest of `width`, e.g.
/// "── Navigation ─────────────────────". Used instead of a bold heading
/// line + a following blank line, so each titled section costs one line
/// instead of two while still reading as clearly divided from the section
/// above/below it.
pub(crate) fn help_section_divider(title: &str, width: usize, th: &Theme) -> Line<'static> {
    const LEFT_RULE: usize = 2;
    let title_w = Span::raw(title.to_string()).width();
    let right_rule = width.saturating_sub(LEFT_RULE + 2 + title_w).max(2);
    Line::from(vec![
        Span::styled("─".repeat(LEFT_RULE), Style::default().fg(th.dim)),
        Span::raw(" "),
        Span::styled(
            title.to_string(),
            Style::default().fg(th.text).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled("─".repeat(right_rule), Style::default().fg(th.dim)),
    ])
}

/// Build one Help popup shortcut entry: the shortcut left-padded to the
/// fixed key column (or left as-is if it's itself longer than the column),
/// followed by its description word-wrapped to fit `width`. Any wrapped
/// continuation line is indented so it lines up under the description
/// column instead of wrapping back to column 0.
pub(crate) fn help_entry_lines(shortcut: &str, desc: &str, width: usize) -> Vec<Line<'static>> {
    help_entry_lines_col(shortcut, desc, 17, width)
}

/// Like [`help_entry_lines`] but with a caller-supplied key-column width. A
/// group of entries whose left-hand sides are longer than the default 17-column
/// shortcut layout (e.g. the report grammar, `REPORT REQUEST NAME [AS COL]`)
/// can pass its *own* widest key so every description in that group still lines
/// up in one column instead of each row's description starting wherever its key
/// happens to end.
pub(crate) fn help_entry_lines_col(
    key: &str,
    desc: &str,
    key_col: usize,
    width: usize,
) -> Vec<Line<'static>> {
    let indent = key.chars().count().max(key_col) + 1;
    let desc_width = width.saturating_sub(indent).max(1);
    let wrapped = word_wrap(desc, desc_width);
    let mut out = Vec::with_capacity(wrapped.len().max(1));
    for (i, chunk) in wrapped.iter().enumerate() {
        if i == 0 {
            out.push(Line::raw(format!("{key:<key_col$} {chunk}")));
        } else {
            out.push(Line::raw(format!("{:indent$}{chunk}", "")));
        }
    }
    out
}

/// Builds one Help "Glossary" tab entry: a coloured icon + bolded label
/// (e.g. "● loaded"), followed by its wrapped description — the glossary
/// counterpart to `help_entry_lines`, styled so the icon/label colour
/// matches exactly what's shown inline next to a substituted variable.
pub(crate) fn glossary_entry_lines(
    icon: &str,
    color: Color,
    label: &str,
    desc: &str,
    width: usize,
) -> Vec<Line<'static>> {
    const KEY_COL: usize = 17;
    let header = format!("{icon} {label}");
    // Use *display* width (columns), not `.chars().count()`: a couple of
    // glossary icons (folder, link) are double-width emoji, so counting
    // chars would under-measure the header by one column for those rows.
    // That one-column error used to throw off the description's own
    // word-wrap budget on its first line only (continuation lines are
    // padded with plain ASCII spaces, so they were never affected) — the
    // visible symptom was the first line starting one column further right
    // than the wrapped lines beneath it, which could shove the last word
    // (often the "—" separator) onto its own orphaned continuation line.
    let header_w = Span::raw(header.clone()).width();
    let indent = header_w.max(KEY_COL) + 1;
    let desc_width = width.saturating_sub(indent).max(1);
    let wrapped = word_wrap(desc, desc_width);
    // Total spaces after the header needed to reach the description's start
    // column (`indent`) — replaces the old `format!("{:<pad_width$}")` (which
    // pads by char count) + a separate literal space, so wide-icon rows are
    // padded by exactly as many columns as narrow-icon ones are.
    let pad = " ".repeat(indent.saturating_sub(header_w));
    let mut out = Vec::with_capacity(wrapped.len().max(1));
    for (i, chunk) in wrapped.iter().enumerate() {
        if i == 0 {
            out.push(Line::from(vec![
                Span::styled(
                    header.clone(),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::raw(pad.clone()),
                Span::raw(chunk.clone()),
            ]));
        } else {
            out.push(Line::raw(format!("{:indent$}{chunk}", "")));
        }
    }
    out
}

pub(crate) fn draw_collection_main(
    f: &mut Frame,
    area: Rect,
    app: &mut TuiApp,
    ci: usize,
    s: &Strings,
    th: &Theme,
) {
    let focused = app.focus == Pane::Main;
    let hurl_view = app.default_request_view == request::RequestView::Hurl;
    let title = if hurl_view {
        s.entry_request_hurl
    } else {
        s.entry_request_json
    };
    let block = panel(title.to_string(), focused, th);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.collections[ci].entries.is_empty() {
        app.main_max_scroll = 0;
        app.main_panel
            .set_content(Arc::from(""), inner.width.max(1) as usize);
        app.main_panel.clear();
        app.main_panel.set_scroll(0);
        app.main_text_area = Rect::default();
        app.main_shadow_icon_positions.clear();
        app.main_scrollbar_area = Rect::default();
        app.push_mouse_hit(
            MouseLayer::Base,
            inner,
            MouseHitTarget::FocusPane(Pane::Main),
        );
        f.render_widget(
            Paragraph::new(Line::styled(
                s.no_requests_hint.to_string(),
                Style::default().fg(th.dim),
            )),
            inner,
        );
        return;
    }

    let col = &app.collections[ci];
    let idx = col.selected_entry.min(col.entries.len() - 1);
    let entry = &col.entries[idx];
    let method = entry.method.clone();
    let url = entry.url.clone();
    let captures = entry.captures.clone();
    let asserts = entry.asserts.clone();
    let expected_status = entry.expected_status;
    // The Hurl view always renders (it's actual Hurl syntax, not JSON, so the
    // "invalid JSON" check below is meaningless for it and skipped entirely).
    let (buf, valid) = if hurl_view {
        (entry.to_hurl(), true)
    } else {
        let buf = col.request_json_buf.clone();
        let valid = serde_json::from_str::<serde_json::Value>(&buf).is_ok();
        (buf, valid)
    };
    // How each `{{ VAR }}` should be shown/coloured in the preview (secrets masked).
    let env = app.effective_env(ci);
    let dvars = crate::request::subst_map(&app.collections[ci], env.as_ref());
    // Keys where the linked Environment's value shadows the active Global
    // Environment's — flagged with a warning icon below.
    let shadowed = app.shadowed_env_keys(ci);
    // `col`/`entry` borrows end here — everything needed is cloned above.

    let mut seen = SubstSeen::default();
    // Top region: method/url (with substitutions) + run hint, then [Captures]/[Asserts].
    let mut top_lines: Vec<Line> = Vec::new();
    let mut first: Vec<Span> = vec![Span::styled(
        format!("{method} "),
        Style::default()
            .fg(method_color(&method))
            .add_modifier(Modifier::BOLD),
    )];
    first.extend(highlight_spans(
        &url,
        &dvars,
        th,
        &mut seen,
        Some(&shadowed),
        None,
    ));
    first.push(Span::styled(
        format!(
            "   [ {} — {} ]",
            s.run_entry.trim(),
            if app.enhanced_keys {
                "^Enter / F5"
            } else {
                "F5"
            }
        ),
        Style::default().fg(th.accent),
    ));
    top_lines.push(Line::from(first));
    // Build the (highlighted) JSON body; this also flags whether anything was
    // substituted so we can show the legend. Also records, per body line,
    // the character offset of every shadow-warning icon inserted into it
    // (`shadow_positions`) — purely a visual annotation, so it's stripped
    // back out of copied/selected text later (see
    // `TuiApp::main_shadow_icon_positions`) rather than corrupting a pasted
    // request with a stray "!".
    let mut shadow_positions: std::collections::HashSet<TextPos> = std::collections::HashSet::new();
    let mut body_lines: Vec<Line> = buf
        .lines()
        .enumerate()
        .map(|(li, l)| {
            let mut cols = Vec::new();
            let spans = highlight_spans(l, &dvars, th, &mut seen, Some(&shadowed), Some(&mut cols));
            for c in cols {
                shadow_positions.insert(TextPos::new(li, c));
            }
            Line::from(spans)
        })
        .collect();
    // `body_lines` is what the panel actually renders — i.e. `buf` with every
    // resolved `{{ VAR }}` already substituted in, exactly like the user sees
    // on screen — rather than `buf` itself, which still has the raw
    // `{{ VAR }}` moustache syntax. The panel derives its plain text from
    // these lines (joined by `\n`), which is what backs its scroll geometry,
    // mouse-selection extraction, and the whole-panel copy fallback, so a
    // copied/selected value always matches what's visually shown instead of
    // the underlying template.
    //
    // `buf.lines()` drops a trailing newline just like `str::lines()` always
    // does, so re-add it as an empty trailing line — otherwise a whole-panel
    // copy of Hurl text would silently lose the trailing newline the raw
    // buffer actually had (and its geometry would be one row short).
    if buf.ends_with('\n') {
        body_lines.push(Line::from(""));
    }
    top_lines.push(if valid {
        Line::styled(
            format!("Enter {}", s.json_enter_to_edit),
            Style::default().fg(th.dim),
        )
    } else {
        Line::styled(s.json_invalid.to_string(), Style::default().fg(th.err))
    });
    if seen.any() {
        let segments = [
            (seen.loaded, s.subst_hint_loaded, th.ok),
            (seen.literal, s.subst_hint_literal, th.subst),
            (seen.pending, s.subst_hint_loading, th.pending),
            (seen.failed, s.subst_hint_missing, th.err),
        ];
        let mut spans: Vec<Span<'static>> = Vec::new();
        for (present, word, color) in segments {
            if !present {
                continue;
            }
            if !spans.is_empty() {
                spans.push(Span::raw(" "));
            }
            spans.push(Span::styled(
                format!("\u{25cf} {word}"),
                Style::default().fg(color),
            ));
        }
        // Rendered with the same "!" icon used inline (not the "●" dot) so
        // the legend visually matches the marker the user actually sees
        // next to shadowed substitutions.
        if seen.shadowed {
            if !spans.is_empty() {
                spans.push(Span::raw(" "));
            }
            spans.push(Span::styled(
                format!("{SHADOW_ICON} {}", s.subst_hint_shadowed),
                Style::default().fg(th.pending),
            ));
        }
        top_lines.push(Line::from(spans));
    }
    if !captures.is_empty() {
        top_lines.push(Line::styled(
            "[Captures]",
            Style::default().fg(th.accent).add_modifier(Modifier::BOLD),
        ));
        for (name, expr) in &captures {
            top_lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(name.clone(), Style::default().fg(th.text)),
                Span::styled(" ← ", Style::default().fg(th.dim)),
                Span::styled(expr.clone(), Style::default().fg(th.dim)),
            ]));
        }
    }
    if !asserts.is_empty() || expected_status.is_some() {
        top_lines.push(Line::styled(
            "[Asserts]",
            Style::default().fg(th.accent).add_modifier(Modifier::BOLD),
        ));
        // Surface the `HTTP <code>` response line (stored separately as
        // `expected_status`) as a synthesized `status == <code>` assert row so
        // the status check reads as one of the asserts, matching how Hurl
        // evaluates it.
        if let Some(code) = expected_status {
            top_lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(format!("status == {code}"), Style::default().fg(th.dim)),
            ]));
        }
        for a in &asserts {
            top_lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(a.clone(), Style::default().fg(th.dim)),
            ]));
        }
    }

    // Push a dim line to visually separate the Request meta-information from
    // the raw request itself
    top_lines.push(Line::from(vec![Span::styled(
        "─".repeat(area.width as usize),
        Style::default().fg(th.dim),
    )]));

    // Keep at least 3 rows for the JSON body when the panel is tall enough.
    let cap = inner.height.saturating_sub(3).max(2);
    let top_h = (top_lines.len() as u16).clamp(2, cap);
    let split = Layout::vertical([Constraint::Length(top_h), Constraint::Min(1)]).split(inner);
    f.render_widget(Paragraph::new(top_lines), split[0]);

    // Clamp scrolling so the user can't scroll past the last line into blank space.
    let text_area = split[1];
    let width = text_area.width as usize;
    // Push the styled body into the panel (rebuilt fresh every frame — the
    // content is always small). The end-of-row wrap marker makes a soft wrap
    // read unambiguously as one logical line rather than several. The panel
    // wraps only the visible window internally, so scrolling/dragging stays
    // responsive even for a large body.
    app.main_panel.set_wrap_marker(Some(wrap_marker(th)));
    app.main_panel.set_styled_content(&body_lines, width);
    let total_lines = app.main_panel.total_rows().min(u16::MAX as u32) as u16;
    let max_scroll = app.main_panel.clamp_scroll(text_area.height);
    app.main_max_scroll = max_scroll;
    let scroll = app.main_panel.scroll();
    let height = text_area.height as usize;
    let visible_wrapped = app.main_panel.visible_rows(text_area.height);

    // Overlay a scrollbar on the panel's right border (not stealing an inner
    // text column) whenever the body has more wrapped rows than fit, so it's
    // visually obvious there's more to see and roughly where in the body the
    // visible window currently sits.
    if max_scroll > 0 {
        let bar_area = Rect {
            x: area.x + area.width - 1,
            y: text_area.y,
            width: 1,
            height: text_area.height,
        };
        draw_scrollbar(
            f,
            bar_area,
            total_lines as usize,
            height,
            scroll as usize,
            th,
        );
        app.main_scrollbar_area = bar_area;
        app.push_mouse_hit(
            MouseLayer::Base,
            bar_area,
            MouseHitTarget::Scroll(MouseScrollTarget::Main),
        );
    } else {
        app.main_scrollbar_area = Rect::default();
    }

    // Record the panel's Rect and shadow-icon positions so mouse selection can
    // map coordinates back to real, copyable text — scoped to this panel only.
    app.main_text_area = text_area;
    app.main_shadow_icon_positions = shadow_positions;
    app.push_mouse_hit(
        MouseLayer::Base,
        text_area,
        MouseHitTarget::FocusPane(Pane::Main),
    );
    app.push_mouse_hit(
        MouseLayer::Base,
        text_area,
        MouseHitTarget::Scroll(MouseScrollTarget::Main),
    );

    f.render_widget(
        Paragraph::new(visible_wrapped).style(Style::default().fg(th.text)),
        text_area,
    );
}

pub(crate) fn draw_response(
    f: &mut Frame,
    area: Rect,
    app: &mut TuiApp,
    ci: usize,
    s: &Strings,
    th: &Theme,
) {
    let focused = app.focus == Pane::Response;
    let block = panel(s.response_heading.to_string(), focused, th);
    let inner = block.inner(area);
    f.render_widget(block, area);

    // The in-flight spinner now tracks the *selected entry* rather than a single
    // shared "loading" flag: an entry is "sending" while its `last_run` is
    // `Running`. So a request that's still in flight shows the spinner, while
    // selecting a *different* entry (whether idle or already finished) shows
    // that entry's own last response — even if some other request is mid-send.
    let entry = app
        .collections
        .get(ci)
        .and_then(|col| col.entries.get(col.selected_entry));
    let loading = entry
        .map(|e| e.last_run == RunStatus::Running)
        .unwrap_or(false);
    let (status, status_text, body, error, asserts, duration) =
        match entry.and_then(|e| e.last_response.as_ref()) {
            Some(r) => (
                r.status,
                r.status_text.clone(),
                r.body.clone(),
                r.error.clone(),
                r.assert_results.clone(),
                r.duration_ms,
            ),
            None => (
                0,
                String::new(),
                Arc::from(""),
                String::new(),
                Vec::new(),
                None,
            ),
        };

    // Cleared here so every early-return path (loading / no-response / error)
    // leaves it empty; only the compact body branch below sets it. This is what
    // the whole-panel `y`-copy consults to return the untruncated body while the
    // panel shows the compacted overview.
    app.resp_full_body = Arc::from("");
    app.resp_compact_line_maps = Vec::new();

    if loading {
        app.resp_max_scroll = 0;
        app.resp_text_area = Rect::default();
        app.resp_panel
            .set_content(Arc::from(""), area.width.max(1) as usize);
        app.resp_panel.clear();
        app.resp_panel.set_scroll(0);
        app.resp_scrollbar_area = Rect::default();
        f.render_widget(
            Paragraph::new(Line::styled(
                format!("⟳ {}", s.sending),
                Style::default().fg(th.accent),
            )),
            inner,
        );
        return;
    }
    if status == 0 {
        // No response was received. A transport/parse failure (or a build error
        // like the Body/Form conflict) leaves an error string but no response,
        // so surface it here; otherwise show the neutral placeholder. A failed
        // assert or status mismatch, by contrast, still produces a real
        // response (status != 0) and is rendered in full below — with the
        // failing checks marked — rather than replacing the response with the
        // error text.
        if !error.is_empty() {
            // Render the runner error *through* the selectable response panel
            // (not a plain Paragraph) so it can be mouse-selected and `y`-copied
            // like any response body — the red fg is applied as the paragraph's
            // fallback style, and the panel still owns wrapping/scrolling for
            // long errors.
            let content: Arc<str> = Arc::from(format!("{} {error}", s.req_error_prefix));
            app.resp_panel.set_wrap_marker(Some(wrap_marker(th)));
            app.resp_panel
                .set_content(content, inner.width.max(1) as usize);
            app.resp_max_scroll = app.resp_panel.clamp_scroll(inner.height);
            app.resp_scrollbar_area = Rect::default();
            let visible_wrapped = app.resp_panel.visible_rows(inner.height);
            app.resp_text_area = inner;
            f.render_widget(
                Paragraph::new(visible_wrapped).style(Style::default().fg(th.err)),
                inner,
            );
            return;
        }
        app.resp_max_scroll = 0;
        app.resp_text_area = Rect::default();
        app.resp_panel
            .set_content(Arc::from(""), area.width.max(1) as usize);
        app.resp_panel.clear();
        app.resp_panel.set_scroll(0);
        app.resp_scrollbar_area = Rect::default();
        f.render_widget(
            Paragraph::new(Line::styled(
                s.no_response_yet.to_string(),
                Style::default().fg(th.dim),
            )),
            inner,
        );
        return;
    }

    let color = match status {
        200..=299 => th.ok,
        400..=599 => th.err,
        _ => Color::Gray,
    };

    // Status line, with an [Asserts] pass/fail badge supplemental to the status.
    let mut status_spans = vec![
        Span::styled(format!("{} ", s.status_label), Style::default().fg(th.text)),
        Span::styled(
            format!("{status} {status_text}"),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
    ];
    let passed = asserts.iter().filter(|a| a.passed).count();
    let total = asserts.len();
    if total > 0 {
        let all_ok = passed == total;
        let badge = if all_ok { th.ok } else { th.err };
        let mark = if all_ok { "\u{2713}" } else { "\u{2717}" };
        status_spans.push(Span::raw("   "));
        status_spans.push(Span::styled(
            format!("[Asserts] {mark} {passed}/{total}"),
            Style::default().fg(badge).add_modifier(Modifier::BOLD),
        ));
    }
    // Response time, when the runner reported one — the same figure reports
    // show as the per-request "Time" column, surfaced here for a single run.
    if let Some(ms) = duration {
        status_spans.push(Span::raw("   "));
        status_spans.push(Span::styled(
            format!("{} {ms} ms", s.response_time_label),
            Style::default().fg(th.dim),
        ));
    }

    // One line per assert (✓/✗ with the expression and, on failure, the actual).
    let assert_lines: Vec<Line> = asserts
        .iter()
        .map(|a| {
            let (mark, c) = if a.passed {
                ("\u{2713}", th.ok)
            } else {
                ("\u{2717}", th.err)
            };
            let mut spans = vec![
                Span::styled(format!("  {mark} "), Style::default().fg(c)),
                Span::styled(a.expr.clone(), Style::default().fg(th.text)),
            ];
            if !a.detail.is_empty() {
                spans.push(Span::styled(
                    format!("  {}", a.detail),
                    Style::default().fg(th.dim),
                ));
            }
            Line::from(spans)
        })
        .collect();

    // Layout: status (1) · error (0/1) · asserts (capped, keeping ≥1 body row)
    // · body (rest). A runner error that *isn't* already spelled out by a
    // failed assert row — a failed `[Captures]`, a transport oddity that still
    // returned a response — gets one error-coloured line so it isn't lost now
    // that a non-empty error no longer replaces the whole response. When an
    // assert failed (passed < total) that ✗ row already carries the reason, so
    // the extra line would just be noise and is skipped.
    let show_err_line = !error.is_empty() && passed == total;
    let err_h: u16 = u16::from(show_err_line);
    let assert_h =
        (assert_lines.len() as u16).min(inner.height.saturating_sub(2).saturating_sub(err_h));
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(err_h),
        Constraint::Length(assert_h),
        Constraint::Min(1),
    ])
    .split(inner);

    f.render_widget(Paragraph::new(Line::from(status_spans)), rows[0]);
    if show_err_line {
        f.render_widget(
            Paragraph::new(Line::styled(
                format!("{} {error}", s.req_error_prefix),
                Style::default().fg(th.err),
            )),
            rows[1],
        );
    }
    if assert_h > 0 {
        f.render_widget(Paragraph::new(assert_lines), rows[2]);
    }

    // Wrap long lines to the body width and clamp scrolling so the user can't
    // scroll past the last line into blank space. The panel caches the
    // wrap/line structure (`set_content` → `rebuild_if_needed`) and reuses it
    // across frames as long as `body`'s identity and the panel width haven't
    // changed, and even a rebuild only wraps the rows actually on screen —
    // this is what keeps dragging a selection or scrolling responsive
    // regardless of how large an "obscenely large" response body is. The
    // end-of-row wrap marker makes a soft wrap read as one logical line.
    let body_area = rows[3];
    let width = body_area.width as usize;
    app.resp_panel.set_wrap_marker(Some(wrap_marker(th)));
    // Compact view (toggled with `c`) shortens long string literals for
    // skimming. It's display-only: cache the full body so a whole-panel
    // `y`-copy still returns the untruncated text (see `whole_panel_text`).
    if app.response_compact {
        app.resp_full_body = body.clone();
        let (compacted, line_maps) = crate::shared_utils::compact_long_strings_mapped(&body);
        app.resp_compact_line_maps = line_maps;
        app.resp_panel.set_content(Arc::from(compacted), width);
    } else {
        app.resp_panel.set_content(body.clone(), width);
    }
    let total_lines = app.resp_panel.total_rows().min(u16::MAX as u32) as u16;
    let max_scroll = app.resp_panel.clamp_scroll(body_area.height);
    app.resp_max_scroll = max_scroll;
    let scroll = app.resp_panel.scroll();

    let visible_wrapped = app.resp_panel.visible_rows(body_area.height);

    // Overlay a scrollbar on the panel's right border (not stealing an inner
    // text column, and safely outside `resp_text_area` so it can never be
    // clicked into as part of a text selection) whenever the body has more
    // wrapped rows than fit.
    if max_scroll > 0 {
        let bar_area = Rect {
            x: area.x + area.width - 1,
            y: body_area.y,
            width: 1,
            height: body_area.height,
        };
        draw_scrollbar(
            f,
            bar_area,
            total_lines as usize,
            body_area.height as usize,
            scroll as usize,
            th,
        );
        app.resp_scrollbar_area = bar_area;
        app.push_mouse_hit(
            MouseLayer::Base,
            bar_area,
            MouseHitTarget::Scroll(MouseScrollTarget::Response),
        );
    } else {
        app.resp_scrollbar_area = Rect::default();
    }

    // Cache the geometry so mouse selection can map coordinates back to real,
    // copyable text — scoped to this panel's own Rect only.
    app.resp_text_area = body_area;
    app.push_mouse_hit(
        MouseLayer::Base,
        body_area,
        MouseHitTarget::FocusPane(Pane::Response),
    );
    app.push_mouse_hit(
        MouseLayer::Base,
        body_area,
        MouseHitTarget::Scroll(MouseScrollTarget::Response),
    );

    f.render_widget(
        Paragraph::new(visible_wrapped).style(Style::default().fg(th.text)),
        body_area,
    );
}

pub(crate) fn draw_footer(
    f: &mut Frame,
    area: Rect,
    s: &Strings,
    th: &Theme,
    can_copy: bool,
    can_compact: bool,
) {
    // Run/Run All (F5 / Alt+F5) now live on the Collections panel's bottom
    // border (see draw_collection_left), and the base-URL row above already
    // shows its own "b" hint — kept out of here to leave room for the rest.
    let mut hint = vec![
        format!("Tab {}", s.foot_focus),
        format!("↑↓ {}", s.foot_move),
        format!("Enter {}", s.foot_edit),
        format!("n {}", s.foot_new),
        format!("F2 {}", s.foot_rename),
        format!("x {}", s.foot_close),
    ];
    // Only shown while `y` would actually do something — the rest of the
    // footer is a fixed set of always-available shortcuts, but `y` copies
    // either the active selection or, with none, the whole Request JSON /
    // Response panel that currently has focus (see `TuiApp::can_copy`).
    if can_copy {
        hint.push(format!("y {}", s.foot_copy_selection));
    }
    // `c` toggles the Response body's compact overview — only meaningful (and
    // only shown) while the Response pane holds focus.
    if can_compact {
        hint.push(format!("c {}", s.foot_compact));
    }
    hint.push(format!("? {}", s.foot_help));
    hint.push(format!("q {}", s.foot_quit));
    let hint = hint.join(" · ");
    f.render_widget(
        Paragraph::new(Line::styled(hint, Style::default().fg(th.dim)))
            .style(Style::default().bg(th.panel)),
        area,
    );
}

/// Render the report-export format strip (`CSV JSON HTML XLSX`) into `row`,
/// highlighting the format that matches `filename`'s extension (an unknown or
/// absent extension highlights CSV, the writer's fallback). Cycled with ↑/↓
/// while the filename field is focused — see `cycle_browser_export_format`.
fn draw_export_format_strip(f: &mut Frame, row: Rect, filename: &str, s: &Strings, th: &Theme) {
    use crate::report::writer::OUTPUT_EXTENSIONS;
    let cur = std::path::Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    let known = cur
        .as_deref()
        .map(|c| OUTPUT_EXTENSIONS.contains(&c))
        .unwrap_or(false);
    let mut spans = vec![Span::styled(
        format!(" {}  ", s.report_export_format_hint),
        Style::default().fg(th.dim),
    )];
    for ext in OUTPUT_EXTENSIONS {
        let active = cur.as_deref() == Some(ext) || (!known && ext == "csv");
        let style = if active {
            Style::default()
                .bg(th.accent)
                .fg(th.bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(th.text)
        };
        spans.push(Span::styled(format!(" {} ", ext.to_uppercase()), style));
        spans.push(Span::raw(" "));
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(th.panel)),
        row,
    );
}

pub(crate) fn draw_overlay(f: &mut Frame, app: &mut TuiApp, s: &Strings, th: &Theme) {
    app.set_mouse_layer(MouseLayer::Overlay);
    // `ReportCellPopup` needs a mutable borrow of its `MultiSelectPanel` to
    // update scroll/content each frame, so it is handled before the immutable
    // `as_ref()` match below.
    if let Some(Overlay::ReportCellPopup {
        title,
        content,
        panel,
    }) = app.overlay.as_mut()
    {
        let inner = super::reports::draw_result_cell_popup_overlay(f, title, content, panel, s, th);
        app.push_mouse_hit(
            MouseLayer::Overlay,
            inner,
            MouseHitTarget::Scroll(MouseScrollTarget::ReportCellPopup),
        );
        return;
    }
    match app.overlay.as_ref().unwrap() {
        Overlay::FileMenu(sel) => {
            let items = file_menu_items(s);
            draw_menu_popup(f, s.file_menu, &items, *sel, th, Some(app));
        }
        Overlay::FileLoadMenu(sel) => {
            let items = file_load_items(s);
            draw_menu_popup(f, s.file_load_menu, &items, *sel, th, Some(app));
        }
        Overlay::FileSaveMenu(sel) => {
            let items = app.file_save_items();
            let labels: Vec<&str> = items.iter().map(|it| it.label(s)).collect();
            draw_menu_popup(f, s.file_save_menu, &labels, *sel, th, Some(app));
        }
        Overlay::FileLoadSource(kind, sel) => {
            let items = file_load_source_items(*kind, s);
            let title = format!("{} {}", s.file_load_menu, kind.name(s));
            draw_menu_popup(f, &title, &items, *sel, th, Some(app));
        }
        Overlay::FileSaveDest(kind, sel) => {
            let items = file_save_dest_items(*kind, s);
            let title = format!("{} {}", s.file_save_menu, kind.name(s));
            draw_menu_popup(f, &title, &items, *sel, th, Some(app));
        }
        Overlay::Options(sel) => {
            let items = [
                s.settings_item_language,
                s.settings_item_theme,
                s.settings_item_preferences,
                s.settings_item_clear,
            ];
            draw_menu_popup(f, s.options_menu, &items, *sel, th, Some(app));
        }
        Overlay::Preferences(sel) => {
            let mark = |b: bool| if b { "[x]" } else { "[ ]" };
            let exit_item = format!("{} {}", mark(app.confirm_on_exit), s.pref_item_confirm_exit);
            let clear_item = format!(
                "{} {}",
                mark(app.confirm_on_clear),
                s.pref_item_confirm_clear
            );
            let delete_env_item = format!(
                "{} {}",
                mark(app.confirm_on_delete_env),
                s.pref_item_confirm_delete_env
            );
            let view_label = match app.default_request_view {
                request::RequestView::Json => "JSON",
                request::RequestView::Hurl => "Hurl",
            };
            let view_item = format!("{}: {view_label}", s.pref_item_default_view);
            let always_save_item = format!(
                "{} {}",
                mark(app.always_save_when_prompted),
                s.pref_item_always_save
            );
            let run_all_batch_item = format!(
                "{} {}",
                mark(app.run_all_batch_mode),
                s.pref_item_run_all_batch
            );
            let items = [
                exit_item.as_str(),
                clear_item.as_str(),
                delete_env_item.as_str(),
                always_save_item.as_str(),
                run_all_batch_item.as_str(),
                view_item.as_str(),
            ];
            draw_menu_popup(f, s.preferences_menu, &items, *sel, th, Some(app));
        }
        Overlay::Confirm { action, sel } => {
            let question: String = match action {
                ConfirmAction::Exit => {
                    let mut q = s.confirm_exit_q.to_string();
                    // Fold the secret-loss warning into the exit popup (rather
                    // than a second popup) when there are unsaved secret edits.
                    if app.has_unsaved_secret_changes() {
                        q.push(' ');
                        q.push_str(s.confirm_exit_secrets);
                    }
                    // Same treatment for in-memory request edits: they have no
                    // file behind them, so quitting is the moment they vanish.
                    let edits = app.unsaved_request_edits();
                    if edits > 0 {
                        q.push(' ');
                        q.push_str(&s.confirm_exit_edits.replace("{n}", &edits.to_string()));
                    }
                    q
                }
                ConfirmAction::Clear => s.confirm_clear_q.to_string(),
                // Scoped to the entity being saved: the collection warning counts
                // only requests, the environment warning only variables.
                ConfirmAction::Save(FileAction::SaveEnv) => s.confirm_save_env_q.replace(
                    "{e}",
                    &app.current_env_id()
                        .map(|id| app.changed_env_count(id))
                        .unwrap_or(0)
                        .to_string(),
                ),
                ConfirmAction::Save(FileAction::SaveReport) => s.confirm_save_report_q.to_string(),
                ConfirmAction::Save(_) => s.confirm_save_collection_q.replace(
                    "{r}",
                    &app.changed_request_count(app.active_tab).to_string(),
                ),
                ConfirmAction::Overwrite(_) => {
                    let name = app
                        .pending_save_path
                        .as_ref()
                        .map(|p| {
                            p.file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_else(|| p.to_string_lossy().into_owned())
                        })
                        .unwrap_or_default();
                    s.confirm_overwrite_q.replace("{f}", &name)
                }
                ConfirmAction::DeleteEnv(_) => s.env_delete_confirm.to_string(),
                ConfirmAction::RevertRequest(ci, ei) => {
                    let name = app
                        .collections
                        .get(*ci)
                        .and_then(|c| c.entries.get(*ei))
                        .map(|e| {
                            let leaf = crate::tree::entry_path(&e.title).pop().unwrap_or_default();
                            if leaf.is_empty() { e.url.clone() } else { leaf }
                        })
                        .unwrap_or_default();
                    s.confirm_revert_request_q.replace("{r}", &name)
                }
                ConfirmAction::RevertEnv(env_id) => {
                    let (name, n) = app
                        .global_envs
                        .iter()
                        .find(|e| e.id == *env_id)
                        .map(|e| {
                            (
                                e.name.clone(),
                                e.vars.iter().filter(|v| v.user_added || v.modified).count(),
                            )
                        })
                        .unwrap_or_default();
                    s.confirm_revert_env_q
                        .replace("{e}", &name)
                        .replace("{n}", &n.to_string())
                }
                ConfirmAction::RerunReport => s.confirm_rerun_report_q.to_string(),
            };
            draw_confirm_popup(
                f,
                &question,
                &[s.confirm_yes, s.confirm_no],
                *sel,
                th,
                Some(app),
            );
        }
        Overlay::LanguageMenu(sel) => {
            let items = [s.lang_english, s.lang_french, s.lang_danish];
            draw_menu_popup(f, s.language_label, &items, *sel, th, Some(app));
        }
        Overlay::ThemeEditor(state) => {
            let entries = app.theme_picker_entries(s);
            super::theme_editor::draw_theme_editor(f, state, &entries, s, th, Some(app));
        }
        Overlay::RequestViewMenu(sel) => {
            let items = [s.view_json_label, s.view_hurl_label];
            draw_menu_popup(f, s.default_request_view_label, &items, *sel, th, Some(app));
        }
        Overlay::Help(tab) => {
            // Widen the popup on spacious terminals so long descriptions
            // need to wrap less often (`centered_rect` clamps this further
            // to whatever's actually available on narrow terminals).
            let box_w = f.area().width.saturating_sub(6).clamp(64, 100);
            let inner_w = (box_w as usize).saturating_sub(2); // minus the left/right border

            // Active type-to-filter query (see `TuiApp::help_query`). A row is
            // kept when any of its text columns contains the query (case-
            // insensitively); an empty query keeps everything.
            let query = app.help_query.to_ascii_lowercase();
            let filtering = !query.is_empty();
            let matches = |texts: &[&str]| -> bool {
                query.is_empty()
                    || texts
                        .iter()
                        .any(|t| t.to_ascii_lowercase().contains(query.as_str()))
            };

            let tab_bar = |active: usize| {
                Line::from(vec![
                    Span::styled(
                        format!(" {} ", s.help_tab_shortcuts),
                        if active == 0 {
                            Style::default()
                                .bg(th.accent)
                                .fg(th.bg)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(th.dim)
                        },
                    ),
                    Span::raw(" "),
                    Span::styled(
                        format!(" {} ", s.help_tab_glossary),
                        if active == 1 {
                            Style::default()
                                .bg(th.accent)
                                .fg(th.bg)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(th.dim)
                        },
                    ),
                    Span::raw(" "),
                    Span::styled(
                        format!(" {} ", s.help_tab_reports),
                        if active == 2 {
                            Style::default()
                                .bg(th.accent)
                                .fg(th.bg)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(th.dim)
                        },
                    ),
                    Span::raw("   "),
                    Span::styled(s.help_tab_switch_hint, Style::default().fg(th.dim)),
                ])
            };

            let shortcuts_body = || {
                // Grouped into short, titled sections (mirroring the
                // Glossary tab's two-heading layout) instead of one long
                // flat list — new users found the un-grouped list hard to
                // scan for the shortcut they needed.
                let groups: [(&str, &[(&str, &str)]); 8] = [
                    (
                        s.help_group_navigation,
                        &[
                            ("Tab / Shift+Tab", s.help_focus),
                            ("↑ ↓  / j k", s.help_move),
                            ("^\u{2191} ^\u{2193}", s.help_page_response),
                            ("← \u{2192}  / h l", s.help_switch_tabs),
                        ],
                    ),
                    (
                        s.help_group_tabs,
                        &[
                            ("[ / ], PgUp/PgDn, ^\u{2190}/\u{2192}", s.help_prev_next_tab),
                            ("F2 / x", s.help_rename_close),
                            ("^W / u", s.help_tab_manage),
                            ("^Shift+\u{2190} \u{2192}", s.help_tab_reorder),
                        ],
                    ),
                    (
                        s.help_group_requests,
                        &[
                            ("Enter", s.help_select),
                            (
                                if app.enhanced_keys {
                                    "^Enter, F5"
                                } else {
                                    "F5"
                                },
                                s.help_run,
                            ),
                            ("Alt+F5", s.help_run_all),
                            ("n", s.help_new),
                            ("Shift+H", s.help_raw_mode),
                            ("Shift+J", s.help_raw_json),
                            ("b", s.help_base_url),
                            ("u (List pane)", s.help_restore_request),
                            ("^r (List pane)", s.help_revert_request),
                            ("m (workspace, List pane)", s.help_move_request),
                            ("c (workspace, List pane)", s.help_copy_request),
                        ],
                    ),
                    (
                        s.help_group_menus,
                        &[
                            ("f / s", s.help_menus),
                            ("\u{2190} / \u{2192} (File menu)", s.help_menu_submenu_nav),
                            ("w", s.help_workspace_browse),
                            ("\u{2192} / Enter (Workspace)", s.help_workspace_open),
                            ("Shift+N (Workspace)", s.help_workspace_new_item),
                            ("Shift+M (Workspace)", s.help_workspace_move_item),
                            ("^r (File browser)", s.help_browser_reset),
                        ],
                    ),
                    (
                        s.help_group_environments,
                        &[
                            ("r (Env popup)", s.help_reload_var),
                            ("^r (Env popup)", s.help_revert_env),
                            ("F2 (Env panel)", s.help_env_rename),
                            ("a", s.help_env_activate),
                            (
                                "a / right-click (workspace, List pane)",
                                s.help_env_activate_workspace,
                            ),
                            ("/", s.help_env_filter),
                            ("o", s.help_env_source),
                            ("x", s.help_env_delete),
                            ("u (Env panel)", s.help_env_reopen),
                            ("p (List pane)", s.help_env_link),
                            ("v", s.help_env_view_linked),
                        ],
                    ),
                    (
                        s.help_group_editing,
                        &[
                            ("", s.help_row_toggle_delete),
                            ("y", s.help_copy_selection),
                            ("c (Response pane)", s.help_compact),
                            ("Alt+Click+Drag", s.help_multi_select),
                            ("F2", s.help_save_editor),
                        ],
                    ),
                    (
                        s.help_group_reports,
                        &[
                            ("Shift+R", s.help_report_new),
                            ("e (report)", s.help_report_edit),
                            ("Enter (report)", s.help_report_nodes),
                            ("r / F5 (report)", s.help_report_run),
                            ("d (report)", s.help_report_dry_run),
                            ("v (report)", s.help_report_view),
                            ("a / Del / Shift+↑↓ (nodes)", s.help_report_nodes_edit),
                            ("Enter (nodes)", s.help_report_nodes_forms),
                            ("Tab / Shift+Tab (report)", s.help_report_focus_cycle),
                            ("↑↓ / Enter (ws tree)", s.help_report_workspace_tree),
                            ("Ctrl+S (report)", s.help_report_export),
                            ("B (report)", s.help_report_baseline),
                            ("c (report)", s.help_report_columns),
                            ("b (report)", s.help_report_bind),
                            ("Esc (report)", s.help_report_leave_edit),
                            ("Ctrl+←/→ (report)", s.help_report_word_move),
                            ("→ (report)", s.help_report_complete),
                        ],
                    ),
                    (
                        s.help_group_panels,
                        &[
                            ("+ / -", s.help_resize),
                            ("< / >", s.help_resize_width),
                            ("Esc", s.help_cancel),
                            ("q, ^C", s.help_quit),
                        ],
                    ),
                ];
                let mut body = Vec::new();
                for (heading, entries) in groups.iter() {
                    let kept: Vec<&(&str, &str)> = entries
                        .iter()
                        .filter(|(k, d)| matches(&[k, d, heading]))
                        .collect();
                    if kept.is_empty() {
                        continue;
                    }
                    if !body.is_empty() {
                        body.push(Line::raw(""));
                    }
                    body.push(help_section_divider(heading, inner_w, th));
                    for &(shortcut, desc) in kept {
                        body.extend(help_entry_lines(shortcut, desc, inner_w));
                    }
                }
                body
            };

            let glossary_body = || {
                let render_group =
                    |body: &mut Vec<Line<'static>>,
                     heading: &str,
                     entries: &[(&str, Color, &str, &str)]| {
                        let kept: Vec<&(&str, Color, &str, &str)> = entries
                            .iter()
                            .filter(|(_, _, label, desc)| matches(&[label, desc, heading]))
                            .collect();
                        if kept.is_empty() {
                            return;
                        }
                        if !body.is_empty() {
                            body.push(Line::raw(""));
                        }
                        body.push(help_section_divider(heading, inner_w, th));
                        for &&(icon, color, label, desc) in kept.iter() {
                            body.extend(glossary_entry_lines(icon, color, label, desc, inner_w));
                        }
                    };

                let mut body: Vec<Line<'static>> = Vec::new();
                let entries: [(&str, Color, &str, &str); 5] = [
                    (
                        "\u{25cf}",
                        th.subst,
                        s.glossary_label_literal,
                        s.glossary_desc_literal,
                    ),
                    (
                        "\u{25cf}",
                        th.ok,
                        s.glossary_label_loaded,
                        s.glossary_desc_loaded,
                    ),
                    (
                        "\u{25cf}",
                        th.pending,
                        s.glossary_label_pending,
                        s.glossary_desc_pending,
                    ),
                    (
                        "\u{25cf}",
                        th.err,
                        s.glossary_label_failed,
                        s.glossary_desc_failed,
                    ),
                    (
                        SHADOW_ICON,
                        th.pending,
                        s.glossary_label_shadowed,
                        s.glossary_desc_shadowed,
                    ),
                ];
                render_group(&mut body, s.glossary_heading, &entries);
                // A second group covers every other icon shown around the
                // app (list rows, tab bar, form editor) so this one tab is
                // a complete legend rather than just the substitution dots.
                let icon_entries: [(&str, Color, &str, &str); 9] = [
                    (
                        "\u{270e}",
                        th.accent,
                        s.glossary_label_modified,
                        s.glossary_desc_modified,
                    ),
                    (
                        "\u{271a}",
                        th.ok,
                        s.glossary_label_added,
                        s.glossary_desc_added,
                    ),
                    (
                        "\u{2713}",
                        th.ok,
                        s.glossary_label_passed,
                        s.glossary_desc_passed,
                    ),
                    (
                        "\u{2717}",
                        th.err,
                        s.glossary_label_run_failed,
                        s.glossary_desc_run_failed,
                    ),
                    (
                        "\u{2026}",
                        th.pending,
                        s.glossary_label_running,
                        s.glossary_desc_running,
                    ),
                    (GIT_ICON, th.text, s.glossary_label_git, s.glossary_desc_git),
                    (
                        LINK_ICON,
                        th.dim,
                        s.glossary_label_linked,
                        s.glossary_desc_linked,
                    ),
                    (
                        FOLDER_ICON,
                        th.accent,
                        s.glossary_label_folder,
                        s.glossary_desc_folder,
                    ),
                    (
                        "\u{2039}\u{203a}",
                        th.dim,
                        s.glossary_label_scroll_hint,
                        s.glossary_desc_scroll_hint,
                    ),
                ];
                render_group(&mut body, s.glossary_heading_icons, &icon_entries);
                body
            };

            let reports_body = || {
                // Render one titled group whose descriptions all align to that
                // group's own widest key (so long grammar left-hand sides don't
                // shove their descriptions out of line with the short ones).
                // Under an active filter, only matching rows are kept and a
                // group with no matches is dropped entirely.
                let group = |body: &mut Vec<Line<'static>>,
                             heading: &'static str,
                             entries: &[(&'static str, &'static str)]| {
                    let kept: Vec<&(&'static str, &'static str)> = entries
                        .iter()
                        .filter(|(k, d)| matches(&[k, d, heading]))
                        .collect();
                    if kept.is_empty() {
                        return;
                    }
                    if !body.is_empty() {
                        body.push(Line::raw(""));
                    }
                    body.push(help_section_divider(heading, inner_w, th));
                    let key_col = kept
                        .iter()
                        .map(|(k, _)| k.chars().count())
                        .max()
                        .unwrap_or(0)
                        .clamp(6, 34);
                    for &&(code, desc) in kept.iter() {
                        body.extend(help_entry_lines_col(code, desc, key_col, inner_w));
                    }
                };

                let mut body: Vec<Line<'static>> = Vec::new();
                // The prose intro is orientation, not a searchable entry, so it's
                // hidden while filtering to keep the results tight.
                if !filtering {
                    body.push(help_section_divider(
                        s.help_reports_about_heading,
                        inner_w,
                        th,
                    ));
                    for para in [s.help_reports_about_1, s.help_reports_about_2] {
                        body.push(Line::from(Span::styled(para, Style::default().fg(th.text))));
                        body.push(Line::raw(""));
                    }
                    // Drop the trailing blank so the first group's own leading
                    // blank isn't doubled up.
                    body.pop();
                }

                group(
                    &mut body,
                    s.help_reports_shortcuts_heading,
                    &[
                        ("Shift+R", s.help_report_new),
                        ("e", s.help_report_edit),
                        ("Enter", s.help_report_nodes),
                        ("r / F5", s.help_report_run),
                        ("d", s.help_report_dry_run),
                        ("v", s.help_report_view),
                        ("a / Del / Shift+↑↓ (nodes)", s.help_report_nodes_edit),
                        ("Enter (nodes)", s.help_report_nodes_forms),
                        ("Tab / Shift+Tab", s.help_report_focus_cycle),
                        ("↑↓ / Enter (ws tree)", s.help_report_workspace_tree),
                        ("Ctrl+S", s.help_report_export),
                        ("B", s.help_report_baseline),
                        ("c", s.help_report_columns),
                        ("Esc", s.help_report_leave_edit),
                        ("Ctrl+←/→", s.help_report_word_move),
                        ("→", s.help_report_complete),
                    ],
                );
                group(
                    &mut body,
                    s.help_reports_grammar_heading,
                    &[
                        ("# collection: PATH", s.help_grammar_collection),
                        ("# name: TEXT / TEXT_{time}", s.help_grammar_name),
                        ("# environment: NAME", s.help_grammar_environment),
                        ("KEY = value", s.help_grammar_assign),
                        ("REQUEST NAME", s.help_grammar_request),
                        ("REPORT REQUEST NAME [AS COL]", s.help_grammar_report),
                        ("REPORT REQUEST NAME SHOW(a, b)", s.help_grammar_show),
                        ("REPORT REQUEST NAME HIDE(a, b)", s.help_grammar_hide),
                        (
                            "REPORT COL AS N STATISTICS(MEAN)",
                            s.help_grammar_statistics,
                        ),
                        ("WITH Name: query [STATISTICS]", s.help_grammar_with),
                        ("Result", s.help_grammar_result),
                        ("PARALLEL[(n)] FOR …", s.help_grammar_parallel),
                    ],
                );
                group(
                    &mut body,
                    s.help_reports_loops_heading,
                    &[
                        ("FOR VAR IN SRC … END", s.help_grammar_for),
                        ("FOR (A, B) IN SRC", s.help_grammar_for_tuple),
                        ("FOR (A, _, ...) IN SRC", s.help_grammar_pattern),
                        ("LIST NAME = SRC", s.help_grammar_list),
                        ("[ \"a\", (\"x\",\"y\") ]", s.help_grammar_list_literal),
                        ("FILES \"dir\" [MATCH \"g\"]", s.help_grammar_files),
                        ("FOLDERS \"dir\" [WITH r=\"g\"]", s.help_grammar_folders),
                        ("TUPLES FROM \"data.csv\"", s.help_grammar_tuples),
                        ("ZIP(a, b, …)", s.help_grammar_zip),
                        ("CONCAT(a, b, …)", s.help_grammar_concat),
                        ("ENVS \"au\", \"eu\"", s.help_grammar_envs),
                        (
                            "BASELINE(FILE(\"snap.baseline\"))",
                            s.help_grammar_baseline_file,
                        ),
                    ],
                );
                body
            };

            let mut lines = vec![
                Line::styled(
                    s.help_heading,
                    Style::default().fg(th.accent).add_modifier(Modifier::BOLD),
                ),
                tab_bar(*tab),
            ];
            // Show the active filter (and let the user see what they've typed)
            // on its own line just under the tab strip.
            if filtering {
                lines.push(Line::from(vec![
                    Span::styled(s.help_filter_label, Style::default().fg(th.dim)),
                    Span::styled(
                        app.help_query.clone(),
                        Style::default().fg(th.accent).add_modifier(Modifier::BOLD),
                    ),
                ]));
            }
            let body = match *tab {
                0 => shortcuts_body(),
                1 => glossary_body(),
                _ => reports_body(),
            };
            // A filter that matches nothing on this tab would otherwise leave a
            // blank void — say so, and point out the filter spans every tab.
            if filtering && body.is_empty() {
                lines.push(Line::raw(""));
                lines.push(Line::styled(
                    s.help_filter_no_matches,
                    Style::default().fg(th.dim),
                ));
            } else {
                lines.extend(body);
            }

            // With no filter, all three tabs share one fixed height (the
            // tallest body) so switching tabs doesn't resize the popup out
            // from under the user — a stable box makes the tab strip read as
            // one steady window rather than a jarring resize on every switch.
            // Under a filter the bodies shrink to the matches, so the popup is
            // sized to the current (filtered) content instead. `centered_rect`
            // further caps this to the terminal's own height on small
            // terminals, in which case the body is scrolled (Up/Down) with a
            // scrollbar on the right border rather than clipping off the
            // bottom silently.
            let content_len = if filtering {
                lines.len()
            } else {
                lines
                    .len()
                    .max(2 + shortcuts_body().len())
                    .max(2 + glossary_body().len())
                    .max(2 + reports_body().len())
            };
            let box_h = content_len as u16 + 2;
            let area = centered_rect(box_w, box_h, f.area());
            f.render_widget(Clear, area);
            let title = format!(
                "{} — {}",
                s.help_title,
                match *tab {
                    0 => s.help_tab_shortcuts,
                    1 => s.help_tab_glossary,
                    _ => s.help_tab_reports,
                }
            );
            let visible_rows = area.height.saturating_sub(2) as usize;
            let max_scroll = content_len.saturating_sub(visible_rows) as u16;
            if app.help_scroll > max_scroll {
                app.help_scroll = max_scroll;
            }
            let scroll = app.help_scroll;
            f.render_widget(
                Paragraph::new(lines)
                    .block(panel(title, true, th))
                    .wrap(Wrap { trim: false })
                    .scroll((scroll, 0)),
                area,
            );
            if max_scroll > 0 {
                let bar_area = Rect {
                    x: area.x + area.width - 1,
                    y: area.y + 1,
                    width: 1,
                    height: visible_rows as u16,
                };
                draw_scrollbar(f, bar_area, content_len, visible_rows, scroll as usize, th);
            }
            if scroll <= 1 {
                let tab_y = area.y + 1 + (1 - scroll);
                if tab_y < area.bottom().saturating_sub(1) {
                    let labels = [
                        (0, s.help_tab_shortcuts),
                        (1, s.help_tab_glossary),
                        (2, s.help_tab_reports),
                    ];
                    let mut x = area.x.saturating_add(1);
                    for (i, label) in labels {
                        let w = label.chars().count() as u16 + 2;
                        if x < area.right().saturating_sub(1) {
                            app.push_mouse_hit(
                                MouseLayer::Overlay,
                                Rect::new(x, tab_y, w.min(area.right() - 1 - x), 1),
                                MouseHitTarget::HelpTab(i),
                            );
                        }
                        x = x.saturating_add(w).saturating_add(1);
                    }
                }
            }
            app.push_mouse_hit(
                MouseLayer::Overlay,
                area,
                MouseHitTarget::Scroll(MouseScrollTarget::Help),
            );
        }
        Overlay::ReportColumns(picker) => {
            draw_report_columns_overlay(f, picker, s, th, Some(app));
        }
        Overlay::ReportBind(picker) => {
            draw_report_bind_overlay(f, picker, s, th, Some(app));
        }
        Overlay::ReportNodeMenu(menu) => {
            draw_report_node_menu_overlay(f, menu, s, th, Some(app));
        }
        Overlay::ReportNodeRequest(form) => {
            draw_report_node_request_overlay(f, form, s, th, Some(app));
        }
        Overlay::ReportNodeEnvs(form) => {
            draw_report_node_envs_overlay(f, form, s, th, Some(app));
        }
        Overlay::ReportNodeWithField(form) => {
            draw_report_node_with_field_overlay(f, form, s, th, Some(app));
        }
        Overlay::ReportNodeAssign(form) => {
            draw_report_node_assign_overlay(f, form, s, th, Some(app));
        }
        Overlay::ReportNodeList(form) => {
            draw_report_node_list_overlay(f, form, s, th, Some(app));
        }
        Overlay::ReportNodeVars(form) => {
            draw_report_node_vars_overlay(f, form, s, th, Some(app));
        }
        Overlay::ReportNodeComputed(form) => {
            draw_report_node_computed_overlay(f, form, s, th, Some(app));
        }
        Overlay::ReportNodeFiles(form) => {
            draw_report_node_files_overlay(f, form, s, th, Some(app));
        }
        Overlay::Prompt {
            kind,
            editor,
            title,
            mask,
            reset_to,
            secret_intact,
            secret_checkbox,
        } => {
            let ml = editor.multiline;
            // Grow the box by one line to fit the "still secret?" checkbox row.
            let h = if ml {
                14
            } else if secret_checkbox.is_some() {
                5
            } else {
                3
            };
            // Build the title/hint first so the box can be widened to fit it:
            // long titles (e.g. the workspace "New report (path relative to
            // workspace)" prompt — longer still in other languages) were being
            // clipped by the panel border on the fixed-width single-line box.
            let mut hint = if matches!(kind, PromptKind::Raw(_)) {
                format!("{title}  ({})", s.raw_mode_hint)
            } else if matches!(kind, PromptKind::RawJson(_)) {
                format!("{title}  ({})", s.raw_json_hint)
            } else if ml {
                format!("{title}  ({})", s.prompt_save_hint_ml)
            } else {
                format!("{title}  ({})", s.prompt_save_hint_sl)
            };
            // Offer a reset only when the field has been changed from its
            // originally-loaded value.
            if reset_to
                .as_deref()
                .is_some_and(|orig| orig != editor.text())
            {
                hint.push_str(&format!("  ·  {}", s.prompt_reset_hint));
            }
            if secret_checkbox.is_some() {
                hint.push_str(&format!("  ·  {}", s.env_still_secret_hint));
            }
            let base_w = if ml {
                (f.area().width * 8 / 10).max(30)
            } else {
                64
            };
            // Widen to fit the title on the top border — 2 columns for the
            // corners plus a little breathing room — but never past the
            // terminal width (centered_rect clamps too).
            let title_w = Line::from(hint.as_str()).width() as u16;
            let w = base_w.max(title_w.saturating_add(4)).min(f.area().width);
            let area = centered_rect(w, h, f.area());
            f.render_widget(Clear, area);
            let block = panel(hint, true, th);
            let inner = block.inner(area);
            f.render_widget(block, area);
            // Reserve the last inner row for the checkbox when applicable.
            let (editor_area, checkbox_area) = if let Some(checked) = secret_checkbox {
                let rows =
                    Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(inner);
                (rows[0], Some((rows[1], checked)))
            } else {
                (inner, None)
            };
            if *mask && *secret_intact {
                // Draw the untouched secret as a fixed-width mask so its length
                // is not leaked (the value itself is never shown).
                f.render_widget(
                    Paragraph::new(crate::environment::SECRET_MASK)
                        .style(Style::default().fg(th.text)),
                    editor_area,
                );
                let cx = editor_area.x + crate::environment::SECRET_MASK.chars().count() as u16;
                f.set_cursor_position(ratatui::layout::Position::new(
                    cx.min(editor_area.right().saturating_sub(1)),
                    editor_area.y,
                ));
            } else {
                render_editor(f, editor_area, editor, *mask, th);
                // Show the dimmed save-extension ghost (".hurl"/".vars") after the
                // filename when it hasn't been typed yet, so Tab/Right can complete
                // it. Only for single-line, unmasked file prompts that still fit.
                let ghost = kind.save_ghost();
                if !ml && !*mask && !ghost.is_empty() {
                    let text = editor.text();
                    let len = text.chars().count() as u16;
                    if !text.ends_with(ghost) && len < editor_area.width {
                        let gx = editor_area.x + len;
                        let avail = editor_area.right().saturating_sub(gx);
                        if avail > 0 {
                            let shown: String = ghost.chars().take(avail as usize).collect();
                            f.render_widget(
                                Paragraph::new(shown).style(Style::default().fg(th.dim)),
                                Rect::new(gx, editor_area.y, avail, 1),
                            );
                        }
                    }
                }
            }
            // Only Raw Mode's / Raw JSON Mode's editor supports mouse
            // click-drag text selection (see `TuiApp::on_mouse`); recording
            // this Rect for every other prompt kind would be harmless but
            // meaningless, so it's scoped to the two cases that actually
            // hit-test against it.
            app.prompt_editor_area = if matches!(kind, PromptKind::Raw(_) | PromptKind::RawJson(_))
            {
                editor_area
            } else {
                Rect::default()
            };
            app.push_mouse_hit(
                MouseLayer::Overlay,
                editor_area,
                MouseHitTarget::PromptEditor,
            );
            if let Some((area, checked)) = checkbox_area {
                let mark = if *checked { "[x]" } else { "[ ]" };
                let fg = if *checked { th.pending } else { th.ok };
                f.render_widget(
                    Paragraph::new(format!("{mark} {}", s.env_still_secret))
                        .style(Style::default().fg(fg)),
                    area,
                );
                app.push_mouse_hit(
                    MouseLayer::Overlay,
                    area,
                    MouseHitTarget::PromptSecretCheckbox,
                );
            }
        }
        Overlay::Browser(action, ex) => {
            let w = (f.area().width * 7 / 10).max(50);
            let h = (f.area().height * 7 / 10).max(10);
            let area = centered_rect(w, h, f.area());
            f.render_widget(Clear, area);
            app.set_mouse_layer(MouseLayer::Overlay);
            if action.is_save_to_folder() {
                // Reserve a bordered filename box at the bottom that the user
                // can Tab to and press Enter to save into the current folder.
                // The report-export picker also gets a one-line format strip
                // above it (CSV/JSON/HTML/XLSX; the active one derived from the
                // typed extension, cycled with ↑/↓ while the name is focused).
                let show_formats = *action == FileAction::SaveReportCsvChooseFolder;
                let rows = if show_formats {
                    Layout::vertical([
                        Constraint::Min(3),
                        Constraint::Length(1),
                        Constraint::Length(3),
                    ])
                    .split(area)
                } else {
                    Layout::vertical([Constraint::Min(3), Constraint::Length(3)]).split(area)
                };
                ex.widget().render_ref(rows[0], f.buffer_mut());
                let name_row = if show_formats { rows[2] } else { rows[1] };
                if show_formats {
                    draw_export_format_strip(f, rows[1], &app.browser_name.text(), s, th);
                }
                let focused = app.browser_name_focused;
                let label = if *action == FileAction::SaveWorkspaceChooseFolder {
                    s.browser_foldername_label
                } else {
                    s.browser_filename_label
                };
                let title = if focused {
                    format!("{label}  ({})", s.browser_name_hint)
                } else {
                    label.to_string()
                };
                // Use the shared themed panel (accent border + bold title when
                // focused, dim otherwise, panel background) so the field looks
                // like every other bordered box in the app.
                let block = panel(title, focused, th);
                let inner = block.inner(name_row);
                f.render_widget(block, name_row);
                if focused {
                    render_editor(f, inner, &app.browser_name, false, th);
                } else {
                    // The folder list owns the cursor while unfocused; render
                    // the pending name statically (no caret) so it's clear the
                    // field isn't the active pane.
                    f.render_widget(
                        Paragraph::new(app.browser_name.text()).style(Style::default().fg(th.dim)),
                        inner,
                    );
                }
                app.push_mouse_hit(MouseLayer::Overlay, inner, MouseHitTarget::BrowserNameField);
                register_browser_hits(app, ex, rows[0]);
            } else if !app.browser_query.is_empty() {
                // An active type-to-filter query gets a one-line strip beneath
                // the list so it's obvious the list is being filtered (and by
                // what) rather than mysteriously short.
                let rows =
                    Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).split(area);
                ex.widget().render_ref(rows[0], f.buffer_mut());
                register_browser_hits(app, ex, rows[0]);
                f.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::styled(s.browser_filter_label, Style::default().fg(th.dim)),
                        Span::styled(
                            app.browser_query.clone(),
                            Style::default().fg(th.accent).add_modifier(Modifier::BOLD),
                        ),
                    ]))
                    // Fill the theme panel background (like the export-format
                    // strip and the Help filter) so the strip blends with the
                    // explorer's own `th.panel` block instead of showing the
                    // terminal's default background.
                    .style(Style::default().bg(th.panel)),
                    rows[1],
                );
            } else {
                ex.widget().render_ref(area, f.buffer_mut());
                register_browser_hits(app, ex, area);
            }
        }
        Overlay::NewRequest(form) => {
            draw_new_request_with_hits(f, form, s, th, app.enhanced_keys, Some(app))
        }
        Overlay::EnvVarForm(form) => draw_env_var_form(f, form, s, th, Some(app)),
        Overlay::RemoteGit(w) => draw_remote_wizard_with_hits(f, w, s, th, Some(app)),
        Overlay::PostmanImport(w) => draw_postman_wizard(f, w, s, th),
        Overlay::GitSave(w) => draw_git_save_wizard_with_hits(f, w, s, th, Some(app)),
        Overlay::EnvPopup(popup) => draw_env_popup(f, app, popup, s, th),
        Overlay::EnvLinkPicker(picker) => draw_env_link_picker(f, app, picker, s, th),
        Overlay::EnvCollision(collision) => draw_env_collision(f, collision, s, th, Some(app)),
        Overlay::WorkspacePicker(picker) => draw_workspace_picker(f, picker, s, th, Some(app)),
        Overlay::CloseGitWorkspace { path, sel, .. } => {
            let question = s
                .close_git_workspace_q
                .replace("{p}", &path.to_string_lossy());
            let choices = [
                s.close_git_workspace_keep,
                s.close_git_workspace_delete,
                s.close_git_workspace_cancel,
            ];
            draw_confirm_popup(f, &question, &choices, *sel, th, Some(app));
        }
        Overlay::WorkspaceReloadConfirm { reload, sel, .. } => {
            let ref_label = if reload.origin.ref_kind == crate::git_remote::RefKind::Branch {
                s.git_branches
            } else {
                s.git_tags
            };
            let question = s
                .workspace_reload_confirm_q
                .replace("{name}", &reload.tab_name)
                .replace(
                    "{ref}",
                    &format!("[{ref_label}] {}", reload.origin.ref_name),
                )
                .replace("{url}", &reload.origin.repo_url);
            draw_confirm_popup(
                f,
                &question,
                &[s.confirm_yes, s.confirm_no],
                *sel,
                th,
                Some(app),
            );
        }
        Overlay::WorkspaceReloadLoading { idx } => {
            let name = app
                .collections
                .get(*idx)
                .map(|c| c.name.as_str())
                .unwrap_or("");
            let text = format!("{} ({name})", s.workspace_reload_loading);
            let w = (text.chars().count() as u16 + 4).max(30);
            let area = centered_rect(w, 5, f.area());
            f.render_widget(Clear, area);
            let block = Block::default().borders(Borders::ALL);
            f.render_widget(
                Paragraph::new(text)
                    .block(block)
                    .wrap(Wrap { trim: true })
                    .alignment(ratatui::layout::Alignment::Center),
                area,
            );
        }
        Overlay::WorkspaceStorageChoice { sel, .. } => {
            let choices = [s.git_workspace_storage_temp, s.git_workspace_storage_choose];
            draw_confirm_popup(f, s.git_workspace_storage_q, &choices, *sel, th, Some(app));
        }
        Overlay::WorkspaceGitSaveUnsaved { sel, .. } => {
            let choices = [
                s.git_save_ws_unsaved_save,
                s.git_save_ws_unsaved_ignore,
                s.git_save_ws_unsaved_cancel,
            ];
            draw_confirm_popup(f, s.git_save_ws_unsaved_q, &choices, *sel, th, Some(app));
        }
        Overlay::WorkspaceSwitchUnsaved { sel, .. } => {
            let choices = [
                s.ws_switch_unsaved_save,
                s.ws_switch_unsaved_discard,
                s.ws_switch_unsaved_cancel,
            ];
            draw_confirm_popup(f, s.ws_switch_unsaved_q, &choices, *sel, th, Some(app));
        }
        // Handled by the early-return above — unreachable in practice.
        Overlay::ReportCellPopup { .. } => unreachable!("ReportCellPopup is drawn above"),
    }
}

fn register_browser_hits(app: &TuiApp, ex: &ratatui_explorer::FileExplorer, area: Rect) {
    let inner = Rect {
        x: area.x.saturating_add(1),
        y: area.y.saturating_add(1),
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };
    app.push_mouse_hit(
        MouseLayer::Overlay,
        inner,
        MouseHitTarget::Scroll(MouseScrollTarget::BrowserList),
    );
    let len = ex.files().len();
    if len == 0 || inner.height == 0 {
        return;
    }
    let visible = inner.height as usize;
    let selected = ex.selected_idx().min(len - 1);
    let first = if selected >= visible {
        selected + 1 - visible
    } else {
        0
    };
    for row in first..len.min(first + visible) {
        app.push_mouse_hit(
            MouseLayer::Overlay,
            Rect::new(inner.x, inner.y + (row - first) as u16, inner.width, 1),
            MouseHitTarget::BrowserListRow(row),
        );
    }
}

/// The "add environment variable" popup: a two-column `Key | Value` table with
/// one editable row. The focused cell shows a cursor and a subtle background.
pub(crate) fn draw_env_var_form(
    f: &mut Frame,
    form: &EnvVarForm,
    s: &Strings,
    th: &Theme,
    app: Option<&TuiApp>,
) {
    let area = centered_rect(70, 7, f.area());
    f.render_widget(Clear, area);
    let block = panel(s.env_add_var_title.to_string(), true, th);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = Layout::vertical([
        Constraint::Length(1), // column headers
        Constraint::Length(1), // input cells
        Constraint::Min(0),    // spacer
        Constraint::Length(1), // key hint
    ])
    .split(inner);
    let split = |r: Rect| {
        Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)])
            .spacing(2)
            .split(r)
    };
    let head = split(rows[0]);
    let cells = split(rows[1]);
    let lbl = |t: &str| {
        Paragraph::new(Span::styled(
            t.to_string(),
            Style::default().fg(th.dim).add_modifier(Modifier::BOLD),
        ))
    };
    f.render_widget(lbl(s.hdr_key), head[0]);
    f.render_widget(lbl(s.hdr_value), head[1]);
    render_line_field(f, cells[0], &form.key, !form.on_value, false, th);
    render_line_field(f, cells[1], &form.value, form.on_value, false, th);
    if let Some(app) = app {
        app.set_mouse_layer(MouseLayer::Overlay);
        app.push_mouse_hit(
            MouseLayer::Overlay,
            cells[0],
            MouseHitTarget::EnvVarField(false),
        );
        app.push_mouse_hit(
            MouseLayer::Overlay,
            cells[1],
            MouseHitTarget::EnvVarField(true),
        );
    }

    let hint = format!("Tab {} · {}", s.env_var_switch, s.prompt_save_hint_sl);
    f.render_widget(
        Paragraph::new(Span::styled(hint, Style::default().fg(th.dim))),
        rows[3],
    );
}

/// Picker (opened with 'p' in the Requests/List pane) to link/unlink a Global
/// Environment to the current collection: "(none)" plus every Global
/// Environment name, with the currently-linked one marked.
pub(crate) fn draw_env_link_picker(
    f: &mut Frame,
    app: &TuiApp,
    picker: &EnvLinkPicker,
    s: &Strings,
    th: &Theme,
) {
    let linked = app.collections.get(picker.ci).and_then(|c| c.linked_env_id);
    let mut labels: Vec<String> = vec![s.env_link_none.to_string()];
    labels.extend(app.global_envs.iter().map(|e| {
        if Some(e.id) == linked {
            format!("\u{2713} {}", e.name)
        } else {
            e.name.clone()
        }
    }));
    let items: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
    draw_menu_popup(
        f,
        s.env_link_picker_title,
        &items,
        picker.sel,
        th,
        Some(app),
    );
}

/// The 4-choice popup shown when loading an environment whose name collides
/// with one already in the Global Environments list.
pub(crate) fn draw_env_collision(
    f: &mut Frame,
    collision: &EnvCollision,
    s: &Strings,
    th: &Theme,
    app: Option<&TuiApp>,
) {
    let items = [
        s.env_collision_replace,
        s.env_collision_keep_both,
        s.env_collision_abort,
        s.env_collision_rename,
    ];
    draw_menu_popup(f, s.env_collision_title, &items, collision.sel, th, app);
}

/// The recursive file-tree popup used both to auto-prompt (when a Workspace
/// tab has no collection chosen yet) and on-demand (global `w` key) to pick
/// which `.hurl`/`.json` file inside a Workspace folder to load. Directory
/// rows are unselectable visual grouping (bold, folder icon); file rows are
/// the only ones `nav()`/Enter act on.
pub(crate) fn draw_workspace_picker(
    f: &mut Frame,
    picker: &WorkspacePickerState,
    s: &Strings,
    th: &Theme,
    app: Option<&TuiApp>,
) {
    let w = (f.area().width * 7 / 10).max(50);
    let h = (f.area().height * 7 / 10).max(10);
    let area = centered_rect(w, h, f.area());
    f.render_widget(Clear, area);
    let filter_label = if picker.filter_hurl_json {
        s.workspace_filter_on
    } else {
        s.workspace_filter_off
    };
    let title = format!(
        "{} [{filter_label}] — {}",
        s.workspace_picker_title,
        picker.root.display()
    );
    let block = panel(title, true, th);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(inner);
    if picker.entries.is_empty() {
        f.render_widget(
            Paragraph::new(s.workspace_no_files).style(Style::default().fg(th.dim)),
            rows[0],
        );
        return;
    }
    let items: Vec<ListItem> = picker
        .entries
        .iter()
        .map(|e| {
            let indent = "  ".repeat(e.depth);
            if e.is_dir {
                ListItem::new(Line::styled(
                    format!("{indent}{FOLDER_ICON} {}/", e.display_name),
                    Style::default().fg(th.accent).add_modifier(Modifier::BOLD),
                ))
            } else if crate::workspace::is_report_file(&e.path) {
                ListItem::new(Line::styled(
                    format!("{indent}{REPORT_ICON} {}", e.display_name),
                    Style::default().fg(th.accent),
                ))
            } else {
                ListItem::new(Line::styled(
                    format!("{indent}{}", e.display_name),
                    Style::default().fg(th.text),
                ))
            }
        })
        .collect();
    let list = List::new(items)
        .highlight_style(
            Style::default()
                .bg(th.accent)
                .fg(th.bg)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("› ");
    let mut st = ListState::default();
    st.select(Some(picker.selected));
    f.render_stateful_widget(list, rows[0], &mut st);
    if let Some(app) = app {
        app.set_mouse_layer(MouseLayer::Overlay);
        app.push_mouse_hit(
            MouseLayer::Overlay,
            rows[0],
            MouseHitTarget::Scroll(MouseScrollTarget::WorkspacePicker),
        );
        let first = st.offset();
        let visible = rows[0].height as usize;
        for row in first..picker.entries.len().min(first + visible) {
            app.push_mouse_hit(
                MouseLayer::Overlay,
                Rect::new(
                    rows[0].x,
                    rows[0].y + (row - first) as u16,
                    rows[0].width,
                    1,
                ),
                MouseHitTarget::WorkspacePickerRow(row),
            );
        }
    }
    let hint = Paragraph::new(Line::styled(
        match picker.mode {
            WsPickerMode::AddRequest => s.workspace_picker_hint_add,
            WsPickerMode::MoveRequest => s.workspace_picker_hint_move,
            WsPickerMode::CopyRequest => s.workspace_picker_hint_copy,
            WsPickerMode::Browse => s.workspace_picker_hint,
        },
        Style::default().fg(th.dim),
    ));
    f.render_widget(hint, rows[1]);
}

/// Renders a menu label written with the "(X)" mnemonic convention (see
/// `app::menu_mnemonic`, which parses the very same strings for key
/// matching) as plain text with the mnemonic letter underlined and the
/// surrounding parens dropped — e.g. `"En(v)ironment…"` renders as
/// "Environment…" with the "v" underlined. Replaces the old bracketed look
/// (which several users found visually distracting) without touching the
/// underlying i18n strings or the key-matching logic, which both keep
/// working against the original "(X)" text. Labels with no such marker
/// (e.g. Preferences toggle rows) render unchanged.
pub(crate) fn mnemonic_spans(label: &str, style: Style) -> Vec<Span<'static>> {
    // Only a mnemonic marker when exactly one char sits between the
    // parens — matches `app::menu_mnemonic`'s own check.
    if let Some(open) = label.find('(')
        && let Some(close) = label[open + 1..]
            .find(')')
            .map(|r| open + 1 + r)
            .filter(|&close| close == open + 2)
    {
        let before = label[..open].to_string();
        let letter = label[open + 1..close].to_string();
        let after = label[close + 1..].to_string();
        let mut spans = Vec::with_capacity(3);
        if !before.is_empty() {
            spans.push(Span::styled(before, style));
        }
        spans.push(Span::styled(
            letter,
            style.add_modifier(Modifier::UNDERLINED),
        ));
        if !after.is_empty() {
            spans.push(Span::styled(after, style));
        }
        return spans;
    }
    vec![Span::styled(label.to_string(), style)]
}

pub(crate) fn draw_menu_popup(
    f: &mut Frame,
    title: &str,
    items: &[&str],
    sel: usize,
    th: &Theme,
    app: Option<&TuiApp>,
) {
    if let Some(app) = app {
        app.set_mouse_layer(MouseLayer::Overlay);
    }
    let width = items
        .iter()
        .map(|i| i.len())
        .max()
        .unwrap_or(10)
        .max(title.len()) as u16
        + 6;
    let height = items.len() as u16 + 2;
    let area = centered_rect(width, height, f.area());
    f.render_widget(Clear, area);
    let list_items: Vec<ListItem> = items
        .iter()
        .map(|i| ListItem::new(Line::from(mnemonic_spans(i, Style::default().fg(th.text)))))
        .collect();
    let list = List::new(list_items)
        .block(panel(title.to_string(), true, th))
        .highlight_style(
            Style::default()
                .bg(th.accent)
                .fg(th.bg)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("› ");
    let mut st = ListState::default();
    st.select(Some(sel));
    f.render_stateful_widget(list, area, &mut st);
    if let Some(app) = app {
        let inner = Rect {
            x: area.x.saturating_add(1),
            y: area.y.saturating_add(1),
            width: area.width.saturating_sub(2),
            height: area.height.saturating_sub(2),
        };
        app.push_mouse_hit(
            MouseLayer::Overlay,
            inner,
            MouseHitTarget::Scroll(MouseScrollTarget::OverlayList),
        );
        let first = st.offset();
        let visible = inner.height as usize;
        for row in first..items.len().min(first + visible) {
            app.push_mouse_hit(
                MouseLayer::Overlay,
                Rect::new(inner.x, inner.y + (row - first) as u16, inner.width, 1),
                MouseHitTarget::OverlayRow(row),
            );
        }
    }
}

/// A small confirmation popup with 2+ choices, laid out in a row and
/// selected with Left/Right/Up/Down/Enter. `sel` is the highlighted index.
/// `question` may contain embedded `\n`s for explicit line breaks (e.g. to
/// put a long path on its own line) — each is wrapped independently.
pub(crate) fn draw_confirm_popup(
    f: &mut Frame,
    question: &str,
    choices: &[&str],
    sel: usize,
    th: &Theme,
    app: Option<&TuiApp>,
) {
    if let Some(app) = app {
        app.set_mouse_layer(MouseLayer::Overlay);
    }
    let choices_len: usize = choices.iter().map(|c| c.len() + 3).sum();
    let q_lines: Vec<&str> = question.split('\n').collect();
    let max_line_len = q_lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    let width = max_line_len.max(choices_len + 4).clamp(24, 76) as u16 + 4;
    let width = width.min(f.area().width.max(1));
    // Grow the popup vertically so the whole (possibly long) question fits: the
    // Save confirmation is much longer than the Exit/Clear ones. Estimate the
    // wrapped line count for the inner text width (matches `Wrap`'s word
    // breaks), for each explicit line of `question` in turn.
    let text_w = width.saturating_sub(2).max(1) as usize;
    let mut lines = 0usize;
    for q_line in &q_lines {
        let mut line_wraps = 1usize;
        let mut col = 0usize;
        for word in q_line.split_whitespace() {
            let wlen = word.chars().count();
            if col == 0 {
                col = wlen;
            } else if col + 1 + wlen <= text_w {
                col += 1 + wlen;
            } else {
                line_wraps += 1;
                col = wlen;
            }
        }
        lines += line_wraps;
    }
    // question lines + a blank spacer + the choices row + the two borders.
    let height = (lines as u16).saturating_add(4);
    let area = centered_rect(width, height, f.area());
    f.render_widget(Clear, area);
    let block = panel("".to_string(), true, th);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(inner);
    f.render_widget(
        Paragraph::new(question)
            .style(Style::default().fg(th.text))
            .wrap(Wrap { trim: true }),
        rows[0],
    );
    let hl = Style::default()
        .bg(th.accent)
        .fg(th.bg)
        .add_modifier(Modifier::BOLD);
    let normal = Style::default().fg(th.text);
    let mut spans = Vec::with_capacity(choices.len() * 2);
    for (i, choice) in choices.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("   "));
        }
        spans.push(Span::styled(
            format!(" {choice} "),
            if sel == i { hl } else { normal },
        ));
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)).alignment(ratatui::layout::Alignment::Center),
        rows[1],
    );
    if let Some(app) = app {
        let line_w = choices.iter().map(|c| c.chars().count() + 2).sum::<usize>()
            + 3 * choices.len().saturating_sub(1);
        let mut x = rows[1]
            .x
            .saturating_add(rows[1].width.saturating_sub(line_w as u16) / 2);
        for (i, choice) in choices.iter().enumerate() {
            if i > 0 {
                x = x.saturating_add(3);
            }
            let w = choice.chars().count() as u16 + 2;
            app.push_mouse_hit(
                MouseLayer::Overlay,
                Rect::new(x, rows[1].y, w, 1),
                MouseHitTarget::ConfirmChoice(i),
            );
            x = x.saturating_add(w);
        }
    }
}
