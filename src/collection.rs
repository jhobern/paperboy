//! The `Collection` container: a loaded `.hurl` file plus app-level state
//! (selected entry, environment, captured values, cached preview). The Hurl
//! request model, parser, serializer and `[Captures]`/`[Asserts]` evaluation
//! live in the [`crate::hurl`] module.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::git_remote::GitOrigin;
use crate::hurl::{HurlEntry, collection_to_hurl};
use crate::tree::{self, Row};

/// A loaded Hurl collection (one .hurl file).
#[derive(Clone)]
pub struct Collection {
    /// Stable runtime id (used to route async capture results back here).
    pub id: u64,
    pub name: String,
    pub entries: Vec<HurlEntry>,
    pub selected_entry: usize,
    /// The Global Environment (if any) linked/"pinned" to this collection —
    /// an id into [`crate::tui::app::TuiApp::global_envs`], not an owned
    /// [`Environment`]. Any number of collections may link the same one. Its
    /// vars take precedence over the active Global Environment's on a
    /// name collision (see [`crate::request::subst_map`]).
    pub linked_env_id: Option<u64>,
    /// Source `.hurl` file this collection was loaded from (used by "Save
    /// Collection"). `None` for the built-in Request tab until saved.
    pub path: Option<PathBuf>,
    /// Where the `.hurl` file was loaded from in git, if it was (used to show
    /// the ⎇ icon on the tab title and to default "Save Collection to Git…").
    pub git_origin: Option<GitOrigin>,
    /// Editable JSON preview of the selected request (vars substituted).
    pub request_json_buf: String,
    /// Entry index `request_json_buf` was built for; `None` means stale.
    pub request_json_for: Option<usize>,
    /// Values captured from responses (Hurl `[Captures]`), available as
    /// `{{ name }}` in subsequent requests. Runtime-only (not persisted).
    pub captures: HashMap<String, String>,
    /// The folder currently being browsed in the Requests list, encoded as a
    /// breadcrumb path (root = empty). Requests are grouped into folders by
    /// splitting their `title` on `/` (see [`crate::tree`]) — this is purely
    /// view state, never persisted, and is kept in sync with `selected_entry`
    /// whenever it changes outside of normal list navigation.
    pub folder: Vec<String>,
    /// Index into the current folder's rows (see [`crate::tree::rows_for`]),
    /// i.e. which row is highlighted in the Requests list. Not persisted.
    pub list_cursor: usize,
    /// Requests removed with `x` (List pane), most-recently-deleted last, so
    /// `u` (List pane) can bring them back in order — the exact parallel of
    /// [`crate::tui::app::TuiApp::closed_tabs`] for individual requests
    /// instead of whole collection tabs. Capped so a long session can't grow
    /// it unbounded. Runtime-only, not persisted.
    pub deleted_entries: Vec<(usize, HurlEntry)>,
    /// Set when this tab is bound to a Workspace folder (see
    /// [`crate::workspace`]) rather than a single stand-alone file. `path`
    /// still tracks whichever file within this folder is currently loaded
    /// (or `None` if the user hasn't picked one yet) — this field just marks
    /// *which* folder it was picked from, so `w` can reopen the picker
    /// scoped to it and the tab bar can show a folder icon.
    pub workspace_root: Option<PathBuf>,
    /// Whether the Workspace file picker for this tab is currently filtering
    /// to `.hurl`/`.json` files only (`true`, the default) or showing every
    /// file (`false`). Remembered across re-opens of the picker for this tab.
    pub workspace_filter_hurl_json: bool,
    /// True once the auto-open-picker prompt (see `TuiApp::draw`'s
    /// auto-prompt check) has been shown and dismissed (Esc/q) for this
    /// still-file-less Workspace tab, so it doesn't immediately reopen every
    /// frame — the user can still bring it back explicitly with `w`.
    /// Transient (not persisted); resets to `false` on restart, which is
    /// fine since a fresh restore re-prompts anyway if the file vanished.
    pub workspace_auto_prompt_dismissed: bool,
    /// Set alongside `workspace_root` when that folder was downloaded from
    /// git (see `TuiApp::confirm_workspace_root_from_git`), rather than
    /// picked from the user's own filesystem — a plain local folder must
    /// never be deleted by the app, but a throwaway git-downloaded temp
    /// directory can be, so closing a tab with this set to `true` offers to
    /// delete it (see `TuiApp::close_active_tab`) instead of closing it
    /// silently like every other tab.
    pub workspace_downloaded_from_git: bool,
    /// Where this Workspace's downloaded files came from in git — set
    /// alongside `workspace_downloaded_from_git`, `None` for a locally
    /// picked folder. Persisted (see `PersistedTab::workspace_git_origin`)
    /// so that if `workspace_root` vanishes (e.g. the OS clears `/tmp`
    /// between sessions), the app can offer to redownload the exact same
    /// commit rather than losing track of the workspace entirely — see
    /// `PersistedTab::into_collection`'s `PendingWorkspaceReload`.
    pub workspace_git_origin: Option<crate::tui::remote::WorkspaceGitOrigin>,
}

static NEXT_COLLECTION_ID: AtomicU64 = AtomicU64::new(1);

/// A process-unique id for a new collection.
pub fn next_collection_id() -> u64 {
    NEXT_COLLECTION_ID.fetch_add(1, Ordering::Relaxed)
}

impl Collection {
    pub fn new(name: String, entries: Vec<HurlEntry>) -> Self {
        let mut c = Self {
            id: next_collection_id(),
            name,
            entries,
            selected_entry: 0,
            linked_env_id: None,
            path: None,
            git_origin: None,
            request_json_buf: String::new(),
            request_json_for: None,
            captures: HashMap::new(),
            folder: Vec::new(),
            list_cursor: 0,
            deleted_entries: Vec::new(),
            workspace_root: None,
            workspace_filter_hurl_json: true,
            workspace_auto_prompt_dismissed: false,
            workspace_downloaded_from_git: false,
            workspace_git_origin: None,
        };
        c.sync_folder_to_selected();
        c
    }

    /// Serialize this collection's entries to Hurl text.
    pub fn to_hurl(&self) -> String {
        collection_to_hurl(&self.entries)
    }

    /// Discard the cached request-JSON preview so it is rebuilt from the current
    /// entry and environment. Call after the environment changes (e.g. reloaded)
    /// so freshly-resolved values flow into the next request.
    pub fn invalidate_request_json(&mut self) {
        self.request_json_buf.clear();
        self.request_json_for = None;
    }

    /// The rows to show in the Requests list for the folder currently being
    /// browsed.
    pub fn rows(&self) -> Vec<Row> {
        tree::rows_for(&self.entries, &self.folder)
    }

    /// Re-derive `folder`/`list_cursor` so the Requests list is browsing (and
    /// highlighting) `selected_entry`. Call this any time `selected_entry` is
    /// changed programmatically (as opposed to normal Up/Down/Enter list
    /// navigation, which keeps the two in sync itself) — e.g. after adding,
    /// deleting, or renaming a request, or restoring persisted state.
    pub fn sync_folder_to_selected(&mut self) {
        if self.entries.is_empty() {
            self.folder = Vec::new();
            self.list_cursor = 0;
            return;
        }
        let idx = self.selected_entry.min(self.entries.len() - 1);
        self.selected_entry = idx;
        self.folder = tree::folder_of(&self.entries, idx);
        let rows = self.rows();
        self.list_cursor = rows.iter().position(|r| *r == Row::Entry(idx)).unwrap_or(0);
    }
}
