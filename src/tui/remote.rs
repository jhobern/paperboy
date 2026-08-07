use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, List, ListItem, ListState, Paragraph, Wrap};

use crate::i18n::Strings;
use crate::remote_flow::{RemoteFlow, RemoteKind, Step, WorkspaceGitFilter};

// The wizard's data model, its file/ref narrowing and its background workers
// live in `crate::remote_flow`, shared with the GUI so the two front-ends
// cannot drift apart. What remains here is the terminal UI's presentation of
// it: the stage the user is on and how each popup is drawn.
pub(crate) use crate::remote_flow::{filter_indices, spawn_workspace_redownload};

use super::app::{MouseHitTarget, MouseLayer, MouseScrollTarget, TuiApp};
use super::draw::*;
use super::editor::*;
use super::theme::*;

/// Which step of the wizard the terminal UI is drawing.
///
/// This is a *view* of [`RemoteFlow`]'s state, not a second copy of it: it
/// carries no data, and [`RemoteWizard::stage`] derives it fresh each time. An
/// in-flight operation and an error both take precedence over the underlying
/// step, because that is what the user needs to see.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemoteStage {
    Connect,
    Loading,
    PickRef,
    PickFile,
    PickWorkspaceFilter,
    Error,
}

/// The terminal UI's "load from a remote git repo" wizard overlay.
///
/// Everything that decides what happens next lives in `flow`; what is left here
/// is how the terminal presents it — text editors with cursors, a filter string
/// and a highlighted row.
pub(crate) struct RemoteWizard {
    pub(crate) flow: RemoteFlow,
    pub(crate) url: Editor,
    pub(crate) token: Editor,
    /// Recently used git URLs (most recent first), offered as a pickable
    /// dropdown below the URL field. A snapshot taken when the wizard opened.
    pub(crate) recent: Vec<String>,
    /// Connect step: which field has focus (0 = URL, 1 = token).
    pub(crate) field: u8,
    /// `Some` while the recent-URLs dropdown has keyboard focus, indexing into
    /// [`RemoteWizard::recent`].
    pub(crate) recent_sel: Option<usize>,
    /// The list steps' typed filter and highlighted row. Shared by the ref and
    /// file pickers because only one of them is ever on screen, and both are
    /// reset whenever the step changes.
    pub(crate) filter: String,
    pub(crate) sel: usize,
}

impl RemoteWizard {
    pub(crate) fn new(kind: RemoteKind, recent: Vec<String>) -> Self {
        Self {
            flow: RemoteFlow::new(kind),
            url: Editor::blank(),
            token: Editor::blank(),
            recent,
            field: 0,
            recent_sel: None,
            filter: String::new(),
            sel: 0,
        }
    }

    pub(crate) fn kind(&self) -> RemoteKind {
        self.flow.kind
    }

    /// The step to draw, derived from the flow.
    pub(crate) fn stage(&self) -> RemoteStage {
        if self.flow.error().is_some() {
            return RemoteStage::Error;
        }
        if self.flow.busy().is_some() {
            return RemoteStage::Loading;
        }
        match self.flow.step() {
            Step::Connect => RemoteStage::Connect,
            Step::PickRef => RemoteStage::PickRef,
            Step::PickFile => RemoteStage::PickFile,
            // The terminal UI hands the "keep or save" question to its own
            // overlay once the download lands, so the wizard is gone by then.
            Step::PickWorkspaceFilter | Step::WorkspaceStorage => RemoteStage::PickWorkspaceFilter,
        }
    }

    /// Copy the on-screen editors into the flow. Called before any transition
    /// that needs them, so the flow never has to know about [`Editor`].
    pub(crate) fn sync_fields(&mut self) {
        self.flow.url = self.url.text();
        self.flow.token = self.token.text();
    }

    /// Reset the list filter and highlight, for when the step changes under us.
    pub(crate) fn reset_list(&mut self) {
        self.filter.clear();
        self.sel = 0;
    }
}

/// A filterable, scrollable list popup (used for the branch/tag and file
/// pickers). `sel` indexes into the *filtered* list.
pub(crate) fn draw_filter_list(
    f: &mut Frame,
    s: &Strings,
    title: &str,
    filter: &str,
    items: &[String],
    sel: usize,
    th: &Theme,
) {
    let w = (f.area().width * 7 / 10).max(50);
    let h = (f.area().height * 7 / 10).max(10);
    let area = centered_rect(w, h, f.area());
    f.render_widget(Clear, area);
    let block = panel(title.to_string(), true, th);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = Layout::vertical([
        Constraint::Length(1), // filter line
        Constraint::Min(1),    // list
        Constraint::Length(1), // hint
    ])
    .split(inner);

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(s.git_filter_label.to_string(), Style::default().fg(th.dim)),
            Span::styled(filter.to_string(), Style::default().fg(th.text)),
        ])),
        rows[0],
    );

    let vis = filter_indices(items.iter().map(|s| s.as_str()), filter);
    let list_items: Vec<ListItem> = vis
        .iter()
        .map(|&i| ListItem::new(Line::styled(items[i].clone(), Style::default().fg(th.text))))
        .collect();
    let list = List::new(list_items)
        .highlight_style(
            Style::default()
                .bg(th.accent)
                .fg(th.bg)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("\u{203a} ");
    let mut st = ListState::default();
    if !vis.is_empty() {
        st.select(Some(sel.min(vis.len() - 1)));
    }
    f.render_stateful_widget(list, rows[1], &mut st);

    f.render_widget(
        Paragraph::new(Line::styled(s.git_filter_hint, Style::default().fg(th.dim))),
        rows[2],
    );
}

/// A small fixed-choice popup (used by the Workspace git-load file-type
/// filter picker) — like `draw_filter_list` but with no search box, since
/// the choices are a short fixed list rather than something worth typing to
/// narrow down.
pub(crate) fn draw_choice_popup(
    f: &mut Frame,
    title: &str,
    items: &[&str],
    sel: usize,
    hint: &str,
    th: &Theme,
) {
    let content_w = items
        .iter()
        .map(|s| s.chars().count())
        .max()
        .unwrap_or(20)
        .max(title.chars().count());
    let w = (content_w as u16 + 6).clamp(30, f.area().width.max(1));
    let h = (items.len() as u16 + 3).min(f.area().height.max(1));
    let area = centered_rect(w, h, f.area());
    f.render_widget(Clear, area);
    let block = panel(title.to_string(), true, th);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(inner);

    let list_items: Vec<ListItem> = items
        .iter()
        .map(|i| ListItem::new(Line::styled(i.to_string(), Style::default().fg(th.text))))
        .collect();
    let list = List::new(list_items)
        .highlight_style(
            Style::default()
                .bg(th.accent)
                .fg(th.bg)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("\u{203a} ");
    let mut st = ListState::default();
    st.select(Some(sel.min(items.len().saturating_sub(1))));
    f.render_stateful_widget(list, rows[0], &mut st);

    f.render_widget(
        Paragraph::new(Line::styled(hint.to_string(), Style::default().fg(th.dim))),
        rows[1],
    );
}

#[cfg(test)]
pub(crate) fn draw_remote_wizard(f: &mut Frame, w: &RemoteWizard, s: &Strings, th: &Theme) {
    draw_remote_wizard_with_hits(f, w, s, th, None);
}

pub(crate) fn draw_remote_wizard_with_hits(
    f: &mut Frame,
    w: &RemoteWizard,
    s: &Strings,
    th: &Theme,
    app: Option<&TuiApp>,
) {
    if let Some(app) = app {
        app.set_mouse_layer(MouseLayer::Overlay);
    }
    let title = match w.kind() {
        RemoteKind::Collection => s.git_collection_menu,
        RemoteKind::Environment => s.git_env_menu,
        RemoteKind::Report => s.git_report_menu,
        RemoteKind::Workspace => s.git_workspace_menu,
    };
    match w.stage() {
        RemoteStage::Connect => {
            let (field, recent_sel) = (w.field, w.recent_sel);
            // Grow the popup to fit the recent-URLs dropdown, if any (capped so
            // it never grows unreasonably tall).
            let recent_rows = w.recent.len().min(5) as u16;
            let h = 10 + recent_rows;
            let area = centered_rect(74, h, f.area());
            f.render_widget(Clear, area);
            let block = panel(title.to_string(), true, th);
            let inner = block.inner(area);
            f.render_widget(block, area);
            let rows = Layout::vertical([
                Constraint::Length(1),           // url label
                Constraint::Length(1),           // url field
                Constraint::Length(recent_rows), // recent-urls dropdown
                Constraint::Length(1),           // spacer
                Constraint::Length(1),           // token label
                Constraint::Length(1),           // token field
                Constraint::Min(1),              // hint
            ])
            .split(inner);
            f.render_widget(
                Paragraph::new(Span::styled(
                    s.git_url_label,
                    Style::default().fg(th.accent),
                )),
                rows[0],
            );
            render_line_field(f, rows[1], &w.url, field == 0, false, th);
            if let Some(app) = app {
                app.push_mouse_hit(
                    MouseLayer::Overlay,
                    rows[1],
                    MouseHitTarget::RemoteWizardRow(0),
                );
            }
            if recent_rows > 0 {
                let items: Vec<ListItem> = w
                    .recent
                    .iter()
                    .take(5)
                    .enumerate()
                    .map(|(i, u)| {
                        let style = if recent_sel == Some(i) {
                            Style::default()
                                .bg(th.accent)
                                .fg(th.bg)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(th.dim)
                        };
                        ListItem::new(Line::styled(u.clone(), style))
                    })
                    .collect();
                f.render_widget(List::new(items), rows[2]);
                if let Some(app) = app {
                    for i in 0..w.recent.len().min(5) {
                        app.push_mouse_hit(
                            MouseLayer::Overlay,
                            Rect::new(rows[2].x, rows[2].y + i as u16, rows[2].width, 1),
                            MouseHitTarget::RemoteWizardRow(10 + i),
                        );
                    }
                }
            }
            f.render_widget(
                Paragraph::new(Span::styled(
                    s.git_token_label,
                    Style::default().fg(th.accent),
                )),
                rows[4],
            );
            render_line_field(f, rows[5], &w.token, field == 1, true, th);
            if let Some(app) = app {
                app.push_mouse_hit(
                    MouseLayer::Overlay,
                    rows[5],
                    MouseHitTarget::RemoteWizardRow(1),
                );
            }
            let hint = if recent_rows > 0 {
                format!("{}  ·  {}", s.git_connect_hint, s.git_recent_hint)
            } else {
                s.git_connect_hint.to_string()
            };
            f.render_widget(
                Paragraph::new(Line::styled(hint, Style::default().fg(th.dim))),
                rows[6],
            );
        }
        RemoteStage::Loading => {
            let msg = w.flow.busy().map_or(s.git_loading_refs, |p| p.label(s));
            let width = (msg
                .len()
                .max(s.git_loading_hint.len())
                .max(title.chars().count()) as u16
                + 4)
            .min(f.area().width);
            let area = centered_rect(width, 4, f.area());
            f.render_widget(Clear, area);
            let block = panel(title.to_string(), true, th);
            let inner = block.inner(area);
            f.render_widget(block, area);
            let rows =
                Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(inner);
            f.render_widget(
                Paragraph::new(Span::styled(
                    msg,
                    Style::default().fg(th.text).add_modifier(Modifier::BOLD),
                )),
                rows[0],
            );
            f.render_widget(
                Paragraph::new(Line::styled(
                    s.git_loading_hint,
                    Style::default().fg(th.dim),
                )),
                rows[1],
            );
        }
        RemoteStage::PickRef => {
            let labels: Vec<String> = w.flow.ref_choices(s).into_iter().map(|r| r.label).collect();
            draw_filter_list(f, s, s.git_pick_ref_title, &w.filter, &labels, w.sel, th);
            register_remote_filter_hits(f, app, &w.filter, &labels, w.sel);
        }
        RemoteStage::PickFile => {
            let files = w.flow.pickable_files();
            draw_filter_list(f, s, s.git_pick_file_title, &w.filter, &files, w.sel, th);
            register_remote_filter_hits(f, app, &w.filter, &files, w.sel);
        }
        RemoteStage::PickWorkspaceFilter => {
            let labels: Vec<&str> = WorkspaceGitFilter::ALL.iter().map(|f| f.label(s)).collect();
            draw_choice_popup(
                f,
                s.git_pick_workspace_filter_title,
                &labels,
                w.sel,
                s.git_workspace_filter_hint,
                th,
            );
            if let Some(app) = app {
                let content_w = labels
                    .iter()
                    .map(|s| s.chars().count())
                    .max()
                    .unwrap_or(20)
                    .max(s.git_pick_workspace_filter_title.chars().count());
                let w = (content_w as u16 + 6).clamp(30, f.area().width.max(1));
                let h = (labels.len() as u16 + 3).min(f.area().height.max(1));
                let area = centered_rect(w, h, f.area());
                let inner = Rect {
                    x: area.x.saturating_add(1),
                    y: area.y.saturating_add(1),
                    width: area.width.saturating_sub(2),
                    height: area.height.saturating_sub(3),
                };
                for i in 0..labels.len().min(inner.height as usize) {
                    app.push_mouse_hit(
                        MouseLayer::Overlay,
                        Rect::new(inner.x, inner.y + i as u16, inner.width, 1),
                        MouseHitTarget::RemoteWizardRow(i),
                    );
                }
            }
        }
        RemoteStage::Error => {
            let e = w.flow.error().unwrap_or_default().to_string();
            let width = (f.area().width * 6 / 10).max(40);
            let area = centered_rect(width, 8, f.area());
            f.render_widget(Clear, area);
            let block = panel(title.to_string(), true, th);
            let inner = block.inner(area);
            f.render_widget(block, area);
            let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(inner);
            f.render_widget(
                Paragraph::new(e)
                    .style(Style::default().fg(th.err))
                    .wrap(Wrap { trim: true }),
                rows[0],
            );
            f.render_widget(
                Paragraph::new(Line::styled(s.git_error_hint, Style::default().fg(th.dim))),
                rows[1],
            );
        }
    }

    fn register_remote_filter_hits(
        f: &Frame,
        app: Option<&TuiApp>,
        filter: &str,
        items: &[String],
        sel: usize,
    ) {
        let Some(app) = app else {
            return;
        };
        let w = (f.area().width * 7 / 10).max(50);
        let h = (f.area().height * 7 / 10).max(10);
        let area = centered_rect(w, h, f.area());
        let inner = Rect {
            x: area.x.saturating_add(1),
            y: area.y.saturating_add(1),
            width: area.width.saturating_sub(2),
            height: area.height.saturating_sub(2),
        };
        let rows = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);
        let vis = filter_indices(items.iter().map(|s| s.as_str()), filter);
        if vis.is_empty() {
            return;
        }
        let visible = rows[1].height as usize;
        let selected = sel.min(vis.len() - 1);
        let first = if selected >= visible {
            selected + 1 - visible
        } else {
            0
        };
        app.push_mouse_hit(
            MouseLayer::Overlay,
            rows[1],
            MouseHitTarget::Scroll(MouseScrollTarget::RemoteWizard),
        );
        for row in first..vis.len().min(first + visible) {
            app.push_mouse_hit(
                MouseLayer::Overlay,
                Rect::new(
                    rows[1].x,
                    rows[1].y + (row - first) as u16,
                    rows[1].width,
                    1,
                ),
                MouseHitTarget::RemoteWizardRow(row),
            );
        }
    }
}
