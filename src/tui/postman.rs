//! The terminal UI's "import a whole Postman workspace" wizard.
//!
//! Every decision about *what happens next* lives in [`crate::postman_flow`],
//! shared with the GUI so the two front-ends cannot drift. What remains here is
//! the terminal's presentation of it: which step is on screen, the text editors
//! with their cursors, and the highlighted row.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Gauge, List, ListItem, ListState, Paragraph, Wrap};

use crate::i18n::Strings;
use crate::postman_flow::{
    PostmanFlow, Step, default_dest_name, human_duration, item_kind_label, plan_summary,
};
use crate::postman_import::ImportFormat;

use super::draw::*;
use super::editor::*;
use super::theme::*;

/// Which step the terminal is drawing.
///
/// A *view* of [`PostmanFlow`]'s state, derived fresh each time rather than
/// stored — an in-flight operation and an error both take precedence over the
/// underlying step, because that is what the user needs to see.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PostmanStage {
    Connect,
    Loading,
    PickWorkspace,
    Options,
    Confirm,
    Downloading,
    Done,
    Error,
}

/// Which cell has focus on the Options step. Ordered top to bottom so Tab and
/// the arrow keys can simply add or subtract one.
pub(crate) const OPTION_ROWS: usize = 6;

/// The wizard overlay's own state: the editors, the focused field and the
/// workspace list's highlighted row.
pub(crate) struct PostmanWizard {
    pub(crate) flow: PostmanFlow,
    pub(crate) key: Editor,
    pub(crate) workspace_ref: Editor,
    pub(crate) base_url: Editor,
    pub(crate) dest: Editor,
    /// Connect step: 0 = key, 1 = workspace id, 2 = API host.
    pub(crate) field: u8,
    /// Options step: which row is focused (0 = destination, 1 = collections,
    /// 2 = environments, 3 = format, 4 = overwrite, 5 = the Import button).
    pub(crate) option_row: usize,
    /// The step a failure interrupted, so dismissing the error returns there
    /// rather than throwing the whole wizard back to the key prompt.
    pub(crate) before_error: Step,
}

impl PostmanWizard {
    pub(crate) fn new() -> Self {
        // An API key in the environment is the one credential a user is likely
        // to already have to hand, and typing a PMAK by hand is miserable.
        let flow = PostmanFlow::new().with_env_key();
        Self {
            key: Editor::new(&flow.key, false),
            workspace_ref: Editor::blank(),
            base_url: Editor::blank(),
            dest: Editor::blank(),
            flow,
            field: 0,
            option_row: 0,
            before_error: Step::Connect,
        }
    }

    pub(crate) fn stage(&self) -> PostmanStage {
        if self.flow.error().is_some() {
            return PostmanStage::Error;
        }
        // Planning happens while the Confirm step is already on screen, so a
        // busy flow only means "Loading" when there is nothing else to show.
        match self.flow.step() {
            Step::Connect => PostmanStage::Connect,
            Step::PickWorkspace => {
                if self.flow.is_busy() {
                    PostmanStage::Loading
                } else {
                    PostmanStage::PickWorkspace
                }
            }
            Step::Options => PostmanStage::Options,
            Step::Confirm => {
                if self.flow.plan().is_none() {
                    PostmanStage::Loading
                } else {
                    PostmanStage::Confirm
                }
            }
            Step::Downloading => PostmanStage::Downloading,
            Step::Done => PostmanStage::Done,
            Step::Failed(_) => PostmanStage::Error,
        }
    }

    /// Copy the on-screen editors into the flow, so the flow never has to know
    /// what an [`Editor`] is.
    pub(crate) fn sync_fields(&mut self) {
        self.flow.key = self.key.text();
        self.flow.workspace_ref = self.workspace_ref.text();
        self.flow.base_url = self.base_url.text();
        self.flow.dest = self.dest.text();
    }

    /// Fill the destination in from the chosen workspace's name, unless the
    /// user has already typed one — a suggestion, never an override.
    pub(crate) fn suggest_dest(&mut self) {
        if !self.dest.text().trim().is_empty() {
            return;
        }
        let base = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let path = base.join(default_dest_name(self.flow.workspace_name()));
        self.dest = Editor::new(&path.to_string_lossy(), false);
        self.flow.dest = self.dest.text();
    }

    /// Remember where a failure came from, so the error screen can go back.
    pub(crate) fn remember_step(&mut self) {
        if !matches!(self.flow.step(), Step::Failed(_)) {
            self.before_error = self.flow.step().clone();
        }
    }
}

/// The Options step's rows, in focus order. Kept next to the drawing code
/// because the row *is* the label.
pub(crate) fn option_labels(w: &PostmanWizard, s: &Strings) -> Vec<String> {
    let check = |on: bool| if on { "[x]" } else { "[ ]" };
    let format = match w.flow.format {
        ImportFormat::Raw => s.postman_format_raw,
        ImportFormat::Hurl => s.postman_format_hurl,
    };
    vec![
        format!("{}: {}", s.postman_dest_label, w.dest.text()),
        format!(
            "{} {}",
            check(w.flow.include_collections),
            s.postman_include_collections
        ),
        format!(
            "{} {}",
            check(w.flow.include_environments),
            s.postman_include_environments
        ),
        format!("{}: {format}", s.postman_format_label),
        format!("{} {}", check(w.flow.overwrite), s.postman_overwrite),
        s.postman_start.to_string(),
    ]
}

pub(crate) fn draw_postman_wizard(f: &mut Frame, w: &PostmanWizard, s: &Strings, th: &Theme) {
    let title = s.postman_title;
    match w.stage() {
        PostmanStage::Connect => draw_connect(f, w, s, th, title),
        PostmanStage::Loading => draw_loading(f, w, s, th, title),
        PostmanStage::PickWorkspace => draw_pick_workspace(f, w, s, th),
        PostmanStage::Options => draw_options(f, w, s, th, title),
        PostmanStage::Confirm => draw_confirm(f, w, s, th, title),
        PostmanStage::Downloading => draw_downloading(f, w, s, th, title),
        PostmanStage::Done => draw_done(f, w, s, th),
        PostmanStage::Error => draw_error(f, w, s, th, title),
    }
}

fn draw_connect(f: &mut Frame, w: &PostmanWizard, s: &Strings, th: &Theme, title: &str) {
    let area = centered_rect(76, 16, f.area());
    f.render_widget(Clear, area);
    let block = panel(title.to_string(), true, th);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let rows = Layout::vertical([
        Constraint::Length(1), // key label
        Constraint::Length(1), // key field
        Constraint::Length(1), // key hint
        Constraint::Length(1), // spacer
        Constraint::Length(1), // workspace label
        Constraint::Length(1), // workspace field
        Constraint::Length(1), // workspace hint
        Constraint::Length(1), // spacer
        Constraint::Length(1), // base url label
        Constraint::Length(1), // base url field
        Constraint::Length(1), // base url hint
        Constraint::Min(1),    // hint
    ])
    .split(inner);

    let label = |t: &'static str| Paragraph::new(Span::styled(t, Style::default().fg(th.accent)));
    let hint = |t: &'static str| Paragraph::new(Line::styled(t, Style::default().fg(th.dim)));

    f.render_widget(label(s.postman_key_label), rows[0]);
    // The key is a credential, so it is masked like the git token field.
    render_line_field(f, rows[1], &w.key, w.field == 0, true, th);
    f.render_widget(hint(s.postman_key_hint), rows[2]);

    f.render_widget(label(s.postman_workspace_label), rows[4]);
    render_line_field(f, rows[5], &w.workspace_ref, w.field == 1, false, th);
    f.render_widget(hint(s.postman_workspace_hint), rows[6]);

    f.render_widget(label(s.postman_base_url_label), rows[8]);
    render_line_field(f, rows[9], &w.base_url, w.field == 2, false, th);
    f.render_widget(hint(s.postman_base_url_hint), rows[10]);

    f.render_widget(hint(s.git_connect_hint), rows[11]);
}

fn draw_loading(f: &mut Frame, w: &PostmanWizard, s: &Strings, th: &Theme, title: &str) {
    let msg = w.flow.busy().map_or(s.postman_busy_listing, |p| p.label(s));
    let width = (msg.chars().count().max(s.git_loading_hint.chars().count()) as u16 + 4)
        .min(f.area().width);
    let area = centered_rect(width, 4, f.area());
    f.render_widget(Clear, area);
    let block = panel(title.to_string(), true, th);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(inner);
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

fn draw_pick_workspace(f: &mut Frame, w: &PostmanWizard, s: &Strings, th: &Theme) {
    let items: Vec<String> = w
        .flow
        .visible_workspaces()
        .iter()
        .map(|ws| ws.name.clone())
        .collect();
    let area = centered_rect(
        (f.area().width * 7 / 10).max(50),
        (f.area().height * 7 / 10).max(10),
        f.area(),
    );
    f.render_widget(Clear, area);
    let block = panel(s.postman_pick_workspace.to_string(), true, th);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .split(inner);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(s.git_filter_label.to_string(), Style::default().fg(th.dim)),
            Span::styled(w.flow.filter.clone(), Style::default().fg(th.text)),
        ])),
        rows[0],
    );
    let list_items: Vec<ListItem> = items
        .iter()
        .map(|n| ListItem::new(Line::styled(n.clone(), Style::default().fg(th.text))))
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
    if !items.is_empty() {
        st.select(Some(w.flow.selected.min(items.len() - 1)));
    }
    f.render_stateful_widget(list, rows[1], &mut st);
    f.render_widget(
        Paragraph::new(Line::styled(s.git_filter_hint, Style::default().fg(th.dim))),
        rows[2],
    );
}

fn draw_options(f: &mut Frame, w: &PostmanWizard, s: &Strings, th: &Theme, title: &str) {
    let labels = option_labels(w, s);
    let area = centered_rect(78, 14, f.area());
    f.render_widget(Clear, area);
    let block = panel(
        format!("{title} \u{2014} {}", w.flow.workspace_name()),
        true,
        th,
    );
    let inner = block.inner(area);
    f.render_widget(block, area);
    let rows = Layout::vertical([
        Constraint::Length(1),                      // header
        Constraint::Length(1),                      // dest label
        Constraint::Length(1),                      // dest editor
        Constraint::Length(OPTION_ROWS as u16 - 1), // the toggles + button
        Constraint::Min(1),                         // format note / hint
    ])
    .split(inner);

    f.render_widget(
        Paragraph::new(Span::styled(
            s.postman_options_title,
            Style::default().fg(th.accent).add_modifier(Modifier::BOLD),
        )),
        rows[0],
    );
    f.render_widget(
        Paragraph::new(Span::styled(
            s.postman_dest_label,
            Style::default().fg(th.accent),
        )),
        rows[1],
    );
    render_line_field(f, rows[2], &w.dest, w.option_row == 0, false, th);

    let list_items: Vec<ListItem> = labels
        .iter()
        .enumerate()
        .skip(1)
        .map(|(i, l)| {
            let style = if w.option_row == i {
                Style::default()
                    .bg(th.accent)
                    .fg(th.bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(th.text)
            };
            ListItem::new(Line::styled(l.clone(), style))
        })
        .collect();
    f.render_widget(List::new(list_items), rows[3]);

    let mut note = String::new();
    if w.flow.format == ImportFormat::Hurl {
        note.push_str(s.postman_format_hurl_note);
        note.push_str("  ·  ");
    }
    note.push_str(s.git_connect_hint);
    f.render_widget(
        Paragraph::new(Line::styled(note, Style::default().fg(th.dim))).wrap(Wrap { trim: true }),
        rows[4],
    );
}

fn draw_confirm(f: &mut Frame, w: &PostmanWizard, s: &Strings, th: &Theme, title: &str) {
    let Some(plan) = w.flow.plan() else {
        return;
    };
    let area = centered_rect(78, 13, f.area());
    f.render_widget(Clear, area);
    let block = panel(title.to_string(), true, th);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let rows = Layout::vertical([
        Constraint::Length(1), // heading
        Constraint::Length(1), // counts
        Constraint::Length(1), // spacer
        Constraint::Length(2), // rate limit note
        Constraint::Length(1), // estimate
        Constraint::Min(1),    // budget warning / hint
    ])
    .split(inner);

    f.render_widget(
        Paragraph::new(Span::styled(
            s.postman_confirm_title,
            Style::default().fg(th.accent).add_modifier(Modifier::BOLD),
        )),
        rows[0],
    );
    f.render_widget(
        Paragraph::new(Line::styled(
            plan_summary(plan, s),
            Style::default().fg(th.text),
        )),
        rows[1],
    );
    f.render_widget(
        Paragraph::new(Line::styled(
            s.postman_rate_limit_note,
            Style::default().fg(th.dim),
        ))
        .wrap(Wrap { trim: true }),
        rows[3],
    );
    f.render_widget(
        Paragraph::new(Line::styled(
            format!(
                "{} {}",
                s.postman_estimate,
                human_duration(plan.estimated_duration(), s)
            ),
            Style::default().fg(th.text).add_modifier(Modifier::BOLD),
        )),
        rows[4],
    );
    let mut tail = Vec::new();
    if plan.strains_monthly_budget() {
        tail.push(Line::styled(
            s.postman_budget_warning,
            Style::default().fg(th.err),
        ));
    }
    tail.push(Line::styled(
        s.git_connect_hint,
        Style::default().fg(th.dim),
    ));
    f.render_widget(Paragraph::new(tail).wrap(Wrap { trim: true }), rows[5]);
}

fn draw_downloading(f: &mut Frame, w: &PostmanWizard, s: &Strings, th: &Theme, title: &str) {
    let p = w.flow.progress();
    let area = centered_rect(74, 9, f.area());
    f.render_widget(Clear, area);
    let block = panel(title.to_string(), true, th);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let rows = Layout::vertical([
        Constraint::Length(1), // gauge
        Constraint::Length(1), // current item
        Constraint::Length(1), // eta
        Constraint::Length(1), // waiting note
        Constraint::Min(1),    // hint
    ])
    .split(inner);

    f.render_widget(
        Gauge::default()
            .gauge_style(Style::default().fg(th.accent).bg(th.panel))
            .ratio(p.fraction().clamp(0.0, 1.0) as f64)
            .label(format!("{}/{}", p.done, p.total)),
        rows[0],
    );
    let current = match p.current_kind {
        Some(kind) => format!("{}: {}", item_kind_label(kind, s), p.current),
        None => p.current.clone(),
    };
    f.render_widget(
        Paragraph::new(Line::styled(current, Style::default().fg(th.text))),
        rows[1],
    );
    if let Some(eta) = p.eta() {
        f.render_widget(
            Paragraph::new(Line::styled(
                format!("{} {}", human_duration(eta, s), s.postman_remaining),
                Style::default().fg(th.dim),
            )),
            rows[2],
        );
    }
    // A paced import spends most of its life deliberately idle; saying so is
    // the difference between "working" and "hung".
    if let Some((reason, secs)) = &p.waiting {
        let label = match reason {
            crate::postman_import::WaitReason::Pacing => s.postman_waiting_paced,
            crate::postman_import::WaitReason::RateLimited => s.postman_waiting_limited,
        };
        f.render_widget(
            Paragraph::new(Line::styled(
                format!("{label} ({secs}s)"),
                Style::default().fg(th.pending),
            )),
            rows[3],
        );
    }
    f.render_widget(
        Paragraph::new(Line::styled(
            s.git_loading_hint,
            Style::default().fg(th.dim),
        )),
        rows[4],
    );
}

fn draw_done(f: &mut Frame, w: &PostmanWizard, s: &Strings, th: &Theme) {
    let area = centered_rect(74, 9, f.area());
    f.render_widget(Clear, area);
    let block = panel(s.postman_done_title.to_string(), true, th);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let mut lines = vec![Line::styled(
        w.flow.dest_path().to_string_lossy().into_owned(),
        Style::default().fg(th.text),
    )];
    let failures = w.flow.failures();
    if !failures.is_empty() {
        lines.push(Line::styled(
            format!("{} {}", failures.len(), s.postman_skipped),
            Style::default().fg(th.pending),
        ));
    }
    lines.push(Line::styled(s.git_error_hint, Style::default().fg(th.dim)));
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
}

fn draw_error(f: &mut Frame, w: &PostmanWizard, s: &Strings, th: &Theme, title: &str) {
    let e = w.flow.error().unwrap_or_default().to_string();
    let width = (f.area().width * 6 / 10).max(40);
    let area = centered_rect(width, 9, f.area());
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
