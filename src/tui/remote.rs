use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, List, ListItem, ListState, Paragraph, Wrap};

use crate::git_remote::{self, RemoteRefs};
use crate::i18n::Strings;
use crate::remote_flow::{RefChoice, RemoteKind, WorkspaceGitFilter};

// The wizard's data model, its file/ref narrowing and its background workers
// live in `crate::remote_flow`, shared with the GUI so the two front-ends
// cannot drift apart. What remains here is the terminal UI's presentation of
// it: the stage the user is on and how each popup is drawn.
pub(crate) use crate::remote_flow::{
    build_ref_choices, filter_indices, relevant_files, spawn_workspace_redownload,
};

use super::app::{MouseHitTarget, MouseLayer, MouseScrollTarget, TuiApp};
use super::draw::*;
use super::editor::*;
use super::theme::*;

/// Result messages from background git operations, delivered over a channel and
/// polled each frame (like environment secret resolution).
pub(crate) enum GitMsg {
    Refs(Result<RemoteRefs, String>),
    /// `(files, repo, commit_sha)` — `commit_sha` is the exact commit the
    /// listing was fetched at (`FETCH_HEAD` resolved), remembered so a
    /// Workspace load can later be redownloaded pinned to this exact commit
    /// rather than "whatever the branch points at now".
    Files(Result<(Vec<String>, PathBuf, String), String>),
    Content(Result<String, String>),
    /// A Workspace load's filtered batch of files finished downloading into
    /// `repo` (the same temp repo dir from `Files`) — on success it becomes
    /// the new tab's `workspace_root`, exactly like a real local folder.
    Workspace(Result<PathBuf, String>),
}

/// Which step of the remote-git wizard is on screen.
pub(crate) enum RemoteStage {
    /// Editing the URL (field 0) and token (field 1). `recent_sel` is `Some`
    /// while the "recently used URLs" dropdown below the URL field has
    /// keyboard focus (indexing into [`RemoteWizard::recent`]).
    Connect {
        field: u8,
        recent_sel: Option<usize>,
    },
    /// A background git op is running; show a phase message until it completes.
    Loading { phase: LoadPhase },
    /// Choose a branch/tag from `refs` (filtered by `filter`).
    PickRef {
        refs: Vec<RefChoice>,
        filter: String,
        sel: usize,
    },
    /// Choose a file path from `files` (filtered by `filter`).
    PickFile {
        files: Vec<String>,
        filter: String,
        sel: usize,
    },
    /// Workspace load only: choose which files to actually download (see
    /// [`WorkspaceGitFilter`]) before checking anything out.
    PickWorkspaceFilter { sel: usize },
    /// A git op failed; show the (token-redacted) error until dismissed.
    Error(String),
}

/// The background git operation currently in flight (for the loading message).
#[derive(Clone, Copy)]
pub(crate) enum LoadPhase {
    Refs,
    Files,
    File,
    /// Downloading a Workspace's filtered batch of files.
    WorkspaceFiles,
}

/// State for the "load from a remote git repo" wizard overlay.
pub(crate) struct RemoteWizard {
    pub(crate) kind: RemoteKind,
    pub(crate) url: Editor,
    pub(crate) token: Editor,
    pub(crate) stage: RemoteStage,
    /// Background git op in flight, if any.
    pub(crate) rx: Option<Receiver<GitMsg>>,
    /// Temp repo from `list_files`, kept alive so the chosen file can be checked
    /// out from it. Cleaned up when the wizard closes.
    pub(crate) repo: Option<PathBuf>,
    /// The file path the user chose (used to title the loaded tab).
    pub(crate) selected_path: Option<String>,
    /// Recently used git URLs (most recent first), offered as a pickable
    /// dropdown below the URL field. A snapshot taken when the wizard opened.
    pub(crate) recent: Vec<String>,
    /// The branch/tag the user picked in `PickRef`, kept around (rather than
    /// discarded once `spawn_git_files` consumes it) so a `GitOrigin` can be
    /// recorded once the file finishes loading.
    pub(crate) chosen_ref: Option<RefChoice>,
    /// The file listing from `list_files`, kept around (instead of only
    /// living inside the `PickFile` stage) so the Workspace filter step can
    /// reuse it without a second network fetch.
    pub(crate) files: Vec<String>,
    /// The [`WorkspaceGitFilter`] chosen in `PickWorkspaceFilter`, kept
    /// around so it can be baked into the [`WorkspaceGitOrigin`] recorded
    /// once the download finishes.
    pub(crate) chosen_workspace_filter: Option<WorkspaceGitFilter>,
    /// The commit sha `list_files` resolved the chosen ref to, kept around
    /// for the same reason as `chosen_workspace_filter` above.
    pub(crate) chosen_sha: Option<String>,
}

impl RemoteWizard {
    pub(crate) fn new(kind: RemoteKind, recent: Vec<String>) -> Self {
        Self {
            kind,
            url: Editor::blank(),
            token: Editor::blank(),
            stage: RemoteStage::Connect {
                field: 0,
                recent_sel: None,
            },
            rx: None,
            repo: None,
            selected_path: None,
            recent,
            chosen_ref: None,
            files: Vec::new(),
            chosen_workspace_filter: None,
            chosen_sha: None,
        }
    }

    pub(crate) fn token_opt(&self) -> Option<String> {
        let t = self.token.text();
        if t.trim().is_empty() { None } else { Some(t) }
    }
}

pub(crate) fn spawn_git_refs(url: String, token: Option<String>) -> Receiver<GitMsg> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(GitMsg::Refs(git_remote::list_refs(&url, token.as_deref())));
    });
    rx
}

pub(crate) fn spawn_git_files(
    url: String,
    token: Option<String>,
    gitref: String,
) -> Receiver<GitMsg> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let res = git_remote::list_files(&url, token.as_deref(), &gitref);
        // If the wizard was cancelled the receiver is gone; clean up the temp
        // repo we created so it doesn't linger on disk.
        if let Err(mpsc::SendError(GitMsg::Files(Ok((_, dir, _))))) = tx.send(GitMsg::Files(res)) {
            git_remote::cleanup(&dir);
        }
    });
    rx
}

pub(crate) fn spawn_git_checkout(repo: PathBuf, path: String) -> Receiver<GitMsg> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(GitMsg::Content(git_remote::checkout_file(&repo, &path)));
    });
    rx
}

/// Check out a Workspace's filtered batch of `paths` into `repo`, then hand
/// `repo` itself back as the new tab's future `workspace_root`.
pub(crate) fn spawn_git_checkout_workspace(repo: PathBuf, paths: Vec<String>) -> Receiver<GitMsg> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let res = git_remote::checkout_files(&repo, &paths).map(|_| repo.clone());
        // The temp repo becomes the persisted `workspace_root`, so drop its
        // `origin` remote to keep the access token out of its `.git/config`.
        if res.is_ok() {
            git_remote::scrub_remote(&repo);
        }
        // If the wizard was cancelled the receiver is gone; clean up the temp
        // repo (which now holds the downloaded workspace files) so it doesn't
        // linger on disk.
        if let Err(mpsc::SendError(GitMsg::Workspace(Ok(dir)))) = tx.send(GitMsg::Workspace(res)) {
            git_remote::cleanup(&dir);
        }
    });
    rx
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
    let title = match w.kind {
        RemoteKind::Collection => s.git_collection_menu,
        RemoteKind::Environment => s.git_env_menu,
        RemoteKind::Report => s.git_report_menu,
        RemoteKind::Workspace => s.git_workspace_menu,
    };
    match &w.stage {
        RemoteStage::Connect { field, recent_sel } => {
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
            render_line_field(f, rows[1], &w.url, *field == 0, false, th);
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
                        let style = if *recent_sel == Some(i) {
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
            render_line_field(f, rows[5], &w.token, *field == 1, true, th);
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
        RemoteStage::Loading { phase } => {
            let msg = match phase {
                LoadPhase::Refs => s.git_loading_refs,
                LoadPhase::Files => s.git_loading_files,
                LoadPhase::File => s.git_loading_file,
                LoadPhase::WorkspaceFiles => s.git_loading_workspace_files,
            };
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
        RemoteStage::PickRef { refs, filter, sel } => {
            let labels: Vec<String> = refs.iter().map(|r| r.label.clone()).collect();
            draw_filter_list(f, s, s.git_pick_ref_title, filter, &labels, *sel, th);
            register_remote_filter_hits(f, app, filter, &labels, *sel);
        }
        RemoteStage::PickFile { files, filter, sel } => {
            draw_filter_list(f, s, s.git_pick_file_title, filter, files, *sel, th);
            register_remote_filter_hits(f, app, filter, files, *sel);
        }
        RemoteStage::PickWorkspaceFilter { sel } => {
            let labels: Vec<&str> = WorkspaceGitFilter::ALL.iter().map(|f| f.label(s)).collect();
            draw_choice_popup(
                f,
                s.git_pick_workspace_filter_title,
                &labels,
                *sel,
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
        RemoteStage::Error(e) => {
            let width = (f.area().width * 6 / 10).max(40);
            let area = centered_rect(width, 8, f.area());
            f.render_widget(Clear, area);
            let block = panel(title.to_string(), true, th);
            let inner = block.inner(area);
            f.render_widget(block, area);
            let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(inner);
            f.render_widget(
                Paragraph::new(e.clone())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn files() -> Vec<String> {
        [
            "api/health.hurl",
            "postman/orders.json",
            "envs/dev.vars",
            ".env",
            ".env.dev-au",
            "reports/nightly.trail",
            "README.md",
            "src/main.rs",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    #[test]
    fn relevant_files_for_a_collection_keeps_only_hurl_and_json() {
        let out = relevant_files(RemoteKind::Collection, &files());
        assert_eq!(out, vec!["api/health.hurl", "postman/orders.json"]);
    }

    /// `.json` is kept for environments as well as collections: the extension
    /// can't tell a Postman environment export from a Postman collection, so
    /// the picker shows both and the content check on load decides.
    #[test]
    fn relevant_files_for_an_environment_keeps_vars_dotenv_and_json_files() {
        let out = relevant_files(RemoteKind::Environment, &files());
        assert_eq!(
            out,
            vec![
                "postman/orders.json",
                "envs/dev.vars",
                ".env",
                ".env.dev-au"
            ]
        );
    }

    #[test]
    fn relevant_files_for_a_report_keeps_only_report_files() {
        let out = relevant_files(RemoteKind::Report, &files());
        assert_eq!(out, vec!["reports/nightly.trail"]);
    }

    #[test]
    fn relevant_files_falls_back_to_everything_when_nothing_matches() {
        let noise: Vec<String> = ["a.md", "b.rs", "c.txt"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        // Never strand the user with an empty picker on an oddly-named repo.
        assert_eq!(relevant_files(RemoteKind::Collection, &noise), noise);
    }
}
