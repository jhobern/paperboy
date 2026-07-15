use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, List, ListItem, ListState, Paragraph, Wrap};

use crate::git_remote::{self, RefKind, RemoteRefs};
use crate::i18n::Strings;

use super::draw::*;
use super::editor::*;
use super::theme::*;

/// Whether the remote-git wizard is loading a collection, an environment, or
/// a whole Workspace (many collection files at once, filtered by extension).
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum RemoteKind {
    Collection,
    Environment,
    Workspace,
}

/// Which files to download when loading a Workspace from git, offered as a
/// small fixed picklist right after the branch/tag is chosen — a big repo
/// may hold plenty of large, unrelated files that should never be pulled
/// down just to browse its collections.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) enum WorkspaceGitFilter {
    HurlAndJson,
    HurlOnly,
    JsonOnly,
    All,
}

impl WorkspaceGitFilter {
    pub(crate) const ALL: [WorkspaceGitFilter; 4] = [
        WorkspaceGitFilter::HurlAndJson,
        WorkspaceGitFilter::HurlOnly,
        WorkspaceGitFilter::JsonOnly,
        WorkspaceGitFilter::All,
    ];

    pub(crate) fn label(self, s: &Strings) -> &'static str {
        match self {
            WorkspaceGitFilter::HurlAndJson => s.git_ws_filter_hurl_json,
            WorkspaceGitFilter::HurlOnly => s.git_ws_filter_hurl,
            WorkspaceGitFilter::JsonOnly => s.git_ws_filter_json,
            WorkspaceGitFilter::All => s.git_ws_filter_all,
        }
    }

    /// Whether `path` (a repo-relative path as returned by `git ls-tree`)
    /// should be downloaded under this filter (case-insensitive extension
    /// match, mirroring `crate::workspace::scan_workspace`'s local filter).
    pub(crate) fn matches(self, path: &str) -> bool {
        let ext = std::path::Path::new(path)
            .extension()
            .and_then(|e| e.to_str());
        match self {
            WorkspaceGitFilter::All => true,
            WorkspaceGitFilter::HurlAndJson => {
                matches!(ext, Some(e) if e.eq_ignore_ascii_case("hurl") || e.eq_ignore_ascii_case("json"))
            }
            WorkspaceGitFilter::HurlOnly => {
                matches!(ext, Some(e) if e.eq_ignore_ascii_case("hurl"))
            }
            WorkspaceGitFilter::JsonOnly => {
                matches!(ext, Some(e) if e.eq_ignore_ascii_case("json"))
            }
        }
    }
}

/// Where a Workspace's downloaded files came from in git — remembered on
/// [`crate::collection::Collection::workspace_git_origin`] so that if the
/// temp download folder vanishes (e.g. the OS clears `/tmp` between
/// sessions), the app can offer to redownload the exact same commit rather
/// than losing track of the workspace entirely. Pinned to a commit **sha**,
/// not just a branch/tag name, so a redownload restores exactly what was
/// there before (not "whatever the branch points at now") — and a failed
/// fetch of that exact sha is a reliable sign the remote's history no
/// longer contains it (force-push, rebase, deleted branch/tag), reported to
/// the user as such. Never holds a token (tokens are intentionally never
/// persisted, same as [`crate::git_remote::GitOrigin`]).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct WorkspaceGitOrigin {
    pub(crate) repo_url: String,
    pub(crate) commit_sha: String,
    pub(crate) ref_kind: RefKind,
    pub(crate) ref_name: String,
    pub(crate) filter: WorkspaceGitFilter,
}

/// A branch or tag choice: `label` is shown to the user, `gitref` is the full
/// ref (e.g. `refs/heads/main`) passed to `git fetch`.
#[derive(Clone)]
pub(crate) struct RefChoice {
    pub(crate) label: String,
    pub(crate) gitref: String,
}

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
    /// After successfully loading a collection from git: ask whether to also
    /// load its environment from the same ref (reusing the already-fetched
    /// file list in [`RemoteWizard::files`] — no second network fetch). `sel`
    /// selects Yes (0) or No (1).
    AskLoadEnvToo { sel: usize },
    /// Choose the environment file from `RemoteWizard::files` (filtered by
    /// `filter`) — the combined collection+environment load's second pick.
    PickEnvFile { filter: String, sel: usize },
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
    /// living inside the `PickFile` stage) so the combined collection+
    /// environment load's second pick (`PickEnvFile`) can reuse it without a
    /// second network fetch.
    pub(crate) files: Vec<String>,
    /// `true` once the wizard has moved on to fetching the *environment*
    /// file in a combined collection+environment load, so `apply_git_msg`
    /// routes the next `GitMsg::Content` to `load_environment_text` (and sets
    /// `env_git_origin`) instead of treating it as the primary (collection)
    /// pick.
    pub(crate) second_pick: bool,
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
            second_pick: false,
            chosen_workspace_filter: None,
            chosen_sha: None,
        }
    }

    pub(crate) fn token_opt(&self) -> Option<String> {
        let t = self.token.text();
        if t.trim().is_empty() { None } else { Some(t) }
    }
}

/// Indices of `items` whose text contains `filter` (case-insensitive).
pub(crate) fn filter_indices<'a>(items: impl Iterator<Item = &'a str>, filter: &str) -> Vec<usize> {
    let f = filter.to_lowercase();
    items
        .enumerate()
        .filter(|(_, s)| f.is_empty() || s.to_lowercase().contains(&f))
        .map(|(i, _)| i)
        .collect()
}

/// Build the flat, filterable branch+tag choice list (branches first).
pub(crate) fn build_ref_choices(refs: &RemoteRefs, s: &Strings) -> Vec<RefChoice> {
    let mut out = Vec::with_capacity(refs.branches.len() + refs.tags.len());
    for b in &refs.branches {
        out.push(RefChoice {
            label: format!("[{}] {b}", s.git_branches),
            gitref: git_remote::branch_ref(b),
        });
    }
    for t in &refs.tags {
        out.push(RefChoice {
            label: format!("[{}] {t}", s.git_tags),
            gitref: git_remote::tag_ref(t),
        });
    }
    out
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
        // If the wizard was cancelled the receiver is gone; clean up the temp
        // repo (which now holds the downloaded workspace files) so it doesn't
        // linger on disk.
        if let Err(mpsc::SendError(GitMsg::Workspace(Ok(dir)))) = tx.send(GitMsg::Workspace(res)) {
            git_remote::cleanup(&dir);
        }
    });
    rx
}

/// Attempt to redownload a Workspace's files from git, pinned to the exact
/// `commit_sha` recorded when it was first downloaded (see
/// [`WorkspaceGitOrigin`]) — used when the local temp folder has vanished
/// (e.g. `/tmp` was cleared) since the last session. Unlike the interactive
/// wizard flow, this never prompts for a branch/tag or a file-type filter —
/// both are already known — and never asks for a token (tokens are never
/// persisted), so a private repo will fail here with an auth error; the
/// user's only recourse in that case is to reload it manually through
/// "Load Workspace from Git…", which does prompt for a token. A failure to
/// fetch the exact recorded sha most often means the remote's history no
/// longer contains it (force-push, rebase, or the branch/tag was deleted).
pub(crate) fn spawn_workspace_redownload(
    origin: WorkspaceGitOrigin,
) -> Receiver<Result<PathBuf, String>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result = match git_remote::list_files(&origin.repo_url, None, &origin.commit_sha) {
            Ok((files, repo, _sha)) => {
                let matched: Vec<String> = files
                    .into_iter()
                    .filter(|p| origin.filter.matches(p))
                    .collect();
                match git_remote::checkout_files(&repo, &matched) {
                    Ok(()) => Ok(repo),
                    Err(e) => {
                        git_remote::cleanup(&repo);
                        Err(e)
                    }
                }
            }
            Err(e) => Err(e),
        };
        // If the caller gave up waiting (app closed mid-fetch), clean up any
        // downloaded folder rather than leaking it.
        if let Err(mpsc::SendError(Ok(dir))) = tx.send(result) {
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

pub(crate) fn draw_remote_wizard(f: &mut Frame, w: &RemoteWizard, s: &Strings, th: &Theme) {
    let title = match w.kind {
        RemoteKind::Collection => s.git_collection_menu,
        RemoteKind::Environment => s.git_env_menu,
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
            }
            f.render_widget(
                Paragraph::new(Span::styled(
                    s.git_token_label,
                    Style::default().fg(th.accent),
                )),
                rows[4],
            );
            render_line_field(f, rows[5], &w.token, *field == 1, true, th);
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
        }
        RemoteStage::PickFile { files, filter, sel } => {
            draw_filter_list(f, s, s.git_pick_file_title, filter, files, *sel, th);
        }
        RemoteStage::AskLoadEnvToo { sel } => {
            draw_confirm_popup(
                f,
                s.git_ask_load_env_q,
                &[s.confirm_yes, s.confirm_no],
                *sel,
                th,
            );
        }
        RemoteStage::PickEnvFile { filter, sel } => {
            draw_filter_list(f, s, s.git_pick_env_file_title, filter, &w.files, *sel, th);
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
}
