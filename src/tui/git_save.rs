//! "Save Collection to Git" wizard: pushes the active collection (and,
//! optionally, its environment) back to the git repository it was loaded
//! from, onto a new commit on a branch or tag.
//!
//! Mirrors [`super::remote`]'s wizard pattern (a background thread + a
//! channel polled each frame) but only ever offered for a collection that
//! already has a remembered [`crate::git_remote::GitOrigin`] (see
//! `Collection::git_origin`).
//!
//! Never does a full checkout/clone: the background push (see
//! [`crate::git_remote::fetch_base`] / `commit_files` / `push_commit`)
//! reuses the same blobless-fetch + `read-tree` plumbing the load wizard
//! uses, touching only the file(s) actually being written.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, List, ListItem, Paragraph, Wrap};

use crate::collection::Collection;
use crate::environment::Environment;
use crate::git_remote::{self, RefKind, RemoteRefs};
use crate::i18n::Strings;

use super::draw::*;
use super::editor::*;
use super::remote::{WorkspaceGitFilter, WorkspaceGitOrigin};
use super::theme::*;

/// What the "save to git" wizard is pushing: a single collection (+ optional
/// environment), or an entire git-loaded Workspace folder.
pub(crate) enum GitSaveSource {
    /// The active collection tab (the original flow). Uses the ChoosePaths
    /// step and pushes `collection.to_hurl()` (plus an optional `.vars`).
    Collection,
    /// A whole Workspace previously downloaded from git — pushes every file
    /// currently on disk under `root` (see
    /// [`crate::workspace::collect_files_for_commit`]), skips the ChoosePaths
    /// step entirely, and repins `workspace_git_origin` on success. `filter`
    /// is carried through so the repinned origin keeps the same download
    /// filter.
    Workspace {
        root: PathBuf,
        filter: WorkspaceGitFilter,
    },
    /// A PaperTrail `.trail` document. Pushes the report's source text to the
    /// chosen path; `report_idx` identifies which report tab to repin on
    /// success (its git origin is updated in place, like a collection's).
    Report { report_idx: usize },
}

/// Whether the user is targeting a branch or a tag.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum GitSaveTarget {
    Branch,
    Tag,
}

/// Whether the name typed/selected in [`GitSaveStage::ChooseTarget`] matched
/// an existing branch at the time the user submitted it (determined against
/// the refs fetched for that stage). Re-checked **again**, freshly, right
/// before pushing — this only records the user's *intent* so a race (someone
/// else creating a same-named ref in between) can be told apart from a
/// deliberate "commit onto an existing branch".
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum TargetIntent {
    NewRef,
    ExistingBranch,
}

/// A background push failure, typed so the caller can show a localized
/// message instead of a raw git error for the two cases we specifically
/// guard against.
pub(crate) enum GitSaveError {
    /// The tag name already exists on the remote — tags are never
    /// overwritten, regardless of when the app last checked.
    TagExists,
    /// The user intended to create a brand-new branch, but by push time the
    /// name now exists on the remote (created by someone else in the
    /// meantime) — surfaced distinctly from a plain push rejection so it
    /// isn't confused with an ordinary non-fast-forward.
    RefExistsRace,
    /// Any other failure (network, non-fast-forward on an existing branch,
    /// git not installed, ...), already redacted of any token.
    Other(String),
}

/// Result messages from the background refs-check / push, polled each frame.
pub(crate) enum GitSaveMsg {
    Refs(Result<RemoteRefs, String>),
    /// A completed push; `Ok` carries the new commit sha so a Workspace save
    /// can repin its `workspace_git_origin` to the exact commit just pushed.
    Pushed(Result<String, GitSaveError>),
}

/// Which step of the "save to git" wizard is on screen.
pub(crate) enum GitSaveStage {
    /// Editing the repo URL (field 0, prefilled from the collection's
    /// remembered origin but overridable) and an optional access token
    /// (field 1).
    Connect { field: u8 },
    /// Choosing whether to also save the environment (only reachable when
    /// one is attached) and the in-repo path(s) to write to.
    ChoosePaths { field: u8 },
    /// Branch/Tag toggle (Tab) + a typeable target name, with an optional
    /// dropdown (opened with Down, like the load wizard's recent-URL list)
    /// of the remote's existing branches to pick from. `refs` is `None`
    /// until the background fetch (spawned on entry) completes.
    ChooseTarget {
        sel: Option<usize>,
        refs: Option<RemoteRefs>,
    },
    /// Editing the auto-generated commit message before pushing.
    CommitMessage,
    /// The background fetch/commit/push is running.
    Pushing,
    /// Pushed successfully.
    Done,
    /// A step failed; message shown until dismissed (closes the wizard).
    Error(String),
}

/// State for the "save the active collection (+ environment) to git" wizard
/// overlay.
pub(crate) struct GitSaveWizard {
    /// Which collection tab is being saved.
    pub(crate) ci: usize,
    pub(crate) url: Editor,
    pub(crate) token: Editor,
    /// Whether this collection has an environment attached at all (if not,
    /// the "also save the environment" step is skipped entirely).
    pub(crate) has_env: bool,
    /// A snapshot of the collection's effective environment (linked, or
    /// active-global) at the time this wizard was opened — used to build the
    /// pushed `.vars` file content and default path. `None` when `has_env`
    /// is `false`.
    pub(crate) env: Option<Environment>,
    /// Whether to also write the environment file in the same commit.
    pub(crate) include_env: bool,
    pub(crate) collection_path: Editor,
    pub(crate) env_path: Editor,
    pub(crate) target_kind: GitSaveTarget,
    pub(crate) target_name: Editor,
    /// Recorded when the user submits [`GitSaveStage::ChooseTarget`]; consumed
    /// by the background push to distinguish a deliberate existing-branch
    /// commit from a "new ref" race.
    pub(crate) target_intent: TargetIntent,
    pub(crate) commit_msg: Editor,
    /// The full ref (e.g. `refs/heads/main`) the collection was originally
    /// loaded from — the base commit for a brand-new branch/tag, refetched
    /// fresh at push time (never reused from load time).
    pub(crate) origin_gitref: String,
    /// Whether this wizard is saving a single collection or a whole
    /// git-loaded Workspace folder.
    pub(crate) source: GitSaveSource,
    pub(crate) stage: GitSaveStage,
    pub(crate) rx: Option<Receiver<GitSaveMsg>>,
}

impl GitSaveWizard {
    /// Build a wizard for collection `ci`, seeding every field with sensible
    /// defaults derived from `col`'s remembered git origin. `env` is the
    /// collection's effective (linked, or active-global) environment, if
    /// any. Panics if `col` has no `git_origin` — callers must gate "Save to
    /// Git" on that first.
    pub(crate) fn new(ci: usize, col: &Collection, env: Option<Environment>) -> Self {
        let origin = col
            .git_origin
            .clone()
            .expect("git-save requires a collection git_origin");
        let has_env = env.is_some();
        let default_env_path = env
            .as_ref()
            .and_then(|e| e.git_origin.as_ref())
            .map(|o| o.path.clone())
            .unwrap_or_else(|| {
                format!(
                    "{}.vars",
                    env.as_ref().map(|e| e.name.as_str()).unwrap_or(&col.name)
                )
            });
        // Prefilling the target name with the original branch defaults the
        // wizard to "append a commit to the branch we loaded from" — the
        // most common case — while staying fully editable for the other two
        // branch options and for a brand-new tag.
        let target_name = match origin.ref_kind {
            RefKind::Branch => origin.ref_name.clone(),
            RefKind::Tag => String::new(),
        };
        Self {
            ci,
            url: Editor::new(&origin.repo_url, false),
            token: Editor::blank(),
            has_env,
            env,
            include_env: has_env,
            collection_path: Editor::new(&origin.path, false),
            env_path: Editor::new(&default_env_path, false),
            target_kind: GitSaveTarget::Branch,
            target_name: Editor::new(&target_name, false),
            target_intent: TargetIntent::ExistingBranch,
            commit_msg: Editor::new(&format!("Update {} via PaperBoy", col.name), false),
            origin_gitref: origin.gitref(),
            source: GitSaveSource::Collection,
            stage: GitSaveStage::Connect { field: 0 },
            rx: None,
        }
    }

    /// Build a wizard for saving a whole git-loaded Workspace tab `ci` back to
    /// the repo it came from, seeded from its remembered
    /// [`WorkspaceGitOrigin`]. Unlike [`Self::new`] there is no environment
    /// and no per-file path to choose (the whole tree at `root` is pushed as
    /// it sits on disk), so the ChoosePaths step is skipped. `root` is the
    /// tab's `workspace_root`.
    pub(crate) fn new_workspace(ci: usize, col: &Collection, origin: &WorkspaceGitOrigin) -> Self {
        let root = col
            .workspace_root
            .clone()
            .expect("workspace git-save requires a workspace_root");
        let (target_kind, origin_gitref) = match origin.ref_kind {
            RefKind::Branch => (
                GitSaveTarget::Branch,
                git_remote::branch_ref(&origin.ref_name),
            ),
            RefKind::Tag => (GitSaveTarget::Tag, git_remote::tag_ref(&origin.ref_name)),
        };
        let target_name = match origin.ref_kind {
            RefKind::Branch => origin.ref_name.clone(),
            RefKind::Tag => String::new(),
        };
        let target_intent = match origin.ref_kind {
            RefKind::Branch => TargetIntent::ExistingBranch,
            RefKind::Tag => TargetIntent::NewRef,
        };
        Self {
            ci,
            url: Editor::new(&origin.repo_url, false),
            token: Editor::blank(),
            has_env: false,
            env: None,
            include_env: false,
            collection_path: Editor::blank(),
            env_path: Editor::blank(),
            target_kind,
            target_name: Editor::new(&target_name, false),
            target_intent,
            commit_msg: Editor::new(&format!("Update {} via PaperBoy", col.name), false),
            origin_gitref,
            source: GitSaveSource::Workspace {
                root,
                filter: origin.filter,
            },
            stage: GitSaveStage::Connect { field: 0 },
            rx: None,
        }
    }

    pub(crate) fn token_opt(&self) -> Option<String> {
        let t = self.token.text();
        if t.trim().is_empty() { None } else { Some(t) }
    }
}

impl GitSaveWizard {
    /// Build a wizard for saving PaperTrail report `report_idx` back to the git
    /// remote it came from, seeded from the report's remembered [`GitOrigin`].
    /// Like a collection, the report has a single per-file path to choose (its
    /// `.trail`) but — unlike a collection — never an accompanying `.vars`, so
    /// the environment step is skipped. Panics if the report has no
    /// `git_origin` (callers gate "Save to Git" on that).
    pub(crate) fn new_report(report_idx: usize, report: &crate::report::Report) -> Self {
        let origin = report
            .git_origin
            .clone()
            .expect("git-save requires a report git_origin");
        let target_name = match origin.ref_kind {
            RefKind::Branch => origin.ref_name.clone(),
            RefKind::Tag => String::new(),
        };
        let target_intent = match origin.ref_kind {
            RefKind::Branch => TargetIntent::ExistingBranch,
            RefKind::Tag => TargetIntent::NewRef,
        };
        Self {
            ci: 0,
            url: Editor::new(&origin.repo_url, false),
            token: Editor::blank(),
            has_env: false,
            env: None,
            include_env: false,
            collection_path: Editor::new(&origin.path, false),
            env_path: Editor::blank(),
            target_kind: GitSaveTarget::Branch,
            target_name: Editor::new(&target_name, false),
            target_intent,
            commit_msg: Editor::new(&format!("Update {} via PaperBoy", report.name), false),
            origin_gitref: origin.gitref(),
            source: GitSaveSource::Report { report_idx },
            stage: GitSaveStage::Connect { field: 0 },
            rx: None,
        }
    }
}

pub(crate) fn spawn_git_save_refs(url: String, token: Option<String>) -> Receiver<GitSaveMsg> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(GitSaveMsg::Refs(git_remote::refs_fresh(
            &url,
            token.as_deref(),
        )));
    });
    rx
}

/// Fetch the base commit fresh, write `files` on top of it (touching nothing
/// else on disk), and push the result to `target_ref` — never a full
/// checkout/clone, never `--force`. `intent` + a **fresh** `refs_fresh` call
/// re-validate the tag-never-overwritten rule and detect a new-ref race,
/// right before doing anything else, per the no-stale-check requirement.
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_git_save_push(
    url: String,
    token: Option<String>,
    origin_gitref: String,
    target_kind: GitSaveTarget,
    target_name: String,
    intent: TargetIntent,
    files: Vec<(String, String)>,
    message: String,
) -> Receiver<GitSaveMsg> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result = (|| -> Result<String, GitSaveError> {
            let fresh =
                git_remote::refs_fresh(&url, token.as_deref()).map_err(GitSaveError::Other)?;
            if target_kind == GitSaveTarget::Tag && fresh.tags.iter().any(|t| t == &target_name) {
                return Err(GitSaveError::TagExists);
            }
            let is_existing_branch = target_kind == GitSaveTarget::Branch
                && fresh.branches.iter().any(|b| b == &target_name);
            if target_kind == GitSaveTarget::Branch
                && intent == TargetIntent::NewRef
                && is_existing_branch
            {
                return Err(GitSaveError::RefExistsRace);
            }
            let base_gitref = if is_existing_branch {
                git_remote::branch_ref(&target_name)
            } else {
                origin_gitref.clone()
            };
            let (repo, base_sha) = git_remote::fetch_base(&url, token.as_deref(), &base_gitref)
                .map_err(GitSaveError::Other)?;
            let (author_name, author_email) = git_remote::author_identity();
            let push_result = (|| -> Result<String, String> {
                let commit_sha = git_remote::commit_files(
                    &repo,
                    &base_sha,
                    &files,
                    &message,
                    &author_name,
                    &author_email,
                )?;
                let target_ref = match target_kind {
                    GitSaveTarget::Branch => git_remote::branch_ref(&target_name),
                    GitSaveTarget::Tag => git_remote::tag_ref(&target_name),
                };
                git_remote::push_commit(&url, token.as_deref(), &repo, &commit_sha, &target_ref)?;
                Ok(commit_sha)
            })();
            git_remote::cleanup(&repo);
            push_result.map_err(GitSaveError::Other)
        })();
        let _ = tx.send(GitSaveMsg::Pushed(result));
    });
    rx
}

pub(crate) fn draw_git_save_wizard(f: &mut Frame, w: &GitSaveWizard, s: &Strings, th: &Theme) {
    let title = s.git_save_title;
    match &w.stage {
        GitSaveStage::Connect { field } => {
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
            render_line_field(f, rows[1], &w.url, *field == 0, false, th);
            f.render_widget(
                Paragraph::new(Span::styled(
                    s.git_token_label,
                    Style::default().fg(th.accent),
                )),
                rows[3],
            );
            render_line_field(f, rows[4], &w.token, *field == 1, true, th);
            f.render_widget(
                Paragraph::new(Line::styled(
                    s.git_connect_hint,
                    Style::default().fg(th.dim),
                )),
                rows[5],
            );
        }
        GitSaveStage::ChoosePaths { field } => {
            let rows_n: u16 = if w.has_env { 8 } else { 4 };
            let area = centered_rect(76, rows_n + 4, f.area());
            f.render_widget(Clear, area);
            let block = panel(title.to_string(), true, th);
            let inner = block.inner(area);
            f.render_widget(block, area);
            let mut constraints = vec![Constraint::Length(1), Constraint::Length(1)]; // collection path label+field
            if w.has_env {
                constraints.push(Constraint::Length(1)); // spacer
                constraints.push(Constraint::Length(1)); // checkbox
                if w.include_env {
                    constraints.push(Constraint::Length(1)); // env path label
                    constraints.push(Constraint::Length(1)); // env path field
                }
            }
            constraints.push(Constraint::Min(1)); // hint
            let rows = Layout::vertical(constraints).split(inner);
            f.render_widget(
                Paragraph::new(Span::styled(
                    if matches!(w.source, GitSaveSource::Report { .. }) {
                        s.git_save_report_path_label
                    } else {
                        s.git_save_collection_path_label
                    },
                    Style::default().fg(th.accent),
                )),
                rows[0],
            );
            render_line_field(f, rows[1], &w.collection_path, *field == 0, false, th);
            if w.has_env {
                let mark = if w.include_env { "[x]" } else { "[ ]" };
                let cb_style = if *field == 1 {
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
                if w.include_env {
                    f.render_widget(
                        Paragraph::new(Span::styled(
                            s.git_save_env_path_label,
                            Style::default().fg(th.accent),
                        )),
                        rows[4],
                    );
                    render_line_field(f, rows[5], &w.env_path, *field == 2, false, th);
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
        GitSaveStage::ChooseTarget { sel, refs } => {
            let branches = refs
                .as_ref()
                .map(|r| r.branches.clone())
                .unwrap_or_default();
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
            let (branch_style, tag_style) = if w.target_kind == GitSaveTarget::Branch {
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
            f.render_widget(Paragraph::new(Span::raw("")), rows[1]);
            render_line_field(f, rows[2], &w.target_name, sel.is_none(), false, th);
            if dropdown_rows > 0 {
                let items: Vec<ListItem> = branches
                    .iter()
                    .take(5)
                    .enumerate()
                    .map(|(i, b)| {
                        let style = if *sel == Some(i) {
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
        GitSaveStage::Error(e) => {
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
