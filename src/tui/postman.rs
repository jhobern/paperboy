//! The terminal UI's "import a whole Postman workspace" wizard.
//!
//! Every decision about *what happens next* lives in [`crate::postman_flow`],
//! shared with the GUI so the two front-ends cannot drift. What remains here is
//! the terminal's presentation of it: which step is on screen, the text editors
//! with their cursors, and the highlighted row.

use super::listscroll::ListScroll;
use std::path::{Path, PathBuf};

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Gauge, List, ListItem, Paragraph, Wrap};

use crate::i18n::Strings;
use crate::postman_flow::{
    KeySource, PostmanFlow, Step, default_dest_name, human_duration, item_kind_label,
    plan_skipped_line, plan_summary,
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
    /// How far down an open collection preview has been scrolled. Lives here
    /// rather than in the flow because it is a property of this screen, not of
    /// what was fetched — the GUI scrolls the same preview with its own.
    pub(crate) preview_scroll: usize,
    /// Key *references* used before, most recent first, straight from the
    /// session. Filtered to the chosen source before being shown, so switching
    /// to SSM doesn't offer 1Password paths.
    pub(crate) recent: Vec<String>,
    /// `Some` while the recent-keys dropdown has keyboard focus, indexing into
    /// [`PostmanWizard::recent_entries`].
    pub(crate) recent_sel: Option<usize>,
    /// What a finished import landed, kept so the receipt can say it rather
    /// than only naming the folder it wrote.
    pub(crate) done: Option<crate::postman_import::ImportSummary>,
}

impl PostmanWizard {
    pub(crate) fn new(recent: Vec<String>) -> Self {
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
            preview_scroll: 0,
            recent,
            recent_sel: None,
            done: None,
        }
    }

    /// The remembered entries that belong to the source now chosen, as the
    /// field's own text — the `op://…` path, not the `{{ … }}` wrapper.
    pub(crate) fn recent_entries(&self) -> Vec<String> {
        self.recent
            .iter()
            .filter_map(|raw| {
                let (src, entry) = KeySource::detect(raw);
                (src == self.key_source && !entry.is_empty()).then_some(entry)
            })
            .collect()
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

fn draw_connect(f: &mut Frame, w: &PostmanWizard, s: &Strings, th: &Theme, title: &str) {
    // Laid out like the request wizard: a label column on the left, the value
    // beside it, and the keys on the border. Four short rows say as much as
    // four fields did with a paragraph of explanation under each — what a
    // field wants is shown *in* the field, as a dim example, where the answer
    // is about to be typed.
    let fields: [(&str, &str, u8); 4] = [
        (s.postman_key_source_label, "", 0),
        (w.key_source.field_label(s), w.key_source.field_hint(s), 1),
        (s.postman_workspace_label, s.postman_workspace_hint, 2),
        (s.postman_base_url_label, s.postman_base_url_hint, 3),
    ];
    // Width the column against *every* label it could ever hold, not just the
    // ones showing: the key row's label changes with the chosen source ("API
    // key" vs "1Password item"), and sizing to the current one made the whole
    // value column jump sideways as the user arrowed through the sources.
    let label_w = label_column(
        fields
            .iter()
            .map(|(l, _, _)| *l)
            .chain(KeySource::ALL.iter().map(|src| src.field_label(s))),
    );

    // The references this key has been read from before, offered under the key
    // field: finding the 1Password item path is the tedious half of setting an
    // import up, and it is the same path every time.
    let recent = w.recent_entries();
    let recent_rows = recent.len().min(5) as u16;

    // A line under the key field saying what picking this source commits the
    // user to. The picker names four sources but says nothing about what each
    // one needs of them, which is the whole question for someone who has never
    // referenced a secret by address before.
    let help = w.key_source.field_help(s);
    let width = 66.min(f.area().width);
    let help_rows = wrapped_height(help, width.saturating_sub(2));
    let area = centered_rect(
        width,
        fields.len() as u16 + recent_rows + help_rows + 2,
        f.area(),
    );
    f.render_widget(Clear, area);
    let hint = if recent_rows > 0 {
        format!(
            "{}  \u{b7}  {}",
            s.postman_connect_hint, s.postman_recent_hint
        )
    } else {
        s.postman_connect_hint.to_string()
    };
    let block = panel_hinted(title.to_string(), &hint, th);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let all_rows = Layout::vertical([
        Constraint::Length(1),           // key source
        Constraint::Length(1),           // key
        Constraint::Length(help_rows),   // what that source means
        Constraint::Length(recent_rows), // remembered references
        Constraint::Length(1),           // workspace
        Constraint::Length(1),           // host
    ])
    .split(inner);
    let rows = [all_rows[0], all_rows[1], all_rows[4], all_rows[5]];
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
                    format!("\u{2039} {} \u{203a}", w.key_source.source_label(s)),
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

    f.render_widget(
        Paragraph::new(Span::styled(help, Style::default().fg(th.dim))).wrap(Wrap { trim: true }),
        all_rows[2],
    );

    if recent_rows > 0 {
        // These are *choices*, not a placeholder. Drawn dim they read as more
        // ghost text -- the screen already has plenty -- and a user who has
        // never pressed Down here has no reason to suspect the line under
        // their key field is a list they can pick from. So each entry gets a
        // marker in the accent colour, sitting in the label gutter so the
        // entries themselves stay aligned with the value column, and the text
        // is drawn at full weight like anything else selectable.
        let gutter = (label_w as usize).saturating_sub(2);
        let items: Vec<ListItem> = recent
            .iter()
            .take(5)
            .enumerate()
            .map(|(i, entry)| {
                let (marker, text) = if w.recent_sel == Some(i) {
                    let sel = Style::default()
                        .bg(th.accent)
                        .fg(th.bg)
                        .add_modifier(Modifier::BOLD);
                    (sel, sel)
                } else {
                    (Style::default().fg(th.accent), Style::default().fg(th.text))
                };
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{:gutter$}\u{203a} ", ""), marker),
                    Span::styled(entry.clone(), text),
                ]))
            })
            .collect();
        f.render_widget(List::new(items), all_rows[3]);
    }
}

fn draw_loading(f: &mut Frame, w: &PostmanWizard, s: &Strings, th: &Theme, title: &str) {
    // Says why it is waiting and how long it has been, not just what it is
    // doing: Postman's rate limit can hold a listing for minutes, and a bare
    // phase label leaves that looking like a hang.
    let msg = w
        .flow
        .busy_line(s)
        .unwrap_or_else(|| s.postman_busy_listing.to_string());
    // The allowance is the fact behind the wait: a listing paced to one call
    // every twelve seconds is a rate limit being respected, not a stall.
    let budget = w.flow.budget_line(s);
    let widest = msg
        .chars()
        .count()
        .max(s.git_loading_hint.chars().count())
        .max(budget.as_ref().map_or(0, |b| b.chars().count()));
    let width = (widest as u16 + 4).min(f.area().width);
    let area = centered_rect(width, if budget.is_some() { 4 } else { 3 }, f.area());
    f.render_widget(Clear, area);
    let block = panel_hinted(title.to_string(), s.git_loading_hint, th);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let mut lines = vec![Line::styled(
        msg,
        Style::default().fg(th.text).add_modifier(Modifier::BOLD),
    )];
    if let Some(budget) = budget {
        lines.push(Line::styled(budget, Style::default().fg(th.dim)));
    }
    f.render_widget(Paragraph::new(lines), inner);
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
    // A Postman-specific hint rather than the shared filter one: importing
    // everything is the reason someone opened this wizard during a migration,
    // and a key nobody is told about may as well not exist.
    let block = panel_hinted(
        s.postman_pick_workspace.to_string(),
        s.postman_pick_workspace_hint,
        th,
    );
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
    // Whatever this particular set of options needs said about it. Importing
    // several workspaces changes the shape of what lands on disk, so it is
    // spelled out here rather than discovered afterwards.
    let mut notes: Vec<&str> = Vec::new();
    if w.flow.target_count() > 1 {
        notes.push(s.postman_import_all_note);
    }
    if w.flow.format == ImportFormat::Hurl {
        notes.push(s.postman_format_hurl_note);
    }
    let width = 78.min(f.area().width);
    let note_h: u16 = notes
        .iter()
        .map(|n| wrapped_height(n, width.saturating_sub(2)))
        .sum();
    let area = centered_rect(width, OPTION_ROWS as u16 + note_h + 3, f.area());
    f.render_widget(Clear, area);
    let block = panel_hinted(
        format!("{title} \u{2014} {}", w.flow.target_label(s)),
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
    // It is a hint about the row rather than part of the path, so it is drawn
    // dim: at the path's own weight it reads as another path segment.
    let browse = format!("  {}", s.postman_browse);
    let room = (rows[1].width as usize).saturating_sub(browse.chars().count());
    let hint_style = if w.option_row == 0 {
        // On the selected row the highlight owns the background, so the hint
        // steps back by dropping the path's bold rather than by changing hue.
        Style::default().bg(th.accent).fg(th.bg)
    } else {
        Style::default().fg(th.dim)
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(elide_left(&dest, room), dest_style),
            Span::styled(browse, hint_style),
        ])),
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

    if !notes.is_empty() {
        let lines: Vec<Line> = notes
            .iter()
            .map(|n| Line::styled(*n, Style::default().fg(th.dim)))
            .collect();
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), rows[3]);
    }
}

fn draw_confirm(f: &mut Frame, w: &PostmanWizard, s: &Strings, th: &Theme, title: &str) {
    let Some(plan) = w.flow.plan() else {
        return;
    };
    // An open preview takes the screen: it is a list to read, and reading it
    // beside the summary would leave neither enough room to be useful.
    if let Some(preview) = w.flow.preview() {
        draw_preview(f, w, preview, s, th, title);
        return;
    }
    let width = 78.min(f.area().width);
    let note_h = wrapped_height(s.postman_rate_limit_note, width.saturating_sub(2));
    let warn_h = if plan.strains_monthly_budget() {
        wrapped_height(s.postman_budget_warning, width.saturating_sub(2))
    } else {
        0
    };
    // Workspaces that could not be listed are said here, not only in the final
    // summary: an import of forty that quietly became thirty-eight is exactly
    // the kind of thing a migration must not find out about afterwards.
    let skipped = plan_skipped_line(plan, s);
    let skip_h = skipped
        .as_deref()
        .map_or(0, |t| wrapped_height(t, width.saturating_sub(2)));
    // Sized to its content — no trailing empty rows, which on a four-line
    // screen read as "something failed to draw".
    // The collections, so the workspace can be read into before any of it is
    // downloaded. Bounded so a 60-collection workspace still leaves the
    // summary and the estimate on screen.
    let list_h = match w.flow.previewable().len() {
        0 => 0,
        n => (n as u16).min(8) + 2, // + the cost note and a blank line
    };
    let area = centered_rect(width, note_h + warn_h + skip_h + list_h + 5, f.area());
    f.render_widget(Clear, area);
    // The hint is on the border like everywhere else, but in the accent rather
    // than dim: nothing on this screen is waiting for anything else, and a dim
    // line here was read as a status message, leaving people parked wondering
    // why nothing was downloading.
    let hint = if w.flow.previewable().is_empty() {
        s.postman_confirm_hint
    } else {
        s.postman_preview_hint
    };
    let block = panel_hinted_styled(
        title.to_string(),
        hint,
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
        Constraint::Length(skip_h), // workspaces that could not be listed
        Constraint::Length(list_h), // collections to preview
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
    if let Some(text) = skipped {
        f.render_widget(
            Paragraph::new(Line::styled(text, Style::default().fg(th.err)))
                .wrap(Wrap { trim: true }),
            rows[5],
        );
    }
    if list_h > 0 {
        draw_preview_list(f, w, s, th, rows[6]);
    }
}

/// The plan's collections, highlighted one at a time, with a line saying what
/// opening one costs. Nothing is fetched until asked for by name.
fn draw_preview_list(f: &mut Frame, w: &PostmanWizard, s: &Strings, th: &Theme, area: Rect) {
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(area);
    let status = match (w.flow.preview_pending(), w.flow.preview_error()) {
        (Some(_), _) => Line::styled(s.postman_preview_busy, Style::default().fg(th.accent)),
        (None, Some(err)) => Line::styled(err.to_string(), Style::default().fg(th.err)),
        (None, None) => Line::styled(s.postman_preview_cost, Style::default().fg(th.dim)),
    };
    f.render_widget(Paragraph::new(status), rows[0]);

    let height = rows[1].height as usize;
    let items = w.flow.previewable();
    // Keep the highlight on screen without a scrollbar's worth of machinery:
    // the window simply follows the selection.
    let first = w.flow.preview_sel.saturating_sub(height.saturating_sub(1));
    let lines: Vec<Line> = items
        .iter()
        .enumerate()
        .skip(first)
        .take(height)
        .map(|(i, item)| {
            let selected = i == w.flow.preview_sel;
            let mark = if selected { "> " } else { "  " };
            let seen = if w.flow.preview_is_cached(item.fetch_id()) {
                " ·"
            } else {
                ""
            };
            let style = if selected {
                Style::default().fg(th.accent).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(th.text)
            };
            Line::styled(format!("{mark}{}{seen}", item.name), style)
        })
        .collect();
    f.render_widget(Paragraph::new(lines), rows[1]);
}

/// One collection, read out: its requests in their folders, then whatever the
/// conversion would not manage exactly.
fn draw_preview(
    f: &mut Frame,
    w: &PostmanWizard,
    preview: &crate::postman_flow::Preview,
    s: &Strings,
    th: &Theme,
    title: &str,
) {
    let width = 78.min(f.area().width);
    let height = (f.area().height * 4 / 5).max(6.min(f.area().height));
    let area = centered_rect(width, height, f.area());
    f.render_widget(Clear, area);
    let block = panel_hinted_styled(
        format!("{title} \u{2014} {}", preview.name),
        s.postman_preview_close_hint,
        Style::default().fg(th.accent).add_modifier(Modifier::BOLD),
        th,
    );
    let inner = block.inner(area);
    f.render_widget(block, area);
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(inner);

    f.render_widget(
        Paragraph::new(Line::styled(
            format!(
                "{} \u{b7} {} {}",
                s.postman_preview_title, preview.requests, s.postman_preview_requests
            ),
            Style::default().fg(th.accent).add_modifier(Modifier::BOLD),
        )),
        rows[0],
    );

    let mut lines: Vec<Line> = Vec::new();
    if preview.rows.is_empty() {
        lines.push(Line::styled(
            s.postman_preview_empty,
            Style::default().fg(th.dim),
        ));
    }
    for row in &preview.rows {
        let indent = " ".repeat(row.depth * 2);
        let label = if row.label.trim().is_empty() {
            s.postman_preview_untitled
        } else {
            row.label.as_str()
        };
        lines.push(match &row.method {
            Some(method) => Line::from(vec![
                Span::styled(format!("{indent}{method} "), Style::default().fg(th.accent)),
                Span::styled(label.to_string(), Style::default().fg(th.text)),
            ]),
            None => Line::styled(format!("{indent}{label}/"), Style::default().fg(th.dim)),
        });
    }
    if !preview.notes.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::styled(
            s.postman_preview_notes,
            Style::default().fg(th.err),
        ));
        for note in &preview.notes {
            lines.push(Line::styled(note.clone(), Style::default().fg(th.dim)));
        }
    }
    // Clamped so scrolling past the end can't leave an empty panel.
    let max = lines.len().saturating_sub(rows[1].height as usize);
    let scroll = w.preview_scroll.min(max);
    f.render_widget(Paragraph::new(lines).scroll((scroll as u16, 0)), rows[1]);
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
    let budget = w.flow.budget_line(s);
    if budget.is_some() {
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
                format!(
                    "{} {} {}",
                    s.postman_estimate,
                    human_duration(eta, s),
                    s.postman_remaining
                ),
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
    if let Some(budget) = budget {
        f.render_widget(
            Paragraph::new(Line::styled(budget, Style::default().fg(th.dim))),
            next(),
        );
    }
}

fn draw_done(f: &mut Frame, w: &PostmanWizard, s: &Strings, th: &Theme) {
    let width = 74u16.min(f.area().width);
    let mut lines: Vec<Line> = Vec::new();
    if let Some(d) = &w.done {
        lines.push(Line::styled(
            crate::postman_flow::imported_counts(d.collections, d.environments, s),
            Style::default().fg(th.text),
        ));
    }
    lines.push(Line::styled(
        format!(
            "{} {}",
            s.postman_done_saved_to,
            w.flow.dest_path().to_string_lossy()
        ),
        Style::default().fg(th.dim),
    ));
    // What the receipt is really for: the folder has just become a tab, and
    // nothing else on screen says so.
    lines.push(Line::styled(
        s.postman_done_opened,
        Style::default().fg(th.text),
    ));
    let failures = w.flow.failures();
    if !failures.is_empty() {
        lines.push(Line::styled(
            format!("{} {}", failures.len(), s.postman_skipped),
            Style::default().fg(th.pending),
        ));
    }
    if w.done.as_ref().is_some_and(|d| d.converted_with_notes) {
        lines.push(Line::styled(
            s.postman_notes_written,
            Style::default().fg(th.dim),
        ));
    }
    // Sized to the wrapped text: the receipt grows a line when something was
    // skipped, and a fixed height would hide exactly that line.
    let body_h: u16 = lines
        .iter()
        .map(|l| {
            let text: String = l.spans.iter().map(|sp| sp.content.as_ref()).collect();
            wrapped_height(&text, width.saturating_sub(2))
        })
        .sum();
    let area = centered_rect(width, body_h + 2, f.area());
    f.render_widget(Clear, area);
    let block = panel_hinted(s.postman_done_title.to_string(), s.git_error_hint, th);
    let inner = block.inner(area);
    f.render_widget(block, area);
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
