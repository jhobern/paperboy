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
use super::remote::*;
use super::theme::*;
use super::wrapcache::TextPos;
use std::sync::Arc;
use tui_panel_select::WrapMarker;

/// Marks a collection/environment title as loaded from git — shown before the
/// name whenever `Collection::git_origin` / `env_git_origin` is set.
pub(crate) const GIT_ICON: &str = "\u{2387}";
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
/// Marks a subfolder row in the request list tree, and (in the request
/// editor's form) hints that a File-kind field's Value opens a file picker
/// on Enter.
pub(crate) const FOLDER_ICON: &str = "\u{1F4C1}";
/// Chevrons on a Workspace collection file row: expanded (requests inlined)
/// vs collapsed.
const COLLECTION_OPEN_ICON: &str = "\u{25BE}"; // ▾
const COLLECTION_CLOSED_ICON: &str = "\u{25B8}"; // ▸

/// A rendered row of the request list, unifying the ordinary title-folder
/// tree ([`tree::Row`]) and the Workspace file-tree ([`WsRow`]) so
/// [`draw_collection_left`] can lay both out with one loop. `Entry.indent`
/// nudges a Workspace request under its collection's file row.
enum LeftRow {
    Up,
    Folder(String),
    Collection { name: String, open: bool },
    Entry { idx: usize, indent: bool },
}

impl LeftRow {
    /// The list rows for tab `col`: the Workspace file-tree when it's bound to
    /// a folder, otherwise the title-folder tree.
    fn build(col: &Collection) -> Vec<LeftRow> {
        if col.is_workspace() {
            col.ws_rows()
                .into_iter()
                .map(|r| match r {
                    WsRow::Up => LeftRow::Up,
                    WsRow::Folder(name) => LeftRow::Folder(name),
                    WsRow::Collection { name, open, .. } => LeftRow::Collection { name, open },
                    WsRow::Request(idx) => LeftRow::Entry { idx, indent: true },
                })
                .collect()
        } else {
            col.rows()
                .into_iter()
                .map(|r| match r {
                    tree::Row::Up => LeftRow::Up,
                    tree::Row::Folder(name) => LeftRow::Folder(name),
                    tree::Row::Entry(idx) => LeftRow::Entry { idx, indent: false },
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
    draw_footer(f, rows[4], &s, &th, app.can_copy());

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
    spans.extend(mnemonic_spans(s.file_menu_label, base));
    spans.push(Span::raw("   "));
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
    // In the report view `focus` is pinned to `Pane::Tabs`, so the tab-bar
    // highlight follows the report's own focus flag instead: it's lit only when
    // `Tab` has rotated focus onto the tab list.
    let focused = if app.active_is_report() {
        app.report_tabbar_focus
    } else {
        app.focus == Pane::Tabs
    };
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
        spans.push(mk(name, app.active_tab == i));
        pos += w;
    }
    // Report tabs follow the collection tabs in the same strip (unified index:
    // `collections.len() + report_index`). A leading icon distinguishes them,
    // and a dirty marker flags unsaved source edits.
    let report_base = app.collections.len();
    for (r, rt) in app.reports.iter().enumerate() {
        spans.push(Span::raw("│"));
        pos += 1;
        // Unsaved edits get a trailing dot (with a leading space so it never
        // crowds the name); the report icon leads.
        let marker = if rt.report.dirty {
            format!(" {}", s.report_dirty_marker)
        } else {
            String::new()
        };
        let name = format!("{}{}{}", s.report_tab_icon, rt.report.name, marker);
        let idx = report_base + r;
        let w = name.chars().count() + 2;
        if app.active_tab == idx {
            active_start = pos;
            active_w = w;
        }
        spans.push(mk(name, app.active_tab == idx));
        pos += w;
    }
    let total_w = pos;
    // Content width available inside the panel borders.
    let avail = area.width.saturating_sub(2) as usize;

    let line = if total_w <= avail || avail == 0 {
        Line::from(spans)
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
        Line::from(out)
    };
    f.render_widget(
        Paragraph::new(line).block(panel(s.tabs_heading.to_string(), focused, th)),
        area,
    );
}

pub(crate) fn draw_body(f: &mut Frame, area: Rect, app: &mut TuiApp, s: &Strings, th: &Theme) {
    // A report tab takes the whole body (no list/environment/response panels),
    // per the design — so branch before the collection-tab layout below, which
    // indexes `app.collections[app.active_tab]` and would panic on a report's
    // unified tab index.
    if app.active_is_report() {
        super::reports::draw_report_body(f, area, app, s, th);
        return;
    }
    let cols =
        Layout::horizontal([Constraint::Length(app.list_width), Constraint::Min(10)]).split(area);
    let ci = app.active_tab;
    draw_collection_left(f, cols[0], app, ci, s, th);
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
    // Columns available for the URL text (after the border, highlight symbol,
    // user-added marker and the fixed method column). Recorded so h-scrolling
    // can be clamped to stop once the URL's end is visible (no blank overscroll).
    let url_w = panes[0].width.saturating_sub(2 + 2 + 2 + 5);
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
            LeftRow::Folder(name) => ListItem::new(Line::from(Span::styled(
                format!("{FOLDER_ICON} {name}/"),
                Style::default().fg(th.accent).add_modifier(Modifier::BOLD),
            ))),
            LeftRow::Collection { name, open } => {
                let chevron = if *open {
                    COLLECTION_OPEN_ICON
                } else {
                    COLLECTION_CLOSED_ICON
                };
                ListItem::new(Line::from(Span::styled(
                    format!("{chevron} {name}"),
                    Style::default().fg(th.text).add_modifier(Modifier::BOLD),
                )))
            }
            LeftRow::Entry { idx, indent } => {
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
                // Workspace request rows are indented one level so they read
                // as children of their collection's file row.
                let mut spans = Vec::new();
                if *indent {
                    spans.push(Span::raw("  "));
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
    if col.is_workspace() {
        if !col.workspace_browse.is_empty() {
            title = format!("{title} › {}", col.workspace_browse.join(" › "));
        }
    } else if !col.folder.is_empty() {
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
    let mut run_hint = format!("{run_key} {} · Alt+F5 {}", s.foot_run, s.foot_run_all);
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
    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .bg(th.accent)
                .fg(th.bg)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("› ");
    let mut st = ListState::default();
    if !view_rows.is_empty() {
        st.select(Some(sel));
    }
    f.render_stateful_widget(list, panes[0], &mut st);

    // Environment panel
    draw_env_panel(f, panes[1], app, s, th);
}

pub(crate) fn draw_env_panel(f: &mut Frame, area: Rect, app: &TuiApp, s: &Strings, th: &Theme) {
    let focused = app.focus == Pane::GlobalEnv;
    // The activate/deactivate hint lives on this panel's bottom border (same
    // convention as the Requests list's Run/Run All hint) since it acts on
    // whichever row is selected here, regardless of which pane has focus.
    let activate_hint = format!("a {}", s.foot_env_activate);
    let block = panel(s.env_heading.to_string(), focused, th)
        .title_bottom(Line::styled(activate_hint, Style::default().fg(th.dim)));
    if app.global_envs.is_empty() {
        let p = Paragraph::new(vec![
            Line::styled(s.env_no_envs.to_string(), Style::default().fg(th.dim)),
            Line::styled(
                format!("{} \u{2192} {}", s.file_menu, s.load_environment),
                Style::default().fg(th.dim),
            ),
        ])
        .block(block)
        .wrap(Wrap { trim: false });
        f.render_widget(p, area);
        return;
    }
    let sel = app
        .global_env_idx
        .min(app.global_envs.len().saturating_sub(1));
    // Columns available for the name text (after the border, highlight
    // symbol and the active-marker column); used to clamp scrolling.
    let text_w = area.width.saturating_sub(2 + 2 + 2);
    app.global_env_scroll_w.set(text_w);
    let sel_len = app
        .global_envs
        .get(sel)
        .map(|e| e.name.chars().count())
        .unwrap_or(0);
    let max_scroll = sel_len.saturating_sub((text_w as usize).saturating_sub(1));
    let hscroll = (app.global_env_hscroll as usize).min(max_scroll);
    let items: Vec<ListItem> = app
        .global_envs
        .iter()
        .enumerate()
        .map(|(i, env)| {
            let is_active = app.active_env_id == Some(env.id);
            // Active: green name + a checkmark marker; git origin shows the
            // same ⎇ icon convention used elsewhere.
            let (marker, marker_fg) = if is_active {
                ("\u{2713} ", th.ok)
            } else {
                ("  ", th.dim)
            };
            let name_color = if is_active { th.ok } else { th.text };
            let selected = i == sel;
            let mark_fg = if selected { th.bg } else { marker_fg };
            let git_prefix = if env.git_origin.is_some() {
                format!("{GIT_ICON} ")
            } else {
                String::new()
            };
            let mut spans = vec![Span::styled(marker, Style::default().fg(mark_fg))];
            let full = format!("{git_prefix}{}", env.name);
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
        .highlight_style(Style::default().bg(th.accent).add_modifier(Modifier::BOLD))
        .highlight_symbol("› ");
    let mut st = ListState::default();
    st.select(Some(sel));
    f.render_stateful_widget(list, area, &mut st);
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
    } else {
        app.main_scrollbar_area = Rect::default();
    }

    // Record the panel's Rect and shadow-icon positions so mouse selection can
    // map coordinates back to real, copyable text — scoped to this panel only.
    app.main_text_area = text_area;
    app.main_shadow_icon_positions = shadow_positions;

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

    // The in-flight spinner is still driven by the single shared "live"
    // response slot (no per-entry concept of "currently sending"), but once
    // a request has finished, the actual status/body/asserts shown always
    // come from the *selected entry's own* last response — not whichever
    // entry happened to finish last — so switching entries after a batch
    // "Run All" shows the right result for each one.
    let loading = app.response.lock().unwrap().loading;
    let entry = app
        .collections
        .get(ci)
        .and_then(|col| col.entries.get(col.selected_entry));
    let (status, status_text, body, error, asserts) =
        match entry.and_then(|e| e.last_response.as_ref()) {
            Some(r) => (
                r.status,
                r.status_text.clone(),
                r.body.clone(),
                r.error.clone(),
                r.assert_results.clone(),
            ),
            None => (0, String::new(), Arc::from(""), String::new(), Vec::new()),
        };

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
    if !error.is_empty() {
        app.resp_max_scroll = 0;
        app.resp_text_area = Rect::default();
        app.resp_panel
            .set_content(Arc::from(""), area.width.max(1) as usize);
        app.resp_panel.clear();
        app.resp_panel.set_scroll(0);
        app.resp_scrollbar_area = Rect::default();
        f.render_widget(
            Paragraph::new(format!("{} {error}", s.req_error_prefix))
                .style(Style::default().fg(th.err))
                .wrap(Wrap { trim: false }),
            inner,
        );
        return;
    }
    if status == 0 {
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

    // Layout: status (1) · asserts (capped, keeping ≥1 body row) · body (rest).
    let assert_h = (assert_lines.len() as u16).min(inner.height.saturating_sub(2));
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(assert_h),
        Constraint::Min(1),
    ])
    .split(inner);

    f.render_widget(Paragraph::new(Line::from(status_spans)), rows[0]);
    if assert_h > 0 {
        f.render_widget(Paragraph::new(assert_lines), rows[1]);
    }

    // Wrap long lines to the body width and clamp scrolling so the user can't
    // scroll past the last line into blank space. The panel caches the
    // wrap/line structure (`set_content` → `rebuild_if_needed`) and reuses it
    // across frames as long as `body`'s identity and the panel width haven't
    // changed, and even a rebuild only wraps the rows actually on screen —
    // this is what keeps dragging a selection or scrolling responsive
    // regardless of how large an "obscenely large" response body is. The
    // end-of-row wrap marker makes a soft wrap read as one logical line.
    let body_area = rows[2];
    let width = body_area.width as usize;
    app.resp_panel.set_wrap_marker(Some(wrap_marker(th)));
    app.resp_panel.set_content(body.clone(), width);
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
    } else {
        app.resp_scrollbar_area = Rect::default();
    }

    // Cache the geometry so mouse selection can map coordinates back to real,
    // copyable text — scoped to this panel's own Rect only.
    app.resp_text_area = body_area;

    f.render_widget(
        Paragraph::new(visible_wrapped).style(Style::default().fg(th.text)),
        body_area,
    );
}

pub(crate) fn draw_footer(f: &mut Frame, area: Rect, s: &Strings, th: &Theme, can_copy: bool) {
    // Run/Run All (F5 / Alt+F5) now live on the Collections panel's bottom
    // border (see draw_collection_left), and the base-URL row above already
    // shows its own "b" hint — kept out of here to leave room for the rest.
    let mut hint = vec![
        format!("Tab {}", s.foot_focus),
        format!("↑↓ {}", s.foot_move),
        format!("Enter {}", s.foot_edit),
        format!("n {}", s.foot_new),
        format!("r {}", s.foot_reload_var),
        format!("f {}", s.foot_file),
        format!("s {}", s.foot_options),
        format!("[ / ], ^←/→ {}", s.help_prev_next_tab),
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
    hint.push(format!("? {}", s.foot_help));
    hint.push(format!("q {}", s.foot_quit));
    let hint = hint.join(" · ");
    f.render_widget(
        Paragraph::new(Line::styled(hint, Style::default().fg(th.dim)))
            .style(Style::default().bg(th.panel)),
        area,
    );
}

pub(crate) fn draw_overlay(f: &mut Frame, app: &mut TuiApp, s: &Strings, th: &Theme) {
    match app.overlay.as_ref().unwrap() {
        Overlay::FileMenu(sel) => {
            let items = file_menu_items(s);
            draw_menu_popup(f, s.file_menu, &items, *sel, th);
        }
        Overlay::FileLoadMenu(sel) => {
            let items = file_load_items(s);
            draw_menu_popup(f, s.file_load_menu, &items, *sel, th);
        }
        Overlay::FileSaveMenu(sel) => {
            let items = file_save_items(s);
            draw_menu_popup(f, s.file_save_menu, &items, *sel, th);
        }
        Overlay::FileLoadSource(kind, sel) => {
            let items = file_load_source_items(s);
            let title = format!("{} {}", s.file_load_menu, kind.name(s));
            draw_menu_popup(f, &title, &items, *sel, th);
        }
        Overlay::FileSaveDest(kind, sel) => {
            let items = file_save_dest_items(*kind, s);
            let title = format!("{} {}", s.file_save_menu, kind.name(s));
            draw_menu_popup(f, &title, &items, *sel, th);
        }
        Overlay::Options(sel) => {
            let items = [
                s.settings_item_language,
                s.settings_item_theme,
                s.settings_item_preferences,
                s.settings_item_clear,
            ];
            draw_menu_popup(f, s.options_menu, &items, *sel, th);
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
            draw_menu_popup(f, s.preferences_menu, &items, *sel, th);
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
            };
            draw_confirm_popup(f, &question, &[s.confirm_yes, s.confirm_no], *sel, th);
        }
        Overlay::LanguageMenu(sel) => {
            let items = [s.lang_english, s.lang_french, s.lang_danish];
            draw_menu_popup(f, s.language_label, &items, *sel, th);
        }
        Overlay::ThemeEditor(state) => {
            let entries = app.theme_picker_entries(s);
            super::theme_editor::draw_theme_editor(f, state, &entries, s, th);
        }
        Overlay::RequestViewMenu(sel) => {
            let items = [s.view_json_label, s.view_hurl_label];
            draw_menu_popup(f, s.default_request_view_label, &items, *sel, th);
        }
        Overlay::Help(tab) => {
            // Widen the popup on spacious terminals so long descriptions
            // need to wrap less often (`centered_rect` clamps this further
            // to whatever's actually available on narrow terminals).
            let box_w = f.area().width.saturating_sub(6).clamp(64, 100);
            let inner_w = (box_w as usize).saturating_sub(2); // minus the left/right border

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
                            ("^r (File browser)", s.help_browser_reset),
                        ],
                    ),
                    (
                        s.help_group_environments,
                        &[
                            ("r (Env popup)", s.help_reload_var),
                            ("F2 (Env panel)", s.help_env_rename),
                            ("a", s.help_env_activate),
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
                            ("Alt+Click+Drag", s.help_multi_select),
                            ("F2", s.help_save_editor),
                        ],
                    ),
                    (
                        s.help_group_reports,
                        &[
                            ("Shift+R", s.help_report_new),
                            ("e / Enter", s.help_report_edit),
                            ("r / F5 (report)", s.help_report_run),
                            ("d (report)", s.help_report_dry_run),
                            ("v (report)", s.help_report_view),
                            ("Tab / Shift+Tab (report)", s.help_report_focus_cycle),
                            ("x (report)", s.help_report_export),
                            ("c (report)", s.help_report_columns),
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
                for (i, (heading, entries)) in groups.iter().enumerate() {
                    if i > 0 {
                        body.push(Line::raw(""));
                    }
                    body.push(help_section_divider(heading, inner_w, th));
                    for &(shortcut, desc) in entries.iter() {
                        body.extend(help_entry_lines(shortcut, desc, inner_w));
                    }
                }
                body
            };

            let glossary_body = || {
                let mut body = vec![help_section_divider(s.glossary_heading, inner_w, th)];
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
                for (icon, color, label, desc) in entries {
                    body.extend(glossary_entry_lines(icon, color, label, desc, inner_w));
                }
                // A second group covers every other icon shown around the
                // app (list rows, tab bar, form editor) so this one tab is
                // a complete legend rather than just the substitution dots.
                body.push(Line::raw(""));
                body.push(help_section_divider(s.glossary_heading_icons, inner_w, th));
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
                for (icon, color, label, desc) in icon_entries {
                    body.extend(glossary_entry_lines(icon, color, label, desc, inner_w));
                }
                body
            };

            let reports_body = || {
                // Render one titled group whose descriptions all align to that
                // group's own widest key (so long grammar left-hand sides don't
                // shove their descriptions out of line with the short ones).
                let group = |body: &mut Vec<Line<'static>>,
                             heading: &'static str,
                             entries: &[(&'static str, &'static str)]| {
                    body.push(help_section_divider(heading, inner_w, th));
                    let key_col = entries
                        .iter()
                        .map(|(k, _)| k.chars().count())
                        .max()
                        .unwrap_or(0)
                        .clamp(6, 34);
                    for &(code, desc) in entries {
                        body.extend(help_entry_lines_col(code, desc, key_col, inner_w));
                    }
                };

                let mut body = vec![help_section_divider(
                    s.help_reports_about_heading,
                    inner_w,
                    th,
                )];
                for para in [s.help_reports_about_1, s.help_reports_about_2] {
                    body.push(Line::from(Span::styled(para, Style::default().fg(th.text))));
                    body.push(Line::raw(""));
                }

                group(
                    &mut body,
                    s.help_reports_shortcuts_heading,
                    &[
                        ("Shift+R", s.help_report_new),
                        ("e / Enter", s.help_report_edit),
                        ("r / F5", s.help_report_run),
                        ("d", s.help_report_dry_run),
                        ("v", s.help_report_view),
                        ("Tab / Shift+Tab", s.help_report_focus_cycle),
                        ("x", s.help_report_export),
                        ("c", s.help_report_columns),
                        ("Esc", s.help_report_leave_edit),
                        ("Ctrl+←/→", s.help_report_word_move),
                        ("→", s.help_report_complete),
                    ],
                );
                body.push(Line::raw(""));
                group(
                    &mut body,
                    s.help_reports_grammar_heading,
                    &[
                        ("# collection: PATH", s.help_grammar_collection),
                        ("# environment: NAME", s.help_grammar_environment),
                        ("KEY = value", s.help_grammar_assign),
                        ("REQUEST NAME", s.help_grammar_request),
                        ("REPORT REQUEST NAME [AS COL]", s.help_grammar_report),
                        ("REPORT REQUEST NAME SHOW(a, b)", s.help_grammar_show),
                        ("Result", s.help_grammar_result),
                        ("PARALLEL[(n)] FOR …", s.help_grammar_parallel),
                    ],
                );
                body.push(Line::raw(""));
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
                        ("ENVS \"au\", \"eu\"", s.help_grammar_envs),
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
            lines.extend(match *tab {
                0 => shortcuts_body(),
                1 => glossary_body(),
                _ => reports_body(),
            });

            // All three tabs share one fixed height (the tallest body) so
            // switching tabs doesn't resize the popup out from under the user
            // — a stable box makes the tab strip read as one steady window
            // rather than a jarring resize on every switch. `centered_rect`
            // further caps this to the terminal's own height on small
            // terminals, in which case the body is scrolled (Up/Down) with a
            // scrollbar on the right border rather than clipping off the
            // bottom silently.
            let content_len = lines
                .len()
                .max(2 + shortcuts_body().len())
                .max(2 + glossary_body().len())
                .max(2 + reports_body().len());
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
        }
        Overlay::ReportDryRun(preview) => {
            let lines = preview.lines(s, th);
            let content_len = lines.len();
            let box_w = f.area().width.saturating_sub(6).clamp(48, 96);
            // Leave room for the border (2) and cap the height to the terminal;
            // long previews scroll rather than overflowing.
            let box_h = (content_len as u16 + 2).min(f.area().height.saturating_sub(2));
            let area = centered_rect(box_w, box_h, f.area());
            f.render_widget(Clear, area);
            let title = format!("{}  ({})", s.report_dry_run_title, s.report_dry_run_hint);
            let visible_rows = area.height.saturating_sub(2) as usize;
            let max_scroll = content_len.saturating_sub(visible_rows) as u16;
            if app.dry_run_scroll > max_scroll {
                app.dry_run_scroll = max_scroll;
            }
            let scroll = app.dry_run_scroll;
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
        }
        Overlay::ReportColumns(picker) => {
            draw_report_columns_overlay(f, picker, s, th);
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
            let w = if ml {
                (f.area().width * 8 / 10).max(30)
            } else {
                64
            };
            let area = centered_rect(w, h, f.area());
            f.render_widget(Clear, area);
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
            if let Some((area, checked)) = checkbox_area {
                let mark = if *checked { "[x]" } else { "[ ]" };
                let fg = if *checked { th.pending } else { th.ok };
                f.render_widget(
                    Paragraph::new(format!("{mark} {}", s.env_still_secret))
                        .style(Style::default().fg(fg)),
                    area,
                );
            }
        }
        Overlay::Browser(action, ex) => {
            let w = (f.area().width * 7 / 10).max(50);
            let h = (f.area().height * 7 / 10).max(10);
            let area = centered_rect(w, h, f.area());
            f.render_widget(Clear, area);
            if action.is_save_to_folder() {
                // Reserve a bordered filename box at the bottom that the user
                // can Tab to and press Enter to save into the current folder.
                let rows =
                    Layout::vertical([Constraint::Min(3), Constraint::Length(3)]).split(area);
                ex.widget().render_ref(rows[0], f.buffer_mut());
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
                let inner = block.inner(rows[1]);
                f.render_widget(block, rows[1]);
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
            } else {
                ex.widget().render_ref(area, f.buffer_mut());
            }
        }
        Overlay::NewRequest(form) => draw_new_request(f, form, s, th, app.enhanced_keys),
        Overlay::EnvVarForm(form) => draw_env_var_form(f, form, s, th),
        Overlay::RemoteGit(w) => draw_remote_wizard(f, w, s, th),
        Overlay::GitSave(w) => draw_git_save_wizard(f, w, s, th),
        Overlay::EnvPopup(popup) => draw_env_popup(f, app, popup, s, th),
        Overlay::EnvLinkPicker(picker) => draw_env_link_picker(f, app, picker, s, th),
        Overlay::EnvCollision(collision) => draw_env_collision(f, collision, s, th),
        Overlay::WorkspacePicker(picker) => draw_workspace_picker(f, picker, s, th),
        Overlay::CloseGitWorkspace { path, sel, .. } => {
            let question = s
                .close_git_workspace_q
                .replace("{p}", &path.to_string_lossy());
            let choices = [
                s.close_git_workspace_keep,
                s.close_git_workspace_delete,
                s.close_git_workspace_cancel,
            ];
            draw_confirm_popup(f, &question, &choices, *sel, th);
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
            draw_confirm_popup(f, &question, &[s.confirm_yes, s.confirm_no], *sel, th);
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
            draw_confirm_popup(f, s.git_workspace_storage_q, &choices, *sel, th);
        }
        Overlay::WorkspaceGitSaveUnsaved { sel, .. } => {
            let choices = [
                s.git_save_ws_unsaved_save,
                s.git_save_ws_unsaved_ignore,
                s.git_save_ws_unsaved_cancel,
            ];
            draw_confirm_popup(f, s.git_save_ws_unsaved_q, &choices, *sel, th);
        }
        Overlay::WorkspaceSwitchUnsaved { sel, .. } => {
            let choices = [
                s.ws_switch_unsaved_save,
                s.ws_switch_unsaved_discard,
                s.ws_switch_unsaved_cancel,
            ];
            draw_confirm_popup(f, s.ws_switch_unsaved_q, &choices, *sel, th);
        }
    }
}

/// The "add environment variable" popup: a two-column `Key | Value` table with
/// one editable row. The focused cell shows a cursor and a subtle background.
pub(crate) fn draw_env_var_form(f: &mut Frame, form: &EnvVarForm, s: &Strings, th: &Theme) {
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
    draw_menu_popup(f, s.env_link_picker_title, &items, picker.sel, th);
}

/// The 4-choice popup shown when loading an environment whose name collides
/// with one already in the Global Environments list.
pub(crate) fn draw_env_collision(f: &mut Frame, collision: &EnvCollision, s: &Strings, th: &Theme) {
    let items = [
        s.env_collision_replace,
        s.env_collision_keep_both,
        s.env_collision_abort,
        s.env_collision_rename,
    ];
    draw_menu_popup(f, s.env_collision_title, &items, collision.sel, th);
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
        "{} — {} [{filter_label}]",
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

pub(crate) fn draw_menu_popup(f: &mut Frame, title: &str, items: &[&str], sel: usize, th: &Theme) {
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
) {
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
}
