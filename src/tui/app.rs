use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui_explorer::FileExplorer;

use crate::collection::Collection;
use crate::environment::{
    EnvUpdate, EnvVar, Environment, PendingSecret, looks_like_env, parse_vars_pending,
    spawn_resolution,
};
use crate::git_remote::{self, GitOrigin, RefKind};
use crate::http::ApiResponse;
use crate::hurl::{HurlEntry, RunStatus};
use crate::i18n::{Language, Status, Strings};
use crate::request::{self, AppVars, CaptureUpdate, build_request_json};

use super::editor::*;
use super::git_save::*;
use super::new_request::*;
use super::remote::*;
use super::wrapcache::{PanelWrap, TextPos};

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum FileAction {
    SaveRequest,
    LoadRequest,
    OpenCollection,
    SaveCollection,
    LoadEnv,
    SaveEnv,
    SaveResponse,
    /// Picking a file for a `File`-kind Form row's Value cell (carries the
    /// row index). The in-progress wizard is parked in `TuiApp::parked_wizard`
    /// while the browser is open and restored once the pick (or cancel)
    /// completes.
    PickFormFile(usize),
    /// Picking a ROOT FOLDER for a new Workspace tab (see
    /// [`crate::workspace`]). Unlike every other `FileAction`, the browser
    /// popup confirms on `Space` (the current working directory), not on
    /// selecting a file with Enter — Enter still just descends into
    /// subfolders as normal navigation.
    OpenWorkspace,
    /// Picking a DESTINATION FOLDER to save a Workspace's files into
    /// permanently (see [`TuiApp::pending_workspace_save`]) — confirms on
    /// `Space` exactly like `OpenWorkspace`, then a name prompt follows.
    SaveWorkspaceChooseFolder,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum PromptKind {
    BaseUrl,
    /// Editing one variable's value in the environment-entries popup:
    /// `(env_id, var_index)` — environments are addressed by id since they
    /// live in the global list, not per-collection.
    EnvValue(u64, usize),
    RenameTab(usize),
    /// Renaming a Global Environment (F2 while the Global Environments list
    /// or its entries popup is focused), addressed by env id.
    RenameEnv(u64),
    /// Naming a freshly-loaded environment after "Rename then add" was
    /// chosen on an [`Overlay::EnvCollision`] name-collision popup; the
    /// environment itself is parked in `TuiApp::pending_collision_env`.
    RenameNewEnv,
    /// Raw Mode: the selected entry's actual Hurl-text representation,
    /// reparsed back into the entry on commit.
    Raw(usize),
    /// Raw JSON Mode: the selected entry's [`build_request_json`] preview,
    /// reparsed back into the entry on commit — the JSON-text counterpart to
    /// `Raw`, opened with Shift+J instead of Shift+H.
    RawJson(usize),
    FilePath(FileAction),
    /// Naming a Workspace being saved to a permanent folder (see
    /// [`TuiApp::pending_workspace_save`]) — the destination directory was
    /// already chosen via the folder browser; this only picks the
    /// workspace's own (sub)folder name within it. Defaults to the tab's
    /// current name.
    WorkspaceSaveName,
}

impl PromptKind {
    /// The ghost suffix to autocomplete in a file-save prompt (`.hurl` for a
    /// collection, `.vars` for an environment), or `""` for other prompts.
    pub(crate) fn save_ghost(&self) -> &'static str {
        match self {
            PromptKind::FilePath(FileAction::SaveCollection) => ".hurl",
            PromptKind::FilePath(FileAction::SaveEnv) => ".vars",
            _ => "",
        }
    }
}

/// A tiny two-field form (a `Key | Value` table) for adding an environment
/// variable by hand. Tab switches cells; Enter commits; Esc cancels.
pub(crate) struct EnvVarForm {
    /// Target Global Environment (by id) the variable is added to.
    pub(crate) env_id: u64,
    pub(crate) key: Editor,
    pub(crate) value: Editor,
    /// Which cell is focused: `false` = Key, `true` = Value.
    pub(crate) on_value: bool,
}

impl EnvVarForm {
    pub(crate) fn new(env_id: u64) -> Self {
        Self {
            env_id,
            key: Editor::blank(),
            value: Editor::blank(),
            on_value: false,
        }
    }

    /// The editor for the currently focused cell.
    pub(crate) fn focused_mut(&mut self) -> &mut Editor {
        if self.on_value {
            &mut self.value
        } else {
            &mut self.key
        }
    }
}

/// State for the popup listing one [`Environment`]'s vars — opened either by
/// pressing Enter on a Global Environments list row, or by viewing a
/// collection's linked environment from the Tabs pane ('v'). Same format and
/// shortcuts as the old inline Environment panel.
pub(crate) struct EnvPopupState {
    pub(crate) env_id: u64,
    pub(crate) idx: usize,
    pub(crate) hscroll: u16,
    /// Content width of the selected row's `key = value` text, recorded
    /// during draw (see `env_scroll_w`/`list_scroll_w`) so `hscroll` can be
    /// clamped to stop at the text's end.
    pub(crate) scroll_w: std::cell::Cell<u16>,
}

impl EnvPopupState {
    pub(crate) fn new(env_id: u64) -> Self {
        Self {
            env_id,
            idx: 0,
            hscroll: 0,
            scroll_w: std::cell::Cell::new(0),
        }
    }
}

/// A picker (opened with 'p' in the Requests/List pane) of which Global
/// Environment to link/unlink to a collection. `sel == 0` means "None"
/// (unlink); `sel == i + 1` means `global_envs[i]`.
pub(crate) struct EnvLinkPicker {
    pub(crate) ci: usize,
    pub(crate) sel: usize,
}

/// A newly loaded environment whose name collides with one already in the
/// Global Environments list — the user is asked how to resolve it.
pub(crate) struct EnvCollision {
    /// The environment freshly parsed from the file/git content — not yet
    /// added to `global_envs`; this popup decides how (or whether) to add it.
    pub(crate) new_env: Environment,
    pub(crate) pending: Vec<PendingSecret>,
    /// id of the already-existing Global Environment sharing its name.
    pub(crate) existing_id: u64,
    pub(crate) sel: usize,
}

/// The Workspace file-tree popup (see [`crate::workspace`]): browsing the
/// folder bound to `collection_idx`'s tab so the user can choose which
/// `.hurl`/`.json` collection file to load into it. Opened by `w` on a
/// Workspace-bound tab, and auto-opened whenever such a tab has no
/// collection chosen yet.
pub(crate) struct WorkspacePickerState {
    /// Index into `TuiApp::collections` of the Workspace tab this popup is
    /// choosing a file for.
    pub(crate) collection_idx: usize,
    pub(crate) root: std::path::PathBuf,
    pub(crate) filter_hurl_json: bool,
    /// The flattened, depth-first tree for `root` at the current filter
    /// setting (see `crate::workspace::scan_workspace`) — rebuilt whenever
    /// the filter is toggled.
    pub(crate) entries: Vec<crate::workspace::WsEntry>,
    /// Index into `entries` of the highlighted row. Always a FILE row (only
    /// files are selectable; directory rows are unselectable visual
    /// grouping), or `entries.len()` if there are no files at all to select.
    pub(crate) selected: usize,
    pub(crate) scroll: u16,
}

impl WorkspacePickerState {
    pub(crate) fn new(
        collection_idx: usize,
        root: std::path::PathBuf,
        filter_hurl_json: bool,
    ) -> Self {
        let entries = crate::workspace::scan_workspace(&root, filter_hurl_json);
        let selected = entries
            .iter()
            .position(|e| !e.is_dir)
            .unwrap_or(entries.len());
        Self {
            collection_idx,
            root,
            filter_hurl_json,
            entries,
            selected,
            scroll: 0,
        }
    }

    /// Re-scan `root` after the filter toggle changes, keeping the
    /// previously selected file highlighted if it's still present.
    pub(crate) fn rescan(&mut self) {
        let selected_path = self.entries.get(self.selected).map(|e| e.path.clone());
        self.entries = crate::workspace::scan_workspace(&self.root, self.filter_hurl_json);
        self.selected = selected_path
            .and_then(|p| self.entries.iter().position(|e| e.path == p))
            .or_else(|| self.entries.iter().position(|e| !e.is_dir))
            .unwrap_or(self.entries.len());
        self.scroll = 0;
    }

    /// Move the selection to the next/previous FILE row (skipping over
    /// directory grouping rows), wrapping neither past the first nor last.
    pub(crate) fn nav(&mut self, delta: i32) {
        let file_idxs: Vec<usize> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| !e.is_dir)
            .map(|(i, _)| i)
            .collect();
        if file_idxs.is_empty() {
            return;
        }
        let cur = file_idxs
            .iter()
            .position(|&i| i == self.selected)
            .unwrap_or(0);
        let next = (cur as i32 + delta).clamp(0, file_idxs.len() as i32 - 1) as usize;
        self.selected = file_idxs[next];
    }
}

pub(crate) enum Overlay {
    /// Top-level File menu: just "(L)oad" / "(S)ave", each opening its own
    /// grouped submenu (see `FileLoadMenu`/`FileSaveMenu`) — replaces the old
    /// flat 12-item list that had grown hard to scan.
    FileMenu(usize),
    /// The "Load" submenu of the File menu (Request / Collection / Collection
    /// from Git / Environment / Environment from Git). Esc/q returns to
    /// `FileMenu(0)`.
    FileLoadMenu(usize),
    /// The "Save" submenu of the File menu (Request / Collection / Collection
    /// As / Collection to Git / Environment / Environment As / Response).
    /// Esc/q returns to `FileMenu(1)`.
    FileSaveMenu(usize),
    /// The "Settings" menu (Language / Preferences / Clear all collections) —
    /// opened with `s`. Not to be confused with `Preferences`, the submenu one
    /// level down that holds the actual toggle-able preferences.
    Options(usize),
    LanguageMenu(usize),
    /// The "Preferences" submenu of the Settings menu: confirm-on-exit,
    /// confirm-on-clear, and the default Request panel view (JSON/Hurl).
    Preferences(usize),
    /// The "Default Request View" submenu of Preferences: JSON / Hurl,
    /// mirroring `LanguageMenu`'s pick-one-and-close pattern.
    RequestViewMenu(usize),
    Confirm {
        action: ConfirmAction,
        sel: usize,
    },
    /// The `?`/F1 Help popup. Holds which of its two tabs is showing: `0` =
    /// keyboard shortcuts, `1` = the substitution colour/icon glossary.
    /// Tab/Shift+Tab (or Left/Right) switches tabs; any other key closes
    /// the popup, same as the old stateless variant.
    Help(usize),
    Prompt {
        kind: PromptKind,
        editor: Editor,
        title: String,
        mask: bool,
        reset_to: Option<String>,
        /// While `true`, the field still holds the untouched original secret and
        /// is drawn as a fixed-width mask (hiding its length); the first edit
        /// clears it wholesale.
        secret_intact: bool,
        /// `Some(still_secret)` when editing an environment value that came
        /// from a secret provider (1Password / SSM): a checkbox the user can
        /// toggle to declare whether the edited value is still sensitive.
        /// `None` for prompts that aren't editing such a variable (the
        /// checkbox isn't shown). Defaults to `Some(true)` — the safe choice —
        /// when applicable.
        secret_checkbox: Option<bool>,
    },
    Browser(FileAction, Box<FileExplorer>),
    NewRequest(Box<NewReq>),
    EnvVarForm(Box<EnvVarForm>),
    RemoteGit(Box<RemoteWizard>),
    GitSave(Box<GitSaveWizard>),
    /// Viewing one Global Environment's vars (see [`EnvPopupState`]).
    EnvPopup(EnvPopupState),
    /// Linking/unlinking a Global Environment to a collection (see
    /// [`EnvLinkPicker`]).
    EnvLinkPicker(EnvLinkPicker),
    /// Resolving a name collision on environment load (see [`EnvCollision`]).
    EnvCollision(Box<EnvCollision>),
    /// The Workspace file-tree popup (see [`WorkspacePickerState`]).
    WorkspacePicker(WorkspacePickerState),
    /// Shown instead of immediately closing a tab whose `workspace_root` was
    /// downloaded from git (see [`Collection::workspace_downloaded_from_git`]):
    /// asks whether to keep the throwaway folder on disk (so `u`/Ctrl+Shift+T
    /// can still reopen the tab from it) or delete it now (in which case the
    /// tab is closed without being added to `closed_tabs`, since there'd be
    /// nothing left to reopen). `idx` is the tab being closed, `path` is its
    /// `workspace_root` (shown so the user knows where the files live if they
    /// choose to keep them). `sel`: 0 = Keep, 1 = Delete, 2 = Cancel.
    CloseGitWorkspace {
        idx: usize,
        path: PathBuf,
        sel: usize,
    },
    /// Shown at startup when a Workspace tab's `workspace_root` folder has
    /// vanished (e.g. the OS cleared `/tmp`) but it's known to have been
    /// downloaded from git — offers to redownload the exact commit it was
    /// last at rather than just reporting it as permanently gone. `idx` is
    /// the tab; `sel`: 0 = Yes, 1 = No. Declining (or Esc) falls back to the
    /// plain [`crate::i18n::Status::WorkspaceFolderMissing`] message, same
    /// as a non-git Workspace whose folder vanished.
    WorkspaceReloadConfirm {
        idx: usize,
        reload: Box<crate::persistence::PendingWorkspaceReload>,
        sel: usize,
    },
    /// A redownload confirmed above is running in the background (see
    /// [`TuiApp::workspace_redownload_rx`]) — shown until it completes.
    WorkspaceReloadLoading {
        idx: usize,
    },
    /// Shown right after a Workspace's files finish downloading from git
    /// (see [`GitMsg::Workspace`]), before the new tab is even created:
    /// asks whether to keep the download in its throwaway temp folder (the
    /// existing default — offered a redownload later if it vanishes) or
    /// save it to a permanent, user-chosen folder right away (see
    /// [`TuiApp::pending_workspace_save`]). `repo` is the downloaded temp
    /// folder, `name` the tab name derived from the repo URL, `origin` what
    /// would be recorded for a later redownload if kept temporary. `sel`:
    /// 0 = keep temporarily, 1 = choose a folder.
    WorkspaceStorageChoice {
        repo: PathBuf,
        name: String,
        origin: Option<WorkspaceGitOrigin>,
        sel: usize,
    },
}

/// Which action a confirmation popup is guarding.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum ConfirmAction {
    Exit,
    Clear,
    /// Save a collection / environment to its ORIGINAL file (clears the "new"
    /// and "modified" markers). Only raised when there are unsaved changes.
    Save(FileAction),
    /// "Save As" to a path that already exists — confirm the overwrite. The
    /// target path is held in [`TuiApp::pending_save_path`].
    Overwrite(FileAction),
    /// Delete the Global Environment at this index in `global_envs` ('x' in
    /// the Global Environments panel) — any collections linked to it become
    /// unlinked.
    DeleteEnv(usize),
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Pane {
    Tabs,
    List,
    /// The "Global Environments" panel: a scrollable list of environment
    /// names (not the vars themselves — see [`Overlay::EnvPopup`] for that).
    GlobalEnv,
    Main,
    Response,
}

/// Where `pane` ranks in top-to-bottom reading order among the panes that
/// can hold a text selection — used to order multi-region copies (Main
/// before Response) regardless of the order the selections were made in.
pub(crate) fn pane_rank(pane: Pane) -> u8 {
    match pane {
        Pane::Main => 0,
        Pane::Response => 1,
        _ => 2,
    }
}

/// The 2 top-level File menu items: "(L)oad" and "(S)ave".
pub(crate) fn file_menu_items(s: &Strings) -> [&'static str; 2] {
    [s.file_menu_item_load, s.file_menu_item_save]
}

/// The 7 items of the File menu's "Load" submenu.
pub(crate) fn file_load_items(s: &Strings) -> [&'static str; 7] {
    [
        s.file_load_item_request,
        s.file_load_item_collection,
        s.file_load_item_collection_git,
        s.file_load_item_environment,
        s.file_load_item_environment_git,
        s.file_load_item_workspace,
        s.file_load_item_workspace_git,
    ]
}

/// The 8 items of the File menu's "Save" submenu.
pub(crate) fn file_save_items(s: &Strings) -> [&'static str; 8] {
    [
        s.file_save_item_request,
        s.file_save_item_collection,
        s.file_save_item_collection_as,
        s.file_save_item_collection_git,
        s.file_save_item_environment,
        s.file_save_item_environment_as,
        s.file_save_item_workspace,
        s.file_save_item_response,
    ]
}

/// Extracts the mnemonic letter embedded in a menu label using the "(X)"
/// convention (e.g. `"(R)equest…"` -> `Some('r')`), or `None` if the label
/// has no such single-character bracketed marker. Deriving the shortcut key
/// straight from the displayed label (rather than a separate hardcoded
/// table) keeps every language's translation free to pick whichever letter
/// reads best in that language, with the key handler automatically matching
/// whatever the current translation shows on screen.
pub(crate) fn menu_mnemonic(label: &str) -> Option<char> {
    let open = label.find('(')?;
    let mut chars = label[open + 1..].chars();
    let c = chars.next()?;
    (chars.next() == Some(')')).then(|| c.to_ascii_lowercase())
}

/// Finds which item in `items` is bound to the mnemonic letter `ch`
/// (case-insensitive), if any — see [`menu_mnemonic`].
pub(crate) fn mnemonic_index(items: &[&str], ch: char) -> Option<usize> {
    let ch = ch.to_ascii_lowercase();
    items
        .iter()
        .position(|label| menu_mnemonic(label) == Some(ch))
}

/// An in-progress or just-finished mouse (or keyboard-extended) text
/// selection, scoped to whichever of the Main (Request JSON) / Response
/// panels it started in. Positions are logical ([`TextPos`]: raw line +
/// char offset), never raw terminal (column, row) cells — so the exact same
/// characters stay selected across a rewrap/rescroll/resize instead of
/// silently re-interpreting stale screen coordinates against new content.
/// Projected onto the current frame's screen space fresh every draw (see
/// `draw::paint_selection_highlight`) via that panel's [`PanelWrap`] cache.
#[derive(Clone, Copy)]
pub(crate) struct TextSelection {
    pub(crate) pane: Pane,
    pub(crate) anchor: TextPos,
    pub(crate) cursor: TextPos,
}

/// A background Workspace-redownload attempt in flight: the tab index it's
/// for, the previously-selected file's path relative to the old root (to
/// re-select it in the fresh checkout if it still exists), and the receiver
/// for the eventual result. See [`TuiApp::workspace_redownload_rx`].
type WorkspaceRedownloadRx = (usize, Option<String>, Receiver<Result<PathBuf, String>>);

/// What an in-progress "Save Workspace" (see [`PendingWorkspaceSave`]) will
/// do once the destination folder and name are both known.
pub(crate) enum WorkspaceSaveTarget {
    /// Copying an already-open Workspace tab's files to a new permanent
    /// folder — `usize` is its index in `TuiApp::collections`, rebound in
    /// place once the copy succeeds.
    ExistingTab(usize),
    /// Finalizing a just-downloaded git Workspace's storage location (see
    /// [`Overlay::WorkspaceStorageChoice`]) — a brand new tab is created
    /// once the copy succeeds. `origin` is what would have been recorded
    /// had the user kept it temporary instead; kept here only so cancelling
    /// this sub-flow (Esc from the folder browser, or an empty name) can
    /// fall back to that outcome rather than losing the download entirely.
    NewGitTab { origin: Option<WorkspaceGitOrigin> },
}

/// An in-progress "Save Workspace" action: copy `source_root`'s files to a
/// permanent, user-chosen folder. `dest_dir` is filled in once the folder
/// browser confirms a destination (see [`TuiApp::workspace_save_pick_folder`]);
/// the final destination is `dest_dir.join(name)`, `name` coming from the
/// follow-up [`PromptKind::WorkspaceSaveName`] prompt (defaulting to
/// `default_name`).
pub(crate) struct PendingWorkspaceSave {
    pub(crate) source_root: PathBuf,
    pub(crate) default_name: String,
    pub(crate) target: WorkspaceSaveTarget,
    pub(crate) dest_dir: Option<PathBuf>,
}

pub struct TuiApp {
    pub(crate) language: Language,
    pub(crate) vars: AppVars,
    pub(crate) collections: Vec<Collection>,
    pub(crate) active_tab: usize,
    pub(crate) response: Arc<Mutex<ApiResponse>>,

    /// The global list of Environments, shared across all collections (see
    /// the "Global Environments" panel, `Pane::GlobalEnv`). Individual
    /// collections may `linked_env_id` one of these; at most one may be
    /// `active_env_id` at a time.
    pub(crate) global_envs: Vec<Environment>,
    /// The currently-activated Global Environment, if any — its vars are
    /// used for substitution in any collection (subject to being overridden
    /// by that collection's own `linked_env_id`, if set, on name collision).
    pub(crate) active_env_id: Option<u64>,

    pub(crate) focus: Pane,
    /// Selected row in the Global Environments list (panel showing env
    /// NAMES only — see `Pane::GlobalEnv`). Renamed from the old `env_idx`,
    /// which used to index the selected *variable* row inside the old
    /// inline Environment panel; that per-variable selection now lives in
    /// `Overlay::EnvPopup`'s own `EnvPopupState::idx`.
    pub(crate) global_env_idx: usize,
    pub(crate) resp_scroll: u16,
    /// Max value `resp_scroll` may take (wrapped content lines − viewport
    /// height); updated each frame by `draw_response` so scrolling stops once
    /// the last line of the response body is in view (no blank overscroll).
    pub(crate) resp_max_scroll: u16,
    pub(crate) main_scroll: u16,
    /// Max value `main_scroll` may take (content lines − viewport height);
    /// updated each frame by `draw_collection_main`.
    pub(crate) main_max_scroll: u16,
    /// Horizontal scroll offset (in characters) for the selected entry's name in
    /// the collections list, so long request URLs can be read end-to-end.
    pub(crate) list_hscroll: u16,
    /// Same, for the selected Global Environment's name in the Global
    /// Environments list (in case a name is longer than the panel is wide).
    pub(crate) global_env_hscroll: u16,
    /// The current ("active") mouse text selection (if any), scoped to
    /// whichever of the Main/Response panels it started in — the one a
    /// plain click-drag creates/replaces, that Shift+Arrow extends, and
    /// that a further Alt+Click+Drag finalizes into `extra_selections`
    /// before starting the next one. `None` when nothing is selected.
    pub(crate) text_selection: Option<TextSelection>,
    /// Additional, already-finished selection regions made via
    /// Alt+Click+Drag (each finalized when the *next* Alt+Click starts a
    /// new region, or cleared entirely by a plain click / Esc). Kept
    /// separate from `text_selection` because only the latter is "live"
    /// (draggable, autoscrollable, Shift+Arrow-extendable) — these are
    /// static once made, and are simply painted and copied alongside it.
    pub(crate) extra_selections: Vec<TextSelection>,
    /// Set while a drag has moved past the top/bottom edge of its panel:
    /// which panel and which direction (-1 up, +1 down) to keep scrolling —
    /// consumed once per event *and* once per idle main-loop tick (see
    /// `tui::run`), so holding the drag outside the panel keeps revealing
    /// (and selecting) further lines even without further mouse movement.
    pub(crate) pending_autoscroll: Option<(Pane, i8)>,
    /// The exact screen Rect the Request JSON body was rendered into last
    /// frame, used to hit-test mouse clicks/drags against this panel.
    pub(crate) main_text_area: Rect,
    /// Cached line/wrap structure for the Request JSON body (see
    /// `wrapcache::PanelWrap`), rebuilt each frame `draw_collection_main`
    /// runs (its content is always small) — the single source of truth for
    /// rendering, mapping mouse coordinates to text, and extracting a
    /// selection, so all three always agree on exactly the same content.
    pub(crate) main_wrap: Option<PanelWrap>,
    /// Character positions (within `main_wrap`'s logical text) of every
    /// shadow-warning icon (see `draw::SHADOW_ICON`) rendered into the
    /// Request JSON/Hurl body this frame — recomputed alongside `main_wrap`
    /// every time it's rebuilt. A purely visual annotation, so it's
    /// excluded from copied/selected text (see `whole_panel_text`,
    /// `concatenated_selection_text`) rather than corrupting a pasted
    /// request with a stray "!" the recipient would have to notice and
    /// remove by hand.
    pub(crate) main_shadow_icon_positions: std::collections::HashSet<TextPos>,
    /// Same as `main_text_area`/`main_wrap`, for the Response body — except
    /// `resp_wrap` is *not* rebuilt unconditionally every frame: it's only
    /// rebuilt when the response body or panel width actually changes (see
    /// `PanelWrap::rebuild_if_needed`), which is what keeps dragging a
    /// selection or scrolling responsive even for an "obscenely large" body.
    pub(crate) resp_text_area: Rect,
    pub(crate) resp_wrap: Option<PanelWrap>,
    /// The exact screen Rect the Request JSON panel's scrollbar thumb/track
    /// was last rendered into (one column, on the panel's right border),
    /// used to hit-test mouse clicks/drags for click-to-jump and
    /// drag-to-scroll. `Rect::default()` (zero size, so it can never be hit)
    /// whenever the panel doesn't need a scrollbar this frame.
    pub(crate) main_scrollbar_area: Rect,
    /// Same as `main_scrollbar_area`, for the Response panel.
    pub(crate) resp_scrollbar_area: Rect,
    /// Set while the user is dragging one of the two panels' scrollbar
    /// thumbs with the mouse (which panel it is), so a `Drag` event keeps
    /// adjusting that panel's scroll even if the cursor briefly leaves the
    /// scrollbar's one-column-wide hit area. Cleared on `Up`.
    pub(crate) scrollbar_drag: Option<Pane>,
    /// The exact screen Rect the Raw Mode (Hurl) editor's text was last
    /// rendered into (see `draw::draw_overlay`'s `Overlay::Prompt` branch) —
    /// used to hit-test mouse clicks/drags for in-editor text selection,
    /// same idea as `main_text_area`/`resp_text_area` but for the overlay
    /// editor. `Rect::default()` whenever no `Overlay::Prompt` is showing.
    pub(crate) prompt_editor_area: Rect,
    /// Content width (columns) available for the scrolled text in each panel,
    /// recorded during draw so the scroll can be clamped to stop at the name's
    /// end (no scrolling past into blank space).
    pub(crate) list_scroll_w: std::cell::Cell<u16>,
    pub(crate) global_env_scroll_w: std::cell::Cell<u16>,
    pub(crate) response_pct: u16,
    /// Width (columns) of the left column (Requests/Environment panels),
    /// user-adjustable with `<`/`>` and persisted across restarts.
    pub(crate) list_width: u16,

    pub(crate) status: Option<Status>,
    pub(crate) overlay: Option<Overlay>,
    /// Vertical scroll offset (rows) into the currently-open Help popup's
    /// body — reset to 0 whenever Help is (re)opened or its tab is
    /// switched. Lets a Help body taller than the terminal be scrolled with
    /// Up/Down instead of those keys just closing the popup.
    pub(crate) help_scroll: u16,
    pub(crate) quit: bool,

    /// Receivers for in-flight background secret resolution (one per env load).
    pub(crate) pending_env: Vec<Receiver<EnvUpdate>>,

    /// Receivers for in-flight response captures (one per run of a capturing entry).
    pub(crate) pending_captures: Vec<Receiver<CaptureUpdate>>,

    /// Receivers for in-flight "Run All" (Alt+F5) passes over a whole collection.
    pub(crate) pending_batch_runs: Vec<Receiver<request::BatchRunUpdate>>,

    /// Folder the file browser last selected a file from; it reopens here.
    pub(crate) last_browse_dir: Option<PathBuf>,

    /// Folder the last *environment* file was loaded from; the environment
    /// picker reopens here (falling back to `last_browse_dir`), so it isn't
    /// dragged around by loads of unrelated file types.
    pub(crate) last_env_dir: Option<PathBuf>,

    /// Directory the file browser started in the last time it was opened, so
    /// `^r` can snap it back there after wandering up/down the tree. Set by
    /// [`TuiApp::open_browser`]; only meaningful while a browser overlay is up.
    pub(crate) browser_origin_dir: Option<PathBuf>,

    /// Settings (persisted): confirm before quitting / closing all collections.
    pub(crate) confirm_on_exit: bool,
    pub(crate) confirm_on_clear: bool,
    /// Preferences (persisted): which of JSON / Hurl text the Main (Request)
    /// panel shows by default, for every request. Changed from the
    /// Preferences submenu (Settings → Preferences → Default Request View).
    pub(crate) default_request_view: request::RequestView,

    /// `true` when the terminal supports the keyboard-enhancement protocol, so
    /// Ctrl+Enter is reported distinctly from a plain Enter. Advanced shortcuts
    /// (Ctrl+Enter) are only advertised in the UI when this is set.
    pub(crate) enhanced_keys: bool,

    /// Target path for a pending "Save As" overwrite confirmation.
    pub(crate) pending_save_path: Option<PathBuf>,

    /// Git URLs the user has loaded a collection/environment from, most recent
    /// first. Offered as a pickable list in the "Load from Git" wizard and
    /// persisted across restarts.
    pub(crate) recent_git_urls: Vec<String>,

    /// Stack of recently closed tabs (with the index they were closed from),
    /// most-recently-closed last, so Ctrl+Shift+T can reopen them in order.
    /// Runtime-only (not persisted).
    pub(crate) closed_tabs: Vec<(usize, Collection)>,

    /// The in-progress New/Edit Request wizard, stashed here while the file
    /// browser is open for `FileAction::PickFormFile` (the overlay is a
    /// single slot, so opening the Browser would otherwise discard it).
    /// Restored (with the picked path applied, on success) once the browser
    /// closes. Runtime-only (not persisted).
    pub(crate) parked_wizard: Option<Box<NewReq>>,
    /// A freshly-loaded environment (plus its still-pending secrets) parked
    /// here while the user is asked for a new name after choosing "Rename
    /// then add" on an [`Overlay::EnvCollision`] popup. Added to
    /// `global_envs` once the rename prompt is committed.
    pub(crate) pending_collision_env: Option<(Environment, Vec<PendingSecret>)>,
    /// Workspace tabs restored with a vanished `workspace_root` that were
    /// originally downloaded from git (see
    /// `persistence::PersistedTab::into_collection`'s `PendingWorkspaceReload`),
    /// queued up so each is offered a redownload one at a time via
    /// [`Overlay::WorkspaceReloadConfirm`] rather than all popping up at once.
    /// `usize` is the tab's index in `collections`.
    pub(crate) pending_workspace_reloads:
        std::collections::VecDeque<(usize, crate::persistence::PendingWorkspaceReload)>,
    /// A background redownload attempt in flight (see
    /// [`Overlay::WorkspaceReloadLoading`]): the tab index it's for, the
    /// previously-selected file's path relative to the old root (to
    /// re-select it in the fresh checkout if it still exists), and the
    /// receiver for the result.
    pub(crate) workspace_redownload_rx: Option<WorkspaceRedownloadRx>,
    /// An in-progress "Save Workspace" (see [`PendingWorkspaceSave`]) — set
    /// while the folder browser / name prompt it drives are open, taken
    /// once the save completes, is cancelled, or falls back to keeping a
    /// fresh git download temporary.
    pub(crate) pending_workspace_save: Option<PendingWorkspaceSave>,
}

impl Default for TuiApp {
    fn default() -> Self {
        Self {
            language: Language::default(),
            vars: AppVars::default(),
            collections: vec![Collection::new("Request".to_string(), Vec::new())],
            active_tab: 0,
            response: Arc::new(Mutex::new(ApiResponse::default())),
            global_envs: Vec::new(),
            active_env_id: None,
            focus: Pane::List,
            global_env_idx: 0,
            resp_scroll: 0,
            resp_max_scroll: 0,
            main_scroll: 0,
            main_max_scroll: 0,
            list_hscroll: 0,
            global_env_hscroll: 0,
            text_selection: None,
            extra_selections: Vec::new(),
            pending_autoscroll: None,
            main_text_area: Rect::default(),
            main_wrap: None,
            main_shadow_icon_positions: std::collections::HashSet::new(),
            resp_text_area: Rect::default(),
            resp_wrap: None,
            main_scrollbar_area: Rect::default(),
            resp_scrollbar_area: Rect::default(),
            scrollbar_drag: None,
            prompt_editor_area: Rect::default(),
            list_scroll_w: std::cell::Cell::new(0),
            global_env_scroll_w: std::cell::Cell::new(0),
            response_pct: 42,
            list_width: 38,
            status: None,
            overlay: None,
            help_scroll: 0,
            quit: false,
            pending_env: Vec::new(),
            pending_captures: Vec::new(),
            pending_batch_runs: Vec::new(),
            last_browse_dir: None,
            last_env_dir: None,
            browser_origin_dir: None,
            confirm_on_exit: true,
            confirm_on_clear: true,
            default_request_view: request::RequestView::default(),
            enhanced_keys: false,
            pending_save_path: None,
            recent_git_urls: Vec::new(),
            closed_tabs: Vec::new(),
            parked_wizard: None,
            pending_collision_env: None,
            pending_workspace_reloads: std::collections::VecDeque::new(),
            workspace_redownload_rx: None,
            pending_workspace_save: None,
        }
    }
}

impl TuiApp {
    /// Whether there's any text selection at all — the "active" one
    /// (`text_selection`) and/or any additional Alt+Click+Drag regions.
    /// Used to gate the `y`/Esc shortcuts.
    pub(crate) fn has_any_selection(&self) -> bool {
        self.text_selection.is_some() || !self.extra_selections.is_empty()
    }

    /// Whether pressing `y` right now would actually copy anything: either
    /// an active selection region, or — with no selection — the whole
    /// content of whichever of the Main (Request JSON) / Response panels
    /// currently has focus (see `input::whole_panel_text`). Drives the
    /// footer's copy hint so it never promises a no-op `y`.
    pub(crate) fn can_copy(&self) -> bool {
        self.has_any_selection() || self.whole_panel_text(self.focus).is_some()
    }

    /// Drop every selection region (the active one and all additional
    /// Alt+Click+Drag ones) — used whenever the underlying panel content is
    /// about to change (a fresh response, a different tab, a different list
    /// entry) so a highlight never lingers over stale content.
    pub(crate) fn clear_selections(&mut self) {
        self.text_selection = None;
        self.extra_selections.clear();
    }

    pub(crate) fn begin_request(&mut self) {
        self.response.lock().unwrap().begin();
    }

    pub(crate) fn run_entry(&mut self, col_idx: usize) {
        if self.collections.get(col_idx).is_none() {
            return;
        }
        let env = self.effective_env(col_idx);
        // Block sending while secrets referenced by this request are still
        // loading (or failed to load).
        let blocking = request::pending_request_keys(&self.collections[col_idx], env.as_ref());
        if !blocking.is_empty() {
            self.status = Some(Status::WaitingSecrets(blocking));
            return;
        }
        self.status = None;
        self.resp_scroll = 0;
        // A fresh response is coming; any selection painted over the old
        // one would be stale.
        self.clear_selections();
        self.pending_autoscroll = None;
        self.begin_request();
        if let Some(rx) = request::run_collection(
            &self.collections[col_idx],
            env.as_ref(),
            self.response.clone(),
        ) {
            self.pending_captures.push(rx);
        }
    }

    /// "Run All" (Alt+F5): run every request in the collection, in order, in
    /// one Hurl execution — the TUI equivalent of the CLI's batch mode, so
    /// captures and the cookie jar chain across the whole run automatically.
    pub(crate) fn run_all_entries(&mut self, col_idx: usize) {
        let Some(col) = self.collections.get(col_idx) else {
            return;
        };
        if col.entries.is_empty() {
            return;
        }
        let env = self.effective_env(col_idx);
        // Block while any request in the collection references a secret
        // that's still loading (or failed) — the same guard as a single run,
        // just checked across every entry instead of only the selected one.
        let blocking = request::pending_request_keys_all(col, env.as_ref());
        if !blocking.is_empty() {
            self.status = Some(Status::WaitingSecrets(blocking));
            return;
        }
        self.status = None;
        self.resp_scroll = 0;
        // A fresh response is coming; any selection painted over the old
        // one would be stale.
        self.clear_selections();
        self.pending_autoscroll = None;
        self.begin_request();
        // Mark every entry as "in progress" immediately (not just when the
        // background thread finishes) so the Requests list shows a live
        // indicator for the whole duration of the run.
        for entry in self.collections[col_idx].entries.iter_mut() {
            entry.last_run = RunStatus::Running;
        }
        if let Some(rx) = request::run_all_entries(
            &self.collections[col_idx],
            env.as_ref(),
            self.response.clone(),
        ) {
            self.pending_batch_runs.push(rx);
        }
    }

    /// Drain background secret-resolution results and apply them to the
    /// matching Global Environment, rebuilding affected request previews.
    pub(crate) fn poll_env_updates(&mut self) {
        request::drain_env_updates(
            &mut self.pending_env,
            &mut self.global_envs,
            &mut self.collections,
        );
    }

    /// Drain completed response captures into their collections so subsequent
    /// requests can substitute the captured values.
    pub(crate) fn poll_capture_updates(&mut self) {
        request::drain_capture_updates(&mut self.pending_captures, &mut self.collections);
    }

    /// Drain completed "Run All" passes: merge captured values into the
    /// collection (so later single-entry runs benefit too), stamp each
    /// entry's pass/fail marker for the Requests list, remember each
    /// entry's own response (`HurlEntry::last_response`) so the Response
    /// pane shows the right one regardless of which entry is selected, and
    /// post a Passed/Failed/Total summary to the status bar.
    pub(crate) fn poll_batch_run_updates(&mut self) {
        if self.pending_batch_runs.is_empty() {
            return;
        }
        let mut still = Vec::new();
        for rx in std::mem::take(&mut self.pending_batch_runs) {
            let mut disconnected = false;
            loop {
                match rx.try_recv() {
                    Ok(update) => {
                        if let Some(col) =
                            self.collections.iter_mut().find(|c| c.id == update.col_id)
                        {
                            for (k, v) in &update.captures {
                                col.captures.insert(k.clone(), v.clone());
                            }
                            col.invalidate_request_json();
                            let mut passed = 0usize;
                            let mut failed = 0usize;
                            for ((entry, result), response) in col
                                .entries
                                .iter_mut()
                                .zip(update.results.iter())
                                .zip(update.responses.iter())
                            {
                                entry.last_run = match result {
                                    Some(true) => RunStatus::Passed,
                                    Some(false) => RunStatus::Failed,
                                    // The runner never reached this entry (batch
                                    // stopped early) — back to "hasn't run".
                                    None => RunStatus::NotRun,
                                };
                                // Only overwrite when this pass actually reached
                                // the entry — an unreached entry keeps whatever
                                // response it last actually received.
                                if let Some(response) = response {
                                    entry.last_response = Some(response.clone());
                                }
                                match result {
                                    Some(true) => passed += 1,
                                    Some(false) => failed += 1,
                                    None => {}
                                }
                            }
                            let total = passed + failed;
                            if total > 0 {
                                self.status = Some(Status::CollectionRunSummary {
                                    passed,
                                    failed,
                                    total,
                                });
                            }
                        }
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
            if !disconnected {
                still.push(rx);
            }
        }
        self.pending_batch_runs = still;
    }

    /// Rebuild the JSON preview buffer for the selected entry if it is stale.
    /// The buffer holds the request in its RAW (unsubstituted) form — the editor
    /// shows the original `{{ VAR }}` text; the preview substitutes at draw time.
    pub(crate) fn refresh_json(&mut self, col_idx: usize) {
        let Some(col) = self.collections.get(col_idx) else {
            return;
        };
        let entry_idx = col.selected_entry;
        if col.entries.is_empty() || col.request_json_for == Some(entry_idx) {
            return;
        }
        let json = build_request_json(&col.entries[entry_idx]);
        let col = &mut self.collections[col_idx];
        col.request_json_buf = json;
        col.request_json_for = Some(entry_idx);
    }

    /// Commit a prompt's edited text. `keep_secret` only matters for
    /// [`PromptKind::EnvValue`] on a variable sourced from a secret provider: when
    /// `false`, the user has declared the edited value is no longer sensitive,
    /// so it is written to the persisted source and the variable is downgraded
    /// to a plain literal.
    pub(crate) fn commit_prompt_with_secrecy(
        &mut self,
        kind: PromptKind,
        text: String,
        keep_secret: bool,
    ) {
        match kind {
            PromptKind::BaseUrl => {
                self.vars.base_url = text;
                self.save_state();
            }
            PromptKind::RenameTab(i) => {
                if !text.trim().is_empty()
                    && let Some(c) = self.collections.get_mut(i)
                {
                    c.name = text.trim().to_string();
                    self.save_state();
                }
            }
            PromptKind::EnvValue(env_id, vi) => {
                let mut pending_secret: Option<PendingSecret> = None;
                if let Some(env) = self.global_envs.iter_mut().find(|e| e.id == env_id)
                    && let Some(var) = env.vars.get_mut(vi)
                {
                    // A user-entered value is concrete: record it (flagging
                    // modified when it differs from the loaded value), honouring
                    // the "still secret?" checkbox for provider-sourced vars.
                    // If the new text now looks like a `{{ op://… }}` /
                    // `{{ ssm:… }}` reference it is reclassified as such and
                    // queued for background resolution, exactly as if it had
                    // been typed into a freshly-loaded `.vars` file.
                    pending_secret = var.set_user_value_secrecy(text, keep_secret, vi);
                }
                // Rebuild every request preview that might use this environment
                // (linked or active-global) so the new value flows in.
                for col in &mut self.collections {
                    col.invalidate_request_json();
                }
                if let Some(secret) = pending_secret {
                    self.pending_env
                        .push(spawn_resolution(env_id, vec![secret]));
                }
            }
            PromptKind::RenameEnv(env_id) => {
                if !text.trim().is_empty()
                    && let Some(env) = self.global_envs.iter_mut().find(|e| e.id == env_id)
                {
                    env.name = text.trim().to_string();
                    self.save_state();
                }
            }
            PromptKind::RenameNewEnv => {
                if let Some((mut env, pending)) = self.pending_collision_env.take() {
                    let name = text.trim();
                    env.name = if name.is_empty() {
                        env.name
                    } else {
                        name.to_string()
                    };
                    let env_id = env.id;
                    self.global_envs.push(env);
                    if !pending.is_empty() {
                        self.pending_env.push(spawn_resolution(env_id, pending));
                    }
                    self.save_state();
                }
            }
            PromptKind::Raw(ci) => {
                // Reparse the edited Hurl text back into the entry; on success
                // this can change any field (Raw Mode exposes everything,
                // including query_params/form_fields/cookies/basic_auth/
                // expected_status, which the Edit Request wizard hides). On
                // failure, reopen the overlay with the user's text intact so
                // they can fix it.
                let entries = crate::hurl::parse_hurl(&text);
                if entries.len() != 1 {
                    let s = Strings::for_language(&self.language);
                    self.status = Some(Status::Error(s.invalid_hurl.to_string()));
                    self.overlay = Some(Overlay::Prompt {
                        kind: PromptKind::Raw(ci),
                        editor: Editor::new(&text, true),
                        title: s.entry_raw_hurl.to_string(),
                        mask: false,
                        reset_to: None,
                        secret_intact: false,
                        secret_checkbox: None,
                    });
                    return;
                }
                let mut parsed = entries.into_iter().next().unwrap();
                if let Some(col) = self.collections.get_mut(ci) {
                    let ei = col.selected_entry;
                    if let Some(entry) = col.entries.get_mut(ei) {
                        let changed = entry.title != parsed.title
                            || entry.method != parsed.method
                            || entry.url != parsed.url
                            || entry.headers != parsed.headers
                            || entry.basic_auth != parsed.basic_auth
                            || entry.form_fields != parsed.form_fields
                            || entry.query_params != parsed.query_params
                            || entry.cookies != parsed.cookies
                            || entry.body != parsed.body
                            || entry.expected_status != parsed.expected_status
                            || entry.captures != parsed.captures
                            || entry.asserts != parsed.asserts;
                        if changed {
                            // Reparsed entries never carry `user_added` (it is
                            // UI-only and never written to Hurl text); preserve
                            // it from the entry being replaced.
                            parsed.user_added = entry.user_added;
                            parsed.modified = true;
                            *entry = parsed;
                        }
                    }
                    col.invalidate_request_json();
                    col.sync_folder_to_selected();
                }
                self.save_state();
            }
            PromptKind::RawJson(ci) => {
                // Reparse the edited JSON text (the same shape build_request_json
                // produces) back into the entry; on success this can change
                // method/url/basic_auth/headers/cookies/query_params/form_fields/
                // body — everything that view exposes — while everything it
                // doesn't (title/expected_status/captures/asserts/user_added)
                // is carried over unchanged. On failure, reopen the overlay
                // with the user's text intact so they can fix it.
                let Some(col) = self.collections.get(ci) else {
                    return;
                };
                let ei = col.selected_entry;
                let Some(base) = col.entries.get(ei).cloned() else {
                    return;
                };
                let mut parsed = match crate::request::apply_request_json(&base, &text) {
                    Ok(parsed) => parsed,
                    Err(_) => {
                        let s = Strings::for_language(&self.language);
                        self.status = Some(Status::Error(s.invalid_request_json.to_string()));
                        self.overlay = Some(Overlay::Prompt {
                            kind: PromptKind::RawJson(ci),
                            editor: Editor::new(&text, true),
                            title: s.entry_raw_json.to_string(),
                            mask: false,
                            reset_to: None,
                            secret_intact: false,
                            secret_checkbox: None,
                        });
                        return;
                    }
                };
                if let Some(col) = self.collections.get_mut(ci) {
                    if let Some(entry) = col.entries.get_mut(ei) {
                        let changed = entry.method != parsed.method
                            || entry.url != parsed.url
                            || entry.headers != parsed.headers
                            || entry.basic_auth != parsed.basic_auth
                            || entry.form_fields != parsed.form_fields
                            || entry.query_params != parsed.query_params
                            || entry.cookies != parsed.cookies
                            || entry.body != parsed.body;
                        if changed {
                            parsed.modified = true;
                            *entry = parsed;
                        }
                    }
                    col.invalidate_request_json();
                    col.sync_folder_to_selected();
                }
                self.save_state();
            }
            PromptKind::FilePath(action) => self.save_as_path(action, text.trim()),
            PromptKind::WorkspaceSaveName => self.finish_workspace_save(text),
        }
    }

    /// Insert (or replace, by key) a hand-added variable into the Global
    /// Environment `env_id`. An empty key is ignored; the key and value are
    /// trimmed. A no-op if `env_id` doesn't match any Global Environment
    /// (shouldn't happen — the add-var form is only ever opened for an
    /// existing entry's popup).
    pub(crate) fn add_env_var(&mut self, env_id: u64, key: String, value: String) {
        let key = key.trim().to_string();
        if key.is_empty() {
            return;
        }
        let var = EnvVar::user(key.clone(), value.trim().to_string());
        if let Some(env) = self.global_envs.iter_mut().find(|e| e.id == env_id) {
            // Replace an existing entry of the same name, else append.
            match env.vars.iter_mut().find(|e| e.key == key) {
                Some(existing) => *existing = var,
                None => env.vars.push(var),
            }
        }
        for col in &mut self.collections {
            col.invalidate_request_json();
        }
    }

    /// Number of new or modified requests in collection `ci` (for the save
    /// confirmation and the list markers).
    pub(crate) fn changed_request_count(&self, ci: usize) -> usize {
        self.collections
            .get(ci)
            .map(|c| {
                c.entries
                    .iter()
                    .filter(|e| e.user_added || e.modified)
                    .count()
            })
            .unwrap_or(0)
    }

    /// Number of new or modified variables in Global Environment `env_id`.
    pub(crate) fn changed_env_count(&self, env_id: u64) -> usize {
        self.global_envs
            .iter()
            .find(|e| e.id == env_id)
            .map(|e| e.vars.iter().filter(|v| v.user_added || v.modified).count())
            .unwrap_or(0)
    }

    /// `true` when any Global Environment holds a secret (1Password / SSM) the
    /// user has edited. Such edits are kept only in memory — writing them to
    /// the plaintext state file would leak the secret — so they are lost when
    /// the app closes.
    pub(crate) fn has_unsaved_secret_changes(&self) -> bool {
        self.global_envs
            .iter()
            .any(|e| e.vars.iter().any(|v| v.is_secret_source() && v.modified))
    }

    /// Clear a collection's "new" (user-added) and "modified" request
    /// markers — called whenever its `.hurl` file is (re)written to disk,
    /// whether by a local Save or a git push, so both paths agree on what
    /// "saved" means.
    fn mark_collection_saved(&mut self, ci: usize) {
        for e in &mut self.collections[ci].entries {
            e.user_added = false;
            e.modified = false;
        }
    }

    /// Clear a Global Environment's "new"/"modified" var markers and reset
    /// each var's `original_value` to the just-saved value — called whenever
    /// its `.vars` file is (re)written to disk, whether by a local Save or a
    /// git push. A no-op if `env_id` doesn't match any Global Environment.
    fn mark_env_saved(&mut self, env_id: u64) {
        if let Some(env) = self.global_envs.iter_mut().find(|e| e.id == env_id) {
            for v in &mut env.vars {
                v.user_added = false;
                v.modified = false;
                v.original_value = v.value.clone();
            }
        }
    }

    pub(crate) fn do_file_action(&mut self, action: FileAction, path: &str) {
        if path.is_empty() {
            return;
        }
        match action {
            FileAction::SaveRequest => {
                let col = &self.collections[self.active_tab];
                let Some(entry) = col.entries.get(col.selected_entry) else {
                    self.status = Some(Status::NoResponse);
                    return;
                };
                let r = serde_json::to_string_pretty(entry)
                    .map_err(|e| e.to_string())
                    .and_then(|j| std::fs::write(path, j).map_err(|e| e.to_string()));
                self.report(r.map(|_| ()));
            }
            FileAction::LoadRequest => {
                let r = std::fs::read_to_string(path)
                    .map_err(|e| e.to_string())
                    .and_then(|t| serde_json::from_str::<HurlEntry>(&t).map_err(|e| e.to_string()));
                match r {
                    Ok(entry) => {
                        let req = &mut self.collections[0];
                        req.entries.push(entry);
                        req.selected_entry = req.entries.len() - 1;
                        req.invalidate_request_json();
                        req.sync_folder_to_selected();
                        self.active_tab = 0;
                        self.focus = Pane::List;
                        self.status = Some(Status::Loaded);
                        self.save_state();
                    }
                    Err(e) => self.status = Some(Status::Error(e)),
                }
            }
            FileAction::OpenCollection => match std::fs::read_to_string(path) {
                Ok(content) => {
                    let name = collection_name_from_path(path, "collection");
                    self.load_collection_text(name, &content, Some(PathBuf::from(path)));
                }
                Err(e) => self.status = Some(Status::Error(e.to_string())),
            },
            FileAction::SaveCollection => {
                let ci = self.active_tab;
                let text = self.collections[ci].to_hurl();
                match std::fs::write(path, text) {
                    Ok(()) => {
                        self.collections[ci].path = Some(PathBuf::from(path));
                        // The requests are now part of the saved file — clear the
                        // "new" (user-added) and "modified" markers.
                        self.mark_collection_saved(ci);
                        self.save_state();
                        self.status = Some(Status::Saved);
                    }
                    Err(e) => self.status = Some(Status::Error(e.to_string())),
                }
            }
            FileAction::LoadEnv => match std::fs::read_to_string(path) {
                Ok(content) => {
                    let name = env_name_from_path(path, "environment");
                    self.load_environment_text(name, &content, Some(PathBuf::from(path)), None);
                }
                Err(e) => self.status = Some(Status::Error(e.to_string())),
            },
            FileAction::SaveEnv => {
                let Some(env_id) = self.current_env_id() else {
                    self.status = Some(Status::NotEnvironment);
                    return;
                };
                let Some(text) = self
                    .global_envs
                    .iter()
                    .find(|e| e.id == env_id)
                    .map(|e| e.to_vars_text())
                else {
                    self.status = Some(Status::NotEnvironment);
                    return;
                };
                match std::fs::write(path, text) {
                    Ok(()) => {
                        if let Some(env) = self.global_envs.iter_mut().find(|e| e.id == env_id) {
                            env.path = Some(PathBuf::from(path));
                        }
                        // Now part of the saved file — clear the "new"/"modified"
                        // markers and treat the current values as the loaded ones.
                        self.mark_env_saved(env_id);
                        self.save_state();
                        self.status = Some(Status::Saved);
                    }
                    Err(e) => self.status = Some(Status::Error(e.to_string())),
                }
            }
            FileAction::SaveResponse => {
                let body = self.response.lock().unwrap().body.clone();
                if body.is_empty() {
                    self.status = Some(Status::NoResponse);
                    return;
                }
                let r = std::fs::write(path, body.as_ref()).map_err(|e| e.to_string());
                self.report(r);
            }
            FileAction::PickFormFile(i) => {
                if let Some(mut form) = self.parked_wizard.take() {
                    if let Some(row) = form.form_fields.get_mut(i) {
                        row.value = Editor::new(path, false);
                        // Auto-infer the content type from the picked file's
                        // extension and set the dropdown to it; the user can
                        // still override it afterwards.
                        let inferred = infer_content_type(path).unwrap_or("");
                        row.ctype = Editor::new(inferred, false);
                    }
                    form.focus = NewField::FormField(i, FormCol::Value);
                    self.overlay = Some(Overlay::NewRequest(form));
                }
            }
            // Never reached: `OpenWorkspace` confirms on `Space` over the
            // current directory (see `Overlay::Browser`'s Enter/Space
            // handling in `input.rs`), not on selecting a file with Enter,
            // so this action never flows through `do_file_action`.
            FileAction::OpenWorkspace => {}
            // Same as `OpenWorkspace` above: the destination folder is
            // confirmed with `Space`, handled directly in `input.rs` via
            // `workspace_save_pick_folder`.
            FileAction::SaveWorkspaceChooseFolder => {}
        }
    }

    pub(crate) fn report(&mut self, r: Result<(), String>) {
        self.status = Some(match r {
            Ok(()) => Status::Saved,
            Err(e) => Status::Error(e),
        });
    }

    /// Load Hurl `content` as a new collection tab. `path` is the local source
    /// file (None for remote sources, so "Save Collection" prompts for a
    /// location). Sets a status and returns false if it isn't a collection.
    pub(crate) fn load_collection_text(
        &mut self,
        name: String,
        content: &str,
        path: Option<PathBuf>,
    ) -> bool {
        let entries = crate::postman::parse_collection(content);
        if entries.is_empty() {
            self.status = Some(Status::NotCollection);
            return false;
        }
        let mut col = Collection::new(name, entries);
        col.path = path;
        self.collections.push(col);
        self.active_tab = self.collections.len() - 1;
        self.focus = Pane::List;
        self.status = Some(Status::Loaded);
        self.save_state();
        true
    }

    /// Create a new Workspace tab bound to `root` (a chosen folder, not yet
    /// any particular file inside it) and immediately open the file-tree
    /// popup so the user picks their first collection right away — see
    /// `crate::workspace`.
    pub(crate) fn confirm_workspace_root(&mut self, root: PathBuf) {
        let name = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.to_string_lossy().into_owned());
        let mut col = Collection::new(name, Vec::new());
        col.workspace_root = Some(root.clone());
        self.collections.push(col);
        let ci = self.collections.len() - 1;
        self.active_tab = ci;
        self.focus = Pane::List;
        self.overlay = Some(Overlay::WorkspacePicker(WorkspacePickerState::new(
            ci, root, true,
        )));
    }

    /// Same as `confirm_workspace_root`, but for a Workspace whose (filtered)
    /// files were just downloaded from git into a throwaway repo directory —
    /// `root` is that directory, reused in place as the tab's
    /// `workspace_root` exactly like any locally chosen folder (its checked-
    /// out files already sit at the right relative paths, and its `.git`
    /// folder is hidden from the picker like any dot-prefixed entry). `name`
    /// is derived from the repo URL rather than the (meaningless, temp-dir)
    /// folder name. Unlike a single collection/environment file load, this
    /// directory is deliberately *not* cleaned up afterwards — the tab keeps
    /// reading from it live for as long as it stays open (and, per the
    /// undo-close history, potentially reopened later in the same session).
    pub(crate) fn confirm_workspace_root_from_git(
        &mut self,
        root: PathBuf,
        name: String,
        origin: Option<WorkspaceGitOrigin>,
    ) {
        let mut col = Collection::new(name, Vec::new());
        col.workspace_root = Some(root.clone());
        col.workspace_downloaded_from_git = true;
        col.workspace_git_origin = origin;
        self.collections.push(col);
        let ci = self.collections.len() - 1;
        self.active_tab = ci;
        self.focus = Pane::List;
        self.overlay = Some(Overlay::WorkspacePicker(WorkspacePickerState::new(
            ci, root, true,
        )));
    }

    /// "Save Workspace…" (File → Save menu): copy the active tab's
    /// Workspace files to a new, permanent folder chosen by the user,
    /// defaulting the new name to the tab's current name. A no-op (with
    /// [`Status::NotWorkspace`]) if the active tab isn't Workspace-bound.
    pub(crate) fn begin_save_workspace_as(&mut self) {
        let ci = self.active_tab;
        let Some(col) = self.collections.get(ci) else {
            return;
        };
        let Some(root) = col.workspace_root.clone() else {
            self.status = Some(Status::NotWorkspace);
            return;
        };
        self.pending_workspace_save = Some(PendingWorkspaceSave {
            source_root: root,
            default_name: col.name.clone(),
            target: WorkspaceSaveTarget::ExistingTab(ci),
            dest_dir: None,
        });
        self.open_browser(FileAction::SaveWorkspaceChooseFolder);
    }

    /// The destination-folder browser (see
    /// [`FileAction::SaveWorkspaceChooseFolder`]) confirmed `dir` — move on
    /// to naming the workspace (defaulting to `PendingWorkspaceSave::default_name`).
    pub(crate) fn workspace_save_pick_folder(&mut self, dir: PathBuf) {
        let Some(pending) = self.pending_workspace_save.as_mut() else {
            return;
        };
        pending.dest_dir = Some(dir);
        let default_name = pending.default_name.clone();
        let s = Strings::for_language(&self.language);
        self.overlay = Some(Overlay::Prompt {
            kind: PromptKind::WorkspaceSaveName,
            editor: Editor::new(&default_name, false),
            title: s.workspace_save_name_prompt.to_string(),
            mask: false,
            reset_to: None,
            secret_intact: false,
            secret_checkbox: None,
        });
    }

    /// Abandon an in-progress "Save Workspace" without completing the copy.
    /// For a just-downloaded git Workspace ([`WorkspaceSaveTarget::NewGitTab`])
    /// this falls back to keeping the download temporary (the pre-existing
    /// behaviour) rather than losing it outright; a no-op for an
    /// already-open tab ([`WorkspaceSaveTarget::ExistingTab`]), which is
    /// simply left as it was.
    fn abandon_pending_workspace_save(&mut self, pending: PendingWorkspaceSave) {
        if let WorkspaceSaveTarget::NewGitTab { origin } = pending.target {
            self.confirm_workspace_root_from_git(pending.source_root, pending.default_name, origin);
        }
    }

    /// Esc from the destination-folder browser — see
    /// `abandon_pending_workspace_save`.
    pub(crate) fn cancel_workspace_save(&mut self) {
        if let Some(pending) = self.pending_workspace_save.take() {
            self.abandon_pending_workspace_save(pending);
        }
    }

    /// Commit the [`PromptKind::WorkspaceSaveName`] prompt: copy
    /// `pending_workspace_save`'s files to `dest_dir/<name>` and either
    /// rebind the existing tab or create a brand new one, depending on
    /// `WorkspaceSaveTarget`. An empty name, a destination that already
    /// exists, or a copy failure all fall back to
    /// `abandon_pending_workspace_save` rather than leaving the app with no
    /// tab at all for a just-downloaded git Workspace.
    pub(crate) fn finish_workspace_save(&mut self, name: String) {
        let Some(pending) = self.pending_workspace_save.take() else {
            return;
        };
        let name = name.trim().to_string();
        let Some(dest_dir) = pending.dest_dir.clone() else {
            return;
        };
        if name.is_empty() {
            self.abandon_pending_workspace_save(pending);
            return;
        }
        let target_path = dest_dir.join(&name);
        let bad_target = target_path.exists()
            || target_path.starts_with(&pending.source_root)
            || pending.source_root.starts_with(&target_path);
        if bad_target {
            self.status = Some(Status::WorkspaceSaveFailed(
                target_path.display().to_string(),
            ));
            self.abandon_pending_workspace_save(pending);
            return;
        }
        match crate::workspace::copy_dir_all(&pending.source_root, &target_path) {
            Ok(()) => match pending.target {
                WorkspaceSaveTarget::ExistingTab(idx) => {
                    let Some(col) = self.collections.get(idx) else {
                        return;
                    };
                    let was_temp_download = col.workspace_downloaded_from_git;
                    let old_root = col.workspace_root.clone();
                    let relative = match (&col.path, &old_root) {
                        (Some(p), Some(root)) => p.strip_prefix(root).ok().map(|r| r.to_path_buf()),
                        _ => None,
                    };
                    if let Some(col) = self.collections.get_mut(idx) {
                        let selected = relative
                            .map(|rel| target_path.join(rel))
                            .filter(|p| p.exists());
                        col.workspace_root = Some(target_path.clone());
                        col.workspace_downloaded_from_git = false;
                        col.workspace_git_origin = None;
                        col.name = name;
                        match selected {
                            Some(path) => match std::fs::read_to_string(&path) {
                                Ok(content) => {
                                    col.entries = crate::postman::parse_collection(&content);
                                    col.path = Some(path);
                                    col.selected_entry = 0;
                                    col.invalidate_request_json();
                                    col.sync_folder_to_selected();
                                }
                                Err(_) => col.path = None,
                            },
                            None => col.path = None,
                        }
                    }
                    if was_temp_download && let Some(root) = &old_root {
                        git_remote::cleanup(root);
                    }
                    self.status = Some(Status::WorkspaceSaved);
                    self.save_state();
                }
                WorkspaceSaveTarget::NewGitTab { .. } => {
                    git_remote::cleanup(&pending.source_root);
                    self.confirm_workspace_root(target_path);
                    self.status = Some(Status::WorkspaceSaved);
                    self.save_state();
                }
            },
            Err(e) => {
                self.status = Some(Status::WorkspaceSaveFailed(e.to_string()));
            }
        }
    }

    /// Load `path` (a file chosen from the `WorkspacePicker`) into the
    /// Workspace tab at `collection_idx`, replacing its entries/path in
    /// place — the tab itself, and its position in `collections`, doesn't
    /// change, only which file it currently shows. The tab's own `name` is
    /// deliberately left untouched here: it's persistent (renameable with
    /// F2, defaulting to the folder/repo name) and must NOT change just
    /// because the user picked a different collection within the Workspace
    /// — only the Requests-list panel title does that, deriving its display
    /// name straight from `path` (see `draw::draw_collection_left`).
    pub(crate) fn load_workspace_file(&mut self, collection_idx: usize, path: PathBuf) {
        let Some(col) = self.collections.get(collection_idx) else {
            return;
        };
        let workspace_root = col.workspace_root.clone();
        let filter = col.workspace_filter_hurl_json;
        match std::fs::read_to_string(&path) {
            Ok(content) => {
                let entries = crate::postman::parse_collection(&content);
                if let Some(col) = self.collections.get_mut(collection_idx) {
                    col.entries = entries;
                    col.selected_entry = 0;
                    col.path = Some(path);
                    col.workspace_root = workspace_root;
                    col.workspace_filter_hurl_json = filter;
                    col.invalidate_request_json();
                    col.sync_folder_to_selected();
                }
                self.active_tab = collection_idx;
                self.focus = Pane::List;
                self.status = Some(Status::Loaded);
                self.save_state();
            }
            Err(e) => self.status = Some(Status::Error(e.to_string())),
        }
    }

    /// Opens (or reopens) the `WorkspacePicker` popup scoped to the active
    /// tab's bound folder — a no-op if the active tab isn't Workspace-bound.
    /// Used by both the global `w` key and the auto-open-on-empty-tab logic.
    pub(crate) fn open_workspace_picker_for_active_tab(&mut self) {
        let ci = self.active_tab;
        let Some(col) = self.collections.get(ci) else {
            return;
        };
        let Some(root) = col.workspace_root.clone() else {
            return;
        };
        let filter = col.workspace_filter_hurl_json;
        self.overlay = Some(Overlay::WorkspacePicker(WorkspacePickerState::new(
            ci, root, filter,
        )));
    }

    /// Called once per frame (see `draw`): if the active tab is Workspace-
    /// bound but has no collection file chosen yet (a freshly-created tab,
    /// or one whose last file vanished on restore) — and the user hasn't
    /// already cancelled this same prompt — pop the file picker open
    /// automatically instead of leaving the user staring at a blank list
    /// with no obvious next step. No-op whenever any overlay is already
    /// showing, so it never steals focus from something else.
    pub(crate) fn maybe_auto_open_workspace_picker(&mut self) {
        if self.overlay.is_some() {
            return;
        }
        let ci = self.active_tab;
        let Some(col) = self.collections.get(ci) else {
            return;
        };
        if col.workspace_root.is_some()
            && col.path.is_none()
            && !col.workspace_auto_prompt_dismissed
        {
            self.open_workspace_picker_for_active_tab();
        }
    }

    /// Pop the next queued [`PendingWorkspaceReload`] (see `apply_persisted`)
    /// into an [`Overlay::WorkspaceReloadConfirm`] popup, if any and no
    /// overlay is already showing — called once right after restoring
    /// persisted state, and again each time one is resolved (Yes/No), so
    /// several affected tabs are prompted one at a time rather than all at
    /// once.
    pub(crate) fn open_next_pending_workspace_reload(&mut self) {
        if self.overlay.is_some() {
            return;
        }
        let Some((idx, reload)) = self.pending_workspace_reloads.pop_front() else {
            return;
        };
        self.overlay = Some(Overlay::WorkspaceReloadConfirm {
            idx,
            reload: Box::new(reload),
            sel: 0,
        });
    }

    /// Start a background attempt to redownload tab `idx`'s Workspace from
    /// git, pinned to `reload`'s recorded commit — see
    /// `remote::spawn_workspace_redownload`.
    pub(crate) fn start_workspace_redownload(
        &mut self,
        idx: usize,
        reload: crate::persistence::PendingWorkspaceReload,
    ) {
        let rx = spawn_workspace_redownload(reload.origin);
        self.workspace_redownload_rx = Some((idx, reload.relative_selected_path, rx));
        self.overlay = Some(Overlay::WorkspaceReloadLoading { idx });
    }

    /// Poll an in-flight Workspace redownload (called each frame). On
    /// success, rebinds tab `idx` to the freshly downloaded folder and
    /// re-selects the previously-active file if it still exists there; on
    /// failure — most often because the exact recorded commit is no longer
    /// reachable on the remote (history rewritten, branch/tag deleted) —
    /// reports that clearly. Either way, a hint to save the Workspace
    /// locally is appended so the user can avoid relying on `/tmp` again.
    pub(crate) fn poll_workspace_redownload_updates(&mut self) {
        let Some((idx, relative_selected_path, rx)) = self.workspace_redownload_rx.take() else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(repo)) => {
                if let Some(col) = self.collections.get_mut(idx) {
                    let selected = relative_selected_path
                        .map(|rel| repo.join(rel))
                        .filter(|p| p.exists());
                    col.workspace_root = Some(repo);
                    col.workspace_downloaded_from_git = true;
                    col.workspace_auto_prompt_dismissed = false;
                    match selected {
                        Some(path) => match std::fs::read_to_string(&path) {
                            Ok(content) => {
                                col.entries = crate::postman::parse_collection(&content);
                                col.path = Some(path);
                                col.selected_entry = 0;
                                col.invalidate_request_json();
                                col.sync_folder_to_selected();
                            }
                            Err(_) => col.path = None,
                        },
                        None => col.path = None,
                    }
                }
                self.status = Some(Status::WorkspaceReloaded);
                self.overlay = None;
                self.save_state();
                self.open_next_pending_workspace_reload();
            }
            Ok(Err(e)) => {
                self.status = Some(Status::WorkspaceReloadFailed(e));
                self.overlay = None;
                self.open_next_pending_workspace_reload();
            }
            Err(mpsc::TryRecvError::Empty) => {
                // Still running — put it back for the next frame's poll.
                self.workspace_redownload_rx = Some((idx, relative_selected_path, rx));
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.overlay = None;
                self.open_next_pending_workspace_reload();
            }
        }
    }

    /// Load `.vars`-style `content` into the Global Environments list,
    /// resolving any secrets in the background. Never attached directly to a
    /// collection — see `set_linked_env` for that (linking is a separate,
    /// explicit action). `path`/`git_origin` record where the environment
    /// came from, so "Save Environment" and future reloads target the right
    /// place. If an environment with the same name already exists, an
    /// [`Overlay::EnvCollision`] popup is opened asking the user to Replace /
    /// Keep both / Abort / Rename then add, and `None` is returned (the load
    /// is only completed once the popup is resolved). Sets a status and
    /// returns the loaded environment's id immediately when there's no name
    /// clash, or `None` if it isn't an environment file.
    pub(crate) fn load_environment_text(
        &mut self,
        name: String,
        content: &str,
        path: Option<PathBuf>,
        git_origin: Option<GitOrigin>,
    ) -> Option<u64> {
        if !looks_like_env(content) {
            self.status = Some(Status::NotEnvironment);
            return None;
        }
        let (mut env, pending) = parse_vars_pending(name, content);
        env.path = path;
        env.git_origin = git_origin;
        if let Some(existing) = self.global_envs.iter().find(|e| e.name == env.name) {
            let existing_id = existing.id;
            self.overlay = Some(Overlay::EnvCollision(Box::new(EnvCollision {
                new_env: env,
                pending,
                existing_id,
                sel: 0,
            })));
            self.status = None;
            return None;
        }
        let id = env.id;
        self.global_envs.push(env);
        for col in &mut self.collections {
            col.invalidate_request_json();
        }
        if !pending.is_empty() {
            self.pending_env.push(spawn_resolution(id, pending));
        }
        self.status = Some(Status::Loaded);
        self.save_state();
        Some(id)
    }

    /// The Global Environment id that "Save Environment" / secret-reload
    /// actions currently target: the selected row in the Global Environments
    /// list, or (if open) the environment shown in the entries popup.
    pub(crate) fn current_env_id(&self) -> Option<u64> {
        if let Some(Overlay::EnvPopup(p)) = &self.overlay {
            return Some(p.env_id);
        }
        self.global_envs.get(self.global_env_idx).map(|e| e.id)
    }

    /// Re-attempt resolving the currently-selected variable (env var /
    /// 1Password / SSM) in the open environment-entries popup that previously
    /// failed to load, without requiring a full environment reload. A no-op
    /// if no popup is open, or the selected variable didn't fail.
    pub(crate) fn reload_selected_env_var(&mut self) {
        let Some(Overlay::EnvPopup(popup)) = &self.overlay else {
            return;
        };
        let (env_id, vi) = (popup.env_id, popup.idx);
        let mut pending_secret = None;
        let mut key = None;
        if let Some(env) = self.global_envs.iter_mut().find(|e| e.id == env_id)
            && let Some(var) = env.vars.get_mut(vi)
            && var.is_failed()
        {
            key = Some(var.key.clone());
            pending_secret = var.reload(vi);
        }
        let Some(key) = key else { return };
        for col in &mut self.collections {
            col.invalidate_request_json();
        }
        if let Some(secret) = pending_secret {
            self.pending_env
                .push(spawn_resolution(env_id, vec![secret]));
        }
        self.status = Some(Status::EnvVarReloading(key));
    }

    /// Toggle activation of the Global Environment at `idx` in `global_envs`:
    /// activating it deactivates whatever else was active (at most one may
    /// be active at a time); activating shows a status-bar notification.
    /// Deactivating the currently-active one leaves no active Global
    /// Environment.
    pub(crate) fn toggle_activate_env(&mut self, idx: usize) {
        let Some(env) = self.global_envs.get(idx) else {
            return;
        };
        let id = env.id;
        let name = env.name.clone();
        if self.active_env_id == Some(id) {
            self.active_env_id = None;
            self.status = Some(Status::EnvDeactivated(name));
        } else {
            self.active_env_id = Some(id);
            self.status = Some(Status::EnvActivated(name));
        }
        for col in &mut self.collections {
            col.invalidate_request_json();
        }
        self.save_state();
    }

    /// Delete the Global Environment at `idx`: any collection linked to it
    /// becomes unlinked, and it's deactivated if it was active.
    pub(crate) fn delete_global_env(&mut self, idx: usize) {
        if idx >= self.global_envs.len() {
            return;
        }
        let id = self.global_envs[idx].id;
        self.global_envs.remove(idx);
        if self.active_env_id == Some(id) {
            self.active_env_id = None;
        }
        for col in &mut self.collections {
            if col.linked_env_id == Some(id) {
                col.linked_env_id = None;
            }
        }
        self.global_env_idx = self
            .global_env_idx
            .min(self.global_envs.len().saturating_sub(1));
        for col in &mut self.collections {
            col.invalidate_request_json();
        }
        self.save_state();
    }

    /// Link (or, if already linked to it, unlink) Global Environment `env_id`
    /// to collection `ci`. Any number of collections may link the same
    /// environment.
    pub(crate) fn set_linked_env(&mut self, ci: usize, env_id: Option<u64>) {
        if let Some(col) = self.collections.get_mut(ci) {
            col.linked_env_id = if col.linked_env_id == env_id {
                None
            } else {
                env_id
            };
            col.invalidate_request_json();
        }
        self.save_state();
    }

    /// Build the effective, merged [`Environment`] used for substitution in
    /// collection `ci`: the active Global Environment's vars, overridden by
    /// the collection's own Linked Environment's vars on any name collision
    /// (Linked wins). `None` when neither is set.
    pub(crate) fn effective_env(&self, ci: usize) -> Option<Environment> {
        let linked = self
            .collections
            .get(ci)
            .and_then(|c| c.linked_env_id)
            .and_then(|id| self.global_envs.iter().find(|e| e.id == id));
        let active = self
            .active_env_id
            .and_then(|id| self.global_envs.iter().find(|e| e.id == id));
        match (linked, active) {
            (None, None) => None,
            (Some(env), None) | (None, Some(env)) => Some(env.clone()),
            (Some(linked), Some(active)) => {
                let mut merged = active.clone();
                for lv in &linked.vars {
                    match merged.vars.iter_mut().find(|v| v.key == lv.key) {
                        Some(existing) => *existing = lv.clone(),
                        None => merged.vars.push(lv.clone()),
                    }
                }
                merged.id = linked.id;
                merged.name = linked.name.clone();
                Some(merged)
            }
        }
    }

    /// Keys defined in *both* the active collection's linked Environment and
    /// the active Global Environment — per `effective_env`'s merge rule the
    /// linked value always wins, so these keys' Global Environment value is
    /// silently shadowed. Used to flag such substitutions in the Request
    /// viewer with a warning icon so the collision isn't invisible.
    pub(crate) fn shadowed_env_keys(&self, ci: usize) -> std::collections::HashSet<String> {
        let linked = self
            .collections
            .get(ci)
            .and_then(|c| c.linked_env_id)
            .and_then(|id| self.global_envs.iter().find(|e| e.id == id));
        let active = self
            .active_env_id
            .and_then(|id| self.global_envs.iter().find(|e| e.id == id));
        match (linked, active) {
            // A collection linked to the very environment that's also active
            // shadows nothing — the same value would be substituted either way.
            (Some(linked), Some(active)) if linked.id != active.id => linked
                .vars
                .iter()
                .filter(|lv| active.vars.iter().any(|av| av.key == lv.key))
                .map(|lv| lv.key.clone())
                .collect(),
            _ => std::collections::HashSet::new(),
        }
    }

    /// Apply the user's choice on an [`Overlay::EnvCollision`] popup,
    /// resolving the name clash between a freshly-loaded environment and one
    /// already in `global_envs`.
    pub(crate) fn resolve_env_collision(&mut self, collision: EnvCollision) {
        let EnvCollision {
            new_env,
            pending,
            existing_id,
            sel,
        } = collision;
        match sel {
            // Replace existing: keep the existing entry's id/path/git_origin
            // (so links and "Save Environment" keep working) but swap in the
            // freshly-loaded vars.
            0 => {
                if let Some(idx) = self.global_envs.iter().position(|e| e.id == existing_id) {
                    let existing = &self.global_envs[idx];
                    let mut env = new_env;
                    env.id = existing.id;
                    env.path = existing.path.clone();
                    env.git_origin = existing.git_origin.clone();
                    self.global_envs[idx] = env;
                    if !pending.is_empty() {
                        self.pending_env
                            .push(spawn_resolution(existing_id, pending));
                    }
                    for col in &mut self.collections {
                        col.invalidate_request_json();
                    }
                    self.save_state();
                }
            }
            // Keep both: add the new environment as a separate entry with
            // its own (already-fresh) id, duplicate name and all.
            1 => {
                let env_id = new_env.id;
                self.global_envs.push(new_env);
                if !pending.is_empty() {
                    self.pending_env.push(spawn_resolution(env_id, pending));
                }
                self.save_state();
            }
            // Abort: discard the freshly-loaded environment entirely.
            2 => {}
            // Rename then add: ask for a new name, then add it once committed.
            _ => {
                self.pending_collision_env = Some((new_env, pending));
                let s = Strings::for_language(&self.language);
                self.overlay = Some(Overlay::Prompt {
                    kind: PromptKind::RenameNewEnv,
                    editor: Editor::blank(),
                    title: s.env_rename_title.to_string(),
                    mask: false,
                    reset_to: None,
                    secret_intact: false,
                    secret_checkbox: None,
                });
            }
        }
    }

    pub(crate) fn open_remote_wizard(&mut self, kind: RemoteKind) {
        self.overlay = Some(Overlay::RemoteGit(Box::new(RemoteWizard::new(
            kind,
            self.recent_git_urls.clone(),
        ))));
    }

    /// Open the "Save Collection to Git…" wizard for the active tab, or show
    /// [`Status::NoGitOrigin`] if it wasn't loaded from git in the first
    /// place (this action is only ever offered for such collections).
    pub(crate) fn open_git_save_wizard(&mut self) {
        let ci = self.active_tab;
        if self.collections[ci].git_origin.is_none() {
            self.status = Some(Status::NoGitOrigin);
            return;
        }
        let env = self.effective_env(ci);
        self.overlay = Some(Overlay::GitSave(Box::new(GitSaveWizard::new(
            ci,
            &self.collections[ci],
            env,
        ))));
    }

    /// Record `url` as the most-recently-used git URL: moved to the front,
    /// deduplicated, and capped to the 10 most recent. Persisted immediately so
    /// it is offered in the "Load from Git" dropdown next time.
    pub(crate) fn remember_git_url(&mut self, url: &str) {
        let url = url.trim();
        if url.is_empty() {
            return;
        }
        self.recent_git_urls.retain(|u| u != url);
        self.recent_git_urls.insert(0, url.to_string());
        self.recent_git_urls.truncate(10);
        self.save_state();
    }

    /// Close the wizard, cleaning up any temp repo it created.
    pub(crate) fn close_remote(&mut self, w: Box<RemoteWizard>) {
        if let Some(repo) = &w.repo {
            git_remote::cleanup(repo);
        }
        self.overlay = None;
    }

    /// Handle a key while the remote-git wizard is open.
    pub(crate) fn on_key_remote(&mut self, mut w: Box<RemoteWizard>, key: KeyEvent) {
        match &mut w.stage {
            RemoteStage::Connect { field, recent_sel } => match key.code {
                // While the recent-URLs dropdown has focus, Esc backs out of it
                // rather than closing the whole wizard.
                KeyCode::Esc if recent_sel.is_some() => *recent_sel = None,
                KeyCode::Esc => return self.close_remote(w),
                KeyCode::Tab | KeyCode::BackTab => {
                    *field = 1 - *field;
                    *recent_sel = None;
                }
                // On the URL field, Down opens (or moves down in) the recent-URLs
                // dropdown instead of jumping to the token field.
                KeyCode::Down if *field == 0 && !w.recent.is_empty() => {
                    *recent_sel = Some(recent_sel.map_or(0, |i| (i + 1).min(w.recent.len() - 1)));
                }
                KeyCode::Up if recent_sel.is_some() => {
                    let i = recent_sel.unwrap();
                    *recent_sel = if i == 0 { None } else { Some(i - 1) };
                }
                KeyCode::Up | KeyCode::Down => {
                    *field = 1 - *field;
                    *recent_sel = None;
                }
                KeyCode::Enter if recent_sel.is_some() => {
                    // Pick the highlighted recent URL and connect immediately,
                    // rather than just populating the field (which would force
                    // the user to press Enter a second time).
                    if let Some(url) = recent_sel.and_then(|i| w.recent.get(i)).cloned() {
                        w.url = Editor::new(&url, false);
                    }
                    *recent_sel = None;
                    if w.url.text().trim().is_empty() {
                        let s = Strings::for_language(&self.language);
                        w.stage = RemoteStage::Error(s.git_url_required.to_string());
                    } else {
                        w.rx = Some(spawn_git_refs(w.url.text(), w.token_opt()));
                        w.stage = RemoteStage::Loading {
                            phase: LoadPhase::Refs,
                        };
                    }
                }
                KeyCode::Enter => {
                    if w.url.text().trim().is_empty() {
                        let s = Strings::for_language(&self.language);
                        w.stage = RemoteStage::Error(s.git_url_required.to_string());
                    } else {
                        w.rx = Some(spawn_git_refs(w.url.text(), w.token_opt()));
                        w.stage = RemoteStage::Loading {
                            phase: LoadPhase::Refs,
                        };
                    }
                }
                _ => {
                    // Typing anything else closes the dropdown and edits the
                    // field normally.
                    *recent_sel = None;
                    let ed = if *field == 0 {
                        &mut w.url
                    } else {
                        &mut w.token
                    };
                    apply_edit_key(ed, key);
                }
            },
            RemoteStage::Loading { .. } => {
                if key.code == KeyCode::Esc {
                    return self.close_remote(w);
                }
            }
            RemoteStage::PickRef { refs, filter, sel } => {
                let vis = filter_indices(refs.iter().map(|r| r.label.as_str()), filter);
                match key.code {
                    KeyCode::Esc => return self.close_remote(w),
                    KeyCode::Up => *sel = sel.saturating_sub(1),
                    KeyCode::Down if *sel + 1 < vis.len() => *sel += 1,
                    KeyCode::Enter => {
                        if let Some(&ri) = vis.get(*sel) {
                            let choice = refs[ri].clone();
                            w.chosen_ref = Some(choice.clone());
                            w.rx =
                                Some(spawn_git_files(w.url.text(), w.token_opt(), choice.gitref));
                            w.stage = RemoteStage::Loading {
                                phase: LoadPhase::Files,
                            };
                        }
                    }
                    KeyCode::Backspace => {
                        filter.pop();
                        *sel = 0;
                    }
                    KeyCode::Char(c) => {
                        filter.push(c);
                        *sel = 0;
                    }
                    _ => {}
                }
            }
            RemoteStage::PickFile { files, filter, sel } => {
                let vis = filter_indices(files.iter().map(|s| s.as_str()), filter);
                match key.code {
                    KeyCode::Esc => return self.close_remote(w),
                    KeyCode::Up => *sel = sel.saturating_sub(1),
                    KeyCode::Down if *sel + 1 < vis.len() => *sel += 1,
                    KeyCode::Enter => {
                        if let (Some(&fi), Some(repo)) = (vis.get(*sel), w.repo.clone()) {
                            let path = files[fi].clone();
                            w.selected_path = Some(path.clone());
                            w.rx = Some(spawn_git_checkout(repo, path));
                            w.stage = RemoteStage::Loading {
                                phase: LoadPhase::File,
                            };
                        }
                    }
                    KeyCode::Backspace => {
                        filter.pop();
                        *sel = 0;
                    }
                    KeyCode::Char(c) => {
                        filter.push(c);
                        *sel = 0;
                    }
                    _ => {}
                }
            }
            RemoteStage::PickWorkspaceFilter { sel } => match key.code {
                KeyCode::Esc => return self.close_remote(w),
                KeyCode::Up => *sel = sel.saturating_sub(1),
                KeyCode::Down => *sel = (*sel + 1).min(WorkspaceGitFilter::ALL.len() - 1),
                KeyCode::Enter => {
                    let choice = WorkspaceGitFilter::ALL[*sel];
                    w.chosen_workspace_filter = Some(choice);
                    let matched: Vec<String> = w
                        .files
                        .iter()
                        .filter(|p| choice.matches(p))
                        .cloned()
                        .collect();
                    if matched.is_empty() {
                        let s = Strings::for_language(&self.language);
                        w.stage = RemoteStage::Error(s.git_workspace_no_matches.to_string());
                    } else if let Some(repo) = w.repo.clone() {
                        w.rx = Some(spawn_git_checkout_workspace(repo, matched));
                        w.stage = RemoteStage::Loading {
                            phase: LoadPhase::WorkspaceFiles,
                        };
                    }
                }
                _ => {}
            },
            RemoteStage::Error(_) => return self.close_remote(w),
        }
        self.overlay = Some(Overlay::RemoteGit(w));
    }

    /// Poll the wizard's in-flight git operation (called each frame).
    pub(crate) fn poll_git_updates(&mut self) {
        if !matches!(self.overlay, Some(Overlay::RemoteGit(_))) {
            return;
        }
        let Some(Overlay::RemoteGit(mut w)) = self.overlay.take() else {
            return;
        };
        let Some(rx) = w.rx.as_ref() else {
            self.overlay = Some(Overlay::RemoteGit(w));
            return;
        };
        match rx.try_recv() {
            Ok(msg) => {
                w.rx = None;
                let keep_open = self.apply_git_msg(&mut w, msg);
                if keep_open {
                    self.overlay = Some(Overlay::RemoteGit(w));
                } else if let Some(repo) = &w.repo {
                    git_remote::cleanup(repo);
                }
            }
            Err(mpsc::TryRecvError::Empty) => self.overlay = Some(Overlay::RemoteGit(w)),
            Err(mpsc::TryRecvError::Disconnected) => {
                w.rx = None;
                self.overlay = Some(Overlay::RemoteGit(w));
            }
        }
    }

    /// Apply a completed git message to the wizard. Returns whether the wizard
    /// should stay open (false = a file was loaded, close it).
    pub(crate) fn apply_git_msg(&mut self, w: &mut RemoteWizard, msg: GitMsg) -> bool {
        match msg {
            GitMsg::Refs(Ok(refs)) => {
                let s = Strings::for_language(&self.language);
                w.stage = RemoteStage::PickRef {
                    refs: build_ref_choices(&refs, &s),
                    filter: String::new(),
                    sel: 0,
                };
                true
            }
            GitMsg::Files(Ok((files, repo, sha))) => {
                w.repo = Some(repo);
                w.files = files.clone();
                w.chosen_sha = Some(sha);
                w.stage = if w.kind == RemoteKind::Workspace {
                    RemoteStage::PickWorkspaceFilter { sel: 0 }
                } else {
                    RemoteStage::PickFile {
                        files,
                        filter: String::new(),
                        sel: 0,
                    }
                };
                true
            }
            GitMsg::Workspace(Ok(repo)) => {
                self.remember_git_url(&w.url.text());
                let name = file_stem(&w.url.text(), "workspace");
                let origin = self.build_workspace_git_origin(w);
                // Ask the user whether to keep this download temporary (the
                // old default behaviour) or save it to a permanent, chosen
                // location right away — see `Overlay::WorkspaceStorageChoice`.
                self.overlay = Some(Overlay::WorkspaceStorageChoice {
                    repo: repo.clone(),
                    name,
                    origin,
                    sel: 0,
                });
                // Ownership of the temp repo dir now belongs to the pending
                // choice/tab — clear it here so `poll_git_updates`'s
                // close-time cleanup (which deletes anything left in
                // `w.repo`) doesn't remove the files we just downloaded.
                w.repo = None;
                false
            }
            GitMsg::Content(Ok(text)) => {
                let path = w.selected_path.clone().unwrap_or_default();
                self.remember_git_url(&w.url.text());
                let origin = self.build_git_origin(w);
                match w.kind {
                    RemoteKind::Collection => {
                        let name = collection_name_from_path(&path, "remote");
                        if self.load_collection_text(name, &text, None) {
                            let ci = self.active_tab;
                            self.collections[ci].git_origin = origin;
                        }
                        false
                    }
                    RemoteKind::Environment => {
                        let name = env_name_from_path(&path, "remote");
                        self.load_environment_text(name, &text, None, origin);
                        false
                    }
                    // A Workspace load never reaches `PickFile`/`Content` —
                    // it takes the `PickWorkspaceFilter` -> `GitMsg::Workspace`
                    // path instead (see above). Unreachable in practice.
                    RemoteKind::Workspace => false,
                }
            }
            GitMsg::Refs(Err(e))
            | GitMsg::Files(Err(e))
            | GitMsg::Content(Err(e))
            | GitMsg::Workspace(Err(e)) => {
                w.stage = RemoteStage::Error(e);
                true
            }
        }
    }

    /// Build the [`GitOrigin`] for the file the wizard just checked out, from
    /// the ref chosen in `PickRef` and the path chosen in `PickFile`. `None` if
    /// either piece of information is missing (shouldn't happen in practice —
    /// both are set before a checkout is ever spawned).
    fn build_git_origin(&self, w: &RemoteWizard) -> Option<GitOrigin> {
        let choice = w.chosen_ref.as_ref()?;
        let path = w.selected_path.clone()?;
        let (ref_kind, ref_name) = git_remote::parse_ref_kind(&choice.gitref);
        Some(GitOrigin {
            repo_url: w.url.text(),
            path,
            ref_kind,
            ref_name,
        })
    }

    /// Build the [`WorkspaceGitOrigin`] for a Workspace whose files just
    /// finished downloading, from the ref chosen in `PickRef`, the commit
    /// sha resolved in `GitMsg::Files`, and the filter chosen in
    /// `PickWorkspaceFilter`. `None` if any piece is missing (shouldn't
    /// happen in practice — all three are set before the checkout is spawned).
    fn build_workspace_git_origin(&self, w: &RemoteWizard) -> Option<WorkspaceGitOrigin> {
        let choice = w.chosen_ref.as_ref()?;
        let commit_sha = w.chosen_sha.clone()?;
        let filter = w.chosen_workspace_filter?;
        let (ref_kind, ref_name) = git_remote::parse_ref_kind(&choice.gitref);
        Some(WorkspaceGitOrigin {
            repo_url: w.url.text(),
            commit_sha,
            ref_kind,
            ref_name,
            filter,
        })
    }

    /// Close the "save to git" wizard. Unlike the load wizard there is no
    /// temp repo to clean up here — the background push manages (and always
    /// cleans up) its own throwaway repo internally, in one shot; the wizard
    /// itself is simply dropped by the caller.
    pub(crate) fn close_git_save(&mut self) {
        self.overlay = None;
    }

    /// Handle a key while the "save to git" wizard is open.
    pub(crate) fn on_key_git_save(&mut self, mut w: Box<GitSaveWizard>, key: KeyEvent) {
        match &mut w.stage {
            GitSaveStage::Connect { field } => match key.code {
                KeyCode::Esc => return self.close_git_save(),
                KeyCode::Tab | KeyCode::BackTab | KeyCode::Up | KeyCode::Down => {
                    *field = 1 - *field
                }
                KeyCode::Enter => {
                    if w.url.text().trim().is_empty() {
                        let s = Strings::for_language(&self.language);
                        w.stage = GitSaveStage::Error(s.git_url_required.to_string());
                    } else {
                        w.stage = GitSaveStage::ChoosePaths { field: 0 };
                    }
                }
                _ => {
                    let ed = if *field == 0 {
                        &mut w.url
                    } else {
                        &mut w.token
                    };
                    apply_edit_key(ed, key);
                }
            },
            GitSaveStage::ChoosePaths { field } => {
                let has_env = w.has_env;
                let include_env = w.include_env;
                let mut visible = vec![0u8];
                if has_env {
                    visible.push(1);
                    if include_env {
                        visible.push(2);
                    }
                }
                match key.code {
                    KeyCode::Esc => return self.close_git_save(),
                    KeyCode::Tab | KeyCode::BackTab | KeyCode::Up | KeyCode::Down => {
                        let idx = visible.iter().position(|f| f == field).unwrap_or(0);
                        let back = matches!(key.code, KeyCode::BackTab | KeyCode::Up);
                        let n = visible.len();
                        let next = if back {
                            (idx + n - 1) % n
                        } else {
                            (idx + 1) % n
                        };
                        *field = visible[next];
                    }
                    KeyCode::Char(' ') if *field == 1 => w.include_env = !w.include_env,
                    KeyCode::Enter => {
                        let paths_ok = !w.collection_path.text().trim().is_empty()
                            && (!w.include_env || !w.env_path.text().trim().is_empty());
                        if paths_ok {
                            let url = w.url.text();
                            let token = w.token_opt();
                            w.rx = Some(spawn_git_save_refs(url, token));
                            w.stage = GitSaveStage::ChooseTarget {
                                sel: None,
                                refs: None,
                            };
                        }
                    }
                    _ => {
                        let f = *field;
                        let ed = match f {
                            2 => Some(&mut w.env_path),
                            1 => None, // the checkbox has no text to type into
                            _ => Some(&mut w.collection_path),
                        };
                        if let Some(ed) = ed {
                            apply_edit_key(ed, key);
                        }
                    }
                }
            }
            GitSaveStage::ChooseTarget { sel, refs } => {
                let branches = refs
                    .as_ref()
                    .map(|r| r.branches.clone())
                    .unwrap_or_default();
                match key.code {
                    KeyCode::Esc if sel.is_some() => *sel = None,
                    KeyCode::Esc => return self.close_git_save(),
                    KeyCode::Tab | KeyCode::BackTab => {
                        w.target_kind = if w.target_kind == GitSaveTarget::Branch {
                            GitSaveTarget::Tag
                        } else {
                            GitSaveTarget::Branch
                        };
                    }
                    KeyCode::Down if sel.is_none() => {
                        if !branches.is_empty() {
                            *sel = Some(0);
                        }
                    }
                    KeyCode::Down => {
                        if let Some(i) = *sel {
                            *sel = Some((i + 1).min(branches.len().saturating_sub(1)));
                        }
                    }
                    KeyCode::Up => {
                        if let Some(i) = *sel {
                            *sel = if i == 0 { None } else { Some(i - 1) };
                        }
                    }
                    KeyCode::Enter if sel.is_some() => {
                        if let Some(name) = sel.and_then(|i| branches.get(i)) {
                            w.target_name = Editor::new(name, false);
                            w.target_kind = GitSaveTarget::Branch;
                        }
                        *sel = None;
                    }
                    KeyCode::Enter => {
                        let name = w.target_name.text().trim().to_string();
                        if !name.is_empty() {
                            let is_existing_branch = w.target_kind == GitSaveTarget::Branch
                                && branches.iter().any(|b| b == &name);
                            w.target_intent = if is_existing_branch {
                                TargetIntent::ExistingBranch
                            } else {
                                TargetIntent::NewRef
                            };
                            if w.commit_msg.text().trim().is_empty() {
                                let ci = w.ci;
                                let default_msg =
                                    format!("Update {} via PaperBoy", self.collections[ci].name);
                                w.commit_msg = Editor::new(&default_msg, false);
                            }
                            w.stage = GitSaveStage::CommitMessage;
                        }
                    }
                    KeyCode::Backspace if sel.is_none() => {
                        w.target_name.backspace();
                    }
                    KeyCode::Char(c) if sel.is_none() => {
                        w.target_name.insert(c);
                    }
                    _ => {
                        // Typing anything while the dropdown is open closes it
                        // and edits the field normally (matches the load
                        // wizard's recent-URL dropdown behaviour).
                        *sel = None;
                        apply_edit_key(&mut w.target_name, key);
                    }
                }
            }
            GitSaveStage::CommitMessage => match key.code {
                KeyCode::Esc => return self.close_git_save(),
                KeyCode::Enter => {
                    if !w.commit_msg.text().trim().is_empty() {
                        let ci = w.ci;
                        let col = &self.collections[ci];
                        let mut files = vec![(w.collection_path.text(), col.to_hurl())];
                        if w.include_env
                            && let Some(env) = w.env.as_ref()
                        {
                            files.push((w.env_path.text(), env.to_vars_text()));
                        }
                        w.rx = Some(spawn_git_save_push(
                            w.url.text(),
                            w.token_opt(),
                            w.origin_gitref.clone(),
                            w.target_kind,
                            w.target_name.text(),
                            w.target_intent,
                            files,
                            w.commit_msg.text(),
                        ));
                        w.stage = GitSaveStage::Pushing;
                    }
                }
                _ => apply_edit_key(&mut w.commit_msg, key),
            },
            GitSaveStage::Pushing => {
                if key.code == KeyCode::Esc {
                    return self.close_git_save();
                }
            }
            GitSaveStage::Done | GitSaveStage::Error(_) => return self.close_git_save(),
        }
        self.overlay = Some(Overlay::GitSave(w));
    }

    /// Poll the "save to git" wizard's in-flight background op (called each
    /// frame).
    pub(crate) fn poll_git_save_updates(&mut self) {
        if !matches!(self.overlay, Some(Overlay::GitSave(_))) {
            return;
        }
        let Some(Overlay::GitSave(mut w)) = self.overlay.take() else {
            return;
        };
        let Some(rx) = w.rx.as_ref() else {
            self.overlay = Some(Overlay::GitSave(w));
            return;
        };
        match rx.try_recv() {
            Ok(msg) => {
                w.rx = None;
                let keep_open = self.apply_git_save_msg(&mut w, msg);
                self.overlay = if keep_open {
                    Some(Overlay::GitSave(w))
                } else {
                    None
                };
            }
            Err(mpsc::TryRecvError::Empty) => self.overlay = Some(Overlay::GitSave(w)),
            Err(mpsc::TryRecvError::Disconnected) => {
                w.rx = None;
                self.overlay = Some(Overlay::GitSave(w));
            }
        }
    }

    /// Apply a completed "save to git" message. Returns whether the wizard
    /// should stay open (both a completed push and a failure stay open, to
    /// show a result/error until the user dismisses it).
    pub(crate) fn apply_git_save_msg(&mut self, w: &mut GitSaveWizard, msg: GitSaveMsg) -> bool {
        match msg {
            GitSaveMsg::Refs(Ok(refs)) => {
                if let GitSaveStage::ChooseTarget { refs: r, .. } = &mut w.stage {
                    *r = Some(refs);
                }
                true
            }
            GitSaveMsg::Refs(Err(e)) => {
                w.stage = GitSaveStage::Error(e);
                true
            }
            GitSaveMsg::Pushed(Ok(())) => {
                self.finish_git_save(w);
                w.stage = GitSaveStage::Done;
                true
            }
            GitSaveMsg::Pushed(Err(err)) => {
                let s = Strings::for_language(&self.language);
                w.stage = GitSaveStage::Error(match err {
                    GitSaveError::TagExists => s.git_tag_exists.to_string(),
                    GitSaveError::RefExistsRace => s.git_ref_exists_race.to_string(),
                    GitSaveError::Other(e) => e,
                });
                true
            }
        }
    }

    /// After a successful push: clear the "new"/"modified" markers (same as
    /// a local Save) and, for a **branch** target only, remember it as the
    /// collection's (and, if included, the environment's) new git origin. A
    /// tag-target save clears the markers too but leaves the remembered
    /// branch origin untouched, per spec.
    fn finish_git_save(&mut self, w: &GitSaveWizard) {
        let ci = w.ci;
        self.mark_collection_saved(ci);
        if w.include_env
            && let Some(env_id) = w.env.as_ref().map(|e| e.id)
        {
            self.mark_env_saved(env_id);
        }
        if w.target_kind == GitSaveTarget::Branch {
            self.collections[ci].git_origin = Some(GitOrigin {
                repo_url: w.url.text(),
                path: w.collection_path.text(),
                ref_kind: RefKind::Branch,
                ref_name: w.target_name.text(),
            });
            if w.include_env
                && let Some(env_id) = w.env.as_ref().map(|e| e.id)
                && let Some(env) = self.global_envs.iter_mut().find(|e| e.id == env_id)
            {
                env.git_origin = Some(GitOrigin {
                    repo_url: w.url.text(),
                    path: w.env_path.text(),
                    ref_kind: RefKind::Branch,
                    ref_name: w.target_name.text(),
                });
            }
        }
        self.remember_git_url(&w.url.text());
        self.save_state();
        self.status = Some(Status::GitSaved);
    }
}

pub(crate) fn file_stem(path: &str, fallback: &str) -> String {
    std::path::Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| fallback.to_string())
}

/// Display name for an environment file. Like [`file_stem`], but a leading-dot
/// filename keeps its full name (so `.env.dev-au` stays `.env.dev-au` rather
/// than being truncated to `.env`, since the part after the dot is a
/// meaningful suffix here, not a throwaway extension).
pub(crate) fn env_name_from_path(path: &str, fallback: &str) -> String {
    match std::path::Path::new(path).file_name() {
        Some(name) if name.to_string_lossy().starts_with('.') => name.to_string_lossy().into_owned(),
        _ => file_stem(path, fallback),
    }
}

/// Display name for a collection file. Only the known collection extensions
/// (`.hurl`/`.json`) are hidden; any other suffix is kept verbatim (so a file
/// like `env.dev-au` shows in full rather than losing its `.dev-au`).
pub(crate) fn collection_name_from_path(path: &str, fallback: &str) -> String {
    let p = std::path::Path::new(path);
    match p.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()) {
        Some(ext) if ext == "hurl" || ext == "json" => file_stem(path, fallback),
        _ => p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| fallback.to_string()),
    }
}
