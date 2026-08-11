//! The terminal UI's "import a whole Postman workspace" wizard.
//!
//! Every decision about *what happens next* lives in [`crate::postman_flow`],
//! shared with the GUI so the two front-ends cannot drift. What remains here is
//! the terminal's presentation of it: which step is on screen, the text editors
//! with their cursors, and the highlighted row.

use super::listscroll::ListScroll;
use std::path::{Path, PathBuf};

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Gauge, List, ListItem, Paragraph, Wrap};

use crate::i18n::Strings;
use crate::postman_flow::{
    KeySource, PostmanFlow, Step, default_dest_name, human_duration, item_kind_label, plan_summary,
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

/// The Connect step's focusable rows: the key source, the key, the workspace
/// id and the API host.
pub(crate) const POSTMAN_CONNECT_FIELDS: u8 = 4;

/// The wizard overlay's own state: the editors, the focused field and the
/// workspace list's highlighted row.
pub(crate) struct PostmanWizard {
    pub(crate) flow: PostmanFlow,
    /// What the key field is asking for — the key itself, or the address of
    /// the place it is kept. The reference syntax is assembled from this and
    /// the field's text (see [`KeySource::reference`]) so nobody has to type
    /// `{{ op://… }}` from memory.
    pub(crate) key_source: KeySource,
    pub(crate) key: Editor,
    pub(crate) workspace_ref: Editor,
    pub(crate) base_url: Editor,
    /// The import destination. Not an [`Editor`]: it is chosen in the file
    /// browser (`FileAction::PostmanDestChooseFolder`) like every other
    /// "save into a folder" in the app, never typed here.
    pub(crate) dest: PathBuf,
    /// Connect step: 0 = key source, 1 = key, 2 = workspace id, 3 = API host.
    pub(crate) field: u8,
    /// Options step: which row is focused (0 = destination, 1 = collections,
    /// 2 = environments, 3 = format, 4 = overwrite, 5 = the Import button).
    pub(crate) option_row: usize,
    /// The step a failure interrupted, so dismissing the error returns there
    /// rather than throwing the whole wizard back to the key prompt.
    pub(crate) before_error: Step,
    /// Where the workspace list is scrolled to, carried between frames (see
    /// [`ListScroll`]).
    pub(crate) list_scroll: ListScroll,
}

impl PostmanWizard {
    pub(crate) fn new() -> Self {
        // An API key in the environment is the one credential a user is likely
        // to already have to hand, and typing a PMAK by hand is miserable.
        let flow = PostmanFlow::new().with_env_key();
        // A seeded key is a pasted one; `detect` keeps that honest if the
        // seeding ever grows to understand references.
        let (key_source, entry) = KeySource::detect(&flow.key);
        Self {
            key_source,
            key: Editor::new(&entry, false),
            workspace_ref: Editor::blank(),
            base_url: Editor::blank(),
            dest: PathBuf::new(),
            flow,
            field: 0,
            option_row: 0,
            before_error: Step::Connect,
            list_scroll: ListScroll::default(),
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
        self.flow.key = self.key_source.reference(&self.key.text());
        self.flow.workspace_ref = self.workspace_ref.text();
        self.flow.base_url = self.base_url.text();
        self.flow.dest = self.dest.to_string_lossy().into_owned();
    }

    /// Fill the destination in from the chosen workspace's name, unless one has
    /// already been picked — a suggestion, never an override.
    /// `base` is where the last import landed, if there was one: workspaces are
    /// downloaded to be kept together, so the folder the user chose last time
    /// is a far better guess than the directory the app was started from.
    pub(crate) fn suggest_dest(&mut self, base: Option<&Path>) {
        if !self.dest.as_os_str().is_empty() {
            return;
        }
        let base = base
            .filter(|p| p.is_dir())
            .map(Path::to_owned)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        self.set_dest(base.join(default_dest_name(self.flow.workspace_name())));
    }

    /// Record a destination chosen in the browser, keeping `flow.dest` in step.
    pub(crate) fn set_dest(&mut self, path: PathBuf) {
        self.flow.dest = path.to_string_lossy().into_owned();
        self.dest = path;
    }

    /// The leaf folder name to seed the browser's inline name editor with, so
    /// reopening the picker offers back what is already chosen.
    pub(crate) fn dest_folder_name(&self) -> String {
        self.dest
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| default_dest_name(self.flow.workspace_name()))
    }

    /// Where the picker should open: the destination's parent, if it exists.
    pub(crate) fn dest_parent(&self) -> Option<PathBuf> {
        self.dest
            .parent()
            .filter(|p| p.is_dir())
            .map(Path::to_owned)
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
        format!("{}: {}", s.postman_dest_label, w.dest.display()),
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

/// How many lines `text` takes when wrapped to `width` columns, the way
/// [`Paragraph`]'s word wrap does it. Used to size a hint's row before it is
/// drawn, so a long sentence (or a longer translation of it) grows the dialog
/// instead of being cut off at the panel edge.
fn wrapped_height(text: &str, width: u16) -> u16 {
    if width == 0 {
        return 1;
    }
    let width = width as usize;
    let mut lines = 1u16;
    let mut col = 0usize;
    for word in text.split_whitespace() {
        let len = word.chars().count();
        if col == 0 {
            col = len;
        } else if col + 1 + len <= width {
            col += 1 + len;
        } else {
            lines += 1;
            col = len;
        }
        // A word longer than the line (a URL) spills onto further lines.
        while col > width {
            lines += 1;
            col -= width;
        }
    }
    lines
}

/// The name of a key source as the selector shows it.
pub(crate) fn key_source_label(src: KeySource, s: &Strings) -> &'static str {
    match src {
        KeySource::Paste => s.postman_key_source_paste,
        KeySource::OnePassword => s.postman_key_source_op,
        KeySource::Ssm => s.postman_key_source_ssm,
        KeySource::Env => s.postman_key_source_env,
    }
}

/// What the key field is asking for, and how to find it — both follow the
/// chosen source, because "Postman API key" is the wrong prompt when the field
/// wants the name of a 1Password item.
fn key_field_label(src: KeySource, s: &Strings) -> &'static str {
    match src {
        KeySource::Paste => s.postman_key_label,
        KeySource::OnePassword => s.postman_key_label_op,
        KeySource::Ssm => s.postman_key_label_ssm,
        KeySource::Env => s.postman_key_label_env,
    }
}

fn key_field_hint(src: KeySource, s: &Strings) -> &'static str {
    match src {
        KeySource::Paste => s.postman_key_hint,
        KeySource::OnePassword => s.postman_key_hint_op,
        KeySource::Ssm => s.postman_key_hint_ssm,
        KeySource::Env => s.postman_key_hint_env,
    }
}

fn draw_connect(f: &mut Frame, w: &PostmanWizard, s: &Strings, th: &Theme, title: &str) {
    // Laid out like the request wizard: a label column on the left, the value
    // beside it, and the keys on the border. Four short rows say as much as
    // four fields did with a paragraph of explanation under each — what a
    // field wants is shown *in* the field, as a dim example, where the answer
    // is about to be typed.
    let fields: [(&str, &str, u8); 4] = [
        (s.postman_key_source_label, "", 0),
        (
            key_field_label(w.key_source, s),
            key_field_hint(w.key_source, s),
            1,
        ),
        (s.postman_workspace_label, s.postman_workspace_hint, 2),
        (s.postman_base_url_label, s.postman_base_url_hint, 3),
    ];
    let label_w = label_column(fields.iter().map(|(l, _, _)| *l));

    let width = 66.min(f.area().width);
    let area = centered_rect(width, fields.len() as u16 + 2, f.area());
    f.render_widget(Clear, area);
    let block = panel_hinted(title.to_string(), s.postman_connect_hint, th);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = Layout::vertical([Constraint::Length(1); 4]).split(inner);
    for (row, (label, placeholder, idx)) in rows.iter().zip(fields.iter()) {
        let cols =
            Layout::horizontal([Constraint::Length(label_w), Constraint::Min(1)]).split(*row);
        f.render_widget(
            Paragraph::new(Span::styled(*label, Style::default().fg(th.accent))),
            cols[0],
        );
        if *idx == 0 {
            // A cycled value, written the way every other one-of-several
            // choice in the app is, and lit when it holds focus.
            let style = if w.field == 0 {
                Style::default().fg(th.accent).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(th.text)
            };
            f.render_widget(
                Paragraph::new(Span::styled(
                    format!("\u{2039} {} \u{203a}", key_source_label(w.key_source, s)),
                    style,
                )),
                cols[1],
            );
            continue;
        }
        // Only a pasted key is itself a credential; a reference is the
        // *address* of one, and masking that would only stop the user
        // checking they typed it correctly.
        let (ed, mask) = match idx {
            1 => (&w.key, w.key_source.is_secret()),
            2 => (&w.workspace_ref, false),
            _ => (&w.base_url, false),
        };
        render_line_field_hinted(f, cols[1], ed, w.field == *idx, mask, placeholder, th);
    }
}

fn draw_loading(f: &mut Frame, w: &PostmanWizard, s: &Strings, th: &Theme, title: &str) {
    let msg = w.flow.busy().map_or(s.postman_busy_listing, |p| p.label(s));
    let width = (msg.chars().count().max(s.git_loading_hint.chars().count()) as u16 + 4)
        .min(f.area().width);
    let area = centered_rect(width, 3, f.area());
    f.render_widget(Clear, area);
    let block = panel_hinted(title.to_string(), s.git_loading_hint, th);
    let inner = block.inner(area);
    f.render_widget(block, area);
    f.render_widget(
        Paragraph::new(Span::styled(
            msg,
            Style::default().fg(th.text).add_modifier(Modifier::BOLD),
        )),
        inner,
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
    let block = panel_hinted(s.postman_pick_workspace.to_string(), s.git_filter_hint, th);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(inner);
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
    let sel = (!items.is_empty()).then(|| w.flow.selected.min(items.len() - 1));
    w.list_scroll.render(f, rows[1], list, sel, items.len());
}

fn draw_options(f: &mut Frame, w: &PostmanWizard, s: &Strings, th: &Theme, title: &str) {
    let labels = option_labels(w, s);
    // What Enter does here depends on the row, so the border says which: a
    // single hint could only be wrong on two rows out of three.
    let hint = match w.option_row {
        0 => s.postman_options_hint_dest,
        r if r == OPTION_ROWS - 1 => s.postman_options_hint_import,
        _ => s.postman_options_hint_toggle,
    };
    let note = (w.flow.format == ImportFormat::Hurl).then_some(s.postman_format_hurl_note);
    let width = 78.min(f.area().width);
    let note_h = note.map_or(0, |n| wrapped_height(n, width.saturating_sub(2)));
    let area = centered_rect(width, OPTION_ROWS as u16 + note_h + 3, f.area());
    f.render_widget(Clear, area);
    let block = panel_hinted(
        format!("{title} \u{2014} {}", w.flow.workspace_name()),
        hint,
        th,
    );
    let inner = block.inner(area);
    f.render_widget(block, area);
    let rows = Layout::vertical([
        Constraint::Length(1),                      // dest label
        Constraint::Length(1),                      // dest editor
        Constraint::Length(OPTION_ROWS as u16 - 1), // the toggles + button
        Constraint::Length(note_h),                 // format note
    ])
    .split(inner);

    f.render_widget(
        Paragraph::new(Span::styled(
            s.postman_dest_label,
            Style::default().fg(th.accent),
        )),
        rows[0],
    );
    let dest = if w.dest.as_os_str().is_empty() {
        s.postman_dest_unset.to_string()
    } else {
        w.dest.display().to_string()
    };
    let dest_style = if w.option_row == 0 {
        Style::default()
            .bg(th.accent)
            .fg(th.bg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(th.text)
    };
    // The "press Enter" affordance is the point of the row, so it keeps its
    // width and the path gives way - elided from the LEFT, since the folder
    // name at the end is what distinguishes one destination from another.
    let browse = format!("  {}", s.postman_browse);
    let room = (rows[1].width as usize).saturating_sub(browse.chars().count());
    f.render_widget(
        Paragraph::new(Line::styled(
            format!("{}{browse}", elide_left(&dest, room)),
            dest_style,
        )),
        rows[1],
    );

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
    f.render_widget(List::new(list_items), rows[2]);

    if let Some(note) = note {
        f.render_widget(
            Paragraph::new(Line::styled(note, Style::default().fg(th.dim)))
                .wrap(Wrap { trim: true }),
            rows[3],
        );
    }
}

fn draw_confirm(f: &mut Frame, w: &PostmanWizard, s: &Strings, th: &Theme, title: &str) {
    let Some(plan) = w.flow.plan() else {
        return;
    };
    let width = 78.min(f.area().width);
    let note_h = wrapped_height(s.postman_rate_limit_note, width.saturating_sub(2));
    let warn_h = if plan.strains_monthly_budget() {
        wrapped_height(s.postman_budget_warning, width.saturating_sub(2))
    } else {
        0
    };
    // Sized to its content — no trailing empty rows, which on a four-line
    // screen read as "something failed to draw".
    let area = centered_rect(width, note_h + warn_h + 5, f.area());
    f.render_widget(Clear, area);
    // The hint is on the border like everywhere else, but in the accent rather
    // than dim: nothing on this screen is waiting for anything else, and a dim
    // line here was read as a status message, leaving people parked wondering
    // why nothing was downloading.
    let block = panel_hinted_styled(
        title.to_string(),
        s.postman_confirm_hint,
        Style::default().fg(th.accent).add_modifier(Modifier::BOLD),
        th,
    );
    let inner = block.inner(area);
    f.render_widget(block, area);
    let rows = Layout::vertical([
        Constraint::Length(1),      // heading
        Constraint::Length(1),      // counts
        Constraint::Length(note_h), // rate limit note
        Constraint::Length(1),      // estimate
        Constraint::Length(warn_h), // budget warning
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
        rows[2],
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
        rows[3],
    );
    if plan.strains_monthly_budget() {
        f.render_widget(
            Paragraph::new(Line::styled(
                s.postman_budget_warning,
                Style::default().fg(th.err),
            ))
            .wrap(Wrap { trim: true }),
            rows[4],
        );
    }
}

fn draw_downloading(f: &mut Frame, w: &PostmanWizard, s: &Strings, th: &Theme, title: &str) {
    let p = w.flow.progress();
    // The bar is the whole point of this screen, so it gets the room: a fixed
    // 74 columns left it stranded in the middle of a wide terminal, drawing a
    // half-width bar for a full-width wait.
    let width = (f.area().width * 9 / 10).max(40.min(f.area().width));
    let eta = p.eta();
    // Only the rows that have something in them — an empty row reserved for an
    // ETA that hasn't been worked out yet is just a hole in the dialog.
    let mut constraints = vec![
        Constraint::Length(1), // gauge
        Constraint::Length(1), // current item
    ];
    if eta.is_some() {
        constraints.push(Constraint::Length(1));
    }
    if p.waiting.is_some() {
        constraints.push(Constraint::Length(1));
    }
    let area = centered_rect(width, constraints.len() as u16 + 2, f.area());
    f.render_widget(Clear, area);
    let block = panel_hinted(title.to_string(), s.git_loading_hint, th);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let rows = Layout::vertical(constraints).split(inner);
    let mut row = 0;
    let mut next = || {
        row += 1;
        rows[row - 1]
    };

    f.render_widget(
        Gauge::default()
            .gauge_style(Style::default().fg(th.accent).bg(th.panel))
            .ratio(p.fraction().clamp(0.0, 1.0) as f64)
            .label(format!("{}/{}", p.done, p.total)),
        next(),
    );
    let current = match p.current_kind {
        Some(kind) => format!("{}: {}", item_kind_label(kind, s), p.current),
        None => p.current.clone(),
    };
    f.render_widget(
        Paragraph::new(Line::styled(current, Style::default().fg(th.text))),
        next(),
    );
    if let Some(eta) = eta {
        f.render_widget(
            Paragraph::new(Line::styled(
                format!("{} {}", human_duration(eta, s), s.postman_remaining),
                Style::default().fg(th.dim),
            )),
            next(),
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
            next(),
        );
    }
}

fn draw_done(f: &mut Frame, w: &PostmanWizard, s: &Strings, th: &Theme) {
    let area = centered_rect(74, 5, f.area());
    f.render_widget(Clear, area);
    let block = panel_hinted(s.postman_done_title.to_string(), s.git_error_hint, th);
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
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
}

fn draw_error(f: &mut Frame, w: &PostmanWizard, s: &Strings, th: &Theme, title: &str) {
    let e = w.flow.error().unwrap_or_default().to_string();
    let width = (f.area().width * 6 / 10).max(40);
    let body_h = wrapped_height(&e, width.saturating_sub(2));
    let area = centered_rect(width, body_h + 2, f.area());
    f.render_widget(Clear, area);
    // Not "press Esc to close": Esc goes back to the step that can be fixed —
    // the key prompt for a rejected key — rather than throwing the import away.
    let block = panel_hinted(title.to_string(), s.postman_error_hint, th);
    let inner = block.inner(area);
    f.render_widget(block, area);
    f.render_widget(
        Paragraph::new(e)
            .style(Style::default().fg(th.err))
            .wrap(Wrap { trim: true }),
        inner,
    );
}

/// Shorten `text` to `width` columns by dropping characters from the FRONT and
/// marking the cut with a leading ellipsis. Used for paths, where the tail (the
/// folder being written) carries far more information than the root.
fn elide_left(text: &str, width: usize) -> String {
    let len = text.chars().count();
    if len <= width {
        return text.to_string();
    }
    // Below two columns there is no room for the marker plus any content, so
    // return what fits rather than an ellipsis that says nothing.
    if width < 2 {
        return text.chars().skip(len - width).collect();
    }
    let kept: String = text.chars().skip(len - (width - 1)).collect();
    format!("\u{2026}{kept}")
}

#[cfg(test)]
mod tests {
    use super::{elide_left, wrapped_height};

    /// The API-key hint is a full sentence with a URL in it, and it is longer
    /// again in French. Sizing its row from the wrapped height is what stopped
    /// it being cut off mid-word at the panel edge.
    #[test]
    fn a_hint_is_measured_at_the_width_it_will_be_drawn_at() {
        assert_eq!(wrapped_height("short enough", 40), 1);
        assert_eq!(wrapped_height("one two three four", 9), 3);
        // A word longer than the line still fits somewhere rather than
        // reporting a height that would clip it.
        assert_eq!(wrapped_height("https://go.postman.co/settings", 10), 3);
        assert_eq!(wrapped_height("anything", 0), 1);
    }

    #[test]
    fn a_path_that_fits_is_left_alone() {
        assert_eq!(elide_left("/tmp/Alpha", 20), "/tmp/Alpha");
        assert_eq!(elide_left("/tmp/Alpha", 10), "/tmp/Alpha");
    }

    /// The folder name is the part that identifies the destination, so it is
    /// the front of the path that goes.
    #[test]
    fn a_long_path_keeps_its_tail() {
        assert_eq!(
            elide_left("/home/someone/work/Alpha", 10),
            "\u{2026}ork/Alpha"
        );
        // The result fills the space it was given, marker included.
        assert_eq!(
            elide_left("/home/someone/work/Alpha", 10).chars().count(),
            10
        );
    }

    /// A pathological width must not panic or produce a lone ellipsis.
    #[test]
    fn a_width_too_small_for_a_marker_still_returns_content() {
        assert_eq!(elide_left("/tmp/Alpha", 1), "a");
        assert_eq!(elide_left("/tmp/Alpha", 0), "");
    }
}
