//! The `Collection` container: a loaded `.hurl` file plus app-level state
//! (selected entry, environment, captured values, cached preview). The Hurl
//! request model, parser, serializer and `[Captures]`/`[Asserts]` evaluation
//! live in the [`crate::hurl`] module.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::git_remote::GitOrigin;
use crate::hurl::{HurlEntry, collection_to_hurl};
use crate::tree::{self, Row};

/// The label shown for a request row in the workspace tree: the leaf segment of
/// its folder-encoded title (e.g. `Auth/Login` → `Login`, since the `Auth`
/// folder is already its own tree row), falling back to the URL when the
/// request is untitled.
fn ws_request_label(entry: &HurlEntry) -> String {
    let leaf = crate::tree::entry_path(&entry.title)
        .pop()
        .unwrap_or_default();
    if leaf.is_empty() {
        entry.url.clone()
    } else {
        leaf
    }
}

/// Parse a collection file into the display labels of its requests, for listing
/// a not-currently-loaded collection's requests in the workspace tree. Returns
/// an empty vec when the file can't be read or parsed — the collection then
/// simply shows no requests until it is opened.
fn read_collection_labels(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|content| {
            crate::postman::parse_collection(&content)
                .iter()
                .map(ws_request_label)
                .collect()
        })
        .unwrap_or_default()
}

/// One row in the Workspace tab's file-tree request list (see
/// [`Collection::ws_rows`]). Unlike [`Row`], which navigates the *virtual*
/// folders encoded in request titles inside one file, this navigates the real
/// filesystem under the workspace root and inlines expanded collections'
/// requests directly beneath their file rows (an accordion).
///
/// The tree is a real expand/collapse tree: `workspace_expanded` (on
/// [`Collection`]) holds the set of open folders *and* open collection files;
/// visibility is derived depth-first from that set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsRow {
    /// A folder in the workspace tree.  `expanded` is true when the folder is
    /// in the tab's `workspace_expanded` set (its children are visible).
    Folder {
        path: PathBuf,
        name: String,
        depth: usize,
        expanded: bool,
    },
    /// A collection file in the workspace tree.  `open` is true when the file's
    /// path is in the tab's `workspace_expanded` set, i.e. its inline request
    /// names are shown beneath it (whether or not it is the loaded file).
    Collection {
        path: PathBuf,
        name: String,
        depth: usize,
        open: bool,
    },
    /// A PaperTrail report file (`.trail`).  Selecting it opens the
    /// workspace-aware report view embedded in the right pane.
    Report {
        path: PathBuf,
        name: String,
        depth: usize,
    },
    /// An environment file (`.vars`).  Selecting it loads the file as a global
    /// environment (the same path as File → Load → Environment) rather than
    /// trying to parse it as a collection.
    Environment {
        path: PathBuf,
        name: String,
        depth: usize,
    },
    /// A request shown indented under its [`WsRow::Collection`] row; `depth` is
    /// the collection's depth + 1. `collection` is the owning file's path and
    /// `idx` the request's position within it. When `loaded` is true the file
    /// is the tab's currently-loaded collection, so `idx` indexes `entries` and
    /// the row renders in full detail; when false the row is drawn from the
    /// cached `name` only (see `workspace_titles`) and selecting it previews the
    /// name — opening it (Enter/Right) loads that collection first.
    Request {
        collection: PathBuf,
        idx: usize,
        name: String,
        depth: usize,
        loaded: bool,
    },
}

impl WsRow {
    /// The filesystem path a row stands for — the file itself for a file row,
    /// the owning collection for a request (which has no file of its own).
    ///
    /// Lets callers that only care *where* a row is (revealing a just-created
    /// file, deciding which folder a new one goes in) avoid re-matching all
    /// five variants each time.
    pub fn path(&self) -> &Path {
        match self {
            WsRow::Folder { path, .. }
            | WsRow::Collection { path, .. }
            | WsRow::Report { path, .. }
            | WsRow::Environment { path, .. } => path,
            WsRow::Request { collection, .. } => collection,
        }
    }
}

/// How long a workspace tree scan is reused before the disk is read again.
///
/// The scan is a recursive `read_dir` of the whole workspace, and the graphical
/// front-end asks for the tree once per frame — so at 60fps this was real
/// filesystem I/O sixty times a second, growing with the size of the tree, just
/// to redraw a list that hadn't changed.
///
/// The window is deliberately short and deliberately *time*-based rather than
/// invalidated by the app's own file operations. A tree can change from outside
/// PaperBoy (another editor, a `git pull`, a test run dropping result files),
/// so a cache keyed only on things PaperBoy knows it did would go stale in
/// exactly the cases the tree matters most. A third of a second is below the
/// threshold at which a file list reads as "live", and it still collapses
/// ~95% of the scans.
const WS_SCAN_TTL: Duration = Duration::from_millis(300);

/// The last workspace tree read off disk, and what it was read for.
#[derive(Clone)]
struct WsScan {
    root: PathBuf,
    filter_hurl_json: bool,
    taken_at: Instant,
    /// [`crate::workspace::tree_generation`] when this was taken, so PaperBoy's
    /// own file operations don't have to wait out [`WS_SCAN_TTL`].
    generation: u64,
    entries: Vec<crate::workspace::WsEntry>,
}

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
    pub workspace_git_origin: Option<crate::remote_flow::WorkspaceGitOrigin>,
    /// For a Workspace tab, the set of *expanded* node paths (absolute) in the
    /// file-tree — both folders (whose child entries are shown) and collection
    /// files (whose inline request names are shown). A node is visible when all
    /// its ancestor folders are also in this set. Persisted so the tree state
    /// survives restarts. Absolute paths in memory; serialised relative to
    /// `workspace_root` in [`crate::persistence`].
    pub workspace_expanded: HashSet<PathBuf>,
    /// For a Workspace tab, the node in the tree the user last selected
    /// (absolute path): a collection file, a `.trail` report or a `.vars`
    /// environment. Persisted (relative to `workspace_root`) so the tab reopens
    /// on whatever was being worked on rather than an empty right-hand pane.
    ///
    /// A selected *request* is already fully described by `path` +
    /// `selected_entry`; this records the collection file in that case, which
    /// is all the restore needs on top of those two.
    ///
    /// Written by the graphical front-end only — the terminal UI drives the
    /// tree from its own cursor and simply carries this through a save/load,
    /// exactly as it does the GUI's panel geometry.
    pub workspace_selected: Option<PathBuf>,
    /// For a Workspace tab, cached request *names* (leaf titles) of expanded
    /// collection files that are **not** the currently-loaded one — the loaded
    /// file renders its rows straight from `entries`, so it needs no cache.
    /// Lets [`Self::ws_rows`] list several collections' requests at once without
    /// re-reading and parsing each file every frame. Populated when switching
    /// away from a loaded file (from its live entries) and when restoring an
    /// expanded collection from disk (see [`Self::rebuild_expanded_titles`]).
    /// Derived state — not persisted.
    pub workspace_titles: HashMap<PathBuf, Vec<String>>,
    /// The most recent workspace tree read off disk, reused for [`WS_SCAN_TTL`]
    /// so redrawing the tree isn't a recursive `read_dir` every frame. Purely
    /// derived state: dropping it only costs one extra scan, which is why it is
    /// neither persisted nor part of any equality check.
    workspace_scan: RefCell<Option<WsScan>>,
    /// Unsaved edits belonging to workspace collection files that are **not**
    /// the currently-loaded one.
    ///
    /// A Workspace tab holds exactly one file's requests in `entries` at a
    /// time, so opening a second collection from the tree used to overwrite —
    /// and silently discard — whatever the user had just typed into the first.
    /// The outgoing file's entries are parked here instead and handed straight
    /// back when it is opened again, which is what makes "edit a request, go
    /// look at another collection, come back" behave the way anyone would
    /// expect. Cleared for a file when it is written to disk
    /// ([`Self::mark_saved`]).
    ///
    /// Runtime-only. A workspace tab's entries are never a trusted snapshot
    /// across a restart (see [`crate::persistence`]), so neither are these.
    pub workspace_pending: HashMap<PathBuf, Vec<HurlEntry>>,
}

static NEXT_COLLECTION_ID: AtomicU64 = AtomicU64::new(1);

/// A process-unique id for a new collection.
pub fn next_collection_id() -> u64 {
    NEXT_COLLECTION_ID.fetch_add(1, Ordering::Relaxed)
}

/// Write collection text to `path`, creating the folder it lives in if that has
/// gone missing since the file was opened. The error carries the path, because
/// a bulk save spans several files and "permission denied" on its own would not
/// say which one refused.
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
fn write_hurl(path: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
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
            workspace_expanded: HashSet::new(),
            workspace_selected: None,
            workspace_titles: HashMap::new(),
            workspace_scan: RefCell::new(None),
            workspace_pending: HashMap::new(),
        };
        c.sync_folder_to_selected();
        c
    }

    /// Serialize this collection's entries to Hurl text.
    pub fn to_hurl(&self) -> String {
        collection_to_hurl(&self.entries)
    }

    /// The first enabled `[Form]`/`[Multipart]` file field with an empty path,
    /// as `(request title, field key)`. Such a field serializes to an invalid
    /// `file,;` line that PaperBoy's own Hurl parser rejects, so a file written
    /// with one couldn't be reloaded. Saves are refused until it's filled in.
    pub fn first_empty_file_field(&self) -> Option<(String, String)> {
        self.entries.iter().find_map(|e| {
            e.first_empty_file_field()
                .map(|k| (e.title.clone(), k.to_string()))
        })
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

    /// True when this tab is bound to a Workspace folder (so the list uses the
    /// filesystem file-tree via [`Self::ws_rows`] instead of [`Self::rows`]).
    pub fn is_workspace(&self) -> bool {
        self.workspace_root.is_some()
    }

    /// The rows to show in a Workspace tab's expand/collapse file tree.
    ///
    /// Uses [`crate::workspace::scan_workspace`] for the full depth-first tree,
    /// then applies the `workspace_expanded` visibility filter: an entry is
    /// shown only when every ancestor folder in the DFS path is in that set.
    /// Folders render with a chevron (expanded ▾ / collapsed ▸); selecting a
    /// collection file opens it (with inline requests beneath); selecting a
    /// report embeds it in the right pane.  Empty for a non-Workspace tab.
    pub fn ws_rows(&self) -> Vec<WsRow> {
        self.ws_rows_at(Instant::now())
    }

    /// [`Self::ws_rows`] as of a given moment, so the scan cache's expiry can be
    /// tested without sleeping.
    pub(crate) fn ws_rows_at(&self, now: Instant) -> Vec<WsRow> {
        self.ws_rows_as_of(now, crate::workspace::tree_generation())
    }

    /// [`Self::ws_rows`] as of a given moment *and* a given tree generation.
    ///
    /// The generation is a parameter rather than read from the global counter
    /// so a test of the time-based expiry can't be perturbed by another test
    /// running in parallel that happens to create a workspace file.
    pub(crate) fn ws_rows_as_of(&self, now: Instant, generation: u64) -> Vec<WsRow> {
        let Some(root) = &self.workspace_root else {
            return Vec::new();
        };

        self.refresh_scan(root, now, generation);
        // Held across the whole loop so the tree is walked in place rather than
        // cloned out of the cache every frame — the point of the cache is to
        // stop doing work per frame, not to trade I/O for an allocation.
        let scan = self.workspace_scan.borrow();
        let full_tree = scan
            .as_ref()
            .map(|s| s.entries.as_slice())
            .unwrap_or_default();
        let mut out = Vec::new();

        // `ancestor_at[d]` holds the absolute path of the most-recently-visited
        // directory at depth d.  A row at depth D is visible iff every slot
        // ancestor_at[0..D] points to a path in `workspace_expanded`.
        let mut ancestor_at: Vec<Option<PathBuf>> = Vec::new();

        for entry in full_tree {
            let d = entry.depth;

            // Moving to a shallower depth: slots d.. are no longer our ancestors.
            if ancestor_at.len() > d {
                ancestor_at.truncate(d);
            }

            // Visible iff every containing ancestor folder is expanded.
            let visible = ancestor_at.iter().all(|opt| {
                opt.as_ref()
                    .is_some_and(|p| self.workspace_expanded.contains(p))
            });

            if entry.is_dir {
                // Record this directory as the current ancestor at depth d,
                // so its descendants can check its expansion state.
                if ancestor_at.len() == d {
                    ancestor_at.push(Some(entry.path.clone()));
                } else {
                    ancestor_at[d] = Some(entry.path.clone());
                }

                if visible {
                    let expanded = self.workspace_expanded.contains(&entry.path);
                    out.push(WsRow::Folder {
                        path: entry.path.clone(),
                        name: entry.display_name.clone(),
                        depth: d,
                        expanded,
                    });
                }
            } else if visible {
                if crate::workspace::is_report_file(&entry.path) {
                    out.push(WsRow::Report {
                        path: entry.path.clone(),
                        name: entry.display_name.clone(),
                        depth: d,
                    });
                } else if crate::workspace::is_env_file(&entry.path) {
                    out.push(WsRow::Environment {
                        path: entry.path.clone(),
                        name: entry.display_name.clone(),
                        depth: d,
                    });
                } else {
                    let expanded = self.workspace_expanded.contains(&entry.path);
                    out.push(WsRow::Collection {
                        path: entry.path.clone(),
                        name: entry.display_name.clone(),
                        depth: d,
                        open: expanded,
                    });
                    if expanded {
                        out.extend(self.request_rows_for(&entry.path, d + 1));
                    }
                }
            }
        }
        out
    }

    /// Read the workspace tree off disk if what's cached is missing, stale, or
    /// was taken for a different root or filter.
    ///
    /// The *visibility* half of [`Self::ws_rows_at`] (the expand/collapse
    /// filter) is deliberately left out of the cache: expanding a folder must
    /// feel instant, and re-filtering an already-scanned tree costs nothing.
    fn refresh_scan(&self, root: &Path, now: Instant, generation: u64) {
        let mut slot = self.workspace_scan.borrow_mut();
        let usable = slot.as_ref().is_some_and(|s| {
            s.root == root
                && s.filter_hurl_json == self.workspace_filter_hurl_json
                && s.generation == generation
                && now.saturating_duration_since(s.taken_at) < WS_SCAN_TTL
        });
        if usable {
            return;
        }
        *slot = Some(WsScan {
            root: root.to_path_buf(),
            filter_hurl_json: self.workspace_filter_hurl_json,
            taken_at: now,
            generation,
            entries: crate::workspace::scan_workspace(root, self.workspace_filter_hurl_json),
        });
    }

    /// Every `.vars` (or env-shaped `.json`) file in this tab's workspace, for
    /// the Environments panel to list alongside the loaded environments.
    ///
    /// Served out of the same cached tree walk [`Self::ws_rows`] uses rather
    /// than scanning the disk itself: both front-ends' environment panels ask
    /// for this list several times per frame — once for the rows, once for the
    /// unfiltered rows behind the "no matches" message, once for the empty
    /// state — and each of those used to be a full recursive `read_dir` of the
    /// workspace. The cached scan holds every non-hidden file whichever way the
    /// tab's display filter is set (the filter only ever *narrows* to the
    /// workspace's own file types, and `.vars` is one of them), so it can
    /// answer this without a second walk.
    ///
    /// Empty for a tab that isn't a workspace.
    pub fn workspace_env_files(&self) -> Vec<PathBuf> {
        self.workspace_env_files_as_of(Instant::now(), crate::workspace::tree_generation())
    }

    /// [`Self::workspace_env_files`] as of a given moment and tree generation,
    /// so the cache can be tested without racing its expiry.
    pub(crate) fn workspace_env_files_as_of(&self, now: Instant, generation: u64) -> Vec<PathBuf> {
        let Some(root) = self.workspace_root.clone() else {
            return Vec::new();
        };
        self.refresh_scan(&root, now, generation);
        let scan = self.workspace_scan.borrow();
        scan.as_ref()
            .map(|s| {
                s.entries
                    .iter()
                    .filter(|e| !e.is_dir && crate::workspace::is_env_file(&e.path))
                    .map(|e| e.path.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The request rows shown under an expanded collection at `path`, indented
    /// to `depth`. For the currently-loaded file the rows come straight from
    /// `entries` (full detail, `loaded: true`); for any other expanded
    /// collection they come from the cached names in `workspace_titles`
    /// (`loaded: false`), so several collections' requests can be listed at once
    /// without re-parsing every file each frame. A collection with no cached
    /// names yet contributes no rows.
    fn request_rows_for(&self, path: &Path, depth: usize) -> Vec<WsRow> {
        if self.path.as_deref() == Some(path) {
            self.entries
                .iter()
                .enumerate()
                .map(|(idx, e)| WsRow::Request {
                    collection: path.to_path_buf(),
                    idx,
                    name: ws_request_label(e),
                    depth,
                    loaded: true,
                })
                .collect()
        } else {
            self.workspace_titles
                .get(path)
                .map(|names| {
                    names
                        .iter()
                        .enumerate()
                        .map(|(idx, name)| WsRow::Request {
                            collection: path.to_path_buf(),
                            idx,
                            name: name.clone(),
                            depth,
                            loaded: false,
                        })
                        .collect()
                })
                .unwrap_or_default()
        }
    }

    /// Cache the loaded file's request names under its own path, derived from
    /// the live `entries` (so in-memory edits/renames are reflected). Call this
    /// just before switching the loaded file away, so a collection left
    /// expanded keeps listing its requests from the cache.
    pub fn snapshot_loaded_titles(&mut self) {
        if let Some(path) = self.path.clone() {
            let names = self.entries.iter().map(ws_request_label).collect();
            self.workspace_titles.insert(path, names);
        }
    }

    /// Load a workspace collection file (Hurl or Postman JSON) at `path` into
    /// this tab, replacing its currently-loaded requests. Front-end agnostic —
    /// it only mutates this `Collection` (both the terminal UI and the GUI call
    /// it, then add their own focus/status handling). The tab's
    /// `workspace_root`/`workspace_filter_hurl_json` are left untouched. Caches
    /// the outgoing file's request names first so a still-expanded previous file
    /// keeps listing its requests, then re-syncs the tree cursor/selection and
    /// expands the new file's ancestors so it's visible.
    pub fn load_workspace_file(&mut self, path: PathBuf) -> std::io::Result<()> {
        // Prefer edits parked when we switched away from this file — they are
        // by definition newer than what is still on disk. Read (and fail) before
        // touching any state, so a vanished file leaves the tab as it was.
        let entries = match self.workspace_pending.get(&path) {
            Some(parked) => parked.clone(),
            None => crate::postman::parse_collection(&std::fs::read_to_string(&path)?),
        };
        self.workspace_pending.remove(&path);
        self.park_pending_edits();
        self.snapshot_loaded_titles();
        self.entries = entries;
        self.selected_entry = 0;
        self.path = Some(path);
        self.invalidate_request_json();
        self.sync_folder_to_selected();
        self.expand_ancestors_for_path();
        self.sync_ws_cursor();
        Ok(())
    }

    /// Park the loaded file's unsaved edits in `workspace_pending` so they
    /// survive a switch to another file in the same Workspace tab. A file with
    /// no edits is not parked — re-reading it from disk is both cheaper and
    /// more correct, since it picks up any change made outside PaperBoy.
    fn park_pending_edits(&mut self) {
        if self.workspace_root.is_none() || !self.has_unsaved_edits() {
            return;
        }
        if let Some(path) = self.path.clone() {
            self.workspace_pending.insert(path, self.entries.clone());
        }
    }

    /// `true` when this collection holds requests that have been added or
    /// edited since it was last read from / written to disk.
    pub fn has_unsaved_edits(&self) -> bool {
        self.entries.iter().any(|e| e.user_added || e.modified)
    }

    /// How many requests in this tab are added-but-unsaved or edited, counting
    /// a Workspace tab's parked files as well as the one it is showing.
    pub fn unsaved_edit_count(&self) -> usize {
        let loaded = self
            .entries
            .iter()
            .filter(|e| e.user_added || e.modified)
            .count();
        let parked: usize = self
            .workspace_pending
            .iter()
            // The loaded file is parked *and* live while it is being shown, so
            // counting both would double it.
            .filter(|(path, _)| self.path.as_deref() != Some(path.as_path()))
            .map(|(_, entries)| {
                entries
                    .iter()
                    .filter(|e| e.user_added || e.modified)
                    .count()
            })
            .sum();
        loaded + parked
    }

    /// How many of this tab's request edits would be gone for good after a
    /// quit — the question to ask before warning about closing the app, as
    /// opposed to [`Self::unsaved_edit_count`], which answers what closing
    /// *this tab* would throw away.
    ///
    /// The two differ because quitting is not the same as discarding. A plain
    /// tab's entries are written to the session state verbatim, edit markers
    /// included, so its edits are still there — still flagged, still unsaved —
    /// next time the app starts; warning about those taught the user to dismiss
    /// a dialog that was never true. A Workspace tab is the exception: it is
    /// bound to a live folder rather than to a snapshot, so its entries are
    /// deliberately not persisted and its selected file is re-read from disk on
    /// restore, which does drop anything edited but not saved.
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub fn edits_lost_on_exit(&self) -> usize {
        if self.workspace_root.is_some() {
            self.unsaved_edit_count()
        } else {
            0
        }
    }

    /// `true` when the workspace collection file at `path` has unsaved edits —
    /// either it is the loaded file and that has been edited, or it was edited
    /// and then switched away from (so it lives in `workspace_pending`). Drives
    /// the "edited" pencil in the workspace tree.
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub fn workspace_file_edited(&self, path: &std::path::Path) -> bool {
        if self.workspace_pending.contains_key(path) {
            return true;
        }
        self.path.as_deref() == Some(path) && self.has_unsaved_edits()
    }

    /// Whether request `idx` of the workspace collection file at `path` has
    /// unsaved edits. Answers for a file that isn't the loaded one too, by
    /// reading the entries parked in `workspace_pending` — the tree lists a
    /// non-loaded collection's requests from the `workspace_titles` cache, which
    /// was snapshotted from those same (edited) entries, so the indices line up.
    /// Without this, opening a second collection made the first one's pencils
    /// disappear from its rows even though the edits were still pending.
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub fn workspace_request_edited(&self, path: &std::path::Path, idx: usize) -> bool {
        let entries = if self.path.as_deref() == Some(path) {
            &self.entries
        } else {
            match self.workspace_pending.get(path) {
                Some(parked) => parked,
                None => return false,
            }
        };
        entries.get(idx).is_some_and(|e| e.user_added || e.modified)
    }

    /// Discard request `ei`'s in-memory edits by reloading that single entry,
    /// from the same position, out of this collection's on-disk file (#19).
    ///
    /// Returns the reverted request's HTTP method on success, or `None` when
    /// there's nothing to revert to — the collection has no file (scratch), the
    /// file can't be read/parsed, or it holds no entry at that position (e.g. a
    /// never-saved request). The other entries and their edits are untouched.
    pub fn revert_request(&mut self, ei: usize) -> Option<String> {
        let path = self.path.clone()?;
        let content = std::fs::read_to_string(&path).ok()?;
        let mut disk = crate::postman::parse_collection(&content);
        if ei >= disk.len() || ei >= self.entries.len() {
            return None;
        }
        let entry = disk.swap_remove(ei);
        let method = entry.method.clone();
        self.entries[ei] = entry; // a freshly parsed entry is clean (not modified/added)
        self.invalidate_request_json();
        self.sync_folder_to_selected();
        Some(method)
    }

    /// Throw away every in-memory edit to the workspace collection file at
    /// `path`, so the tab shows exactly what is on disk again.
    ///
    /// Works whether or not `path` is the file this tab currently has loaded:
    /// an edited file switched away from lives on in `workspace_pending`, and
    /// its requests are what the tree lists for it, so both places have to be
    /// dropped or the edits would come back the moment it was reopened. Errors
    /// if the file can't be re-read, and changes nothing in that case.
    pub fn revert_workspace_file(&mut self, path: &std::path::Path) -> std::io::Result<()> {
        let entries = crate::postman::parse_collection(&std::fs::read_to_string(path)?);
        self.workspace_pending.remove(path);
        if self.path.as_deref() == Some(path) {
            let sel = self.selected_entry;
            self.entries = entries;
            self.selected_entry = sel.min(self.entries.len().saturating_sub(1));
            self.invalidate_request_json();
            self.sync_folder_to_selected();
        } else {
            // Not loaded: the tree lists it from the title cache, which was
            // snapshotted off the edited entries. Re-snapshot from disk so the
            // row names match the file again.
            let names = entries.iter().map(ws_request_label).collect();
            self.workspace_titles.insert(path.to_path_buf(), names);
        }
        Ok(())
    }

    /// Clear this collection's "new"/"edited" request markers, and drop any
    /// parked edits for its file — called whenever its `.hurl` is written to
    /// disk (local Save or git push) so every save path agrees on what "saved"
    /// means.
    pub fn mark_saved(&mut self) {
        for e in &mut self.entries {
            e.user_added = false;
            e.modified = false;
        }
        if let Some(path) = &self.path {
            self.workspace_pending.remove(path);
        }
    }

    /// Write every edited file this Workspace tab is holding back to disk — the
    /// one it is showing as well as the ones parked in `workspace_pending` —
    /// and clear the edit markers. Returns how many files were written, or the
    /// first path that could not be, so the caller can say which one failed.
    ///
    /// This is what "Save all changes" on the quit dialog needs, and it is
    /// deliberately the same set of files that [`Self::edits_lost_on_exit`]
    /// counts: an ordinary tab is left alone because its edits are persisted to
    /// the session state rather than lost, so silently writing them out to a
    /// file on the way out of the app would be doing something the user never
    /// asked for. A Workspace tab's edits, by contrast, have a file they came
    /// from and are otherwise dropped on exit.
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub fn save_workspace_edits(&mut self) -> Result<usize, String> {
        if self.workspace_root.is_none() {
            return Ok(0);
        }
        let mut written = 0usize;
        // The loaded file first: saving it also drops its parked copy, so the
        // parked pass below can't then write a stale snapshot back over it.
        if self.has_unsaved_edits()
            && let Some(path) = self.path.clone()
        {
            write_hurl(&path, &self.to_hurl())?;
            written += 1;
        }
        let parked: Vec<(PathBuf, Vec<HurlEntry>)> = self
            .workspace_pending
            .iter()
            .filter(|(path, _)| self.path.as_deref() != Some(path.as_path()))
            .map(|(p, e)| (p.clone(), e.clone()))
            .collect();
        for (path, entries) in parked {
            if !entries.iter().any(|e| e.user_added || e.modified) {
                continue;
            }
            write_hurl(&path, &collection_to_hurl(&entries))?;
            written += 1;
        }
        self.workspace_pending.clear();
        self.mark_saved();
        Ok(written)
    }

    /// Re-read the request names of every expanded collection file that isn't
    /// the currently-loaded one, populating `workspace_titles` from disk. Used    /// after restoring persisted state, where collections expanded last session
    /// must list their requests without having been opened yet this session.
    pub fn rebuild_expanded_titles(&mut self) {
        let loaded = self.path.clone();
        let paths: Vec<PathBuf> = self.workspace_expanded.iter().cloned().collect();
        for p in paths {
            if Some(&p) == loaded.as_ref()
                || !p.is_file()
                || crate::workspace::is_report_file(&p)
                || crate::workspace::is_env_file(&p)
            {
                continue;
            }
            let names = read_collection_labels(&p);
            self.workspace_titles.insert(p, names);
        }
    }

    /// Expand all ancestor folders of the currently-loaded file, and the file
    /// itself, so it (and its inline requests) are visible in the workspace
    /// tree.  A no-op for a non-Workspace tab or when no file is loaded.  Called
    /// by [`crate::tui::app`] after loading a file and by [`crate::persistence`]
    /// when restoring state.
    pub fn expand_ancestors_for_path(&mut self) {
        let (Some(root), Some(path)) = (&self.workspace_root, &self.path) else {
            return;
        };
        // Clone to avoid the simultaneous &self borrow.
        let root = root.clone();
        let path = path.clone();
        // The loaded file itself is expanded so its requests show by default.
        self.workspace_expanded.insert(path.clone());
        if let Some(parent) = path.parent()
            && let Ok(rel) = parent.strip_prefix(&root)
        {
            let mut cur = root;
            for component in rel.components() {
                cur.push(component);
                self.workspace_expanded.insert(cur.clone());
            }
        }
    }

    /// For a Workspace tab, move `list_cursor` onto the row for
    /// `selected_entry` (a request of the open collection) if it's visible,
    /// otherwise onto the open collection's file row, otherwise the top.
    /// A no-op for a non-Workspace tab.
    pub fn sync_ws_cursor(&mut self) {
        if !self.is_workspace() {
            return;
        }
        let rows = self.ws_rows();
        let sel = self.selected_entry;
        let loaded = self.path.clone();
        let target = rows
            .iter()
            .position(|r| {
                matches!(r, WsRow::Request { collection, idx, .. }
                    if *idx == sel && Some(collection) == loaded.as_ref())
            })
            .or_else(|| {
                rows.iter().position(|r| {
                    matches!(r, WsRow::Collection { path, open: true, .. }
                        if Some(path) == loaded.as_ref())
                })
            })
            .unwrap_or(0);
        self.list_cursor = target.min(rows.len().saturating_sub(1));
    }

    /// Re-derive `folder`/`list_cursor` so the Requests list is browsing (and
    /// highlighting) `selected_entry`. Call this any time `selected_entry` is
    /// changed programmatically (as opposed to normal Up/Down/Enter list
    /// navigation, which keeps the two in sync itself) — e.g. after adding,
    /// deleting, or renaming a request, or restoring persisted state.
    /// A Workspace tab's `list_cursor` indexes the file tree
    /// ([`Self::ws_rows`]), not the request list, so the row it wants is the
    /// one [`Self::sync_ws_cursor`] computes. Writing a request-list index
    /// into it here would point at an unrelated file (usually the top of the
    /// tree), which is what saving an edited request used to do: commit the
    /// wizard, and the selection left the request and jumped to the first row
    /// of the workspace.
    pub fn sync_folder_to_selected(&mut self) {
        if self.is_workspace() {
            let idx = self
                .selected_entry
                .min(self.entries.len().saturating_sub(1));
            self.selected_entry = idx;
            if !self.entries.is_empty() {
                self.folder = tree::folder_of(&self.entries, idx);
            }
            self.sync_ws_cursor();
            return;
        }
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

#[cfg(test)]
mod ws_scan_tests {
    use super::*;
    use std::fs;

    fn tmp_root(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("paperboy_ws_scan_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn workspace_at(root: &Path) -> Collection {
        let mut c = Collection::new("ws".into(), Vec::new());
        c.workspace_root = Some(root.to_path_buf());
        c
    }

    fn names(rows: &[WsRow]) -> Vec<String> {
        rows.iter()
            .map(|r| match r {
                WsRow::Folder { name, .. }
                | WsRow::Report { name, .. }
                | WsRow::Environment { name, .. }
                | WsRow::Collection { name, .. } => name.clone(),
                other => format!("{other:?}"),
            })
            .collect()
    }

    /// The graphical front-end asks for the tree once per frame. Reading it off
    /// disk that often is real I/O on every mouse move, so a scan is reused for
    /// [`WS_SCAN_TTL`] — and then genuinely re-read, because a workspace can
    /// change from outside PaperBoy.
    #[test]
    fn the_workspace_tree_is_read_off_disk_at_most_once_per_ttl() {
        let root = tmp_root("ttl");
        fs::write(root.join("a.hurl"), "").unwrap();
        let c = workspace_at(&root);

        // The generation is held fixed: this test is about the *time* window,
        // and PaperBoy is not the one making the change.
        let t0 = Instant::now();
        assert_eq!(names(&c.ws_rows_as_of(t0, 7)), vec!["a.hurl"]);

        // A file appears behind PaperBoy's back. Within the window the tree is
        // the one already in hand — that is the whole point of the cache.
        fs::write(root.join("b.hurl"), "").unwrap();
        assert_eq!(
            names(&c.ws_rows_as_of(t0 + WS_SCAN_TTL / 2, 7)),
            vec!["a.hurl"],
            "still serving the cached scan"
        );

        // Once the window passes, the disk is read again and the new file shows
        // up without anyone having told PaperBoy about it.
        assert_eq!(
            names(&c.ws_rows_as_of(t0 + WS_SCAN_TTL + Duration::from_millis(1), 7)),
            vec!["a.hurl", "b.hurl"],
            "the tree catches up with the filesystem"
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// The environments panel's file list comes out of the same cached scan the
    /// tree does, so drawing the panel doesn't walk the workspace again —
    /// several times per frame, as it used to.
    #[test]
    fn the_environment_file_list_is_served_from_the_tree_scan() {
        let root = tmp_root("envscan");
        fs::write(root.join("a.hurl"), "").unwrap();
        fs::write(root.join("dev.vars"), "K=1").unwrap();
        let c = workspace_at(&root);

        let t0 = Instant::now();
        assert_eq!(
            c.workspace_env_files_as_of(t0, 7),
            vec![root.join("dev.vars")],
            "the workspace's environment files, and only those"
        );

        // Written behind PaperBoy's back and *not* seen, which is the proof:
        // a fresh scan would have found it, so this answer came from the cache
        // the tree filled in above.
        fs::write(root.join("prod.vars"), "K=2").unwrap();
        assert_eq!(
            c.workspace_env_files_as_of(t0 + WS_SCAN_TTL / 2, 7),
            vec![root.join("dev.vars")],
            "no second walk of the disk"
        );
        // And it does catch up once the window passes, like the tree does.
        assert_eq!(
            c.workspace_env_files_as_of(t0 + WS_SCAN_TTL + Duration::from_millis(1), 7),
            vec![root.join("dev.vars"), root.join("prod.vars")]
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// A tab that isn't a workspace has no files to offer — and must not scan
    /// anything looking for them.
    #[test]
    fn a_non_workspace_tab_lists_no_environment_files() {
        let c = Collection::new("scratch".into(), Vec::new());
        assert!(c.workspace_env_files().is_empty());
    }

    /// The panel lists a workspace's environments whether or not the tab's
    /// display filter is narrowing the *tree* to collections — the filter
    /// chooses what the tree shows, not what environments exist.
    #[test]
    fn the_display_filter_does_not_hide_environment_files() {
        let root = tmp_root("envfilter");
        fs::write(root.join("dev.vars"), "K=1").unwrap();
        let mut c = workspace_at(&root);
        c.workspace_filter_hurl_json = true;
        assert_eq!(c.workspace_env_files(), vec![root.join("dev.vars")]);
        c.workspace_filter_hurl_json = false;
        assert_eq!(c.workspace_env_files(), vec![root.join("dev.vars")]);
        let _ = fs::remove_dir_all(&root);
    }

    /// The cache must not answer a question it wasn't asked: changing the
    /// filter (or the root) has to re-read, however fresh the last scan is.
    #[test]
    fn changing_the_filter_or_the_root_bypasses_a_fresh_scan() {
        let root = tmp_root("keys");
        fs::write(root.join("a.hurl"), "").unwrap();
        fs::write(root.join("notes.txt"), "").unwrap();
        let mut c = workspace_at(&root);

        let t0 = Instant::now();
        assert_eq!(names(&c.ws_rows_as_of(t0, 7)), vec!["a.hurl"], "filtered");

        c.workspace_filter_hurl_json = false;
        assert_eq!(
            names(&c.ws_rows_as_of(t0, 7)),
            vec!["a.hurl", "notes.txt"],
            "showing everything, at the very same instant"
        );

        let other = tmp_root("keys_other");
        fs::write(other.join("z.hurl"), "").unwrap();
        c.workspace_root = Some(other.clone());
        assert_eq!(
            names(&c.ws_rows_as_of(t0, 7)),
            vec!["z.hurl"],
            "a different root is a different tree"
        );

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&other);
    }

    /// PaperBoy's own edits must not have to wait out the window: creating a
    /// file and then not finding it in the tree is a bug, not a stale cache.
    #[test]
    fn the_app_s_own_file_operations_show_up_at_once() {
        let root = tmp_root("generation");
        fs::write(root.join("a.hurl"), "").unwrap();
        let c = workspace_at(&root);

        let t0 = Instant::now();
        assert_eq!(names(&c.ws_rows_at(t0)), vec!["a.hurl"]);

        crate::workspace::create_item(&root, &root, "b", crate::workspace::NewItemKind::Collection)
            .expect("created");
        assert_eq!(
            names(&c.ws_rows_at(t0)),
            vec!["a.hurl", "b.hurl"],
            "at the very same instant, well inside the scan window"
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// Expanding a folder is a *view* change, not a filesystem one, so it must
    /// take effect on the very next frame rather than waiting out the TTL.
    #[test]
    fn expanding_a_folder_shows_its_contents_immediately() {
        let root = tmp_root("expand");
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("sub/inner.hurl"), "").unwrap();
        let mut c = workspace_at(&root);

        let t0 = Instant::now();
        assert_eq!(names(&c.ws_rows_as_of(t0, 7)), vec!["sub"], "collapsed");

        c.workspace_expanded.insert(root.join("sub"));
        assert_eq!(
            names(&c.ws_rows_as_of(t0, 7)),
            vec!["sub", "inner.hurl"],
            "no wait for the scan window: the filter isn't cached"
        );

        let _ = fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod revert_tests {
    use super::*;
    use std::fs;

    fn tmp_root(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("paperboy_revert_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A file edited and then switched away from keeps its edits in
    /// `workspace_pending`, and the tree lists its requests from the title
    /// cache snapshotted off those same edited entries. Reverting it has to
    /// clear both, or reopening the file would bring the edits back and the
    /// tree would go on showing the edited names in the meantime.
    #[test]
    fn reverting_a_file_that_isnt_loaded_drops_its_parked_edits() {
        let root = tmp_root("parked");
        let a = root.join("a.hurl");
        let b = root.join("b.hurl");
        fs::write(&a, "GET https://example.com/a\n").unwrap();
        fs::write(&b, "GET https://example.com/b\n").unwrap();
        let mut col = Collection::new("ws".into(), Vec::new());
        col.workspace_root = Some(root.clone());

        col.load_workspace_file(a.clone()).unwrap();
        col.entries[0].url = "https://edited.example".into();
        col.entries[0].modified = true;
        // Switching away parks the edits and caches the edited row names.
        col.load_workspace_file(b.clone()).unwrap();
        assert!(col.workspace_file_edited(&a), "the edits are parked");

        col.revert_workspace_file(&a).unwrap();

        assert!(!col.workspace_file_edited(&a), "and now they are gone");
        col.load_workspace_file(a.clone()).unwrap();
        assert_eq!(
            col.entries[0].url, "https://example.com/a",
            "reopening the file shows what is on disk, not the discarded edit"
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// Reverting the loaded file leaves the tab showing it — same file, same
    /// request selected — with the edits gone.
    #[test]
    fn reverting_the_loaded_file_restores_it_in_place() {
        let root = tmp_root("loaded");
        let a = root.join("a.hurl");
        fs::write(
            &a,
            "GET https://example.com/a\nGET https://example.com/a2\n",
        )
        .unwrap();
        let mut col = Collection::new("ws".into(), Vec::new());
        col.workspace_root = Some(root.clone());
        col.load_workspace_file(a.clone()).unwrap();
        col.selected_entry = 1;
        col.entries[1].url = "https://edited.example".into();
        col.entries[1].modified = true;

        col.revert_workspace_file(&a).unwrap();

        assert_eq!(col.path.as_deref(), Some(a.as_path()));
        assert_eq!(col.selected_entry, 1, "the selection stays where it was");
        assert_eq!(col.entries[1].url, "https://example.com/a2");
        assert!(!col.has_unsaved_edits());
        let _ = fs::remove_dir_all(&root);
    }

    /// A file that has vanished can't be reverted to — and the attempt must
    /// leave the in-memory edits alone rather than half-clearing them.
    #[test]
    fn reverting_an_unreadable_file_changes_nothing() {
        let root = tmp_root("missing");
        let a = root.join("a.hurl");
        fs::write(&a, "GET https://example.com/a\n").unwrap();
        let mut col = Collection::new("ws".into(), Vec::new());
        col.workspace_root = Some(root.clone());
        col.load_workspace_file(a.clone()).unwrap();
        col.entries[0].url = "https://edited.example".into();
        col.entries[0].modified = true;
        fs::remove_file(&a).unwrap();

        assert!(col.revert_workspace_file(&a).is_err());
        assert_eq!(col.entries[0].url, "https://edited.example");
        assert!(col.has_unsaved_edits());
        let _ = fs::remove_dir_all(&root);
    }
}
