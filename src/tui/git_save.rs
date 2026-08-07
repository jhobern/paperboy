//! "Save to Git" wizard — the terminal UI's presentation of
//! [`crate::save_flow`].
//!
//! The steps, the background work and the push rules all live in the shared
//! flow, which the GUI drives too. What remains here is the terminal UI's own
//! half: the [`Editor`]s behind each field, which field has focus, the branch
//! dropdown selection, and the drawing.
//!
//! Only ever offered for an item that already remembers where it came from
//! (`Collection::git_origin`, `workspace_git_origin`, or `Report::git_origin`).

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, List, ListItem, Paragraph, Wrap};

use crate::collection::Collection;
use crate::environment::Environment;
use crate::i18n::Strings;
use crate::save_flow::{SaveFlow, SaveSource, SaveTargetKind, Step};

use super::app::{MouseHitTarget, MouseLayer, TuiApp};
use super::draw::*;
use super::editor::*;
use super::theme::*;
use crate::remote_flow::WorkspaceGitOrigin;

/// Which step of the wizard is on screen.
///
/// Carries no data: it is derived from the shared flow by
/// [`GitSaveWizard::stage`], so the drawing can never show one step while the
/// flow believes it is on another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GitSaveStage {
    Connect,
    ChoosePaths,
    ChooseTarget,
    CommitMessage,
    Pushing,
    Done,
    Error,
}

/// The terminal UI's "save to git" overlay.
pub(crate) struct GitSaveWizard {
    /// The shared state machine. The GUI drives the same one, so the two
    /// front-ends cannot disagree about how a save behaves.
    pub(crate) flow: SaveFlow,
    /// A snapshot of the collection's effective environment (linked, or
    /// active-global) taken when the wizard opened — used to build the pushed
    /// `.vars` and to know whether to offer the step at all.
    pub(crate) env: Option<Environment>,
    pub(crate) url: Editor,
    pub(crate) token: Editor,
    pub(crate) collection_path: Editor,
    pub(crate) env_path: Editor,
    pub(crate) target_name: Editor,
    pub(crate) commit_msg: Editor,
    /// Which field has focus on the current step.
    pub(crate) field: u8,
    /// The open branch dropdown's selection, or `None` when it is closed.
    pub(crate) sel: Option<usize>,
}

impl GitSaveWizard {
    /// Build a wizard for collection `ci`. `env` is the collection's effective
    /// environment, if it has one. Panics if `col` has no `git_origin` —
    /// callers gate "Save to Git" on that first.
    pub(crate) fn new(ci: usize, col: &Collection, env: Option<Environment>) -> Self {
        let flow = SaveFlow::for_collection(ci, col, env.as_ref());
        Self::from_flow(flow, env)
    }

    /// Build a wizard for pushing a whole git-loaded workspace tab back.
    pub(crate) fn new_workspace(ci: usize, col: &Collection, origin: &WorkspaceGitOrigin) -> Self {
        Self::from_flow(SaveFlow::for_workspace(ci, col, origin), None)
    }

    /// Build a wizard for pushing a PaperTrail report back.
    pub(crate) fn new_report(report_idx: usize, report: &crate::report::Report) -> Self {
        Self::from_flow(SaveFlow::for_report(report_idx, report), None)
    }

    /// Wrap a seeded flow in the editors that present it.
    fn from_flow(flow: SaveFlow, env: Option<Environment>) -> Self {
        Self {
            url: Editor::new(&flow.url, false),
            token: Editor::blank(),
            collection_path: Editor::new(&flow.path, false),
            env_path: Editor::new(&flow.env_path, false),
            target_name: Editor::new(&flow.target_name, false),
            commit_msg: Editor::new(&flow.message, false),
            field: 0,
            sel: None,
            env,
            flow,
        }
    }

    /// Copy what the user has typed into the flow, so every decision the flow
    /// makes is based on the text actually on screen. Call before handing it
    /// anything to act on.
    pub(crate) fn sync(&mut self) {
        self.flow.url = self.url.text();
        self.flow.token = self.token.text();
        self.flow.path = self.collection_path.text();
        self.flow.env_path = self.env_path.text();
        self.flow.target_name = self.target_name.text();
        self.flow.message = self.commit_msg.text();
    }

    /// The step to draw, derived from the shared flow.
    pub(crate) fn stage(&self) -> GitSaveStage {
        match self.flow.step() {
            Step::Failed(_) => GitSaveStage::Error,
            Step::Done => GitSaveStage::Done,
            _ if self.flow.is_busy() && !self.on_ref_step() => GitSaveStage::Pushing,
            Step::Connect => GitSaveStage::Connect,
            Step::ChoosePaths => GitSaveStage::ChoosePaths,
            Step::ChooseTarget => GitSaveStage::ChooseTarget,
            Step::CommitMessage => GitSaveStage::CommitMessage,
        }
    }

    /// The ref listing is fetched *while* the user is already choosing a
    /// target, so that wait mustn't be shown as a blocking "pushing" screen.
    fn on_ref_step(&self) -> bool {
        matches!(self.flow.step(), Step::ChooseTarget)
    }

    /// Whether the "also save the environment" step has anything to offer.
    pub(crate) fn has_env(&self) -> bool {
        self.env.is_some() && self.flow.source.can_include_env()
    }

    /// The error text to show on [`GitSaveStage::Error`].
    pub(crate) fn error_text(&self) -> &str {
        self.flow.error().unwrap_or_default()
    }
}

pub(crate) fn draw_git_save_wizard_with_hits(
    f: &mut Frame,
    w: &GitSaveWizard,
    s: &Strings,
    th: &Theme,
    app: Option<&TuiApp>,
) {
    if let Some(app) = app {
        app.set_mouse_layer(MouseLayer::Overlay);
    }
    let title = s.git_save_title;
    match w.stage() {
        GitSaveStage::Connect => {
            let field = w.field;
            let area = centered_rect(74, 8, f.area());
            f.render_widget(Clear, area);
            let block = panel(title.to_string(), true, th);
            let inner = block.inner(area);
            f.render_widget(block, area);
            let rows = Layout::vertical([
                Constraint::Length(1), // url label
                Constraint::Length(1), // url field
                Constraint::Length(1), // spacer
                Constraint::Length(1), // token label
                Constraint::Length(1), // token field
                Constraint::Min(1),    // hint
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
                    MouseHitTarget::GitSaveWizardRow(0),
                );
            }
            f.render_widget(
                Paragraph::new(Span::styled(
                    s.git_token_label,
                    Style::default().fg(th.accent),
                )),
                rows[3],
            );
            render_line_field(f, rows[4], &w.token, field == 1, true, th);
            if let Some(app) = app {
                app.push_mouse_hit(
                    MouseLayer::Overlay,
                    rows[4],
                    MouseHitTarget::GitSaveWizardRow(1),
                );
            }
            f.render_widget(
                Paragraph::new(Line::styled(
                    s.git_connect_hint,
                    Style::default().fg(th.dim),
                )),
                rows[5],
            );
        }
        GitSaveStage::ChoosePaths => {
            let field = w.field;
            let rows_n: u16 = if w.has_env() { 8 } else { 4 };
            let area = centered_rect(76, rows_n + 4, f.area());
            f.render_widget(Clear, area);
            let block = panel(title.to_string(), true, th);
            let inner = block.inner(area);
            f.render_widget(block, area);
            let mut constraints = vec![Constraint::Length(1), Constraint::Length(1)]; // collection path label+field
            if w.has_env() {
                constraints.push(Constraint::Length(1)); // spacer
                constraints.push(Constraint::Length(1)); // checkbox
                if w.flow.include_env {
                    constraints.push(Constraint::Length(1)); // env path label
                    constraints.push(Constraint::Length(1)); // env path field
                }
            }
            constraints.push(Constraint::Min(1)); // hint
            let rows = Layout::vertical(constraints).split(inner);
            f.render_widget(
                Paragraph::new(Span::styled(
                    if matches!(w.flow.source, SaveSource::Report { .. }) {
                        s.git_save_report_path_label
                    } else {
                        s.git_save_collection_path_label
                    },
                    Style::default().fg(th.accent),
                )),
                rows[0],
            );
            render_line_field(f, rows[1], &w.collection_path, field == 0, false, th);
            if let Some(app) = app {
                app.push_mouse_hit(
                    MouseLayer::Overlay,
                    rows[1],
                    MouseHitTarget::GitSaveWizardRow(0),
                );
            }
            if w.has_env() {
                let mark = if w.flow.include_env { "[x]" } else { "[ ]" };
                let cb_style = if field == 1 {
                    Style::default()
                        .bg(th.accent)
                        .fg(th.bg)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(th.text)
                };
                f.render_widget(
                    Paragraph::new(Line::styled(
                        format!("{mark} {}", s.git_save_include_env_label),
                        cb_style,
                    )),
                    rows[3],
                );
                if let Some(app) = app {
                    app.push_mouse_hit(
                        MouseLayer::Overlay,
                        rows[3],
                        MouseHitTarget::GitSaveWizardRow(1),
                    );
                }
                if w.flow.include_env {
                    f.render_widget(
                        Paragraph::new(Span::styled(
                            s.git_save_env_path_label,
                            Style::default().fg(th.accent),
                        )),
                        rows[4],
                    );
                    render_line_field(f, rows[5], &w.env_path, field == 2, false, th);
                    if let Some(app) = app {
                        app.push_mouse_hit(
                            MouseLayer::Overlay,
                            rows[5],
                            MouseHitTarget::GitSaveWizardRow(2),
                        );
                    }
                }
            }
            let hint_row = rows.len() - 1;
            f.render_widget(
                Paragraph::new(Line::styled(
                    s.git_save_step_hint,
                    Style::default().fg(th.dim),
                )),
                rows[hint_row],
            );
        }
        GitSaveStage::ChooseTarget => {
            let sel = w.sel;
            let refs = w.flow.refs();
            let branches = &refs.branches;
            let dropdown_rows = if sel.is_some() {
                branches.len().min(5) as u16
            } else {
                0
            };
            let area = centered_rect(74, 8 + dropdown_rows, f.area());
            f.render_widget(Clear, area);
            let block = panel(title.to_string(), true, th);
            let inner = block.inner(area);
            f.render_widget(block, area);
            let rows = Layout::vertical([
                Constraint::Length(1),             // kind toggle
                Constraint::Length(1),             // name label
                Constraint::Length(1),             // name field
                Constraint::Length(dropdown_rows), // existing-branches dropdown
                Constraint::Min(1),                // hint
            ])
            .split(inner);
            let (branch_style, tag_style) = if w.flow.target_kind == SaveTargetKind::Branch {
                (
                    Style::default()
                        .bg(th.accent)
                        .fg(th.bg)
                        .add_modifier(Modifier::BOLD),
                    Style::default().fg(th.dim),
                )
            } else {
                (
                    Style::default().fg(th.dim),
                    Style::default()
                        .bg(th.accent)
                        .fg(th.bg)
                        .add_modifier(Modifier::BOLD),
                )
            };
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(format!(" {} ", s.git_save_branch_label), branch_style),
                    Span::raw(" "),
                    Span::styled(format!(" {} ", s.git_save_tag_label), tag_style),
                ])),
                rows[0],
            );
            if let Some(app) = app {
                app.push_mouse_hit(
                    MouseLayer::Overlay,
                    rows[0],
                    MouseHitTarget::GitSaveWizardRow(0),
                );
            }
            f.render_widget(Paragraph::new(Span::raw("")), rows[1]);
            render_line_field(f, rows[2], &w.target_name, sel.is_none(), false, th);
            if let Some(app) = app {
                app.push_mouse_hit(
                    MouseLayer::Overlay,
                    rows[2],
                    MouseHitTarget::GitSaveWizardRow(1),
                );
            }
            if dropdown_rows > 0 {
                let items: Vec<ListItem> = branches
                    .iter()
                    .take(5)
                    .enumerate()
                    .map(|(i, b)| {
                        let style = if sel == Some(i) {
                            Style::default()
                                .bg(th.accent)
                                .fg(th.bg)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(th.dim)
                        };
                        ListItem::new(Line::styled(b.clone(), style))
                    })
                    .collect();
                f.render_widget(List::new(items), rows[3]);
                if let Some(app) = app {
                    for i in 0..branches.len().min(5) {
                        app.push_mouse_hit(
                            MouseLayer::Overlay,
                            Rect::new(rows[3].x, rows[3].y + i as u16, rows[3].width, 1),
                            MouseHitTarget::GitSaveWizardRow(10 + i),
                        );
                    }
                }
            }
            let hint_row = rows.len() - 1;
            f.render_widget(
                Paragraph::new(Line::styled(
                    s.git_save_target_hint,
                    Style::default().fg(th.dim),
                )),
                rows[hint_row],
            );
        }
        GitSaveStage::CommitMessage => {
            let area = centered_rect(74, 6, f.area());
            f.render_widget(Clear, area);
            let block = panel(title.to_string(), true, th);
            let inner = block.inner(area);
            f.render_widget(block, area);
            let rows = Layout::vertical([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Min(1),
            ])
            .split(inner);
            f.render_widget(
                Paragraph::new(Span::styled(
                    s.git_save_commit_msg_label,
                    Style::default().fg(th.accent),
                )),
                rows[0],
            );
            render_line_field(f, rows[1], &w.commit_msg, true, false, th);
            if let Some(app) = app {
                app.push_mouse_hit(
                    MouseLayer::Overlay,
                    rows[1],
                    MouseHitTarget::GitSaveWizardRow(0),
                );
            }
            f.render_widget(
                Paragraph::new(Line::styled(
                    s.git_save_commit_msg_hint,
                    Style::default().fg(th.dim),
                )),
                rows[2],
            );
        }
        GitSaveStage::Pushing => {
            let width = (s.git_save_pushing.len() as u16 + 4).min(f.area().width);
            let area = centered_rect(width, 3, f.area());
            f.render_widget(Clear, area);
            let block = panel(title.to_string(), true, th);
            let inner = block.inner(area);
            f.render_widget(block, area);
            f.render_widget(
                Paragraph::new(Span::styled(
                    s.git_save_pushing,
                    Style::default().fg(th.text).add_modifier(Modifier::BOLD),
                )),
                inner,
            );
        }
        GitSaveStage::Done => {
            let width = (s.git_save_success.len() as u16 + 4).min(f.area().width);
            let area = centered_rect(width, 3, f.area());
            f.render_widget(Clear, area);
            let block = panel(title.to_string(), true, th);
            let inner = block.inner(area);
            f.render_widget(block, area);
            f.render_widget(
                Paragraph::new(Span::styled(
                    s.git_save_success,
                    Style::default().fg(th.text).add_modifier(Modifier::BOLD),
                )),
                inner,
            );
        }
        GitSaveStage::Error => {
            let e = w.error_text();
            let width = (f.area().width * 6 / 10).max(40);
            let area = centered_rect(width, 8, f.area());
            f.render_widget(Clear, area);
            let block = panel(title.to_string(), true, th);
            let inner = block.inner(area);
            f.render_widget(block, area);
            let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(inner);
            f.render_widget(
                Paragraph::new(e.to_string())
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
}
