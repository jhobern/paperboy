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
use crate::hurl::{HurlEntry, RunStatus, collection_to_hurl};
use crate::tree::{self, Row};

/// The cached headline of one request in a collection that isn't loaded: its
/// **full** title (folder segments included, so the workspace tree can nest it
/// — see [`WsRow::RequestFolder`]) and what it does.
///
/// The method is cached with the name because a list of bare names makes the
/// reader open a file to find out which row is the POST — the one thing the
/// badge exists to save them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WsTitle {
    /// The request's whole title, e.g. `"Auth/Tokens/Refresh"`. Empty for an
    /// untitled request, which is displayed as its `url` instead.
    pub name: String,
    /// What to show when the request has no title. Kept apart from `name`
    /// rather than substituted into it because a URL is full of `/` and the
    /// tree reads `/` as folder nesting — an untitled `GET https://x/a/b` must
    /// be one row, not an `https:` folder holding an `x` folder.
    pub url: String,
    pub method: String,
}

/// What a run left behind on one request: its pass/fail marker and the
/// response it came back with.
///
/// A Workspace tab holds one file's requests at a time, and a file with no
/// unsaved edits is re-read from disk when it is opened again — so a run's
/// result lived exactly as long as the tab stayed on that collection. Running
/// a request, looking at another folder and coming back lost both the tick and
/// the response, which is precisely the moment someone wants to compare them.
/// Results are parked here instead, per file, and handed back when the file
/// returns; the tree also reads them so a file that isn't loaded still shows
/// what happened to its requests this session.
///
/// Runtime-only, like [`Collection::workspace_pending`]: a response is a
/// point-in-time answer from a server, not a fact about the collection, and
/// has no business surviving a restart (or being written to disk, where the
/// headers it carries would outlive the session that earned them).
#[derive(Debug, Clone, Default)]
pub struct RunRecord {
    /// Identifies the request the result belongs to, so a file edited outside
    /// PaperBoy between the run and the reopen can't hand a stale response to
    /// whatever request now sits at that position.
    key: String,
    last_run: RunStatus,
    last_response: Option<crate::http::ApiResponse>,
}

/// How a request is recognised across a reload: what it is and where it goes.
fn run_key(entry: &HurlEntry) -> String {
    format!("{}\u{1}{}\u{1}{}", entry.title, entry.method, entry.url)
}

fn ws_request_title(entry: &HurlEntry) -> WsTitle {
    WsTitle {
        name: entry.title.clone(),
        url: entry.url.clone(),
        method: entry.method.clone(),
    }
}

/// The name shown on a request's row: the last segment of its title, since the
/// segments before it are drawn as the folder rows above it. An untitled
/// request shows `url` instead — it still has to be findable in the tree.
fn ws_leaf_label(title: &str, url: &str) -> String {
    let leaf = crate::tree::entry_path(title).pop().unwrap_or_default();
    if leaf.is_empty() {
        url.to_string()
    } else {
        leaf
    }
}

/// Parse a collection file into the display labels of its requests, for listing
/// a not-currently-loaded collection's requests in the workspace tree. Returns
/// an empty vec when the file can't be read or parsed — the collection then
/// simply shows no requests until it is opened.
fn read_collection_labels(path: &Path) -> Vec<WsTitle> {
    std::fs::read_to_string(path)
        .map(|content| {
            crate::postman::parse_collection(&content)
                .iter()
                .map(ws_request_title)
                .collect()
        })
        .unwrap_or_default()
}

/// One row in the Workspace tab's file-tree request list (see
/// [`Collection::ws_rows`]). This navigates the real filesystem under the
/// workspace root and inlines expanded collections' requests directly beneath
/// their file rows (an accordion) — and, within a file, the *virtual* folders
/// encoded in request titles (see [`crate::tree`]), so an imported Postman
/// collection keeps the folder structure it was written with instead of
/// collapsing into one long list of leaf names.
///
/// The tree is a real expand/collapse tree: `workspace_expanded` (on
/// [`Collection`]) holds the set of open folders *and* open collection files
/// *and* open virtual request folders; visibility is derived depth-first from
/// that set. Unlike [`Row`], which shows one virtual folder at a time with an
/// `Up` row, this shows the whole nesting inline.
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
    /// A *virtual* folder inside a collection file: the leading path segments
    /// of its requests' titles (`"Auth/Login"` → an `Auth` folder holding
    /// `Login`). It has no file of its own, so `path` is a synthetic key —
    /// the collection file's path with the folder's segments pushed onto it
    /// (see [`request_folder_path`]). That key can't collide with a real
    /// entry, because the collection is a *file* and so has no children on
    /// disk, which lets these rows share `workspace_expanded` (and its
    /// persistence, and the move/rename repointing) with real folders.
    RequestFolder {
        /// The collection file whose requests this folder groups.
        collection: PathBuf,
        /// Synthetic expand/collapse key; see above.
        path: PathBuf,
        /// The folder's own name — the one title segment, not the whole path.
        name: String,
        depth: usize,
        expanded: bool,
    },
    /// A request shown indented under its [`WsRow::Collection`] row, or under
    /// the [`WsRow::RequestFolder`] rows its title nests it in; `depth` is the
    /// containing row's depth + 1. `collection` is the owning file's path and
    /// `idx` the request's position within it. When `loaded` is true the file
    /// is the tab's currently-loaded collection, so `idx` indexes `entries` and
    /// the row renders in full detail; when false the row is drawn from the
    /// cached name and method (see `workspace_titles`) and selecting it previews
    /// the name — opening it (Enter/Right) loads that collection first.
    Request {
        collection: PathBuf,
        idx: usize,
        /// The request's *leaf* name: any folder segments of its title are
        /// shown as the [`WsRow::RequestFolder`] rows above it, not repeated
        /// here.
        name: String,
        /// The request's HTTP method, known for every listed request whether or
        /// not its file is the loaded one.
        method: String,
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
    /// six variants each time. A [`WsRow::RequestFolder`] answers with its
    /// synthetic key rather than its collection, because the callers that ask
    /// are the ones toggling `workspace_expanded`.
    pub fn path(&self) -> &Path {
        match self {
            WsRow::Folder { path, .. }
            | WsRow::Collection { path, .. }
            | WsRow::Report { path, .. }
            | WsRow::Environment { path, .. }
            | WsRow::RequestFolder { path, .. } => path,
            WsRow::Request { collection, .. } => collection,
        }
    }

    /// How far the row is indented in the tree.
    pub fn depth(&self) -> usize {
        match self {
            WsRow::Folder { depth, .. }
            | WsRow::Collection { depth, .. }
            | WsRow::Report { depth, .. }
            | WsRow::Environment { depth, .. }
            | WsRow::RequestFolder { depth, .. }
            | WsRow::Request { depth, .. } => *depth,
        }
    }

    /// The text shown for the row — what a typed filter matches against.
    pub fn name(&self) -> &str {
        match self {
            WsRow::Folder { name, .. }
            | WsRow::Collection { name, .. }
            | WsRow::Report { name, .. }
            | WsRow::Environment { name, .. }
            | WsRow::RequestFolder { name, .. }
            | WsRow::Request { name, .. } => name,
        }
    }
}

/// Narrow an already-built Workspace tree to the rows matching `query`, keeping
/// every ancestor of a match so the survivors still read as a tree rather than
/// as a flat list of names with no indication of which file they came from.
///
/// A plain collection's filter flattens instead ([`crate::tree::rows_matching`])
/// because there the only context a match has is its title, which the row
/// already spells out in full. A workspace row's context is the *file* it lives
/// in, which the row does not repeat — so dropping the ancestors would leave a
/// screen of request names with no way to tell two identically-named requests
/// in different collections apart.
///
/// Matching is case-insensitive and on the substring, matching the Requests and
/// Environments filters; a folder that matches keeps its whole subtree, since
/// naming a folder is the obvious way to ask for what is in it.
fn filter_ws_rows(rows: Vec<WsRow>, query: &str) -> Vec<WsRow> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return rows;
    }
    let mut keep = vec![false; rows.len()];
    // The indices of the rows containing the one being looked at, innermost
    // last — a match marks all of them, which is what keeps the tree readable.
    let mut ancestors: Vec<usize> = Vec::new();
    // Set while inside a matched folder's subtree, holding that folder's depth,
    // so everything under it is kept without having to match on its own.
    let mut inside: Option<usize> = None;
    for (i, row) in rows.iter().enumerate() {
        let d = row.depth();
        while ancestors.last().is_some_and(|&a| rows[a].depth() >= d) {
            ancestors.pop();
        }
        if inside.is_some_and(|kept| d <= kept) {
            inside = None;
        }
        if inside.is_some() || row.name().to_lowercase().contains(&needle) {
            keep[i] = true;
            for &a in &ancestors {
                keep[a] = true;
            }
            if inside.is_none() && matches!(row, WsRow::Folder { .. } | WsRow::RequestFolder { .. })
            {
                inside = Some(d);
            }
        }
        ancestors.push(i);
    }
    rows.into_iter()
        .zip(keep)
        .filter_map(|(r, k)| k.then_some(r))
        .collect()
}

/// A title for a copy of `title` that no entry in `entries` already carries.
///
/// A request's title is its *identifier*: reports address requests by name
/// (see `report::run::resolve_qualified`), and two entries sharing a title
/// make the name ambiguous — which breaks the reference for **both** of them,
/// not just the new one. So a duplicate can't simply reuse the name; it has to
/// arrive with one of its own.
///
/// Folders are derived by splitting the title on `/` (see [`crate::tree`]), so
/// only the leaf is renamed — a copy belongs in the same folder as its
/// original. Copying a copy counts on from the existing suffix rather than
/// stacking them, so repeated duplication gives `Login (2)`, `Login (3)` …
/// instead of `Login (2) (2)`.
pub fn unique_entry_title(entries: &[HurlEntry], title: &str) -> String {
    let (prefix, leaf) = match title.rfind('/') {
        Some(i) => title.split_at(i + 1),
        None => ("", title),
    };
    // Strip any trailing " (n)" so the counter continues rather than nests.
    let stem = leaf
        .rsplit_once(" (")
        .and_then(|(head, tail)| {
            tail.strip_suffix(')')
                .filter(|d| !d.is_empty() && d.chars().all(|c| c.is_ascii_digit()))
                .map(|_| head)
        })
        .unwrap_or(leaf);
    let taken: HashSet<&str> = entries.iter().map(|e| e.title.as_str()).collect();
    // Starts at 2 because the original is, in effect, number one.
    (2..)
        .map(|n| format!("{prefix}{stem} ({n})"))
        .find(|candidate| !taken.contains(candidate.as_str()))
        .unwrap_or_else(|| title.to_string())
}

/// Where the index `i` ends up after the entry at `from` is moved to `to`.
///
/// Only indices *between* the two move, and they all move one step in the
/// opposite direction to the entry itself; `from` becomes `to` by definition.
fn shift_index(i: usize, from: usize, to: usize) -> usize {
    if i == from {
        to
    } else if from < to && i > from && i <= to {
        i - 1
    } else if to < from && i >= to && i < from {
        i + 1
    } else {
        i
    }
}

/// The synthetic `workspace_expanded` key for the virtual folder `folder`
/// (title segments) inside the collection file at `collection` — see
/// [`WsRow::RequestFolder`].
///
/// Segments are sanitised before being pushed: a request titled `"../x/y"` (or
/// one with a `\` on Windows) would otherwise produce a key pointing outside
/// the collection, which both collides with real paths and survives into
/// persisted state. The sanitised form only ever has to be *consistent*, never
/// reversible — nothing reads a folder name back out of one of these keys.
pub fn request_folder_path(collection: &Path, folder: &[String]) -> PathBuf {
    let mut path = collection.to_path_buf();
    for seg in folder {
        let safe: String = seg
            .chars()
            .map(|c| if std::path::is_separator(c) { '_' } else { c })
            .collect();
        path.push(match safe.trim_matches('.') {
            "" => "_",
            _ => safe.as_str(),
        });
    }
    path
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
    /// A typed filter over the Requests list: while non-empty, the list shows
    /// every request whose title contains this, flattened across folders (see
    /// [`crate::tree::rows_matching`]) instead of the folder being browsed.
    ///
    /// Lives on the collection rather than on either front-end so both show the
    /// same narrowed list for the same tab, the way `workspace_filter_hurl_json`
    /// already does — and so switching tabs keeps each tab's own filter.
    ///
    /// Runtime-only, deliberately: a filter restored from a previous session
    /// would present a collection that looks like it has lost most of its
    /// requests, with the reason parked in a strip nobody has looked at yet.
    pub list_query: String,
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
    pub workspace_titles: HashMap<PathBuf, Vec<WsTitle>>,
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

    /// Whether the loaded entries differ *structurally* from the file they
    /// came from — a request removed, restored or reordered.
    ///
    /// [`Self::has_unsaved_edits`] otherwise answers by scanning the entries
    /// for `user_added`/`modified` flags, which only ever say "this request
    /// was edited". Removing one leaves nothing behind to carry a flag, and
    /// reordering changes no request at all, so both read as "no edits" — and
    /// a Workspace tab, whose entries are held in memory and re-read from disk
    /// when it switches away and back, would then throw the change away
    /// without a word. Cleared by [`Self::mark_saved`] like any other marker.
    ///
    /// Runtime-only, for the same reason `workspace_pending` is.
    pub structure_modified: bool,

    /// The parked files (see `workspace_pending`) whose entries differ
    /// structurally from disk — `structure_modified` for a file that isn't the
    /// loaded one. Kept separately because `workspace_pending` stores only the
    /// entries, and a deletion is precisely the change that leaves no trace in
    /// them.
    pub workspace_structure_modified: HashSet<PathBuf>,

    /// Run results for this Workspace tab's files, keyed by file and indexed
    /// the way [`Self::workspace_titles`] is — see [`RunRecord`] for why they
    /// outlive the file being loaded, and why they are runtime-only.
    pub workspace_runs: HashMap<PathBuf, Vec<RunRecord>>,
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
            list_query: String::new(),
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
            structure_modified: false,
            workspace_structure_modified: HashSet::new(),
            workspace_runs: HashMap::new(),
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

    /// The rows to show in the Requests list: the folder currently being
    /// browsed, or — while a filter is typed — every match across the whole
    /// collection.
    pub fn rows(&self) -> Vec<Row> {
        if self.list_filter_active() {
            tree::rows_matching(&self.entries, &self.list_query)
        } else {
            tree::rows_for(&self.entries, &self.folder)
        }
    }

    /// How many rows the left-hand list pane is showing, whichever kind of tab
    /// it is: a Workspace tab draws its filesystem tree, every other tab draws
    /// the loaded collection's requests. The two are different lists of
    /// different lengths, and reading `list_cursor` against the wrong one is
    /// how `Alt+↑↓` came to reorder requests nobody was pointing at.
    pub fn list_row_count(&self) -> usize {
        if self.is_workspace() {
            self.ws_rows().len()
        } else {
            self.rows().len()
        }
    }

    /// Whether the Requests list is currently narrowed by a typed filter.
    ///
    /// Trimmed, so a query of nothing but spaces counts as no filter at all —
    /// it matches every request anyway, and treating it as active would leave
    /// the list flattened out of its folders for no visible reason.
    pub fn list_filter_active(&self) -> bool {
        !self.list_query.trim().is_empty()
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

    /// The entry index under the cursor when it is a *loaded* request on a
    /// Workspace tab — that is, exactly when `m` (move) and `c` (copy) have
    /// something to transfer to another collection file.
    ///
    /// A single predicate shared by the key handler and the footer hint, so the
    /// footer can never advertise a key that would silently do nothing: a
    /// folder row, a report, or a collection file whose requests haven't been
    /// read in yet are all rows where `m`/`c` return without acting.
    pub(crate) fn ws_transfer_target(&self) -> Option<usize> {
        if !self.is_workspace() || self.workspace_root.is_none() {
            return None;
        }
        match self.ws_rows().into_iter().nth(self.list_cursor) {
            Some(WsRow::Request {
                idx, loaded: true, ..
            }) => Some(idx),
            _ => None,
        }
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

        // A typed filter searches the whole workspace, so while one is active
        // every *folder* counts as expanded: a search that could only find what
        // was already on screen would be no search at all. Collection files are
        // deliberately left alone — expanding one reads its requests off disk
        // (or out of the title cache), and doing that for every file in the
        // tree on each keystroke would stall a large workspace. So a filter
        // reaches every file, report and environment in the workspace, and the
        // requests of the collections already open.
        let filtering = self.list_filter_active();

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
            let visible = filtering
                || ancestor_at.iter().all(|opt| {
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
                    let expanded = filtering || self.workspace_expanded.contains(&entry.path);
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
        if filtering {
            return filter_ws_rows(out, &self.list_query);
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
    /// The request rows shown under an expanded collection at `path`, indented
    /// to `depth`, nested into the virtual folders their titles encode. For the
    /// currently-loaded file the rows come straight from `entries` (full detail,
    /// `loaded: true`); for any other expanded collection they come from the
    /// cached titles in `workspace_titles` (`loaded: false`), so several
    /// collections' requests can be listed at once without re-parsing every file
    /// each frame. A collection with no cached names yet contributes no rows.
    fn request_rows_for(&self, path: &Path, depth: usize) -> Vec<WsRow> {
        // (index in the file, full title, url, method) for every request to
        // list, in file order — the one shape both sources reduce to.
        let listing: Vec<(usize, String, String, String)> = if self.path.as_deref() == Some(path) {
            self.entries
                .iter()
                .enumerate()
                .map(|(idx, e)| (idx, e.title.clone(), e.url.clone(), e.method.clone()))
                .collect()
        } else {
            match self.workspace_titles.get(path) {
                Some(titles) => titles
                    .iter()
                    .enumerate()
                    .map(|(idx, t)| (idx, t.name.clone(), t.url.clone(), t.method.clone()))
                    .collect(),
                None => return Vec::new(),
            }
        };
        let loaded = self.path.as_deref() == Some(path);
        let mut out = Vec::new();
        self.push_request_rows(path, &listing, &[], depth, loaded, &mut out);
        out
    }

    /// Append the rows for the virtual folder `folder` of the collection at
    /// `path`: its direct subfolders (each recursed into when expanded), then
    /// its direct requests.
    ///
    /// Folders come before requests, and both keep the order the file puts them
    /// in rather than being sorted — a collection is an ordered script (the
    /// login that captures a token has to read as coming first), so re-ordering
    /// it for display would misrepresent what running it does.
    fn push_request_rows(
        &self,
        path: &Path,
        listing: &[(usize, String, String, String)],
        folder: &[String],
        depth: usize,
        loaded: bool,
        out: &mut Vec<WsRow>,
    ) {
        let mut seen: Vec<String> = Vec::new();
        let mut leaves: Vec<&(usize, String, String, String)> = Vec::new();

        for item in listing {
            let segs = tree::entry_path(&item.1);
            if segs.len() <= folder.len() || segs[..folder.len()] != *folder {
                continue;
            }
            if segs.len() == folder.len() + 1 {
                leaves.push(item);
            } else if !seen.contains(&segs[folder.len()]) {
                seen.push(segs[folder.len()].clone());
            }
        }

        for name in seen {
            let mut child = folder.to_vec();
            child.push(name.clone());
            let key = request_folder_path(path, &child);
            let expanded = self.workspace_expanded.contains(&key);
            out.push(WsRow::RequestFolder {
                collection: path.to_path_buf(),
                path: key,
                name,
                depth,
                expanded,
            });
            if expanded {
                self.push_request_rows(path, listing, &child, depth + 1, loaded, out);
            }
        }

        for (idx, title, url, method) in leaves {
            out.push(WsRow::Request {
                collection: path.to_path_buf(),
                idx: *idx,
                name: ws_leaf_label(title, url),
                method: method.clone(),
                depth,
                loaded,
            });
        }
    }

    /// Cache the loaded file's request names under its own path, derived from
    /// the live `entries` (so in-memory edits/renames are reflected). Call this
    /// just before switching the loaded file away, so a collection left
    /// expanded keeps listing its requests from the cache.
    pub fn snapshot_loaded_titles(&mut self) {
        if let Some(path) = self.path.clone() {
            let titles = self.entries.iter().map(ws_request_title).collect();
            self.workspace_titles.insert(path, titles);
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
        // Park the outgoing file *before* adopting the incoming file's flag,
        // since parking reads `structure_modified` for the file being left.
        self.park_pending_edits();
        let incoming_structural = self.workspace_structure_modified.remove(&path);
        self.park_run_results();
        self.snapshot_loaded_titles();
        self.entries = entries;
        self.structure_modified = incoming_structural;
        self.restore_run_results(&path);
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
            if self.structure_modified {
                self.workspace_structure_modified.insert(path.clone());
            }
            self.workspace_pending.insert(path, self.entries.clone());
        }
    }

    /// Park the loaded file's run results so they survive a switch to another
    /// file in the same Workspace tab (see [`RunRecord`]).
    ///
    /// Unlike [`Self::park_pending_edits`] this runs whether or not the file
    /// has been edited: an unedited file is re-read from disk when it comes
    /// back, and a fresh parse has never been run.
    fn park_run_results(&mut self) {
        if self.workspace_root.is_none() {
            return;
        }
        let Some(path) = self.path.clone() else {
            return;
        };
        let records: Vec<RunRecord> = self
            .entries
            .iter()
            .map(|e| RunRecord {
                key: run_key(e),
                last_run: e.last_run,
                last_response: e.last_response.clone(),
            })
            .collect();
        // Nothing has been run: don't hold a row of empty records that would
        // only have to be checked later.
        if records
            .iter()
            .all(|r| r.last_run == RunStatus::NotRun && r.last_response.is_none())
        {
            self.workspace_runs.remove(&path);
        } else {
            self.workspace_runs.insert(path, records);
        }
    }

    /// Put previously parked run results back onto the entries just loaded
    /// from `path`, by position and only where the request still matches (see
    /// [`RunRecord::key`]). An entry that already carries a result keeps it:
    /// entries handed back from `workspace_pending` were never re-read, so
    /// theirs is the live one.
    fn restore_run_results(&mut self, path: &Path) {
        let Some(records) = self.workspace_runs.get(path) else {
            return;
        };
        for (entry, record) in self.entries.iter_mut().zip(records.iter()) {
            if entry.last_run != RunStatus::NotRun || entry.last_response.is_some() {
                continue;
            }
            if run_key(entry) == record.key {
                entry.last_run = record.last_run;
                entry.last_response = record.last_response.clone();
            }
        }
    }

    /// The run marker to show for request `idx` of the workspace file at
    /// `path`, whether or not that file is the one this tab has loaded.
    ///
    /// The tree used to draw markers for the loaded collection only, on the
    /// grounds that nothing else could have been run — which stopped being
    /// true the moment results outlived the file being loaded.
    pub fn workspace_run_status(&self, path: &Path, idx: usize) -> RunStatus {
        if self.path.as_deref() == Some(path) {
            return self
                .entries
                .get(idx)
                .map(|e| e.last_run)
                .unwrap_or(RunStatus::NotRun);
        }
        // A file with parked *edits* keeps its entries whole, results included;
        // only a file that will be re-read needs the parked records.
        if let Some(parked) = self.workspace_pending.get(path) {
            return parked
                .get(idx)
                .map(|e| e.last_run)
                .unwrap_or(RunStatus::NotRun);
        }
        self.workspace_runs
            .get(path)
            .and_then(|r| r.get(idx))
            .map(|r| r.last_run)
            .unwrap_or(RunStatus::NotRun)
    }

    /// `true` when this collection holds requests that have been added or
    /// edited since it was last read from / written to disk.
    pub fn has_unsaved_edits(&self) -> bool {
        self.structure_modified || self.entries.iter().any(|e| e.user_added || e.modified)
    }

    /// How many unsaved changes this tab is holding, counting a Workspace
    /// tab's parked files as well as the one it is showing.
    ///
    /// Mostly this is the added-but-unsaved and edited requests, one apiece.
    /// A file that has been changed *structurally* — a request removed,
    /// restored or reordered — counts one more, because no surviving request
    /// carries a marker for it and it would otherwise total zero: this number
    /// gates the "you have unsaved edits" prompt on closing a tab
    /// ([`crate::gui::app::GuiApp::request_close_tab`]), so a delete-only
    /// change reading as nothing meant closing the tab threw it away without
    /// asking. One per file rather than one per removal, because how many
    /// there were is not recorded — only that the list no longer matches disk.
    pub fn unsaved_edit_count(&self) -> usize {
        let edited = |entries: &[HurlEntry]| {
            entries
                .iter()
                .filter(|e| e.user_added || e.modified)
                .count()
        };
        let loaded = edited(&self.entries) + usize::from(self.structure_modified);
        let parked: usize = self
            .workspace_pending
            .iter()
            // The loaded file is parked *and* live while it is being shown, so
            // counting both would double it.
            .filter(|(path, _)| self.path.as_deref() != Some(path.as_path()))
            .map(|(path, entries)| {
                edited(entries) + usize::from(self.workspace_structure_modified.contains(path))
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
            // Reverting throws away *edits*, not the record of what happened
            // when these requests were last run — so the results are parked
            // across the reload like they are across a file switch.
            self.park_run_results();
            self.entries = entries;
            self.restore_run_results(path);
            self.selected_entry = sel.min(self.entries.len().saturating_sub(1));
            self.invalidate_request_json();
            self.sync_folder_to_selected();
        } else {
            // Not loaded: the tree lists it from the title cache, which was
            // snapshotted off the edited entries. Re-snapshot from disk so the
            // row names match the file again.
            let titles = entries.iter().map(ws_request_title).collect();
            self.workspace_titles.insert(path.to_path_buf(), titles);
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
        self.structure_modified = false;
        if let Some(path) = &self.path {
            self.workspace_pending.remove(path);
            self.workspace_structure_modified.remove(path);
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
            // A parked file whose only change was a deletion or a reorder has
            // no flagged entry to find — `workspace_structure_modified` is the
            // only record that it differs from disk.
            if !self.workspace_structure_modified.contains(&path)
                && !entries.iter().any(|e| e.user_added || e.modified)
            {
                continue;
            }
            write_hurl(&path, &collection_to_hurl(&entries))?;
            written += 1;
        }
        self.workspace_pending.clear();
        // Everything parked has just been written, so no file is structurally
        // ahead of disk any more; `mark_saved` only clears the loaded one.
        self.workspace_structure_modified.clear();
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
            let titles = read_collection_labels(&p);
            self.workspace_titles.insert(p, titles);
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
        // A request nested in a virtual folder is hidden until that folder is
        // open, and this is the one call that says "put the cursor on the
        // selected request" — so it has to make it reachable first, or the
        // cursor would silently fall back to the file row every time a nested
        // request was selected from outside the tree (loading a file, saving
        // the wizard, renaming).
        self.expand_selected_request_folders();
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

    /// Open every virtual folder containing the loaded file's `selected_entry`,
    /// so its row is visible in the workspace tree. A no-op when the selected
    /// request sits at the top level of its file (the common case), so this
    /// doesn't fight a user who has deliberately folded things away.
    fn expand_selected_request_folders(&mut self) {
        let Some(path) = self.path.clone() else {
            return;
        };
        let Some(title) = self
            .entries
            .get(self.selected_entry)
            .map(|e| e.title.clone())
        else {
            return;
        };
        let segs = tree::entry_path(&title);
        // The last segment is the request itself, not a folder.
        for n in 1..segs.len() {
            self.workspace_expanded
                .insert(request_folder_path(&path, &segs[..n]));
        }
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

    /// Remove the entry at `idx`, recording it (with the index it came from)
    /// in `deleted_entries` so [`Self::restore_last_deleted`] can bring it
    /// back. This is the part of "delete a request" both front-ends have to
    /// agree on — one undo history and one 20-entry cap — everything around it
    /// differs (the terminal UI also moves its list cursor, sets a status line
    /// and persists state; the graphical one doesn't), so those stay in each
    /// front-end's own delete method instead of being forced in here.
    ///
    /// Returns `None` for an out-of-range `idx` rather than panicking the way
    /// `Vec::remove` would: the index reaching here came from a list row or a
    /// context menu rendered from an earlier borrow of `entries`, so it is
    /// exactly the kind of index that can go stale between being read and
    /// being used.
    pub fn remove_entry_recording_undo(&mut self, idx: usize) -> Option<HurlEntry> {
        if idx >= self.entries.len() {
            return None;
        }
        let removed = self.entries.remove(idx);
        self.structure_modified = true;
        self.deleted_entries.push((idx, removed.clone()));
        if self.deleted_entries.len() > 20 {
            self.deleted_entries.remove(0);
        }
        Some(removed)
    }

    /// Move the entry at `from` so it sits at index `to`, shifting everything
    /// between them along. Returns whether anything actually moved.
    ///
    /// The order of `entries` is not cosmetic: `run_all_entries` walks it in
    /// order, so it decides which request captures a token before another one
    /// uses it. Until this existed the only way to change that was to delete a
    /// request and recreate it further down.
    ///
    /// Remove-and-insert rather than a swap, because the two requests a user
    /// sees as neighbours need not be neighbours in `entries` at all — folders
    /// are derived by splitting titles on `/` (see [`crate::tree`]), so a
    /// folder's requests can be scattered through the vector with other
    /// folders' requests in between. Swapping would drag whichever unrelated
    /// request sat at `to` across to `from`; shifting leaves everything else in
    /// the order it was.
    ///
    /// Reordering is safe for reports, which address requests by title rather
    /// than position (`report::run::resolve_qualified`) — unlike renaming.
    pub fn move_entry(&mut self, from: usize, to: usize) -> bool {
        let len = self.entries.len();
        if from >= len || to >= len || from == to {
            return false;
        }
        let entry = self.entries.remove(from);
        self.entries.insert(to, entry);
        // The selection is a position, so it has to be re-derived rather than
        // left pointing at whatever slid into the old index: the moved request
        // takes its selection with it, and a selection either side of the move
        // shifts by one only if the move stepped across it.
        self.selected_entry = shift_index(self.selected_entry, from, to);
        // A reorder changes no request, so nothing else would record it — see
        // `structure_modified`.
        self.structure_modified = true;
        self.invalidate_request_json();
        self.sync_folder_to_selected();
        true
    }

    /// Move the entry at `from` so it ends up immediately *before* the entry
    /// currently at `before` — the drag-and-drop spelling of [`Self::move_entry`],
    /// where a drop lands in the gap above a row rather than on a slot number.
    /// `before == entries.len()` means "after the last one". Returns whether
    /// anything actually moved.
    ///
    /// The index has to be adjusted when dragging *downwards*: `move_entry`
    /// removes before it inserts, so once the dragged request is lifted out
    /// every row below it slides up by one and the gap the user aimed at is now
    /// one lower. Without this the request would consistently land one place
    /// short of where it was dropped, which reads as the drop being ignored.
    ///
    /// Only the GUI drags requests; the terminal UI reorders with `Alt+↑↓`
    /// through `move_entry`, so this is dead code without the `gui` feature.
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub fn move_entry_before(&mut self, from: usize, before: usize) -> bool {
        let len = self.entries.len();
        if from >= len || before > len {
            return false;
        }
        // Dropping into either gap touching the request is where it already is.
        if before == from || before == from + 1 {
            return false;
        }
        let to = if from < before { before - 1 } else { before };
        self.move_entry(from, to)
    }

    /// Reopen the most recently deleted entry (if any), re-inserting it as
    /// close as possible to the index it was removed from, and return that
    /// index so the caller can select it. `None` when there is nothing to
    /// restore, which both front-ends treat as a no-op.
    pub fn restore_last_deleted(&mut self) -> Option<usize> {
        let (idx, entry) = self.deleted_entries.pop()?;
        let idx = idx.min(self.entries.len());
        self.entries.insert(idx, entry);
        // Restoring is as much a structural change as removing: putting a
        // request back can't be assumed to return the list to a saved state,
        // since it lands at the nearest index rather than necessarily its old
        // one, and anything else may have moved meanwhile.
        self.structure_modified = true;
        Some(idx)
    }
}

#[cfg(test)]
mod undo_delete_tests {
    use super::*;

    fn entry(title: &str) -> HurlEntry {
        let mut e = HurlEntry::default();
        e.title = title.into();
        e
    }

    #[test]
    fn remove_entry_recording_undo_records_index_and_entry() {
        let mut c = Collection::new("c".into(), vec![entry("a"), entry("b"), entry("c")]);
        let removed = c
            .remove_entry_recording_undo(1)
            .expect("index 1 is in range");
        assert_eq!(removed.title, "b");
        assert_eq!(
            c.entries
                .iter()
                .map(|e| e.title.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "c"]
        );
        assert_eq!(c.deleted_entries.len(), 1);
        assert_eq!(c.deleted_entries[0].0, 1);
        assert_eq!(c.deleted_entries[0].1.title, "b");
    }

    #[test]
    fn deleted_entries_cap_holds_at_20() {
        let mut c = Collection::new(
            "c".into(),
            (0..25).map(|i| entry(&format!("r{i}"))).collect(),
        );
        for _ in 0..25 {
            c.remove_entry_recording_undo(0);
        }
        // The oldest deletions are dropped so the history never grows without
        // bound over a long session; only the most recent 20 survive.
        assert_eq!(c.deleted_entries.len(), 20);
        assert_eq!(c.deleted_entries.first().unwrap().1.title, "r5");
        assert_eq!(c.deleted_entries.last().unwrap().1.title, "r24");
    }

    #[test]
    fn restore_last_deleted_reinserts_at_recorded_index() {
        let mut c = Collection::new("c".into(), vec![entry("a"), entry("b"), entry("c")]);
        c.remove_entry_recording_undo(1);
        assert_eq!(
            c.entries
                .iter()
                .map(|e| e.title.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "c"]
        );
        let idx = c.restore_last_deleted().unwrap();
        assert_eq!(idx, 1);
        assert_eq!(
            c.entries
                .iter()
                .map(|e| e.title.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b", "c"]
        );
        assert!(c.deleted_entries.is_empty());
    }

    #[test]
    fn removing_an_out_of_range_entry_is_a_no_op_rather_than_a_panic() {
        let mut c = Collection::new("c".into(), vec![entry("a")]);
        assert!(c.remove_entry_recording_undo(7).is_none());
        assert_eq!(c.entries.len(), 1, "nothing was removed");
        assert!(c.deleted_entries.is_empty(), "and nothing was recorded");
    }

    #[test]
    fn restore_last_deleted_is_none_when_history_empty() {
        let mut c = Collection::new("c".into(), vec![entry("a")]);
        assert!(c.restore_last_deleted().is_none());
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

/// The workspace tree's *virtual* folders: the ones encoded in request titles
/// inside a single collection file, as opposed to real directories.
#[cfg(test)]
mod request_folder_tests {
    use super::*;
    use std::fs;

    fn tmp_root(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("paperboy_reqfold_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A collection whose requests carry folder-encoded titles, the way a
    /// Postman import writes them.
    fn nested_collection(root: &Path) -> (Collection, PathBuf) {
        let path = root.join("api.hurl");
        fs::write(
            &path,
            "# Auth/Login\nGET https://example.com/login\n\n\
             # Auth/Tokens/Refresh\nPOST https://example.com/refresh\n\n\
             # Health\nGET https://example.com/health\n",
        )
        .unwrap();
        let mut col = Collection::new("ws".into(), Vec::new());
        col.workspace_root = Some(root.to_path_buf());
        col.load_workspace_file(path.clone()).unwrap();
        (col, path)
    }

    /// What the tree shows, as `(indent, label)` pairs, ignoring row kind.
    fn shape(col: &Collection) -> Vec<(usize, String)> {
        col.ws_rows()
            .into_iter()
            .map(|r| match r {
                WsRow::Folder { name, depth, .. }
                | WsRow::Collection { name, depth, .. }
                | WsRow::Report { name, depth, .. }
                | WsRow::Environment { name, depth, .. }
                | WsRow::RequestFolder { name, depth, .. }
                | WsRow::Request { name, depth, .. } => (depth, name),
            })
            .collect()
    }

    /// The bug this whole feature exists for: a Postman import names its
    /// requests `folder/request`, and the workspace tree used to drop
    /// everything but the leaf — so a hundred requests from twenty folders
    /// arrived as one flat, ambiguous list with several rows called `Login`.
    #[test]
    fn titles_with_slashes_nest_instead_of_flattening_into_one_list() {
        let root = tmp_root("nest");
        let (col, _) = nested_collection(&root);

        assert_eq!(
            shape(&col),
            vec![
                (0, "api.hurl".to_string()),
                // Folders come before the file's own top-level requests, and
                // `Auth` is open because the selected request (the first one)
                // lives in it — see `expand_selected_request_folders`.
                (1, "Auth".to_string()),
                (2, "Tokens".to_string()),
                (2, "Login".to_string()),
                (1, "Health".to_string()),
            ],
            "each title segment is a row of its own"
        );
        assert!(
            !shape(&col).iter().any(|(_, n)| n.contains('/')),
            "no row still carries a raw `folder/request` name"
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// A folder the selection isn't in stays shut: revealing the selected
    /// request must not amount to expanding the whole file.
    #[test]
    fn folders_the_selection_is_not_in_start_closed() {
        let root = tmp_root("closed");
        let (col, _) = nested_collection(&root);

        assert!(
            !shape(&col).iter().any(|(_, n)| n == "Refresh"),
            "Auth/Tokens is closed, so the request inside it isn't listed"
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// Opening a folder reveals its direct children only — its own subfolder
    /// stays closed until asked for, exactly like a directory.
    #[test]
    fn opening_a_virtual_folder_reveals_one_level_at_a_time() {
        let root = tmp_root("open");
        let (mut col, path) = nested_collection(&root);

        col.workspace_expanded
            .insert(request_folder_path(&path, &["Auth".to_string()]));
        assert_eq!(
            shape(&col),
            vec![
                (0, "api.hurl".to_string()),
                (1, "Auth".to_string()),
                (2, "Tokens".to_string()),
                (2, "Login".to_string()),
                (1, "Health".to_string()),
            ],
            "Auth's own folder and request, indented under it"
        );

        col.workspace_expanded.insert(request_folder_path(
            &path,
            &["Auth".to_string(), "Tokens".to_string()],
        ));
        assert_eq!(
            shape(&col),
            vec![
                (0, "api.hurl".to_string()),
                (1, "Auth".to_string()),
                (2, "Tokens".to_string()),
                (3, "Refresh".to_string()),
                (2, "Login".to_string()),
                (1, "Health".to_string()),
            ],
            "the nested folder opens independently"
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// A request row keeps the index of its request in the *file*, however
    /// deeply the tree nests it — that index is what selecting and running the
    /// row acts on, so nesting must not renumber anything.
    #[test]
    fn nesting_does_not_disturb_the_request_indices() {
        let root = tmp_root("idx");
        let (mut col, path) = nested_collection(&root);
        col.workspace_expanded
            .insert(request_folder_path(&path, &["Auth".to_string()]));
        col.workspace_expanded.insert(request_folder_path(
            &path,
            &["Auth".to_string(), "Tokens".to_string()],
        ));

        let found: Vec<(usize, String)> = col
            .ws_rows()
            .into_iter()
            .filter_map(|r| match r {
                WsRow::Request { idx, name, .. } => Some((idx, name)),
                _ => None,
            })
            .collect();
        assert_eq!(
            found,
            vec![
                (1, "Refresh".to_string()),
                (0, "Login".to_string()),
                (2, "Health".to_string()),
            ],
            "the file's own order is what the indices mean"
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// An untitled request falls back to showing its URL — which is full of
    /// `/`. That fallback is a *label*, not a path, so it must not be split
    /// into an `https:` folder holding an `example.com` folder.
    #[test]
    fn an_untitled_requests_url_is_never_read_as_folders() {
        let root = tmp_root("untitled");
        let path = root.join("bare.hurl");
        fs::write(&path, "GET https://example.com/a/b/c\n").unwrap();
        let mut col = Collection::new("ws".into(), Vec::new());
        col.workspace_root = Some(root.clone());
        col.load_workspace_file(path).unwrap();

        assert_eq!(
            shape(&col),
            vec![
                (0, "bare.hurl".to_string()),
                (1, "https://example.com/a/b/c".to_string()),
            ],
            "one row, showing the whole URL"
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// Selecting a request from outside the tree (loading a file, saving the
    /// wizard, renaming) has to make its row reachable, or the cursor would
    /// land somewhere else entirely because the row was folded away.
    #[test]
    fn selecting_a_nested_request_opens_the_folders_hiding_it() {
        let root = tmp_root("reveal");
        let (mut col, _) = nested_collection(&root);

        col.selected_entry = 1; // Auth/Tokens/Refresh
        col.sync_ws_cursor();

        let rows = col.ws_rows();
        let cursor = rows.get(col.list_cursor);
        assert!(
            matches!(cursor, Some(WsRow::Request { idx: 1, .. })),
            "the cursor is on the selected request, not on a fallback row: {cursor:?}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// The expand/collapse key is a path, and a title is free text: `..` or a
    /// separator in a request name must not produce a key that climbs out of
    /// the collection it belongs to (and then gets persisted).
    #[test]
    fn a_folder_key_cannot_escape_its_collection() {
        let collection = Path::new("/ws/api.hurl");
        let key = request_folder_path(
            collection,
            &["..".to_string(), "a/b".to_string(), ".".to_string()],
        );
        assert!(
            key.starts_with(collection),
            "every key stays under its collection: {key:?}"
        );
        assert!(
            !key.components().any(|c| c.as_os_str() == ".."),
            "and never contains a parent hop: {key:?}"
        );
    }
}

/// A Workspace tab's edits are held in memory and re-read from disk when the
/// tab switches away and back, so anything the tab doesn't recognise as an
/// edit is silently discarded. These pin down that removing or reordering a
/// request counts — neither touches a *surviving* entry's `user_added` /
/// `modified` flags, which is all `has_unsaved_edits` used to look at.
#[cfg(test)]
mod structure_edit_tests {
    use super::*;

    fn ws_collection(dir: &std::path::Path, titles: &[&str]) -> (Collection, PathBuf) {
        let a = dir.join("a.hurl");
        let b = dir.join("b.hurl");
        let entries: Vec<HurlEntry> = titles
            .iter()
            .map(|t| HurlEntry {
                title: (*t).to_string(),
                method: "GET".into(),
                url: "http://x".into(),
                ..Default::default()
            })
            .collect();
        std::fs::write(&a, collection_to_hurl(&entries)).unwrap();
        std::fs::write(&b, "GET http://other\n").unwrap();
        let mut col = Collection::new("ws".into(), entries);
        col.workspace_root = Some(dir.to_path_buf());
        col.path = Some(a.clone());
        (col, b)
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("paperboy_structedit_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn deleting_a_request_survives_a_workspace_file_switch() {
        let dir = temp_dir("delete");
        let (mut col, other) = ws_collection(&dir, &["Login", "Logout"]);

        col.remove_entry_recording_undo(0);
        assert!(
            col.has_unsaved_edits(),
            "a deletion is an unsaved edit, even though no surviving entry is flagged"
        );

        let a = col.path.clone().unwrap();
        col.load_workspace_file(other).unwrap();
        col.load_workspace_file(a).unwrap();

        let titles: Vec<&str> = col.entries.iter().map(|e| e.title.as_str()).collect();
        assert_eq!(
            titles,
            vec!["Logout"],
            "the deleted request must not come back from disk"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The subtler half: a file whose only change was a deletion, parked when
    /// the tab switched away from it. `workspace_pending` holds its entries,
    /// but a deletion leaves no flagged entry among them, so the save-all pass
    /// had nothing to tell it apart from an untouched file.
    /// The count gates the GUI's "unsaved edits" prompt on closing a tab, so
    /// a delete-only change reading as zero meant the tab closed silently and
    /// the deletion went with it.
    fn titles(c: &Collection) -> Vec<&str> {
        c.entries.iter().map(|e| e.title.as_str()).collect()
    }

    fn plain(titles: &[&str]) -> Collection {
        Collection::new(
            "c".into(),
            titles
                .iter()
                .map(|t| HurlEntry {
                    title: (*t).to_string(),
                    method: "GET".into(),
                    ..Default::default()
                })
                .collect(),
        )
    }

    #[test]
    fn moving_an_entry_shifts_the_ones_it_steps_over() {
        let mut c = plain(&["a", "b", "c", "d"]);
        assert!(c.move_entry(0, 2), "a moves down past b and c");
        assert_eq!(titles(&c), vec!["b", "c", "a", "d"]);

        let mut c = plain(&["a", "b", "c", "d"]);
        assert!(c.move_entry(3, 1), "d moves up past c and b");
        assert_eq!(titles(&c), vec!["a", "d", "b", "c"]);
    }

    #[test]
    fn a_move_that_cannot_happen_is_a_no_op() {
        let mut c = plain(&["a", "b"]);
        assert!(!c.move_entry(1, 1), "nowhere to go");
        assert!(!c.move_entry(0, 9), "past the end");
        assert!(!c.move_entry(9, 0), "from nowhere");
        assert_eq!(titles(&c), vec!["a", "b"]);
        assert!(
            !c.structure_modified,
            "and a move that did not happen is not an unsaved change"
        );
    }

    /// Dropping into a gap is the drag-and-drop spelling of a move, and the
    /// index needs adjusting when the drag goes downwards — the dragged request
    /// is lifted out before it is put back, so everything below it slides up by
    /// one first.
    #[test]
    fn a_drop_lands_in_the_gap_it_was_aimed_at() {
        // Downwards: "a" dropped into the gap before "d" must end up between
        // "c" and "d", not between "b" and "c".
        let mut c = plain(&["a", "b", "c", "d"]);
        assert!(c.move_entry_before(0, 3));
        assert_eq!(titles(&c), vec!["b", "c", "a", "d"]);

        // Upwards needs no adjustment: nothing below the gap has moved.
        let mut c = plain(&["a", "b", "c", "d"]);
        assert!(c.move_entry_before(3, 1));
        assert_eq!(titles(&c), vec!["a", "d", "b", "c"]);

        // Past the last row: the one gap that isn't before any entry.
        let mut c = plain(&["a", "b", "c"]);
        assert!(c.move_entry_before(0, 3));
        assert_eq!(titles(&c), vec!["b", "c", "a"]);
    }

    /// Both gaps touching a request are where it already is, so a drop there
    /// must not register as an edit — it would mark the file unsaved for a
    /// change nobody made.
    #[test]
    fn dropping_a_request_back_where_it_started_changes_nothing() {
        let mut c = plain(&["a", "b", "c"]);
        assert!(!c.move_entry_before(1, 1), "the gap above it");
        assert!(!c.move_entry_before(1, 2), "the gap below it");
        assert!(!c.move_entry_before(9, 0), "from nowhere");
        assert!(!c.move_entry_before(0, 9), "into nowhere");
        assert_eq!(titles(&c), vec!["a", "b", "c"]);
        assert!(!c.structure_modified);
    }

    /// The selection is a position, so every move has to re-derive it.
    #[test]
    fn the_selection_follows_whatever_it_was_pointing_at() {
        // The moved entry carries the selection with it.
        let mut c = plain(&["a", "b", "c"]);
        c.selected_entry = 0;
        c.move_entry(0, 2);
        assert_eq!(c.selected_entry, 2, "still on 'a'");

        // A selection the move steps across shifts by one.
        let mut c = plain(&["a", "b", "c"]);
        c.selected_entry = 1;
        c.move_entry(0, 2);
        assert_eq!(c.selected_entry, 0, "still on 'b', which slid up");

        // A selection outside the moved span is left alone.
        let mut c = plain(&["a", "b", "c", "d"]);
        c.selected_entry = 3;
        c.move_entry(0, 2);
        assert_eq!(c.selected_entry, 3, "still on 'd'");
    }

    /// A reorder is exactly the change no request records, which is what
    /// `structure_modified` exists for.
    #[test]
    fn reordering_counts_as_an_unsaved_change() {
        let dir = temp_dir("reorder");
        let (mut col, _) = ws_collection(&dir, &["Login", "Logout"]);
        let a = col.path.clone().unwrap();

        assert!(col.move_entry(0, 1));
        assert!(col.has_unsaved_edits());
        assert_eq!(col.unsaved_edit_count(), 1);

        assert_eq!(col.save_workspace_edits().unwrap(), 1);
        let on_disk = std::fs::read_to_string(&a).unwrap();
        assert!(
            on_disk.find("Logout").unwrap() < on_disk.find("Login").unwrap(),
            "the new order reached the file: {on_disk}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_structural_edit_counts_as_an_unsaved_change() {
        let dir = temp_dir("count");
        let (mut col, other) = ws_collection(&dir, &["Login", "Logout"]);
        assert_eq!(col.unsaved_edit_count(), 0);

        col.remove_entry_recording_undo(0);
        assert_eq!(
            col.unsaved_edit_count(),
            1,
            "the deletion is a change, even with no request left to flag it"
        );

        // Once parked, it still counts — and only once.
        col.load_workspace_file(other).unwrap();
        assert_eq!(col.unsaved_edit_count(), 1);

        col.save_workspace_edits().unwrap();
        assert_eq!(col.unsaved_edit_count(), 0, "and saving settles it");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_parked_files_structural_edit_is_written_too() {
        let dir = temp_dir("parked");
        let (mut col, other) = ws_collection(&dir, &["Login", "Logout"]);
        let a = col.path.clone().unwrap();

        col.remove_entry_recording_undo(0);
        // Switch away, so the edit is parked rather than loaded.
        col.load_workspace_file(other).unwrap();
        assert!(
            col.workspace_pending.contains_key(&a),
            "the deletion was parked rather than discarded"
        );

        assert_eq!(
            col.save_workspace_edits().unwrap(),
            1,
            "the parked file was written"
        );
        let on_disk = std::fs::read_to_string(&a).unwrap();
        assert!(
            !on_disk.contains("Login"),
            "the parked deletion reached the file: {on_disk}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An untouched file must still be left alone — the flag has to be cleared
    /// on the way through, or every later save would rewrite files needlessly
    /// (and stamp over changes made outside PaperBoy).
    #[test]
    fn saving_clears_the_structural_marks_it_just_wrote() {
        let dir = temp_dir("clears");
        let (mut col, _) = ws_collection(&dir, &["Login", "Logout"]);

        col.remove_entry_recording_undo(0);
        col.save_workspace_edits().unwrap();
        assert!(col.workspace_structure_modified.is_empty());
        assert!(!col.structure_modified);
        assert_eq!(
            col.save_workspace_edits().unwrap(),
            0,
            "a second save has nothing left to write"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_structural_edit_is_written_by_save_workspace_edits() {
        let dir = temp_dir("save");
        let (mut col, _) = ws_collection(&dir, &["Login", "Logout"]);
        let a = col.path.clone().unwrap();

        col.remove_entry_recording_undo(0);
        assert_eq!(
            col.save_workspace_edits().unwrap(),
            1,
            "the file was written"
        );

        let on_disk = std::fs::read_to_string(&a).unwrap();
        assert!(
            !on_disk.contains("Login"),
            "the deletion reached the file: {on_disk}"
        );
        assert!(
            !col.has_unsaved_edits(),
            "and saving clears the structural edit, like any other"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
