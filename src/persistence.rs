//! Persistence of the whole application state (tabs, collections, environments)
//! between sessions.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::collection::Collection;
use crate::env_panel::EnvSource;
use crate::environment::{Environment, PendingSecret, parse_vars_pending};
use crate::git_remote::GitOrigin;
use crate::hurl::HurlEntry;
use crate::i18n::Language;
use crate::remote_flow::WorkspaceGitOrigin;
use crate::request::RequestView;

// ── Session state persistence ───────────────────────────────────────────────

/// One persisted tab: its display name plus the requests it holds.
/// A persisted environment: the variables' *source* form (provider references
/// like `{{ op://… }}`, or literals) — never resolved secret values — so it can
/// be reloaded and re-resolved on the next launch without writing secrets to disk.
/// Since environments are now global (shared across collections), this also
/// carries the source `.vars` path/git origin (moved here from `PersistedTab`).
#[derive(Serialize, Deserialize, Default, Clone)]
pub struct PersistedEnv {
    pub name: String,
    #[serde(default)]
    pub vars: Vec<PersistedVar>,
    /// Source `.vars` file this environment was loaded from (used by "Save
    /// Environment"). `None` for a hand-made environment until saved.
    #[serde(default)]
    pub path: Option<String>,
    /// Where the `.vars` file was loaded from in git, if it was.
    #[serde(default)]
    pub git_origin: Option<GitOrigin>,
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct PersistedVar {
    pub key: String,
    /// The `.vars` source token (reference or literal), re-parsed on load.
    pub raw: String,
    #[serde(default)]
    pub user_added: bool,
}

impl PersistedEnv {
    pub fn from_environment(env: &Environment) -> Self {
        Self {
            name: env.name.clone(),
            vars: env
                .vars
                .iter()
                .map(|v| PersistedVar {
                    key: v.key.clone(),
                    raw: v.raw.clone(),
                    user_added: v.user_added,
                })
                .collect(),
            path: env.path.as_ref().map(|p| p.to_string_lossy().into_owned()),
            git_origin: env.git_origin.clone(),
        }
    }

    /// Rebuild the [`Environment`], leaving any provider references marked
    /// `loading` rather than resolving them immediately. Returns the
    /// environment and its list of pending secrets (if any) so the caller can
    /// gather several environments' pending secrets together and resolve them
    /// all in a single background batch (see [`crate::environment::spawn_resolution_many`]),
    /// instead of one authorization prompt per environment.
    pub fn restore(&self) -> (Environment, Vec<PendingSecret>) {
        let content = self
            .vars
            .iter()
            .map(|v| format!("{}={}", v.key, v.raw))
            .collect::<Vec<_>>()
            .join("\n");
        let (mut env, pending) = parse_vars_pending(self.name.clone(), &content);
        // Re-apply the hand-added marker (lost by re-parsing as a file).
        for pv in &self.vars {
            if pv.user_added
                && let Some(var) = env.vars.iter_mut().find(|x| x.key == pv.key)
            {
                var.user_added = true;
            }
        }
        env.path = self
            .path
            .clone()
            .filter(|s| !s.is_empty())
            .map(std::path::PathBuf::from);
        env.git_origin = self.git_origin.clone();
        (env, pending)
    }
}

#[derive(Serialize, Deserialize, Default)]
pub struct PersistedTab {
    pub name: String,
    #[serde(default)]
    pub entries: Vec<HurlEntry>,
    #[serde(default)]
    pub selected_entry: usize,
    /// Source `.hurl` path, so "Save Collection" still targets the right file
    /// after a restart.
    #[serde(default)]
    pub path: Option<String>,
    /// Where the `.hurl` file was loaded from in git, if it was, so the ⎇ icon
    /// and "Save Collection to Git…" defaults survive a restart.
    #[serde(default)]
    pub git_origin: Option<GitOrigin>,
    /// Index into `PersistedState.global_envs` of the Global Environment
    /// linked/"pinned" to this collection, if any. Remapped to a fresh
    /// `Environment` id on restore (ids aren't stable across restarts).
    #[serde(default)]
    pub linked_env_index: Option<usize>,
    /// Root folder this tab is bound to as a Workspace (see
    /// [`crate::workspace`]), if any. When set, `entries` is NOT a trusted
    /// snapshot — [`Self::into_collection`] re-reads `path` fresh from disk
    /// instead, since a Workspace is a live folder rather than a frozen file.
    #[serde(default)]
    pub workspace_root: Option<String>,
    /// Remembered `.hurl`/`.json` filter toggle for this tab's Workspace
    /// picker. Missing/`None` (older state files) defaults to `true`.
    #[serde(default)]
    pub workspace_filter_hurl_json: Option<bool>,
    /// Whether `workspace_root` is a throwaway folder downloaded from git
    /// rather than a folder the user picked themselves — carried across a
    /// restart so closing this tab later still offers to delete it (see
    /// [`Collection::workspace_downloaded_from_git`]).
    #[serde(default)]
    pub workspace_downloaded_from_git: bool,
    /// Where this tab's Workspace was downloaded from in git, if it was —
    /// carried across a restart so a vanished `workspace_root` (see
    /// [`Self::into_collection`]) can still be offered a redownload instead
    /// of just being reported as permanently missing.
    #[serde(default)]
    pub workspace_git_origin: Option<WorkspaceGitOrigin>,
    /// Expanded folder paths in the workspace file tree, stored as
    /// forward-slash-separated paths relative to `workspace_root`.  Absent in
    /// older state files (field defaults to empty), which means all folders
    /// start collapsed — a safe, backwards-compatible default.
    #[serde(default)]
    pub workspace_expanded_paths: Vec<String>,
    /// The workspace-tree node the user last selected, as a forward-slash path
    /// relative to `workspace_root` (see [`Collection::workspace_selected`]).
    /// Absent in older state files, which simply means the tab reopens on its
    /// loaded collection file as it always did.
    #[serde(default)]
    pub workspace_selected_path: Option<String>,
}

/// Enough information to offer redownloading a Workspace whose entire
/// `workspace_root` has vanished (see [`PersistedTab::into_collection`]) —
/// only produced when the vanished folder was originally downloaded from
/// git (an ordinary local folder that was simply moved/deleted has nothing
/// to redownload, so the tab is just reset with no prompt for it).
pub struct PendingWorkspaceReload {
    pub tab_name: String,
    pub origin: WorkspaceGitOrigin,
    /// The previously-selected file's path *relative to* the old (now dead)
    /// `workspace_root`, so it can be re-resolved against a freshly
    /// downloaded folder — which will have a different absolute temp path.
    pub relative_selected_path: Option<String>,
}

/// A path rendered as forward-slash-separated components, so relative paths in
/// `state.json` round-trip between platforms.
fn rel_slashes(rel: &std::path::Path) -> String {
    rel.components()
        .filter_map(|comp| comp.as_os_str().to_str())
        .collect::<Vec<_>>()
        .join("/")
}

impl PersistedTab {
    /// Snapshot a collection's persistable parts. `linked_env_index` is
    /// filled in by the caller (it needs the full global list to resolve the
    /// collection's `linked_env_id` to an index).
    pub fn from_collection(c: &Collection, linked_env_index: Option<usize>) -> Self {
        Self {
            name: c.name.clone(),
            // A Workspace-bound tab's entries are never a trusted snapshot —
            // its folder is re-scanned and the selected file re-read fresh
            // from disk on restore (see `into_collection`), so there's no
            // point (and some risk of staleness) in persisting them here.
            entries: if c.workspace_root.is_some() {
                Vec::new()
            } else {
                c.entries.clone()
            },
            selected_entry: c.selected_entry,
            path: c.path.as_ref().map(|p| p.to_string_lossy().into_owned()),
            git_origin: c.git_origin.clone(),
            linked_env_index,
            workspace_root: c
                .workspace_root
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned()),
            workspace_filter_hurl_json: c
                .workspace_root
                .is_some()
                .then_some(c.workspace_filter_hurl_json),
            workspace_downloaded_from_git: c.workspace_downloaded_from_git,
            workspace_git_origin: c.workspace_git_origin.clone(),
            // Serialise expanded paths relative to workspace_root using
            // forward slashes so they survive cross-platform round-trips.
            workspace_expanded_paths: c
                .workspace_root
                .as_ref()
                .map(|root| {
                    let mut paths: Vec<String> = c
                        .workspace_expanded
                        .iter()
                        .filter_map(|abs| abs.strip_prefix(root).ok().map(rel_slashes))
                        .filter(|s| !s.is_empty())
                        .collect();
                    paths.sort(); // deterministic JSON
                    paths
                })
                .unwrap_or_default(),
            workspace_selected_path: match (&c.workspace_root, &c.workspace_selected) {
                (Some(root), Some(sel)) => sel.strip_prefix(root).ok().map(rel_slashes),
                _ => None,
            },
        }
    }

    /// Rebuild a collection from persisted data. `linked_env_id` is resolved
    /// by the caller from `linked_env_index` (see [`Self::from_collection`]),
    /// against the freshly-restored global environments' ids. Also returns
    /// a [`PendingWorkspaceReload`] whenever this tab's entire Workspace
    /// root has vanished *and* it's known to have come from git — the
    /// caller (`TuiApp::apply_persisted`) uses this to offer redownloading
    /// it instead of just reporting it as permanently gone.
    pub fn into_collection(
        self,
        linked_env_id: Option<u64>,
    ) -> (Collection, Option<PendingWorkspaceReload>) {
        let workspace_root = self
            .workspace_root
            .filter(|s| !s.is_empty())
            .map(std::path::PathBuf::from);
        let path = self
            .path
            .filter(|s| !s.is_empty())
            .map(std::path::PathBuf::from);

        // If the whole workspace root is gone (not just the last-selected
        // file) — e.g. it was a git-downloaded temp folder and the OS swept
        // /tmp since the last session — there's nothing left to re-read or
        // re-scan at all. Reset the tab back to an ordinary "no collection
        // chosen yet" tab (no folder/git icons, no phantom root) rather than
        // silently keeping a dead path around: an empty workspace picker
        // with no explanation would be far more confusing than a plain tab,
        // and `workspace_downloaded_from_git` would otherwise keep offering
        // a nonsensical "keep the (already-gone) folder?" choice on close.
        // The caller (`TuiApp::apply_persisted`) detects this by comparing
        // `workspace_root` before/after and surfaces a status message (or,
        // if `pending_reload` below is `Some`, a redownload prompt instead).
        let root_missing = workspace_root.as_ref().is_some_and(|r| !r.exists());

        // Captured *before* anything below is reset/moved away: if this was
        // a git download, remember enough to offer redownloading it — the
        // previously-selected file's path *relative to* the (now-dead) root,
        // so it can be re-resolved against a freshly downloaded folder,
        // which will have a different absolute temp path.
        let pending_reload = if root_missing {
            self.workspace_git_origin.clone().map(|origin| {
                let relative_selected_path = match (&path, &workspace_root) {
                    (Some(p), Some(root)) => p
                        .strip_prefix(root)
                        .ok()
                        .map(|rel| rel.to_string_lossy().into_owned()),
                    _ => None,
                };
                PendingWorkspaceReload {
                    tab_name: self.name.clone(),
                    origin,
                    relative_selected_path,
                }
            })
        } else {
            None
        };

        let workspace_root = if root_missing { None } else { workspace_root };

        // A Workspace tab is bound to a live folder, not a frozen file: on
        // restart, re-read whichever file was last selected straight from
        // disk. If the root (or just that one file) has vanished since, fall
        // back to the empty "no collection chosen yet" state instead of
        // showing stale content — the picker auto-opens to let the user pick
        // a replacement.
        let (entries, path) = if workspace_root.is_some() {
            match path
                .as_ref()
                .filter(|p| p.exists())
                .and_then(|p| std::fs::read_to_string(p).ok())
            {
                Some(content) => (crate::postman::parse_collection(&content), path),
                None => (Vec::new(), None),
            }
        } else if root_missing {
            (Vec::new(), None)
        } else {
            (self.entries, path)
        };

        let mut c = Collection::new(self.name, entries);
        c.selected_entry = self.selected_entry.min(c.entries.len().saturating_sub(1));
        c.path = path;
        c.git_origin = self.git_origin;
        c.linked_env_id = linked_env_id;
        c.workspace_root = workspace_root;
        c.workspace_filter_hurl_json = self.workspace_filter_hurl_json.unwrap_or(true);
        c.workspace_downloaded_from_git = self.workspace_downloaded_from_git && !root_missing;
        c.workspace_git_origin = if root_missing {
            None
        } else {
            self.workspace_git_origin
        };
        c.sync_folder_to_selected();
        // Restore expanded folders: convert relative slash-paths back to
        // absolute paths by prepending workspace_root.
        c.workspace_expanded = if let Some(root) = &c.workspace_root {
            self.workspace_expanded_paths
                .iter()
                .filter(|s| !s.is_empty())
                .map(|rel| root.join(rel))
                .collect()
        } else {
            std::collections::HashSet::new()
        };
        // Ensure the loaded file's ancestor folders are expanded so it is
        // immediately visible, then position the cursor on it.
        c.expand_ancestors_for_path();
        // Collections that were left expanded last session must list their
        // requests without having been opened yet this session — read their
        // names from disk into the cache.
        c.rebuild_expanded_titles();
        c.sync_ws_cursor();
        // Only remember a selection whose file is still there — a workspace is a
        // live folder, so the node may well have been deleted or renamed since,
        // and reopening on a path that no longer exists would just report an
        // error the user didn't ask for.
        c.workspace_selected = match (&c.workspace_root, &self.workspace_selected_path) {
            (Some(root), Some(rel)) if !rel.is_empty() => {
                Some(root.join(rel)).filter(|p| p.exists())
            }
            _ => None,
        };
        (c, pending_reload)
    }
}

/// One persisted report tab (see [`crate::report::Report`]). The `.trail`
/// *source text* is snapshotted (like [`PersistedTab`] snapshots collection
/// entries) so an unsaved scratch report survives a restart; `path`/`git_origin`
/// keep "Save" targeting the right place after a restart.
#[derive(Serialize, Deserialize, Default, Clone)]
pub struct PersistedReport {
    pub name: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub git_origin: Option<GitOrigin>,
    /// When this report was opened from a Workspace tree, the workspace root
    /// folder (absolute path) so it re-embeds in that Workspace collection tab's
    /// right pane on restart. `None` for an ordinary standalone report tab.
    #[serde(default)]
    pub workspace_root: Option<String>,
    /// For a Workspace-embedded report, whether it was the one *shown* in its
    /// Workspace tab's right pane (`true`) rather than merely retained while the
    /// tab showed its request/response view (`false`). Ignored for standalone
    /// reports. Older state files (no field) default to `true`, matching the
    /// "opened ⇒ shown" behaviour.
    #[serde(default = "default_true")]
    pub embedded_active: bool,
}

fn default_true() -> bool {
    true
}

impl PersistedReport {
    /// Snapshot a report's persistable parts.
    pub fn from_report(r: &crate::report::Report) -> Self {
        Self {
            name: r.name.clone(),
            text: r.text.clone(),
            path: r.path.as_ref().map(|p| p.to_string_lossy().into_owned()),
            git_origin: r.git_origin.clone(),
            // Workspace context lives on the TUI `ReportTab`, not the core
            // `Report`; the tui layer fills these in when snapshotting (see
            // `TuiApp::to_persisted`).
            workspace_root: None,
            embedded_active: true,
        }
    }

    /// Rebuild an in-memory report from persisted state. A restored report is
    /// not marked dirty (it matches what was last saved/persisted).
    pub fn into_report(self) -> crate::report::Report {
        crate::report::Report {
            id: crate::report::report::next_report_id(),
            name: self.name,
            text: self.text,
            path: self.path.map(PathBuf::from),
            git_origin: self.git_origin,
            dirty: false,
        }
    }
}

/// Which view the GUI's centre column was showing, so it reopens on the same
/// thing rather than always dropping the user back on the request editor.
///
/// A report opened from a Workspace file has no stable identity to persist (it
/// is addressed by an on-disk path that may have moved), so those record as
/// [`GuiView::Reports`] — the list is still the right place to land.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum GuiView {
    /// The request editor (the default view).
    #[default]
    Requests,
    /// The reports list.
    Reports,
    /// The block editor open on the session report at this index.
    Report(usize),
}

/// Window and panel geometry for the graphical front-end.
///
/// The terminal UI's `list_width`/`response_pct` are measured in character
/// cells and percentages of a text grid, so they cannot describe a pixel layout
/// the user dragged with a mouse — rounding one into the other would creep the
/// panels every time the two front-ends were used in turn. The GUI therefore
/// records its own geometry here and the two leave each other's alone.
///
/// Every size is optional: `None` means "never adjusted", which is what lets
/// the GUI fall back to a layout derived from the terminal defaults on a fresh
/// profile.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug, Default)]
pub struct GuiLayout {
    /// Inner size of the window in logical points.
    #[serde(default)]
    pub window: Option<(f32, f32)>,
    /// Width of the left column (Requests + Global Environments).
    #[serde(default)]
    pub left_width: Option<f32>,
    /// Height of the Global Environments panel inside the left column.
    #[serde(default)]
    pub env_height: Option<f32>,
    /// Height of the Response panel under the request editor.
    #[serde(default)]
    pub response_height: Option<f32>,
    /// Height of the report editor's diagnostics panel.
    #[serde(default)]
    pub report_diag_height: Option<f32>,
    /// Width of the report editor's block palette column.
    #[serde(default)]
    pub report_palette_width: Option<f32>,
    /// Which centre-column view was open.
    #[serde(default)]
    pub view: GuiView,
    /// Whether the open report editor was showing Blocks or Source. Stored as
    /// a flag rather than the full `EditorView` because the Results view has
    /// nothing to show until the report is run again.
    #[serde(default)]
    pub report_source_view: bool,
}

/// The full application state saved between sessions. Environments are stored
/// in *source* form only (references/literals) so resolved secrets are never
/// written to disk; they are re-resolved on load.
#[derive(Serialize, Deserialize)]
pub struct PersistedState {
    #[serde(default)]
    pub language: Language,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub tabs: Vec<PersistedTab>,
    /// Persisted report tabs (see [`PersistedReport`]). Restored after the
    /// collection tabs on launch.
    #[serde(default)]
    pub reports: Vec<PersistedReport>,
    /// The tab that was active when the app was last closed, so it reopens on
    /// the same tab.
    #[serde(default)]
    pub active_tab: usize,
    /// Last folder a file was chosen from in the TUI file browser, so it can
    /// reopen there next time.
    #[serde(default)]
    pub last_browse_dir: Option<String>,
    /// Last folder an *environment* file was loaded from, so the environment
    /// picker can reopen there independently of other file loads.
    #[serde(default)]
    pub last_env_dir: Option<String>,
    /// Ask for confirmation before quitting the app.
    #[serde(default = "yes")]
    pub confirm_on_exit: bool,
    /// Ask for confirmation before closing all collections.
    #[serde(default = "yes")]
    pub confirm_on_clear: bool,
    /// Ask for confirmation before deleting a Global Environment.
    #[serde(default = "yes")]
    pub confirm_on_delete_env: bool,
    /// When set, auto-pick "Save" on a Save/Discard/Cancel unsaved-changes
    /// prompt (Workspace collection switch or git push) instead of showing it.
    #[serde(default)]
    pub always_save_when_prompted: bool,
    /// Width (in columns) of the left column (Requests/Environment panels),
    /// user-adjustable with `<`/`>`.
    #[serde(default = "default_list_width")]
    pub list_width: u16,
    /// Height (as a percentage) of the Response/Environment panels, relative to
    /// their column, user-adjustable with `+`/`-`.
    #[serde(default = "default_response_pct")]
    pub response_pct: u16,
    /// Git URLs the user has loaded a collection/environment from, most recent
    /// first, offered as a pickable list in the "Load from Git" wizard.
    #[serde(default)]
    pub recent_git_urls: Vec<String>,
    /// Which of JSON / Hurl text the Main (Request) panel shows by default for
    /// every request (Settings → Preferences → Default Request View).
    #[serde(default)]
    pub default_request_view: RequestView,
    /// Run "Run All" in batch mode (whole collection in one Hurl execution)
    /// rather than streaming per-entry. Off by default (streaming).
    #[serde(default)]
    pub run_all_batch_mode: bool,
    /// User-created themes (Settings → Theme). Built-in presets are not stored.
    #[serde(default)]
    pub custom_themes: Vec<crate::tui::theme::ThemeSpec>,
    /// The explicitly-chosen theme name, or `None` to follow the language
    /// preset. Persisted so a manual theme choice survives restarts.
    #[serde(default)]
    pub active_theme: Option<String>,
    /// The global list of Environments (source form only — no resolved
    /// secrets), shared across all collections. Replaces the old per-tab
    /// `env` field.
    #[serde(default)]
    pub global_envs: Vec<PersistedEnv>,
    /// Index into `global_envs` of the currently-activated Global
    /// Environment, if any.
    #[serde(default)]
    pub active_global_env: Option<usize>,
    /// Which source(s) the Environments panel lists. Shared by both front-ends
    /// because they share the same row model and saved state file.
    #[serde(default)]
    pub env_source: EnvSource,
    /// GUI-only window/panel geometry and last-open view (see [`GuiLayout`]).
    /// Ignored by the terminal UI, which round-trips it untouched so using one
    /// front-end never discards the other's layout.
    #[serde(default)]
    pub gui: GuiLayout,
}

fn yes() -> bool {
    true
}

fn default_list_width() -> u16 {
    38
}

fn default_response_pct() -> u16 {
    42
}

impl Default for PersistedState {
    fn default() -> Self {
        Self {
            language: Language::default(),
            base_url: String::new(),
            tabs: Vec::new(),
            reports: Vec::new(),
            active_tab: 0,
            last_browse_dir: None,
            last_env_dir: None,
            confirm_on_exit: true,
            confirm_on_clear: true,
            confirm_on_delete_env: true,
            always_save_when_prompted: false,
            list_width: default_list_width(),
            response_pct: default_response_pct(),
            recent_git_urls: Vec::new(),
            default_request_view: RequestView::default(),
            run_all_batch_mode: false,
            custom_themes: Vec::new(),
            active_theme: None,
            global_envs: Vec::new(),
            active_global_env: None,
            env_source: EnvSource::Both,
            gui: GuiLayout::default(),
        }
    }
}

/// Location of the saved state file. Honours `PAPERBOY_STATE_DIR` (used by
/// tests), else falls back to the platform config directory.
fn state_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("PAPERBOY_STATE_DIR") {
        return Some(PathBuf::from(dir).join("state.json"));
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .or_else(|| std::env::var_os("APPDATA").map(PathBuf::from))?;
    Some(base.join("paperboy").join("state.json"))
}

/// Load the saved state, or `None` if there is no readable/parsable state file.
pub fn load_state() -> Option<PersistedState> {
    let text = fs::read_to_string(state_path()?).ok()?;
    serde_json::from_str(&text).ok()
}

/// Write the given state to disk, creating the config directory if needed.
/// A no-op under `cargo test` so unit tests never touch the real config dir
/// (end-to-end persistence is covered by running the real binary).
pub fn save_state(state: &PersistedState) {
    if cfg!(test) {
        return;
    }
    let Some(path) = state_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(state) {
        let _ = fs::write(path, json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_report_round_trips_through_serde_and_back() {
        let mut r = crate::report::Report::from_text(
            "nightly",
            "# name: Nightly\n# collection: c.hurl\nREQUEST Oauth\n",
        );
        r.path = Some(PathBuf::from("/tmp/nightly.trail"));

        let persisted = PersistedReport::from_report(&r);
        let json = serde_json::to_string(&persisted).unwrap();
        let back: PersistedReport = serde_json::from_str(&json).unwrap();
        let restored = back.into_report();

        assert_eq!(restored.name, "Nightly");
        assert_eq!(restored.text, r.text);
        assert_eq!(restored.path, r.path);
        assert!(!restored.dirty, "a restored report is not dirty");
    }

    /// A Workspace tab reopens on whatever node was last selected in its tree,
    /// including the `.trail` reports and `.vars` environments that previously
    /// left no trace at all.
    #[test]
    fn a_workspaces_selected_node_round_trips_and_drops_when_it_vanishes() {
        let root =
            std::env::temp_dir().join(format!("paperboy_ws_selected_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("nightly")).unwrap();
        let report = root.join("nightly/run.trail");
        std::fs::write(&report, "REQUEST A\n").unwrap();

        let mut c = Collection::new("ws".to_string(), Vec::new());
        c.workspace_root = Some(root.clone());
        c.workspace_selected = Some(report.clone());

        let persisted = PersistedTab::from_collection(&c, None);
        assert_eq!(
            persisted.workspace_selected_path.as_deref(),
            Some("nightly/run.trail"),
            "stored relative to the root, with forward slashes"
        );

        let json = serde_json::to_string(&persisted).unwrap();
        let back: PersistedTab = serde_json::from_str(&json).unwrap();
        let (restored, _) = back.into_collection(None);
        assert_eq!(restored.workspace_selected, Some(report.clone()));

        // A workspace is a live folder: a node deleted since the last session
        // must not come back as a selection that only produces an error.
        std::fs::remove_file(&report).unwrap();
        let json = serde_json::to_string(&PersistedTab::from_collection(&c, None)).unwrap();
        let back: PersistedTab = serde_json::from_str(&json).unwrap();
        let (restored, _) = back.into_collection(None);
        assert_eq!(restored.workspace_selected, None);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// An older `state.json` has no selection recorded; the tab must still open.
    #[test]
    fn a_state_file_without_a_workspace_selection_still_loads() {
        let tab: PersistedTab = serde_json::from_str(r#"{"name":"ws","entries":[]}"#).unwrap();
        assert_eq!(tab.workspace_selected_path, None);
        let (c, _) = tab.into_collection(None);
        assert_eq!(c.workspace_selected, None);
    }

    #[test]
    fn persisted_state_defaults_have_no_reports() {
        let state = PersistedState::default();
        assert!(state.reports.is_empty());
    }
}
